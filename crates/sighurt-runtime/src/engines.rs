//! The engines compiled into this build, as every frontend sees them.
//!
//! Cargo features decide which exist. A descriptor is the engine's name, what it supports and
//! which settings of `[engines.<name>]` it honors. A frontend does not repeat that list: it
//! binds each engine ([`crate::registry::Registry::bind`]) and supplies only how the runtime
//! starts on its target, how its views present and the renderer label. The core's
//! [`Environment`](sighurt_core::Environment) is built from these descriptors.
//!
//! The order is the order runtimes shut down: Servo, Wasm, WPE WebKit, Ladybird, the demo engine,
//! NetSurf. The web engines, those that open http(s) pages, are the ones `page.switch-engine` cycles, in
//! that same order.
//!
//! Must not own: how a runtime is constructed or how it presents. Those name the frontend's
//! target and permit types.

use sighurt_api::engine::EngineCapabilities;
#[cfg(any(feature = "engine-demo", feature = "engine-wasm"))]
use sighurt_api::engine::SiteDataKinds;

/// One compiled-in engine, before a frontend binds it to a target and a permit.
pub struct Descriptor {
    /// Its name in the configuration.
    pub name: &'static str,
    /// Whether it opens http(s) pages. `page.switch-engine` cycles these, in [`descriptors`]
    /// order, among the ones the configuration enables.
    pub web: bool,
    /// What it supports. The core needs this before the runtime exists.
    pub capabilities: EngineCapabilities,
    /// The settings of `[engines.<name>]` it honors.
    pub honors: &'static [&'static str],
}

/// The engines of this build, in shutdown order.
pub fn descriptors() -> Vec<Descriptor> {
    vec![
        #[cfg(feature = "engine-servo")]
        Descriptor {
            name: "servo",
            web: true,
            capabilities: sighurt_engine_servo::CAPABILITIES,
            honors: &[
                "user-agent",
                "storage",
                "experimental-features",
                "preferences",
            ],
        },
        #[cfg(feature = "engine-wasm")]
        Descriptor {
            name: "wasm",
            web: false,
            capabilities: wasm_capabilities(),
            honors: &["storage"],
        },
        #[cfg(feature = "engine-wpe")]
        Descriptor {
            name: "wpe",
            web: true,
            capabilities: sighurt_engine_wpe::CAPABILITIES,
            honors: &["user-agent", "storage"],
        },
        #[cfg(feature = "engine-ladybird")]
        Descriptor {
            name: "ladybird",
            web: true,
            capabilities: sighurt_engine_ladybird::CAPABILITIES,
            honors: &["storage"],
        },
        #[cfg(feature = "engine-demo")]
        Descriptor {
            name: "demo",
            web: false,
            // `sighurt_engine_demo::CAPABILITIES`: content as structure, and cookies and
            // storage in memory. Repeated here so this crate does not depend on the demo
            // engine: that crate names the host's target types. `Registry::start` warns if a
            // runtime reports otherwise.
            capabilities: EngineCapabilities {
                semantics: true,
                site_data: SiteDataKinds {
                    cookies: true,
                    storage: true,
                    caches: false,
                    hsts: false,
                },
                ..EngineCapabilities::default()
            },
            honors: &[],
        },
        #[cfg(feature = "engine-netsurf")]
        Descriptor {
            name: "netsurf",
            web: true,
            capabilities: sighurt_engine_netsurf::CAPABILITIES,
            honors: &["user-agent"],
        },
    ]
}

/// What `WasmRuntime::capabilities` reports. `Registry::start` warns if they ever differ.
#[cfg(feature = "engine-wasm")]
fn wasm_capabilities() -> EngineCapabilities {
    EngineCapabilities {
        history_traversal: true,
        page_zoom: true,
        site_data: SiteDataKinds::default(),
        ..EngineCapabilities::default()
    }
}

#[cfg(test)]
mod tests {
    use sighurt_core::config::Config;

    use super::descriptors;
    use crate::registry::{Binding, Registry};

    #[test]
    fn the_core_learns_what_each_engine_honors() {
        let bindings = descriptors()
            .iter()
            .map(|desc| Binding {
                name: desc.name,
                renderer: "test",
                start: |_waker, _launch| Err("the test does not start an engine".into()),
            })
            .collect();
        let registry = Registry::<(), ()>::bind(bindings);
        // No chrome and no color names: the texts below set neither, and the built-in defaults
        // still parse.
        let env = registry.environment(Vec::new(), &[]);
        assert_eq!(
            env.engine_settings.keys().collect::<Vec<_>>(),
            env.engines.keys().collect::<Vec<_>>()
        );
        // A valid value of each setting the core knows; an engine honoring another fails here.
        let valid = |name: &str| match name {
            "user-agent" => "\"Sighurt\"",
            "storage" => "\"ephemeral\"",
            "experimental-features" => "true",
            "preferences" => "{ dom_adoptedstylesheet_enabled = true }",
            other => panic!("no valid value of {other} here"),
        };
        for engine in registry.engines() {
            let settings: String = engine
                .honors
                .iter()
                .map(|name| format!("{name} = {}\n", valid(name)))
                .collect();
            let text = format!("version = 1\n[engines.{}]\n{settings}", engine.name);
            let config = Config::parse(Some(&text), &env);
            assert!(config.is_ok(), "{}: {config:?}", engine.name);
        }
        #[cfg(feature = "engine-servo")]
        assert_eq!(
            env.engine_settings["servo"],
            [
                "user-agent",
                "storage",
                "experimental-features",
                "preferences"
            ]
        );
        #[cfg(feature = "engine-wasm")]
        assert_eq!(env.engine_settings["wasm"], ["storage"]);
        #[cfg(feature = "engine-wpe")]
        assert_eq!(env.engine_settings["wpe"], ["user-agent", "storage"]);
        #[cfg(feature = "engine-ladybird")]
        assert_eq!(env.engine_settings["ladybird"], ["storage"]);
        #[cfg(feature = "engine-netsurf")]
        assert_eq!(env.engine_settings["netsurf"], ["user-agent"]);
        #[cfg(feature = "engine-demo")]
        assert!(env.engine_settings["demo"].is_empty());
        // Web engines are a property of the descriptors, in their order, whatever this build
        // compiled in. Servo, WPE, Ladybird and NetSurf open http(s) pages; Wasm and the demo
        // engine do not.
        let web: Vec<String> = descriptors()
            .iter()
            .filter(|desc| desc.web)
            .map(|desc| desc.name.to_owned())
            .collect();
        assert_eq!(env.web_engines, web);
        for desc in descriptors() {
            assert_eq!(
                desc.web,
                matches!(desc.name, "servo" | "wpe" | "ladybird" | "netsurf"),
                "{}",
                desc.name
            );
        }
    }
}
