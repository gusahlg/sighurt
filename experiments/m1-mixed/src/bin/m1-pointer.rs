//! A virtual pointer that stays plugged in while it reads commands from stdin, for the headless
//! checks (headless-checks.sh). Only ever run it inside a private compositor.
//!
//! ```text
//! move X Y W H   move to (X, Y) of a W×H logical layout (the output's logical size)
//! click          press and release the left button
//! ```
//!
//! wlrctl creates a new virtual pointer for each command and removes it right after, so the
//! seat's pointer capability comes and goes, and a client misses the events sent before it
//! has bound a new `wl_pointer`.

use std::io::BufRead;
use std::time::Instant;

use smithay_client_toolkit::reexports::protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;
use smithay_client_toolkit::reexports::protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_pointer::ButtonState;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{delegate_noop, Connection, Dispatch, QueueHandle};

const BTN_LEFT: u32 = 0x110;

struct State;

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(State: ZwlrVirtualPointerManagerV1);
delegate_noop!(State: ZwlrVirtualPointerV1);

fn main() -> Result<(), String> {
    let connection = Connection::connect_to_env().map_err(|e| e.to_string())?;
    let (globals, mut queue) =
        registry_queue_init::<State>(&connection).map_err(|e| e.to_string())?;
    let qh = queue.handle();
    let manager: ZwlrVirtualPointerManagerV1 = globals
        .bind(&qh, 1..=2, ())
        .map_err(|e| format!("zwlr_virtual_pointer_manager_v1: {e}"))?;
    let pointer = manager.create_virtual_pointer(None, &qh, ());
    queue.roundtrip(&mut State).map_err(|e| e.to_string())?;
    let start = Instant::now();
    let time = || start.elapsed().as_millis() as u32;
    for line in std::io::stdin().lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["move", numbers @ ..] if numbers.len() == 4 => {
                let n: Vec<u32> = numbers.iter().filter_map(|n| n.parse().ok()).collect();
                if let [x, y, width, height] = n[..] {
                    pointer.motion_absolute(time(), x, y, width, height);
                    pointer.frame();
                }
            }
            ["click"] => {
                pointer.button(time(), BTN_LEFT, ButtonState::Pressed);
                pointer.frame();
                pointer.button(time(), BTN_LEFT, ButtonState::Released);
                pointer.frame();
            }
            _ => eprintln!("m1-pointer: unknown command {line:?}"),
        }
        queue.roundtrip(&mut State).map_err(|e| e.to_string())?;
    }
    pointer.destroy();
    queue.roundtrip(&mut State).map_err(|e| e.to_string())?;
    Ok(())
}
