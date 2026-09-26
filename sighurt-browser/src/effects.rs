//! Carrying out the core's effects: opening and closing views with their targets, engine
//! commands, visibility and focus changes, service requests and session writes.
//!
//! Follows the view close order: revoke commands, clear focus and captures, cancel the
//! document's service work, stop presentation, close the view in its engine, then retire the
//! target after its graphics resources.
//!
//! Must not own: decisions; an effect is carried out as the core stated it.
