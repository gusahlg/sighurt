//! The embedded guests: what each imports is what the engine gives an internal page, and no
//! more.

use sighurt_engine_wasm::pages::{allowed_import, FUNCTIONS};
use sighurt_pages::{built, PAGES};

#[test]
fn every_blob_imports_only_oxide_and_the_sig_functions_the_engine_has() {
    let engine = wasmtime::Engine::default();
    let mut checked = 0;
    for (page, wasm) in built() {
        let module = wasmtime::Module::new(&engine, wasm)
            .unwrap_or_else(|e| panic!("sig://{}: {e:#}", page.name));
        for import in module.imports() {
            let (from, name) = (import.module(), import.name());
            match from {
                "oxide" => assert!(allowed_import(name), "sig://{} imports {name}", page.name),
                "sig" => assert!(FUNCTIONS.contains(&name), "sig://{}: sig.{name}", page.name),
                other => panic!("sig://{} imports {other}.{name}", page.name),
            }
        }
        let exports: Vec<&str> = module.exports().map(|e| e.name()).collect();
        assert!(exports.contains(&"start_app"), "sig://{}", page.name);
        checked += 1;
    }
    assert!(checked >= 1, "sig://help is built");
}

#[test]
fn help_is_built_and_names_are_unique() {
    assert!(PAGES
        .iter()
        .any(|page| page.name == "help" && page.wasm.is_some()));
    for (i, page) in PAGES.iter().enumerate() {
        assert!(PAGES[i + 1..].iter().all(|other| other.name != page.name));
        assert!(!page.grants.is_empty());
    }
}
