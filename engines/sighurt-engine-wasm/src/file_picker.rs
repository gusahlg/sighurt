//! Host-side native file and folder picker for Oxide guest modules.
//!
//! Guests call `api_file_pick` / `api_folder_pick` to invoke the OS picker
//! and receive opaque `u32` handles. Paths never cross the sandbox boundary;
//! the host keeps a `HashMap<handle, PathBuf>` and exposes reads via
//! `api_file_read`, `api_file_read_range`, and `api_file_metadata`.
//!
//! `api_folder_entries` lists a picked directory as JSON, pre-allocating
//! sub-handles for each child so the guest can read files without ever
//! seeing the underlying path.
//!
//! A dialog opens only within [`USER_ACTIVATION`](crate::capabilities::USER_ACTIVATION) of a
//! click or key press in the page, one per activation, and never for a worker
//! ([`dialog_allowed`]). A path has one handle however often it is listed, and a document has
//! at most [`MAX_HANDLES`]; a listing names at most [`MAX_FOLDER_ENTRIES`] entries. Reads go
//! into the guest's buffer a chunk at a time once the buffer is known to be in guest memory.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use anyhow::Result;
use wasmtime::{Caller, Linker};

use crate::capabilities::{
    dialog_allowed, guest_range, guest_str, next_handle, read_into_guest, write_guest, HostState,
};
use crate::engine::Wait;

/// Most handles a document may hold: picked files and folders and the entries listed in them.
pub const MAX_HANDLES: usize = 65_536;
/// Most entries one `api_folder_entries` call lists.
pub const MAX_FOLDER_ENTRIES: usize = 10_000;

/// One picked file or folder, keyed by an opaque handle the guest holds.
pub struct PickedEntry {
    pub path: PathBuf,
    pub is_dir: bool,
}

/// All picker state for a tab. A path keeps the handle it was first given.
#[derive(Default)]
pub struct FilePickerState {
    entries: HashMap<u32, PickedEntry>,
    /// The handle of each path in `entries`.
    handles: HashMap<PathBuf, u32>,
    last_id: u32,
}

impl FilePickerState {
    /// The handle of `path`: the one it has, or a new one; `None` once there are
    /// [`MAX_HANDLES`].
    fn alloc(&mut self, path: PathBuf, is_dir: bool) -> Option<u32> {
        if let Some(&id) = self.handles.get(&path) {
            return Some(id);
        }
        if self.entries.len() >= MAX_HANDLES {
            return None;
        }
        let id = next_handle(&mut self.last_id);
        self.handles.insert(path.clone(), id);
        self.entries.insert(id, PickedEntry { path, is_dir });
        Some(id)
    }

    fn get(&self, handle: u32) -> Option<&PickedEntry> {
        self.entries.get(&handle)
    }
}

fn mime_for_extension(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        "txt" => "text/plain",
        "md" => "text/markdown",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" => "text/javascript",
        "json" => "application/json",
        "xml" => "application/xml",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

fn modified_ms(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn file_name_of(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn json_escape(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The entry behind `handle`, if it is a file (`dir` false) or folder (`dir` true).
fn picked(caller: &Caller<'_, HostState>, handle: u32, dir: Option<bool>) -> Option<PickedEntry> {
    let state = caller.data().file_picker.lock().unwrap();
    let entry = state.get(handle)?;
    (dir.is_none() || dir == Some(entry.is_dir)).then(|| PickedEntry {
        path: entry.path.clone(),
        is_dir: entry.is_dir,
    })
}

/// Writes `bytes` if they fit in `out_cap`: returns their length, `-(length)` when they don't
/// fit, or -2 when the buffer is outside guest memory.
fn write_or_size(
    caller: &mut Caller<'_, HostState>,
    out_ptr: u32,
    out_cap: u32,
    bytes: &[u8],
) -> i64 {
    if bytes.len() > out_cap as usize {
        -(bytes.len() as i64)
    } else if write_guest(caller, out_ptr, bytes) {
        bytes.len() as i64
    } else {
        -2
    }
}

/// Register all file picker host functions.
pub fn register_file_picker_functions(linker: &mut Linker<HostState>) -> Result<()> {
    // api_file_pick(title, title_len, filters, filters_len, multiple, out_ptr, out_cap) -> i32
    //   filters: comma-separated extensions ("png,jpg,gif"); empty string = all files.
    //   out buffer receives u32 handles (little-endian) up to `out_cap / 4`.
    //   Returns count of handles written, or -1 if the user cancelled.
    linker.func_wrap(
        "oxide",
        "api_file_pick",
        |mut caller: Caller<'_, HostState>,
         title_ptr: u32,
         title_len: u32,
         filters_ptr: u32,
         filters_len: u32,
         multiple: u32,
         out_ptr: u32,
         out_cap: u32|
         -> i32 {
            if !guest_range(&caller, out_ptr, out_cap) || !dialog_allowed(caller.data(), "FILE") {
                return -1;
            }
            let title = guest_str(&caller, title_ptr, title_len)
                .unwrap_or_else(|| "Sighurt: Select a file".to_string());
            let filters = guest_str(&caller, filters_ptr, filters_len).unwrap_or_default();

            let mut dialog = rfd::FileDialog::new().set_title(&title);
            let exts: Vec<&str> = filters
                .split(',')
                .map(|s| s.trim().trim_start_matches('.'))
                .filter(|s| !s.is_empty())
                .collect();
            if !exts.is_empty() {
                dialog = dialog.add_filter("Files", &exts);
            }
            let paths = caller.data().budget.wait(Wait::User, || {
                if multiple != 0 {
                    dialog.pick_files().unwrap_or_default()
                } else {
                    dialog.pick_file().into_iter().collect()
                }
            });
            let paths = paths.unwrap_or_default();
            if paths.is_empty() {
                return -1;
            }

            let handles: Vec<u8> = {
                let mut state = caller.data().file_picker.lock().unwrap();
                paths
                    .into_iter()
                    .take((out_cap / 4) as usize)
                    .map_while(|path| state.alloc(path, false))
                    .flat_map(u32::to_le_bytes)
                    .collect()
            };
            if !write_guest(&mut caller, out_ptr, &handles) {
                return -1;
            }
            (handles.len() / 4) as i32
        },
    )?;

    // api_folder_pick(title_ptr, title_len) -> u32
    //   Returns a folder handle, or 0 on cancel.
    linker.func_wrap(
        "oxide",
        "api_folder_pick",
        |caller: Caller<'_, HostState>, title_ptr: u32, title_len: u32| -> u32 {
            if !dialog_allowed(caller.data(), "FOLDER") {
                return 0;
            }
            let title = guest_str(&caller, title_ptr, title_len)
                .unwrap_or_else(|| "Sighurt: Select a folder".to_string());
            let picked = caller.data().budget.wait(Wait::User, || {
                rfd::FileDialog::new().set_title(&title).pick_folder()
            });
            let mut state = caller.data().file_picker.lock().unwrap();
            picked
                .flatten()
                .and_then(|path| state.alloc(path, true))
                .unwrap_or(0)
        },
    )?;

    // api_folder_entries(handle, out_ptr, out_cap) -> i32
    //   Writes JSON array of entries:
    //     [{"name":"a.txt","size":123,"is_dir":false,"handle":42}, ...]
    //   Sub-handles are allocated on the fly so guests can read children
    //   without learning any host path; listing again gives the same ones. At most
    //   `MAX_FOLDER_ENTRIES` entries, and only those that get a handle. Returns bytes written,
    //   -1 on bad handle, -2 on io error, or negative of required size if truncated.
    linker.func_wrap(
        "oxide",
        "api_folder_entries",
        |mut caller: Caller<'_, HostState>, handle: u32, out_ptr: u32, out_cap: u32| -> i32 {
            let Some(dir) = picked(&caller, handle, Some(true)) else {
                return -1;
            };
            let Ok(read_dir) = std::fs::read_dir(&dir.path) else {
                return -2;
            };
            let children: Vec<(PathBuf, bool, u64)> = read_dir
                .flatten()
                .take(MAX_FOLDER_ENTRIES)
                .filter_map(|entry| {
                    let meta = entry.metadata().ok()?;
                    Some((entry.path(), meta.is_dir(), meta.len()))
                })
                .collect();

            let mut json = String::from("[");
            let mut state = caller.data().file_picker.lock().unwrap();
            for (path, is_dir, size) in children {
                let name = file_name_of(&path);
                let Some(id) = state.alloc(path, is_dir) else {
                    break;
                };
                if json.len() > 1 {
                    json.push(',');
                }
                json.push_str("{\"name\":");
                json_escape(&name, &mut json);
                json.push_str(&format!(
                    ",\"size\":{size},\"is_dir\":{is_dir},\"handle\":{id}}}"
                ));
            }
            drop(state);
            json.push(']');
            write_or_size(&mut caller, out_ptr, out_cap, json.as_bytes()) as i32
        },
    )?;

    // api_file_read(handle, out_ptr, out_cap) -> i64
    //   Reads the full file. Returns bytes written, -1 invalid handle,
    //   -2 io error or a buffer outside guest memory, or -(required size) if the buffer is
    //   too small. The file goes into the buffer a chunk at a time.
    linker.func_wrap(
        "oxide",
        "api_file_read",
        |mut caller: Caller<'_, HostState>, handle: u32, out_ptr: u32, out_cap: u32| -> i64 {
            let Some(file) = picked(&caller, handle, Some(false)) else {
                return -1;
            };
            let Ok(mut opened) = std::fs::File::open(&file.path) else {
                return -2;
            };
            let Ok(size) = opened.metadata().map(|meta| meta.len()) else {
                return -2;
            };
            if size > u64::from(out_cap) {
                return -(size.min(i64::MAX as u64) as i64);
            }
            if !guest_range(&caller, out_ptr, out_cap) {
                return -2;
            }
            match read_into_guest(&mut caller, out_ptr, size as u32, &mut opened) {
                Ok(n) => i64::from(n),
                Err(_) => -2,
            }
        },
    )?;

    // api_file_read_range(handle, offset_lo, offset_hi, len, out_ptr, out_cap) -> i64
    //   Reads [offset .. offset+len) from the file. Returns bytes written,
    //   -1 invalid handle, -2 io error. Short reads are returned verbatim
    //   (EOF reached before `len`).
    linker.func_wrap(
        "oxide",
        "api_file_read_range",
        |mut caller: Caller<'_, HostState>,
         handle: u32,
         offset_lo: u32,
         offset_hi: u32,
         len: u32,
         out_ptr: u32,
         out_cap: u32|
         -> i64 {
            use std::io::{Read, Seek, SeekFrom};
            let Some(file) = picked(&caller, handle, Some(false)) else {
                return -1;
            };
            let len = len.min(out_cap);
            if !guest_range(&caller, out_ptr, len) {
                return -2;
            }
            let offset = (u64::from(offset_hi) << 32) | u64::from(offset_lo);
            let read = std::fs::File::open(&file.path).and_then(|mut f| {
                f.seek(SeekFrom::Start(offset))?;
                read_into_guest(&mut caller, out_ptr, len, &mut f.take(u64::from(len)))
            });
            match read {
                Ok(n) => i64::from(n),
                _ => -2,
            }
        },
    )?;

    // api_file_metadata(handle, out_ptr, out_cap) -> i32
    //   Writes JSON: {"name":"a.txt","size":123,"mime":"image/png",
    //                 "modified_ms":1712000000000,"is_dir":false}
    //   Returns bytes written, -1 invalid handle, -2 io error, or
    //   -(required size) if the buffer is too small.
    linker.func_wrap(
        "oxide",
        "api_file_metadata",
        |mut caller: Caller<'_, HostState>, handle: u32, out_ptr: u32, out_cap: u32| -> i32 {
            let Some(entry) = picked(&caller, handle, None) else {
                return -1;
            };
            let Ok(meta) = std::fs::metadata(&entry.path) else {
                return -2;
            };
            let mime = if entry.is_dir {
                "inode/directory"
            } else {
                let ext = entry.path.extension().unwrap_or_default();
                mime_for_extension(&ext.to_string_lossy())
            };
            let mut json = String::from("{\"name\":");
            json_escape(&file_name_of(&entry.path), &mut json);
            json.push_str(&format!(",\"size\":{},\"mime\":", meta.len()));
            json_escape(mime, &mut json);
            json.push_str(&format!(
                ",\"modified_ms\":{},\"is_dir\":{}}}",
                modified_ms(&meta),
                entry.is_dir
            ));
            write_or_size(&mut caller, out_ptr, out_cap, json.as_bytes()) as i32
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_keeps_its_handle_and_handles_are_bounded() {
        let mut state = FilePickerState::default();
        let a = state.alloc("/a".into(), false).unwrap();
        assert_eq!(state.alloc("/a".into(), false), Some(a));
        for i in 1..MAX_HANDLES {
            state.alloc(format!("/{i}").into(), false).unwrap();
        }
        assert_eq!(state.alloc("/new".into(), false), None);
        assert_eq!(state.alloc("/a".into(), false), Some(a));
    }
}
