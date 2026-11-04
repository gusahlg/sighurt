//! Compiles the chrome's WGSL program, shared by every GPU backend
//! (`sighurt_render::gpu::SHADER`, read here from its file), to SPIR-V with naga: both entry
//! points in one module, in `$OUT_DIR/chrome.spv`. naga's default SPIR-V options flip the
//! position's y (`ADJUST_COORDINATE_SPACE`), so WGSL's y-up clip space meets Vulkan's y-down
//! framebuffer with a positive viewport; bindings keep their WGSL group and binding numbers.

use std::path::PathBuf;

fn main() {
    let source =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sighurt-render/src/gpu/shaders.wgsl");
    println!("cargo::rerun-if-changed={}", source.display());
    let wgsl = std::fs::read_to_string(&source).expect("the shared WGSL program");
    let module = naga::front::wgsl::parse_str(&wgsl).expect("the WGSL parses");
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    .expect("the WGSL validates");
    let mut options = naga::back::spv::Options::default();
    // The same bytes in debug and release builds.
    options.flags.remove(naga::back::spv::WriterFlags::DEBUG);
    let words = naga::back::spv::write_vec(&module, &info, &options, None).expect("SPIR-V");
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    std::fs::write(out.join("chrome.spv"), bytes).expect("written");
}
