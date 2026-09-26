//! The configuration schema: parsing and validating configuration text into typed settings
//! (enabled engines and the default engine, key bindings as parsed commands, chrome settings).
//!
//! The schema is versioned. User settings are merged over the built-in defaults, and unknown
//! keys are errors. Configuration is validated completely before it replaces the active one;
//! an invalid reload leaves the previous configuration active and reports the error. Key
//! bindings and theme apply live; the chrome renderer needs a restart; disabling an engine
//! stops new views while existing ones stay until closed.
//!
//! Must not own: reading or watching files, which the executable does.
