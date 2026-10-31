//! The actions this configuration defines itself: commands computed from what the browser shows
//! (`sighurt_api::keymap::Browser`). Owns those two functions; must not read anything but the
//! `Browser` it is given, keep state, or do I/O.

use std::num::NonZeroUsize;

use sighurt_api::commands::Command;
use sighurt_api::keymap::Browser;

/// `y y`: copy the selected tab's address. Nothing to copy: the key is taken and nothing runs.
pub fn copy_address(browser: &dyn Browser) -> Vec<Command> {
    browser
        .tab(browser.active_tab())
        .and_then(|tab| tab.url)
        .map(|url| vec![Command::ClipboardCopy(url.to_owned())])
        .unwrap_or_default()
}

/// `c o`: close every tab but the selected one, highest number first so the numbers of the ones
/// still to close don't move.
pub fn close_other_tabs(browser: &dyn Browser) -> Vec<Command> {
    let active = browser.active_tab();
    (0..browser.tab_count())
        .rev()
        .filter(|&index| index != active)
        .filter_map(|index| NonZeroUsize::new(index + 1))
        .map(|number| Command::TabClose(Some(number)))
        .collect()
}
