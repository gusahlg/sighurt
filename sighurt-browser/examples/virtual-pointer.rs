//! A virtual pointer that stays plugged in while it reads commands from stdin, for the headless
//! end-to-end tests (`sighurt-browser/tests/headless.sh` and `engines.sh`). Only ever run it
//! inside a private compositor (`tools/headless-sway`); it moves the pointer of whatever
//! compositor it reaches.
//!
//! ```text
//! move X Y W H   move to (X, Y) of a W×H logical layout (the output's logical size)
//! click [BUTTON] press and release BUTTON: left (the default), side or extra (the mouse's
//!                back and forward buttons)
//! press          press the left button and hold it
//! release        release it
//! ```
//!
//! wlrctl creates a new virtual pointer for each command and removes it right after, so the
//! seat's pointer capability comes and goes, and a client misses the events sent before it has
//! bound a new `wl_pointer`. (The same helper as `experiments/m1-mixed`'s `m1-pointer`, which
//! can't be built without Servo.)

use std::io::BufRead;
use std::time::Instant;

use smithay_client_toolkit::reexports::protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;
use smithay_client_toolkit::reexports::protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_pointer::ButtonState;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{delegate_noop, Connection, Dispatch, QueueHandle};

const BTN_LEFT: u32 = 0x110;
const BTN_SIDE: u32 = 0x113;
const BTN_EXTRA: u32 = 0x114;

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
            ["click", button @ ..] if button.len() <= 1 => {
                let button = match button {
                    [] | ["left"] => BTN_LEFT,
                    ["side"] => BTN_SIDE,
                    ["extra"] => BTN_EXTRA,
                    _ => {
                        eprintln!("virtual-pointer: unknown button in {line:?}");
                        continue;
                    }
                };
                pointer.button(time(), button, ButtonState::Pressed);
                pointer.frame();
                pointer.button(time(), button, ButtonState::Released);
                pointer.frame();
            }
            ["press"] => {
                pointer.button(time(), BTN_LEFT, ButtonState::Pressed);
                pointer.frame();
            }
            ["release"] => {
                pointer.button(time(), BTN_LEFT, ButtonState::Released);
                pointer.frame();
            }
            _ => eprintln!("virtual-pointer: unknown command {line:?}"),
        }
        queue.roundtrip(&mut State).map_err(|e| e.to_string())?;
    }
    pointer.destroy();
    queue.roundtrip(&mut State).map_err(|e| e.to_string())?;
    Ok(())
}
