//! Clipboard and primary selection: the data-device objects, offers and serials.
//!
//! Performs only clipboard operations the core has allowed, keeping the ordinary clipboard
//! (`wl_data_device`) and the primary selection (`zwp_primary_selection_device_v1`) apart. The
//! seat that last had input does the work, with the serial of its latest input event.
//!
//! Offering: [`Host::clipboard_write`] takes the content in each MIME type and serves every
//! request for it through a non-blocking pipe inside the event loop. Receiving:
//! [`Host::clipboard_read`] picks the first requested type the offer has, reads the pipe as data
//! arrives, and reports [`HostEvent::ClipboardRead`]. Every transfer is bounded: at most
//! [`MAX_TRANSFER`] bytes (a larger read fails with [`ServiceError::TooLarge`]), at most
//! [`TRANSFER_TIMEOUT`] for the other side to finish, and at most [`MAX_WRITES`] transfers out at
//! once; [`Host::clipboard_cancel`] abandons a read. A stalled source or reader never blocks the
//! loop. UTF-8 text is also offered and accepted as `UTF8_STRING`, the name X11 programs use.
//!
//! Drag-and-drop is a related but separate capability and is not implemented: drag offers are
//! ignored, and a file drop grants no filesystem access.
//!
//! Must not own: the decision whether content may read or write the clipboard.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::rc::Rc;
use std::time::{Duration, Instant};

use calloop::{Interest, PostAction, RegistrationToken};
use sighurt_api::ids::RequestId;
use sighurt_api::services::{ClipboardData, Selection, ServiceError};
use smithay_client_toolkit::data_device_manager::data_device::DataDeviceHandler;
use smithay_client_toolkit::data_device_manager::data_offer::{DataOfferHandler, DragOffer};
use smithay_client_toolkit::data_device_manager::data_source::{
    CopyPasteSource, DataSourceHandler,
};
use smithay_client_toolkit::data_device_manager::WritePipe;
use smithay_client_toolkit::primary_selection::device::PrimarySelectionDeviceHandler;
use smithay_client_toolkit::primary_selection::selection::{
    PrimarySelectionSource, PrimarySelectionSourceHandler,
};
use wayland_client::protocol::wl_data_device::WlDataDevice;
use wayland_client::protocol::wl_data_device_manager::DndAction;
use wayland_client::protocol::wl_data_source::WlDataSource;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, QueueHandle};
use wayland_protocols::wp::primary_selection::zv1::client::zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1;
use wayland_protocols::wp::primary_selection::zv1::client::zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1;

use crate::connection::{Host, HostEvent, TimerEvent};

/// The most bytes one transfer moves, whatever the requester allows.
pub const MAX_TRANSFER: usize = 16 << 20;

/// How long the other side of a transfer has to finish it.
pub const TRANSFER_TIMEOUT: Duration = Duration::from_secs(10);

/// Transfers out (other programs pasting from us) at once; older ones are dropped beyond it.
pub const MAX_WRITES: usize = 8;

const UTF8_TEXT: &str = "text/plain;charset=utf-8";
const X11_UTF8_TEXT: &str = "UTF8_STRING";

/// Which offered type to read for the `wanted` ones, most preferred first: `(offered, wanted)`.
/// Types match without regard to case; UTF-8 text also matches its X11 name.
pub(crate) fn choose_type(offered: &[String], wanted: &[String]) -> Option<(String, String)> {
    wanted.iter().find_map(|want| {
        let exact = offered.iter().find(|o| o.eq_ignore_ascii_case(want));
        let alias = || {
            want.eq_ignore_ascii_case(UTF8_TEXT)
                .then(|| offered.iter().find(|o| o.as_str() == X11_UTF8_TEXT))
                .flatten()
        };
        exact.or_else(alias).map(|o| (o.clone(), want.clone()))
    })
}

/// The item that answers a request for `mime`.
fn item_for<'a>(items: &'a [ClipboardData], mime: &str) -> Option<&'a ClipboardData> {
    let wanted = if mime == X11_UTF8_TEXT {
        UTF8_TEXT
    } else {
        mime
    };
    items
        .iter()
        .find(|i| i.mime_type.eq_ignore_ascii_case(wanted))
}

/// Accumulates a read, enforcing its limit.
#[derive(Debug)]
pub(crate) struct Incoming {
    data: Vec<u8>,
    limit: usize,
}

impl Incoming {
    pub fn new(limit: usize) -> Self {
        Self {
            data: Vec::new(),
            limit: limit.min(MAX_TRANSFER),
        }
    }

    /// Adds `chunk`; `Err` once more than the limit arrived.
    pub fn push(&mut self, chunk: &[u8]) -> Result<(), ServiceError> {
        if self.data.len() + chunk.len() > self.limit {
            return Err(ServiceError::TooLarge);
        }
        self.data.extend_from_slice(chunk);
        Ok(())
    }
}

/// What we offer on one selection.
struct Offer {
    items: Rc<[ClipboardData]>,
    // Dropping the source withdraws the offer.
    _source: Source,
    wl: SourceId,
}

// Held only for their `Drop`, which withdraws the offer.
#[allow(dead_code)]
enum Source {
    Clipboard(CopyPasteSource),
    Primary(PrimarySelectionSource),
}

#[derive(Clone, PartialEq, Eq)]
enum SourceId {
    Clipboard(WlDataSource),
    Primary(ZwpPrimarySelectionSourceV1),
}

struct ReadTransfer {
    request: RequestId,
    mime_type: String,
    incoming: Incoming,
    pipe: Option<RegistrationToken>,
    timer: Option<RegistrationToken>,
}

struct WriteTransfer {
    data: Rc<[ClipboardData]>,
    item: usize,
    written: usize,
    pipe: Option<RegistrationToken>,
    timer: Option<RegistrationToken>,
    started: Instant,
}

/// The host's clipboard state.
#[derive(Default)]
pub(crate) struct Clipboard {
    offers: [Option<Offer>; 2],
    reads: HashMap<u64, ReadTransfer>,
    writes: HashMap<u64, WriteTransfer>,
    next: u64,
}

fn slot(selection: Selection) -> usize {
    match selection {
        Selection::Clipboard => 0,
        Selection::Primary => 1,
    }
}

/// A non-blocking, close-on-exec pipe: `(read end, write end)`.
fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    use rustix::pipe::{pipe_with, PipeFlags};
    Ok(pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK)?)
}

impl Host {
    /// The seat clipboard operations use: the one with the latest input.
    fn clipboard_seat(&self) -> Option<usize> {
        let active = self.active_seat;
        self.seats
            .iter()
            .position(|s| Some(s.id) == active)
            .or_else(|| self.seats.iter().position(|s| s.serial.is_some()))
    }

    /// Replaces `selection`'s content with `items`, one per MIME type. Other programs read it
    /// from the host until something else takes the selection.
    pub fn clipboard_write(
        &mut self,
        selection: Selection,
        items: Vec<ClipboardData>,
    ) -> Result<(), ServiceError> {
        if items.iter().map(|i| i.data.len()).sum::<usize>() > MAX_TRANSFER {
            return Err(ServiceError::TooLarge);
        }
        let unavailable = || ServiceError::Failed("no seat can set the clipboard yet".into());
        let index = self.clipboard_seat().ok_or_else(unavailable)?;
        let seat = &self.seats[index];
        let serial = seat.serial.ok_or_else(unavailable)?;
        let mut types: Vec<String> = items.iter().map(|i| i.mime_type.clone()).collect();
        if types.iter().any(|t| t.eq_ignore_ascii_case(UTF8_TEXT)) {
            types.push(X11_UTF8_TEXT.into());
        }
        let (source, wl) = match selection {
            Selection::Clipboard => {
                let (Some(manager), Some(device)) = (&self.globals.data_device, &seat.data_device)
                else {
                    return Err(ServiceError::Unsupported);
                };
                let source = manager.create_copy_paste_source(&self.qh, types);
                source.set_selection(device, serial);
                let wl = SourceId::Clipboard(source.inner().clone());
                (Source::Clipboard(source), wl)
            }
            Selection::Primary => {
                let (Some(manager), Some(device)) = (&self.globals.primary, &seat.primary) else {
                    return Err(ServiceError::Unsupported);
                };
                let source = manager.create_selection_source(&self.qh, types);
                source.set_selection(device, serial);
                let wl = SourceId::Primary(source.inner().clone());
                (Source::Primary(source), wl)
            }
        };
        self.clipboard.offers[slot(selection)] = Some(Offer {
            items: items.into(),
            _source: source,
            wl,
        });
        Ok(())
    }

    /// Reads `selection` in the first of `mime_types` it offers, at most `limit` bytes. The
    /// result arrives as [`HostEvent::ClipboardRead`] for `request`, unless it is cancelled.
    pub fn clipboard_read(
        &mut self,
        request: RequestId,
        selection: Selection,
        mime_types: &[String],
        limit: usize,
    ) {
        if let Err(error) = self.start_read(request, selection, mime_types, limit) {
            self.events.push_back(HostEvent::ClipboardRead {
                request,
                result: Err(error),
            });
        }
    }

    fn start_read(
        &mut self,
        request: RequestId,
        selection: Selection,
        mime_types: &[String],
        limit: usize,
    ) -> Result<(), ServiceError> {
        let index = self.clipboard_seat().ok_or(ServiceError::Unsupported)?;
        let seat = &self.seats[index];
        let empty = || ServiceError::Failed("the clipboard is empty".into());
        let no_type =
            || ServiceError::Failed("the clipboard holds none of the requested types".into());
        let failed = |error: io::Error| {
            ServiceError::Failed(format!("reading the clipboard failed: {error}"))
        };
        let (read_end, mime_type) = match selection {
            Selection::Clipboard => {
                let device = seat.data_device.as_ref().ok_or(ServiceError::Unsupported)?;
                let offer = device.data().selection_offer().ok_or_else(empty)?;
                let offered = offer.with_mime_types(|types| types.to_vec());
                let (offered, wanted) = choose_type(&offered, mime_types).ok_or_else(no_type)?;
                let (read_end, write_end) = pipe().map_err(failed)?;
                offer.inner().receive(offered, write_end.as_fd());
                (read_end, wanted)
            }
            Selection::Primary => {
                let device = seat.primary.as_ref().ok_or(ServiceError::Unsupported)?;
                let offer = device.data().selection_offer().ok_or_else(empty)?;
                let offered = offer.with_mime_types(|types| types.to_vec());
                let (offered, wanted) = choose_type(&offered, mime_types).ok_or_else(no_type)?;
                let (read_end, write_end) = pipe().map_err(failed)?;
                offer.receive_to_fd(offered, write_end);
                (read_end, wanted)
            }
        };
        // Our write end is closed now (sent and dropped); the source holds its own copy.
        let id = self.clipboard.next;
        self.clipboard.next += 1;
        let pipe = self.loop_ops.pipe(File::from(read_end), Interest::READ, id);
        let timer = self
            .loop_ops
            .timer(Instant::now() + TRANSFER_TIMEOUT, TimerEvent::Transfer(id));
        self.clipboard.reads.insert(
            id,
            ReadTransfer {
                request,
                mime_type,
                incoming: Incoming::new(limit),
                pipe,
                timer,
            },
        );
        Ok(())
    }

    /// Abandons the read for `request`: no event follows for it.
    pub fn clipboard_cancel(&mut self, request: RequestId) {
        let id = self
            .clipboard
            .reads
            .iter()
            .find(|(_, r)| r.request == request)
            .map(|(&id, _)| id);
        if let Some(read) = id.and_then(|id| self.clipboard.reads.remove(&id)) {
            for token in [read.pipe, read.timer].into_iter().flatten() {
                self.loop_ops.remove(token);
            }
        }
    }

    /// Starts sending `mime` of the offer `source` to `fd`.
    fn start_write(&mut self, source: SourceId, mime: String, fd: OwnedFd) {
        let Some(offer) = self
            .clipboard
            .offers
            .iter()
            .flatten()
            .find(|o| o.wl == source)
        else {
            return; // A withdrawn offer: dropping the fd tells the reader.
        };
        let Some(item) = offer
            .items
            .iter()
            .position(|i| Some(i) == item_for(&offer.items, &mime))
        else {
            return;
        };
        let data = Rc::clone(&offer.items);
        if self.clipboard.writes.len() >= MAX_WRITES {
            let oldest = self
                .clipboard
                .writes
                .iter()
                .min_by_key(|(_, w)| w.started)
                .map(|(&id, _)| id);
            if let Some(write) = oldest.and_then(|id| self.clipboard.writes.remove(&id)) {
                for token in [write.pipe, write.timer].into_iter().flatten() {
                    self.loop_ops.remove(token);
                }
            }
        }
        if rustix::io::ioctl_fionbio(&fd, true).is_err() {
            return;
        }
        let id = self.clipboard.next;
        self.clipboard.next += 1;
        let pipe = self.loop_ops.pipe(File::from(fd), Interest::WRITE, id);
        let timer = self
            .loop_ops
            .timer(Instant::now() + TRANSFER_TIMEOUT, TimerEvent::Transfer(id));
        self.clipboard.writes.insert(
            id,
            WriteTransfer {
                data,
                item,
                written: 0,
                pipe,
                timer,
                started: Instant::now(),
            },
        );
    }

    /// Transfer `id`'s pipe is ready. Moves what it can without blocking.
    pub(crate) fn transfer_ready(&mut self, id: u64, file: &File) -> PostAction {
        if let Some(read) = self.clipboard.reads.get_mut(&id) {
            let mut buffer = [0; 64 * 1024];
            let result = loop {
                match (&mut &*file).read(&mut buffer) {
                    Ok(0) => break Some(Ok(())),
                    Ok(n) => {
                        if let Err(error) = read.incoming.push(&buffer[..n]) {
                            break Some(Err(error));
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break None,
                    Err(e) => {
                        break Some(Err(ServiceError::Failed(format!(
                            "reading the clipboard failed: {e}"
                        ))))
                    }
                }
            };
            let Some(result) = result else {
                return PostAction::Continue;
            };
            let read = self.clipboard.reads.remove(&id).expect("found above");
            if let Some(timer) = read.timer {
                self.loop_ops.remove(timer);
            }
            let result = result.map(|()| ClipboardData {
                mime_type: read.mime_type,
                data: read.incoming.data,
            });
            self.events.push_back(HostEvent::ClipboardRead {
                request: read.request,
                result,
            });
            return PostAction::Remove;
        }
        if let Some(write) = self.clipboard.writes.get_mut(&id) {
            let data = &write.data[write.item].data;
            let done = loop {
                if write.written == data.len() {
                    break true;
                }
                match (&mut &*file).write(&data[write.written..]) {
                    Ok(n) => write.written += n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break false,
                    Err(_) => break true, // The reader went away.
                }
            };
            if !done {
                return PostAction::Continue;
            }
            if let Some(timer) = self.clipboard.writes.remove(&id).and_then(|w| w.timer) {
                self.loop_ops.remove(timer);
            }
            return PostAction::Remove;
        }
        PostAction::Remove
    }

    /// Transfer `id` ran out of time.
    pub(crate) fn transfer_timed_out(&mut self, id: u64) {
        if let Some(read) = self.clipboard.reads.remove(&id) {
            if let Some(pipe) = read.pipe {
                self.loop_ops.remove(pipe);
            }
            self.events.push_back(HostEvent::ClipboardRead {
                request: read.request,
                result: Err(ServiceError::Failed(
                    "the clipboard's owner did not answer in time".into(),
                )),
            });
        }
        if let Some(pipe) = self.clipboard.writes.remove(&id).and_then(|w| w.pipe) {
            self.loop_ops.remove(pipe);
        }
    }

    fn source_cancelled(&mut self, source: SourceId) {
        for offer in &mut self.clipboard.offers {
            if offer.as_ref().is_some_and(|o| o.wl == source) {
                *offer = None;
            }
        }
    }
}

impl DataDeviceHandler for Host {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataDevice,
        _: f64,
        _: f64,
        _: &WlSurface,
    ) {
    }
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}
    // The offer is read when asked for, from the device's current selection.
    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
}

impl DataOfferHandler for Host {
    fn source_actions(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
    fn selected_action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
}

impl DataSourceHandler for Host {
    fn accept_mime(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: Option<String>,
    ) {
    }

    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        source: &WlDataSource,
        mime: String,
        fd: WritePipe,
    ) {
        self.start_write(SourceId::Clipboard(source.clone()), mime, fd.into());
    }

    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        self.source_cancelled(SourceId::Clipboard(source.clone()));
    }

    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

impl PrimarySelectionDeviceHandler for Host {
    fn selection(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ZwpPrimarySelectionDeviceV1,
    ) {
    }
}

impl PrimarySelectionSourceHandler for Host {
    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        source: &ZwpPrimarySelectionSourceV1,
        mime: String,
        fd: WritePipe,
    ) {
        self.start_write(SourceId::Primary(source.clone()), mime, fd.into());
    }

    fn cancelled(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        source: &ZwpPrimarySelectionSourceV1,
    ) {
        self.source_cancelled(SourceId::Primary(source.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn the_first_wanted_type_on_offer_wins() {
        let offered = types(&["text/html", "text/plain;charset=utf-8", "UTF8_STRING"]);
        let wanted = types(&["image/png", "text/plain;charset=utf-8", "text/html"]);
        assert_eq!(
            choose_type(&offered, &wanted),
            Some((UTF8_TEXT.into(), UTF8_TEXT.into()))
        );
        assert_eq!(choose_type(&offered, &types(&["image/png"])), None);
    }

    #[test]
    fn utf8_text_matches_its_x11_name_and_any_case() {
        let x11 = types(&["UTF8_STRING", "STRING"]);
        assert_eq!(
            choose_type(&x11, &types(&[UTF8_TEXT])),
            Some(("UTF8_STRING".into(), UTF8_TEXT.into()))
        );
        let upper = types(&["text/plain;charset=UTF-8"]);
        assert_eq!(
            choose_type(&upper, &types(&[UTF8_TEXT])).map(|(o, _)| o),
            Some("text/plain;charset=UTF-8".into())
        );
        let items = [ClipboardData {
            mime_type: UTF8_TEXT.into(),
            data: b"hej".to_vec(),
        }];
        assert_eq!(item_for(&items, X11_UTF8_TEXT), Some(&items[0]));
        assert_eq!(item_for(&items, "text/html"), None);
    }

    #[test]
    fn reads_stop_at_their_limit() {
        let mut incoming = Incoming::new(10);
        assert_eq!(incoming.push("räksmörg".as_bytes()), Ok(()));
        assert_eq!(incoming.push("ås".as_bytes()), Err(ServiceError::TooLarge));
        let capped = Incoming::new(usize::MAX);
        assert_eq!(capped.limit, MAX_TRANSFER, "the host's own cap applies");
    }
}
