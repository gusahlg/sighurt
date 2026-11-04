//! The actions this configuration defines itself: commands computed from what the browser shows
//! (`sighurt_api::keymap::Browser`). Owns those functions; must not read anything but the
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

/// `y f`: copy the link the pointer is over in the selected tab: its status text, when that is
/// a URL. Anything else: the key is taken and nothing runs.
pub fn copy_link(browser: &dyn Browser) -> Vec<Command> {
    browser
        .tab(browser.active_tab())
        .and_then(|tab| tab.status)
        .filter(|status| is_url(status))
        .map(|link| vec![Command::ClipboardCopy(link.to_owned())])
        .unwrap_or_default()
}

/// Whether `text` looks like an absolute URL: a scheme (a letter, then letters, digits, `+`,
/// `-` or `.`), a colon, something after it, and no whitespace. Status text that only says
/// something about a link is not one.
fn is_url(text: &str) -> bool {
    let Some((scheme, rest)) = text.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && !rest.is_empty()
        && !text.contains(char::is_whitespace)
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
