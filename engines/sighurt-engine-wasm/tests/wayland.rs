//! `CpuTarget` for the Wayland host's SHM `TargetLease`, against a real compositor: a private
//! headless sway.
//!
//! The test is `#[ignore]`d, so a plain `cargo test` lists it as ignored; run it with
//! `engines/sighurt-engine-wasm/tests/headless.sh`, which includes it. Outside that script's compositor
//! it refuses to start.

use std::time::{Duration, Instant};

use calloop::EventLoop;
use sighurt_api::geometry::{LogicalSize, WindowRect};
use sighurt_api::ids::{TargetId, WindowId};
use sighurt_engine_wasm::{CpuTarget, Frame};
use sighurt_wayland::{
    FramePermit, Host, HostEvent, InputRegion, Placement, PresentMode, TargetDiagnostics,
    TargetLease, TargetPlacement, WindowOptions,
};

/// Whether this process runs inside tools/headless-sway, as the test script starts it.
fn headless() -> bool {
    let private = std::env::var("XDG_RUNTIME_DIR").is_ok_and(|d| d.starts_with("/tmp/sighl."));
    std::env::var_os("SIGHURT_HEADLESS").is_some() && private
}

/// A window with two SHM targets side by side, `a` on the left and `b` on the right.
struct Window {
    event_loop: EventLoop<'static, Host>,
    host: Host,
    window: WindowId,
    a: TargetLease,
    b: TargetLease,
}

impl Window {
    fn new() -> Self {
        assert!(
            headless(),
            "refusing to run outside tools/headless-sway: run engines/sighurt-engine-wasm/tests/headless.sh"
        );
        let event_loop: EventLoop<'static, Host> = EventLoop::try_new().unwrap();
        let mut host = Host::new(&event_loop.handle(), |host: &mut Host| host).expect("a host");
        let window = WindowId::next();
        host.create_window(window, WindowOptions::default());
        let a = host.create_target(window, PresentMode::Shm, None).unwrap();
        let b = host.create_target(window, PresentMode::Shm, None).unwrap();
        Self {
            event_loop,
            host,
            window,
            a,
            b,
        }
    }

    fn place(&mut self, size: LogicalSize) {
        let half = size.width / 2;
        let entry = |target: TargetId, x: u32, width: u32| TargetPlacement {
            target,
            rect: WindowRect {
                x: x as i32,
                y: 0,
                width,
                height: size.height,
            },
            visible: true,
            input: InputRegion::Whole,
        };
        let targets = vec![
            entry(self.a.id(), 0, half),
            entry(self.b.id(), half, size.width - half),
        ];
        let placement = Placement {
            window: self.window,
            size,
            targets,
        };
        self.host.apply_placement(placement).expect("a placement");
    }

    /// Runs the loop until both targets have a permit, and returns them: `a`'s, then `b`'s.
    fn permits(&mut self) -> (FramePermit, FramePermit) {
        self.host.invalidate(self.a.id());
        self.host.invalidate(self.b.id());
        let (mut a, mut b) = (None, None);
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            while let Some(event) = self.host.next_event() {
                if let HostEvent::WindowConfigured { size, .. } = event {
                    self.place(size);
                }
            }
            for permit in self.host.permits() {
                if permit.target() == self.a.id() {
                    a = Some(permit);
                } else if permit.target() == self.b.id() {
                    b = Some(permit);
                }
            }
            self.host.flush();
            if let (Some(_), Some(_)) = (&a, &b) {
                return (a.unwrap(), b.unwrap());
            }
            assert!(Instant::now() < end, "timed out waiting for permits");
            self.event_loop
                .dispatch(Some(Duration::from_millis(20)), &mut self.host)
                .expect("dispatch");
        }
    }

    fn diagnostics(&self, target: TargetId) -> TargetDiagnostics {
        self.host
            .diagnostics()
            .targets
            .into_iter()
            .find(|t| t.target == target)
            .expect("a live target")
    }
}

/// Paints the whole frame blue.
fn blue(frame: Frame<'_>) -> Result<(), String> {
    assert_eq!(frame.stride, 4 * frame.size.width as usize);
    for pixel in frame.pixels.as_chunks_mut::<4>().0 {
        *pixel = [0xff, 0, 0, 0xff];
    }
    Ok(())
}

/// A lease presents only permits for its own target; a frame that fails to paint is not
/// submitted; one that paints is submitted at the permit's metrics.
#[test]
#[ignore = "needs tools/headless-sway: run engines/sighurt-engine-wasm/tests/headless.sh"]
fn a_lease_submits_only_frames_of_its_own_permits_that_painted() {
    let mut window = Window::new();
    let (a, b) = window.permits();
    let mut painted = 0;
    let refused = window.b.submit(a, &mut |_| {
        painted += 1;
        Ok(())
    });
    assert!(refused.is_err(), "another target's permit was accepted");
    assert_eq!(painted, 0, "another target's permit was painted");
    let failed = window.b.submit(b, &mut |_| Err("the guest failed".into()));
    assert_eq!(failed, Err("the guest failed".into()));
    for lease in [&window.a, &window.b] {
        let target = window.diagnostics(lease.id());
        assert_eq!(target.stats.submissions, 0, "{:?}", target.stats);
    }

    // Both can present after that, each frame at its permit's metrics.
    let (a, b) = window.permits();
    let sizes = [a.metrics().buffer(), b.metrics().buffer()];
    let mut lent = Vec::new();
    for (lease, permit) in [(&mut window.a, a), (&mut window.b, b)] {
        lease
            .submit(permit, &mut |frame| {
                lent.push(frame.size);
                blue(frame)
            })
            .expect("a frame submitted");
    }
    assert_eq!(lent, sizes);
    for lease in [&window.a, &window.b] {
        let target = window.diagnostics(lease.id());
        assert_eq!(target.stats.submissions, 1, "{:?}", target.stats);
        assert!(target.buffers.buffers <= 2, "{:?}", target.buffers);
        let size = target.metrics.buffer();
        assert!(size.width > 0 && size.height > 0);
    }
}
