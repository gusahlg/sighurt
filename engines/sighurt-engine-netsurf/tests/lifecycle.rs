//! One process, one NetSurf. This binary is the only test that shuts it down:
//! a later init in the same process fails, so no other test lives here.

use sighurt_engine_netsurf::ffi;

#[test]
fn one_runtime_per_process() {
    assert_eq!(ffi::state(), 0, "nothing has started NetSurf");
    assert_eq!(ffi::init_default(), 0, "nsh_init");
    assert_eq!(ffi::state(), 1);
    ffi::shutdown();
    assert_eq!(ffi::state(), 2, "shutdown finishes the runtime");
    assert_ne!(
        ffi::init_default(),
        0,
        "a second init in this process must fail without calling netsurf_init again"
    );
    assert_eq!(ffi::state(), 2);
    ffi::shutdown();
    assert_eq!(
        ffi::state(),
        2,
        "shutdown after the runtime is finished does nothing"
    );
}
