//! Interaction tests (plan §25): editing with Swedish letters, AltGr and dead keys, paste and
//! selection, input-method preedit with the caret where it is drawn, pointer drags that leave
//! the chrome, tab selection, focus moves, and hit targets that stay on the drawn controls at
//! every scale.

mod common;

use std::num::NonZeroUsize;

use common::*;
use sighurt_api::commands::Command;
use sighurt_api::events::Cursor;
use sighurt_api::geometry::{SurfacePoint, WindowPoint, WindowRect};
use sighurt_api::ids::RequestId;
use sighurt_api::input::{
    ButtonState, FocusEvent, Key, KeyCode, KeyEvent, KeyboardFocus, Modifiers, PointerButton,
    PointerEvent, Preedit, TextInputEvent,
};
use sighurt_ui::accessibility::NodeId;
use sighurt_ui::chrome::{CHROME, TABS};
use sighurt_ui::extension::ChromeAction;
use sighurt_ui::hit_test::{hit_test, Target};
use sighurt_ui::layout::{Status, Tabs};
use sighurt_ui::{Colors, Damage, LayoutOptions, Model, Rgba, Snapshot, TabContent};

fn n(n: usize) -> NonZeroUsize {
    NonZeroUsize::new(n).unwrap()
}

fn press_key(key: Key, code: u32, modifiers: Modifiers, text: Option<&str>) -> KeyEvent {
    KeyEvent {
        code: KeyCode(code),
        key,
        state: ButtonState::Pressed,
        repeat: false,
        modifiers,
        consumed: Modifiers::default(),
        text: text.map(Into::into),
    }
}

/// A key that types `text`.
fn typed(text: &str) -> KeyEvent {
    press_key(
        Key::Character(text.into()),
        30,
        Modifiers::default(),
        Some(text),
    )
}

fn named(name: &str, modifiers: Modifiers) -> KeyEvent {
    press_key(Key::Named(name.into()), 0, modifiers, None)
}

fn ctrl(letter: &str, code: u32) -> KeyEvent {
    let control = Modifiers {
        control: true,
        ..Modifiers::default()
    };
    press_key(Key::Character(letter.into()), code, control, None)
}

const SHIFT: Modifiers = Modifiers {
    shift: true,
    control: false,
    alt: false,
    alt_graph: false,
    logo: false,
    caps_lock: false,
    num_lock: false,
};

fn type_text(chrome: &mut Model, text: &str) {
    for c in text.chars() {
        chrome.key(&typed(&c.to_string()));
    }
}

fn button(position: WindowPoint, button: PointerButton, state: ButtonState) -> PointerEvent {
    PointerEvent::Button {
        button,
        state,
        position: SurfacePoint {
            x: position.x,
            y: position.y,
        },
        modifiers: Modifiers::default(),
    }
}

fn center(rect: WindowRect) -> WindowPoint {
    WindowPoint {
        x: f64::from(rect.x) + f64::from(rect.width) / 2.0,
        y: f64::from(rect.y) + f64::from(rect.height) / 2.0,
    }
}

/// Presses and releases the primary button at window position `at` on the chrome surface.
fn click(chrome: &mut Model, at: WindowPoint) -> Vec<ChromeAction> {
    let surface = chrome.layout().chrome;
    let local = WindowPoint {
        x: at.x - f64::from(surface.x),
        y: at.y - f64::from(surface.y),
    };
    let mut actions = chrome.pointer(
        surface,
        &button(local, PointerButton::Left, ButtonState::Pressed),
    );
    actions.extend(chrome.pointer(
        surface,
        &button(local, PointerButton::Left, ButtonState::Released),
    ));
    actions
}

fn commands(actions: &[ChromeAction]) -> Vec<Command> {
    actions
        .iter()
        .filter_map(|action| match action {
            ChromeAction::Command(command) => Some(command.clone()),
            _ => None,
        })
        .collect()
}

/// A chrome with the address line focused (as the core does for `location.focus`) and `text`
/// typed.
fn editing(text: &str) -> Model {
    let mut chrome = chrome(800, 600, three_tabs());
    chrome.window_focus(FocusEvent::Activated);
    chrome.set_focus(KeyboardFocus::Chrome);
    assert!(chrome.state().focused);
    type_text(&mut chrome, text);
    chrome
}

/// The clipboard request among `actions`.
fn paste_request(actions: &[ChromeAction]) -> RequestId {
    let request = actions.iter().find_map(|action| match action {
        ChromeAction::Paste(request) => Some(*request),
        _ => None,
    });
    request.unwrap_or_else(|| panic!("no clipboard read in {actions:?}"))
}

/// The snapshot with its tab `n` (from 0) active.
fn switched(snapshot: &Snapshot, n: usize) -> Snapshot {
    let mut snapshot = snapshot.clone();
    for (i, tab) in snapshot.tabs.iter_mut().enumerate() {
        tab.active = i == n;
    }
    snapshot
}

fn address(chrome: &Model) -> &str {
    chrome.state().address.text()
}

#[test]
fn swedish_typing_with_dead_keys_and_altgr() {
    let mut chrome = editing("räksmörgås");
    assert_eq!(address(&chrome), "räksmörgås");
    // A dead key types nothing; the key that completes it types the composed letter.
    chrome.key(&press_key(
        Key::Dead(Some('¨')),
        27,
        Modifiers::default(),
        None,
    ));
    chrome.key(&press_key(
        Key::Character("ä".into()),
        30,
        Modifiers::default(),
        Some("ä"),
    ));
    // AltGr + 2 is "@" on a Swedish layout: typing, not a Ctrl+Alt shortcut.
    let alt_graph = Modifiers {
        alt_graph: true,
        ..Modifiers::default()
    };
    let mut at = press_key(Key::Character("@".into()), 3, alt_graph, Some("@"));
    at.consumed = alt_graph;
    chrome.key(&at);
    assert_eq!(address(&chrome), "räksmörgåsä@");
    // Releases and repeats: a release types nothing, a repeat types again.
    let mut release = typed("x");
    release.state = ButtonState::Released;
    chrome.key(&release);
    let mut repeat = typed("x");
    repeat.repeat = true;
    chrome.key(&repeat);
    assert_eq!(address(&chrome), "räksmörgåsä@x");
    // Backspace deletes whole characters.
    for _ in 0..4 {
        chrome.key(&named("Backspace", Modifiers::default()));
    }
    assert_eq!(address(&chrome), "räksmörgå");
}

/// An Alt shortcut the core didn't bind types nothing, as in the old URL bar, unless the layout
/// used Alt to make the character.
#[test]
fn alt_shortcuts_type_nothing() {
    let mut chrome = editing("");
    let alt = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    chrome.key(&press_key(Key::Character("f".into()), 33, alt, Some("f")));
    assert_eq!(
        chrome.state().address.selected_text(),
        Some("https://sighurt.example/")
    );
    let mut made = press_key(Key::Character("ŋ".into()), 34, alt, Some("ŋ"));
    made.consumed = alt;
    chrome.key(&made);
    assert_eq!(address(&chrome), "ŋ");
}

#[test]
fn selection_copy_cut_and_paste() {
    let mut chrome = editing("https://exempel.se/blåbär");
    // Shift+Ctrl+Left selects the last word; Ctrl+C copies it.
    let word_select = Modifiers {
        shift: true,
        control: true,
        ..Modifiers::default()
    };
    chrome.key(&named("ArrowLeft", word_select));
    assert_eq!(chrome.state().address.selected_text(), Some("blåbär"));
    let actions = chrome.key(&ctrl("c", 46));
    assert!(
        actions.contains(&ChromeAction::Copy("blåbär".into())),
        "{actions:?}"
    );
    // Ctrl+V asks for the clipboard; its text replaces the selection without line breaks.
    let request = paste_request(&chrome.key(&ctrl("v", 47)));
    chrome.paste(request, "lingon\nsylt\r\n");
    assert_eq!(address(&chrome), "https://exempel.se/lingonsylt");
    // Shift+Home selects to the start; Ctrl+X cuts.
    chrome.key(&named("Home", SHIFT));
    let actions = chrome.key(&ctrl("x", 45));
    assert!(actions.contains(&ChromeAction::Copy("https://exempel.se/lingonsylt".into())));
    assert_eq!(address(&chrome), "");
    // Ctrl+A on a layout without Latin letters still selects all: the physical key counts.
    type_text(&mut chrome, "ö");
    chrome.key(&ctrl("ф", 30));
    assert_eq!(chrome.state().address.selected_text(), Some("ö"));
}

#[test]
fn keys_reach_the_line_only_while_it_has_focus() {
    let mut chrome = chrome(800, 600, three_tabs());
    chrome.key(&typed("x"));
    assert_eq!(address(&chrome), "https://sighurt.example/");
    assert!(!chrome.state().address.is_modified());
}

#[test]
fn escape_discards_and_enter_opens() {
    let mut chrome = editing("sylt");
    let actions = chrome.key(&named("Escape", Modifiers::default()));
    assert!(
        actions.contains(&ChromeAction::Focus(KeyboardFocus::Page)),
        "{actions:?}"
    );
    assert!(
        actions.contains(&ChromeAction::TextInput(None)),
        "{actions:?}"
    );
    assert_eq!(address(&chrome), "https://sighurt.example/");
    assert!(!chrome.state().focused);

    // An empty line opens nothing.
    let mut chrome = editing("");
    chrome.key(&ctrl("a", 30));
    chrome.key(&named("Delete", Modifiers::default()));
    let actions = chrome.key(&named("Enter", Modifiers::default()));
    assert_eq!(commands(&actions), []);
    assert!(actions.contains(&ChromeAction::Focus(KeyboardFocus::Page)));
}

/// Enter opens what was typed. The line keeps showing it until its tab commits another address,
/// stops loading or is switched away from, while the tab's committed address (in the snapshot,
/// and the page's accessible value) stays what it was.
#[test]
fn an_opened_line_stays_until_its_load_ends() {
    let enter = named("Enter", Modifiers::default());
    let mut chrome = editing("  rust borrow checker ");
    let actions = chrome.key(&enter);
    let open = Command::PageOpen("rust borrow checker".into());
    assert_eq!(commands(&actions), [open]);
    // The core runs it: the page gets focus and the tab loads.
    chrome.set_focus(KeyboardFocus::Page);
    assert!(!chrome.state().focused);
    let mut snapshot = chrome.snapshot().clone();
    snapshot.tabs[0].loading = true;
    chrome.set_snapshot(snapshot.clone());
    assert_eq!(address(&chrome), "  rust borrow checker ");
    let tree = chrome.accessibility();
    let page = tree.node(NodeId::Page).unwrap();
    assert_eq!(page.value.as_deref(), Some("https://sighurt.example/"));
    // It commits.
    snapshot.tabs[0].url = "https://search.example/?q=rust".into();
    chrome.set_snapshot(snapshot);
    assert_eq!(address(&chrome), "https://search.example/?q=rust");
    assert!(!chrome.state().address.is_modified());

    // A load that stops (it failed, or was stopped) shows the committed address again; the
    // snapshot may come before the focus move.
    let mut chrome = editing("sylt");
    chrome.key(&enter);
    let mut snapshot = chrome.snapshot().clone();
    snapshot.tabs[0].loading = true;
    chrome.set_snapshot(snapshot.clone());
    chrome.set_focus(KeyboardFocus::Page);
    assert_eq!(address(&chrome), "sylt");
    snapshot.tabs[0].loading = false;
    chrome.set_snapshot(snapshot);
    assert_eq!(address(&chrome), "https://sighurt.example/");

    // So does a load that never started, at the next snapshot, even an unchanged one.
    let mut chrome = editing("sylt");
    chrome.key(&enter);
    chrome.set_focus(KeyboardFocus::Page);
    assert_eq!(address(&chrome), "sylt");
    chrome.set_snapshot(chrome.snapshot().clone());
    assert_eq!(address(&chrome), "https://sighurt.example/");

    // Switching tabs drops it.
    let mut chrome = editing("sylt");
    chrome.key(&enter);
    chrome.set_focus(KeyboardFocus::Page);
    let mut snapshot = chrome.snapshot().clone();
    snapshot.tabs[0].loading = true;
    chrome.set_snapshot(snapshot.clone());
    chrome.set_snapshot(switched(&snapshot, 1));
    assert_eq!(address(&chrome), "https://recept.example/sylt");

    // Typed into again, it is an edit: a commit meanwhile doesn't replace it, and the page
    // taking focus drops it.
    let mut chrome = editing("sylt");
    chrome.key(&enter);
    chrome.set_focus(KeyboardFocus::Page);
    let mut snapshot = chrome.snapshot().clone();
    snapshot.tabs[0].loading = true;
    chrome.set_snapshot(snapshot.clone());
    chrome.set_focus(KeyboardFocus::Chrome);
    assert_eq!(chrome.state().address.selected_text(), Some("sylt"));
    type_text(&mut chrome, "lingon");
    snapshot.tabs[0].url = "https://sighurt.example/sylt".into();
    snapshot.tabs[0].loading = false;
    chrome.set_snapshot(snapshot);
    assert_eq!(address(&chrome), "lingon");
    chrome.set_focus(KeyboardFocus::Page);
    assert_eq!(address(&chrome), "https://sighurt.example/sylt");
}

/// The core decides where focus goes after a command or a click in the page, and the chrome
/// follows without reporting the move back.
#[test]
fn focus_follows_the_core() {
    let mut chrome = chrome(800, 600, three_tabs());
    chrome.window_focus(FocusEvent::Activated);
    let actions = chrome.set_focus(KeyboardFocus::Chrome);
    assert!(chrome.state().focused);
    assert!(
        matches!(actions[..], [ChromeAction::TextInput(Some(_))]),
        "{actions:?}"
    );
    assert_eq!(
        chrome.state().address.selected_text(),
        Some("https://sighurt.example/")
    );
    // The page takes focus (a tab switch, back, a click in the page): what was typed is
    // dropped.
    type_text(&mut chrome, "halv");
    assert_eq!(
        chrome.set_focus(KeyboardFocus::Page),
        [ChromeAction::TextInput(None)]
    );
    assert!(!chrome.state().focused);
    assert_eq!(address(&chrome), "https://sighurt.example/");
    chrome.take_damage();
    assert_eq!(chrome.set_focus(KeyboardFocus::Page), []);
    assert!(!chrome.take_damage().any());
}

/// A clipboard answer counts only for the latest request, once, and only while the line has
/// kept focus and its tab: an answer that comes after the user left the line is dropped (§12
/// transfers are asynchronous, so a stalled source can answer late).
#[test]
fn a_late_clipboard_answer_is_ignored() {
    let ctrl_v = ctrl("v", 47);
    let mut chrome = editing("");
    let stale = paste_request(&chrome.key(&ctrl_v));
    // The user gives up on it: Escape, another tab, and into the line again.
    chrome.key(&named("Escape", Modifiers::default()));
    let other = switched(chrome.snapshot(), 1);
    chrome.set_snapshot(other);
    chrome.set_focus(KeyboardFocus::Chrome);
    chrome.paste(stale, "stale clipboard");
    assert_eq!(address(&chrome), "https://recept.example/sylt");

    let first = paste_request(&chrome.key(&ctrl_v));
    let latest = paste_request(&chrome.key(&ctrl_v));
    chrome.paste(first, "first");
    chrome.paste(latest, "latest");
    chrome.paste(latest, " again");
    assert_eq!(address(&chrome), "latest");

    // A tab switch while the line has focus drops the request too.
    let request = paste_request(&chrome.key(&ctrl_v));
    let back = switched(chrome.snapshot(), 0);
    chrome.set_snapshot(back);
    chrome.paste(request, "late");
    assert_eq!(address(&chrome), "https://sighurt.example/");
}

#[test]
fn typed_commands_run_after_a_colon() {
    let mut chrome = editing("");
    chrome.key(&ctrl("a", 30));
    type_text(&mut chrome, ":tab.close 2");
    let actions = chrome.key(&named("Enter", Modifiers::default()));
    assert_eq!(commands(&actions), [Command::TabClose(Some(n(2)))]);
    assert!(actions.contains(&ChromeAction::Focus(KeyboardFocus::Page)));
    assert_eq!(address(&chrome), "https://sighurt.example/");

    // A bad command keeps the line and focus, and says why until the line changes.
    let mut chrome = editing("");
    chrome.key(&ctrl("a", 30));
    type_text(&mut chrome, ":tab.explode");
    chrome.take_damage();
    let actions = chrome.key(&named("Enter", Modifiers::default()));
    assert_eq!(commands(&actions), []);
    assert!(chrome.state().focused);
    let error = chrome.state().command_error.clone().unwrap();
    assert!(error.contains("unknown command"), "{error}");
    assert!(
        chrome.take_damage().status,
        "the status strip shows the error"
    );
    let tree = chrome.accessibility();
    let node = tree
        .node(sighurt_ui::accessibility::NodeId::Address)
        .unwrap();
    assert!(node.states.invalid);
    chrome.key(&named("Backspace", Modifiers::default()));
    assert_eq!(chrome.state().command_error, None);
}

#[test]
fn the_committed_address_is_not_the_typed_one() {
    let mut chrome = editing("");
    chrome.key(&ctrl("a", 30));
    type_text(&mut chrome, "halv");
    // The page redirects meanwhile: the typed text stays.
    let mut snapshot = chrome.snapshot().clone();
    snapshot.tabs[0].url = "https://sighurt.example/start".into();
    chrome.set_snapshot(snapshot.clone());
    assert_eq!(address(&chrome), "halv");
    // Escape shows what the tab has committed, not what was typed.
    chrome.key(&named("Escape", Modifiers::default()));
    assert_eq!(address(&chrome), "https://sighurt.example/start");

    // Switching tabs drops what was typed.
    let mut chrome = editing("halv");
    let mut snapshot = chrome.snapshot().clone();
    snapshot.tabs[0].active = false;
    snapshot.tabs[2].active = true;
    chrome.set_snapshot(snapshot.clone());
    assert_eq!(address(&chrome), "sig://home");
    // Going back, the first tab shows its own address again.
    snapshot.tabs[0].active = true;
    snapshot.tabs[2].active = false;
    chrome.set_snapshot(snapshot);
    assert_eq!(address(&chrome), "https://sighurt.example/");
}

#[test]
fn a_new_tab_focuses_its_address_selected() {
    let mut chrome = chrome(800, 600, three_tabs());
    chrome.window_focus(FocusEvent::Activated);
    let new_tab = center(chrome.layout().new_tab);
    let actions = click(&mut chrome, new_tab);
    assert_eq!(commands(&actions), [Command::TabNew]);
    // The core opens a fourth tab, active, at home.
    let mut snapshot = chrome.snapshot().clone();
    snapshot.tabs[0].active = false;
    let mut new = tab("sig://home", "sig://home", TabContent::Internal);
    new.active = true;
    snapshot.tabs.push(new);
    chrome.set_snapshot(snapshot);
    // For tab.new, the core focuses the address line.
    chrome.set_focus(KeyboardFocus::Chrome);
    assert_eq!(chrome.state().address.selected_text(), Some("sig://home"));
    // Typing replaces it.
    type_text(&mut chrome, "å");
    assert_eq!(address(&chrome), "å");
}

#[test]
fn clicking_the_address_selects_then_places_the_cursor() {
    let mut chrome = chrome(800, 600, three_tabs());
    chrome.window_focus(FocusEvent::Activated);
    let field = chrome.layout().address;
    let actions = click(&mut chrome, center(field));
    assert!(
        actions.contains(&ChromeAction::Focus(KeyboardFocus::Chrome)),
        "{actions:?}"
    );
    let caret = actions.iter().find_map(|action| match action {
        ChromeAction::TextInput(Some(field)) => Some(field.state.clone()),
        _ => None,
    });
    assert!(caret.is_some(), "the input method is told: {actions:?}");
    assert_eq!(
        chrome.state().address.selected_text(),
        Some("https://sighurt.example/")
    );
    // A second click, left of the text's end, places the cursor there.
    let start = WindowPoint {
        x: f64::from(field.x) + 13.0,
        y: center(field).y,
    };
    click(&mut chrome, start);
    assert_eq!(chrome.state().address.selection(), 0..0);
}

#[test]
fn dragging_out_of_the_chrome_selects_and_never_sticks() {
    let mut chrome = editing("");
    chrome.key(&ctrl("a", 30));
    type_text(&mut chrome, "https://exempel.se/");
    let surface = chrome.layout().chrome;
    let field = chrome.layout().address;
    let start = WindowPoint {
        x: f64::from(field.x) + 13.0,
        y: center(field).y,
    };
    chrome.pointer(
        surface,
        &button(start, PointerButton::Left, ButtonState::Pressed),
    );
    // The host keeps sending the drag to the chrome, far outside it, until the release.
    let outside = SurfacePoint {
        x: 5000.0,
        y: 400.0,
    };
    let motion = PointerEvent::Motion {
        position: outside,
        modifiers: Modifiers::default(),
    };
    chrome.pointer(surface, &motion);
    assert_eq!(
        chrome.state().address.selected_text(),
        Some("https://exempel.se/")
    );
    let release = button(
        WindowPoint {
            x: 5000.0,
            y: 400.0,
        },
        PointerButton::Left,
        ButtonState::Released,
    );
    assert_eq!(commands(&chrome.pointer(surface, &release)), []);
    assert!(!chrome.state().selecting);
    assert_eq!(chrome.state().pressed, None);
    // Moving back without the button changes nothing.
    chrome.pointer(
        surface,
        &PointerEvent::Motion {
            position: SurfacePoint {
                x: start.x,
                y: start.y,
            },
            modifiers: Modifiers::default(),
        },
    );
    assert_eq!(
        chrome.state().address.selected_text(),
        Some("https://exempel.se/")
    );

    // Losing the window mid-drag ends it too.
    chrome.pointer(
        surface,
        &button(start, PointerButton::Left, ButtonState::Pressed),
    );
    chrome.window_focus(FocusEvent::Deactivated);
    assert!(!chrome.state().selecting);
    assert_eq!(chrome.state().pressed, None);
}

#[test]
fn tabs_select_close_and_open() {
    let mut chrome = chrome(800, 600, three_tabs());
    let layout = chrome.layout().clone();
    let ids: Vec<_> = chrome.snapshot().tabs.iter().map(|tab| tab.id).collect();
    // By identity, never by position.
    let actions = click(&mut chrome, center(layout.tabs[1].rect));
    assert_eq!(actions, [ChromeAction::SelectTab(ids[1])]);
    let actions = click(&mut chrome, center(layout.tabs[2].close.unwrap()));
    assert_eq!(actions, [ChromeAction::CloseTab(ids[2])]);
    // Pressing one tab and releasing on another is no click.
    let surface = layout.chrome;
    chrome.pointer(
        surface,
        &button(
            center(layout.tabs[0].rect),
            PointerButton::Left,
            ButtonState::Pressed,
        ),
    );
    let actions = chrome.pointer(
        surface,
        &button(
            center(layout.tabs[1].rect),
            PointerButton::Left,
            ButtonState::Released,
        ),
    );
    assert_eq!(actions, []);
    // A release without a press does nothing either.
    let actions = chrome.pointer(
        surface,
        &button(
            center(layout.tabs[1].rect),
            PointerButton::Left,
            ButtonState::Released,
        ),
    );
    assert_eq!(actions, []);
}

#[test]
fn toolbar_buttons() {
    let mut chrome = chrome(800, 600, three_tabs());
    let layout = chrome.layout().clone();
    // Back is disabled: no command, no hover.
    assert_eq!(commands(&click(&mut chrome, center(layout.back))), []);
    assert_eq!(
        commands(&click(&mut chrome, center(layout.forward))),
        [Command::PageForward]
    );
    assert_eq!(
        commands(&click(&mut chrome, center(layout.reload))),
        [Command::PageReload]
    );
    // While the page loads, the same button stops it.
    let mut snapshot = chrome.snapshot().clone();
    snapshot.tabs[0].loading = true;
    chrome.set_snapshot(snapshot);
    assert_eq!(
        commands(&click(&mut chrome, center(layout.reload))),
        [Command::PageStop]
    );
}

#[test]
fn hover_sets_the_cursor_and_repaints_only_the_chrome() {
    let mut chrome = chrome(800, 600, three_tabs());
    chrome.take_damage();
    let surface = chrome.layout().chrome;
    let over = |at: WindowPoint| PointerEvent::Motion {
        position: SurfacePoint { x: at.x, y: at.y },
        modifiers: Modifiers::default(),
    };
    let actions = chrome.pointer(surface, &over(center(chrome.layout().address)));
    assert!(
        actions.contains(&ChromeAction::Cursor(Cursor::Text)),
        "{actions:?}"
    );
    let damage = chrome.take_damage();
    assert_eq!(
        damage,
        Damage {
            chrome: true,
            tabs: false,
            status: false,
            page: false
        }
    );
    // Moving within the same control repaints nothing.
    let mut inside = center(chrome.layout().address);
    inside.x += 3.0;
    assert_eq!(chrome.pointer(surface, &over(inside)), []);
    assert!(!chrome.take_damage().any());
    let actions = chrome.pointer(surface, &over(center(chrome.layout().reload)));
    assert!(
        actions.contains(&ChromeAction::Cursor(Cursor::Pointer)),
        "{actions:?}"
    );
    let actions = chrome.pointer(surface, &PointerEvent::Leave);
    assert_eq!(actions, [ChromeAction::Cursor(Cursor::Default)]);
    // The disabled back button doesn't react.
    chrome.pointer(surface, &over(center(chrome.layout().back)));
    assert_eq!(chrome.state().hover, None);
}

#[test]
fn idle_snapshots_repaint_nothing() {
    let mut chrome = chrome(800, 600, three_tabs());
    chrome.take_damage();
    // The executable may refresh the snapshot for reasons the chrome doesn't show (a page
    // painting a frame changes nothing here): no damage.
    let same = chrome.snapshot().clone();
    assert_eq!(chrome.set_snapshot(same.clone()), []);
    assert!(!chrome.take_damage().any());
    // A hovered link repaints the status strip alone.
    let mut hovered = same.clone();
    hovered.status = Some("https://sighurt.example/other".into());
    chrome.set_snapshot(hovered);
    assert_eq!(
        chrome.take_damage(),
        Damage {
            chrome: false,
            tabs: false,
            status: true,
            page: false
        }
    );
    // A title repaints the chrome alone; the home page's contents don't matter while an engine
    // shows the page.
    let mut titled = chrome.snapshot().clone();
    titled.tabs[1].title = "Lingonsylt".into();
    titled.home.config_path = "/elsewhere".into();
    chrome.set_snapshot(titled);
    assert_eq!(
        chrome.take_damage(),
        Damage {
            chrome: true,
            tabs: false,
            status: false,
            page: false
        }
    );
    // Switching to the home tab repaints the page area too.
    let mut home = chrome.snapshot().clone();
    home.tabs[0].active = false;
    home.tabs[2].active = true;
    chrome.set_snapshot(home);
    assert!(chrome.draws_page());
    assert_eq!(
        chrome.take_damage(),
        Damage {
            chrome: true,
            tabs: false,
            status: false,
            page: true
        }
    );
}

#[test]
fn input_method_preedit_and_commit() {
    let mut chrome = editing("sök ");
    assert_eq!(chrome.text_input(&TextInputEvent::default()), []);
    let actions = chrome.text_input(&TextInputEvent {
        preedit: Some(Preedit {
            text: "かな".into(),
            cursor: Some(6..6),
        }),
        ..TextInputEvent::default()
    });
    // The caret moved right, past the preedit, and the text itself is unchanged.
    let state = actions
        .iter()
        .find_map(|action| match action {
            ChromeAction::TextInput(Some(field)) => Some(field.state.clone()),
            _ => None,
        })
        .expect("the caret moved");
    assert_eq!(address(&chrome), "sök ");
    assert_eq!(state.surrounding.as_ref().unwrap().text, "sök ");
    chrome.text_input(&TextInputEvent {
        commit: Some("かな".into()),
        ..TextInputEvent::default()
    });
    assert_eq!(address(&chrome), "sök かな");
    assert_eq!(chrome.state().address.preedit(), None);
}

/// The input method's caret rectangle is where the caret is drawn, at every scale and at two
/// font sizes.
#[test]
fn the_input_method_caret_is_where_the_caret_is_drawn() {
    for (scale, font_size) in SCALES
        .into_iter()
        .flat_map(|scale| [(scale, 14), (scale, 18)])
    {
        let options = LayoutOptions {
            font_size,
            ..LayoutOptions::default()
        };
        let mut chrome = arranged(800, 600, three_tabs(), Colors::default(), options);
        chrome.window_focus(FocusEvent::Activated);
        chrome.set_focus(KeyboardFocus::Chrome);
        type_text(&mut chrome, "blåbär");
        // A trailing space keeps the caret clear of the last glyph.
        let state = chrome
            .key(&typed(" "))
            .into_iter()
            .find_map(|action| match action {
                ChromeAction::TextInput(Some(field)) => Some(field.state),
                _ => None,
            })
            .expect("typing moves the caret");
        let rect = chrome.layout().chrome;
        let image = paint(&mut chrome, CHROME, scale);
        let ratio = f64::from(image.width) / f64::from(rect.width);
        let what = format!("{font_size} px at {scale}/120");
        // The caret is the rightmost run of text-colored pixels in the middle of the line.
        let text = Colors::default().text;
        let text = [text.r, text.g, text.b, 255];
        let top = (f64::from(state.caret.y) * ratio).round() as u32;
        let bottom = (f64::from(state.caret.y + state.caret.height as i32) * ratio).round() as u32;
        let middle = (top + bottom) / 2;
        let is_text = |x: u32| image.pixel(x, middle) == text;
        let right = (0..image.width)
            .rev()
            .find(|&x| is_text(x))
            .expect("a caret is drawn");
        let mut left = right;
        while left > 0 && is_text(left - 1) {
            left -= 1;
        }
        let reported = f64::from(state.caret.x) * ratio;
        assert!(
            (reported - f64::from(left)).abs() <= ratio + 0.5,
            "{what}: caret reported at {reported} px, drawn from {left} px"
        );
        // It is as tall as reported.
        for y in top..bottom {
            assert_eq!(image.pixel(right, y), text, "{what}, row {y}");
        }
    }
}

/// Hit targets stay on the drawn controls at 100 %, 125 %, 150 % and 200 %, with every
/// `[chrome.standard]` option and two font sizes: every pixel painted in a control's own color
/// hits that control, and every pixel that hits a control lies within what is drawn for it.
/// Distinct colors make each control's drawing identifiable.
#[test]
fn hit_targets_stay_on_drawn_controls_at_every_scale() {
    let colors = Colors {
        hover: Rgba::rgb(0xff0000),
        tab: Rgba::rgb(0x00ff00),
        surface: Rgba::rgb(0x0000ff),
        ..Colors::default()
    };
    let default = LayoutOptions::default();
    let variants = [
        default,
        LayoutOptions {
            compact: true,
            ..default
        },
        LayoutOptions {
            buttons: false,
            status: Status::Overlay,
            ..default
        },
        LayoutOptions {
            tabs: Tabs::Left,
            ..default
        },
        LayoutOptions {
            tabs: Tabs::Left,
            compact: true,
            font_size: 18,
            ..default
        },
        LayoutOptions {
            tabs: Tabs::Hidden,
            font_size: 18,
            ..default
        },
    ];
    for options in variants {
        let sizes: &[(u32, u32)] = match options == default {
            true => &[(800, 600), (1001, 777), (333, 250)],
            false => &[(1001, 777), (333, 250)],
        };
        for &(width, height) in sizes {
            for scale in SCALES {
                let mut chrome = arranged(width, height, three_tabs(), colors, options);
                let layout = chrome.layout().clone();
                let what = format!("{width}×{height} at {scale}/120, {options:?}");
                let active = Target::Tab(chrome.snapshot().tabs[0].id);
                // The hovered button, the active tab and the address field, each in its own
                // color, on the surface each is drawn on.
                let hovered = match options.buttons {
                    true => vec![Some(Target::Forward), Some(Target::ReloadOrStop)],
                    false => vec![None],
                };
                for target in hovered {
                    let mut expected = vec![(CHROME, Target::Address, colors.surface)];
                    if let Some(target) = target {
                        let rect = match target {
                            Target::Forward => layout.forward,
                            _ => layout.reload,
                        };
                        let at = center(rect);
                        chrome.pointer(
                            layout.chrome,
                            &PointerEvent::Motion {
                                position: SurfacePoint { x: at.x, y: at.y },
                                modifiers: Modifiers::default(),
                            },
                        );
                        expected.push((CHROME, target, colors.hover));
                    }
                    match options.tabs {
                        Tabs::Top => expected.push((CHROME, active, colors.tab)),
                        Tabs::Left => expected.push((TABS, active, colors.tab)),
                        Tabs::Hidden => {}
                    }
                    for surface in [CHROME, TABS] {
                        let mut on = expected.iter().filter(|(s, ..)| *s == surface).peekable();
                        if on.peek().is_none() {
                            continue;
                        }
                        let image = paint(&mut chrome, surface, scale);
                        let rect = rect_of(&chrome, surface);
                        for &(_, target, color) in on {
                            check_target(&image, rect, &layout, target, color, &what);
                        }
                    }
                }
            }
        }
    }
}

/// Checks `target`, drawn in `color` on `image`, a surface placed at `surface`.
fn check_target(
    image: &Image,
    surface: WindowRect,
    layout: &sighurt_ui::Layout,
    target: Target,
    color: Rgba,
    what: &str,
) {
    let ratio = (
        f64::from(image.width) / f64::from(surface.width),
        f64::from(image.height) / f64::from(surface.height),
    );
    let color = [color.r, color.g, color.b, 255];
    // A tab's close button sits on the tab, drawn over its background.
    let part_of = |hit: Target| match (hit, target) {
        (Target::CloseTab(close), Target::Tab(tab)) => close == tab,
        _ => hit == target,
    };
    let hits = |x: u32, y: u32| {
        let point = WindowPoint {
            x: f64::from(surface.x) + (f64::from(x) + 0.5) / ratio.0,
            y: f64::from(surface.y) + (f64::from(y) + 0.5) / ratio.1,
        };
        hit_test(layout, point).is_some_and(part_of)
    };
    let (mut left, mut top, mut right, mut bottom) = (u32::MAX, u32::MAX, 0, 0);
    for y in 0..image.height {
        for x in 0..image.width {
            if image.pixel(x, y) == color {
                assert!(
                    hits(x, y),
                    "{what}: {target:?}'s pixel ({x}, {y}) hits something else"
                );
                (left, top) = (left.min(x), top.min(y));
                (right, bottom) = (right.max(x), bottom.max(y));
            }
        }
    }
    assert!(left <= right, "{what}: {target:?} isn't drawn");
    // Borders and anti-aliased corners are drawn in other colors, so allow the address
    // field's border (a pixel or two) around the colored area.
    let slack = (ratio.0.round() as u32).max(1) + 1;
    for y in 0..image.height {
        for x in 0..image.width {
            if hits(x, y) {
                let inside = x + slack >= left
                    && x <= right + slack
                    && y + slack >= top
                    && y <= bottom + slack;
                assert!(
                    inside,
                    "{what}: ({x}, {y}) hits {target:?} outside its drawing"
                );
            }
        }
    }
}

/// Positions arrive local to the surface the event came from and are converted once: a click
/// on the chrome's own page (at y = 84 in the window) at local y = 10 is on the page, not on the
/// tab strip at window y = 10.
#[test]
fn surface_positions_are_converted_once() {
    let mut chrome = chrome(800, 600, showing("sig://home", TabContent::Internal));
    chrome.set_focus(KeyboardFocus::Chrome);
    let page = chrome.layout().page;
    let local = WindowPoint { x: 10.0, y: 10.0 };
    let actions = chrome.pointer(
        page,
        &button(local, PointerButton::Left, ButtonState::Pressed),
    );
    assert!(
        actions.contains(&ChromeAction::Focus(KeyboardFocus::Page)),
        "{actions:?}"
    );
    assert_eq!(chrome.state().pressed, None);
    // The status strip: local (400, 10) is the strip's middle, where nothing reacts.
    let status = chrome.layout().status;
    let actions = chrome.pointer(
        status,
        &PointerEvent::Motion {
            position: SurfacePoint { x: 400.0, y: 10.0 },
            modifiers: Modifiers::default(),
        },
    );
    assert_eq!(actions, []);
    assert_eq!(chrome.state().hover, None);
}

#[test]
fn accessibility_matches_the_hit_targets() {
    let mut chrome = chrome(800, 600, three_tabs());
    chrome.set_focus(KeyboardFocus::Chrome);
    let layout = chrome.layout().clone();
    let tree = chrome.accessibility();
    assert_eq!(tree.focus, Some(sighurt_ui::accessibility::NodeId::Address));
    let mut targets = vec![
        (Target::NewTab, layout.new_tab),
        (Target::Back, layout.back),
        (Target::Forward, layout.forward),
        (Target::ReloadOrStop, layout.reload),
        (Target::Address, layout.address),
    ];
    for tab in &layout.tabs {
        targets.push((Target::Tab(tab.id), tab.rect));
        targets.push((Target::CloseTab(tab.id), tab.close.unwrap()));
    }
    for (target, rect) in targets {
        let node = tree
            .node(sighurt_ui::accessibility::node_for(target))
            .unwrap_or_else(|| panic!("no node for {target:?}"));
        assert_eq!(node.bounds, rect, "{target:?}");
    }
    let back = tree.node(sighurt_ui::accessibility::NodeId::Back).unwrap();
    assert!(back.states.disabled);
    let tab = tree
        .node(sighurt_ui::accessibility::NodeId::Tab(layout.tabs[1].id))
        .unwrap();
    assert!(tab.states.busy && !tab.states.selected);
    assert!(tab.name.starts_with("Blåbärssylt"));

    // The home page's text is in the tree, not only in pixels.
    let mut home = common::chrome(800, 600, showing("sig://home", TabContent::Internal));
    let tree = home.accessibility();
    let page = tree.node(sighurt_ui::accessibility::NodeId::Page).unwrap();
    let names: Vec<&str> = page
        .children
        .iter()
        .map(|id| tree.node(*id).unwrap().name.as_str())
        .collect();
    assert_eq!(names[0], "Sighurt");
    assert!(names.contains(&"servo"), "{names:?}");
}

#[test]
fn an_empty_snapshot_and_window_are_harmless() {
    let mut chrome = chrome(0, 0, Snapshot::default());
    assert_eq!(click(&mut chrome, WindowPoint { x: 1.0, y: 1.0 }), []);
    chrome.set_focus(KeyboardFocus::Chrome);
    type_text(&mut chrome, "a");
    assert_eq!(address(&chrome), "a");
    let actions = chrome.key(&named("Enter", Modifiers::default()));
    assert_eq!(commands(&actions), [Command::PageOpen("a".into())]);
}

/// The mouse's side buttons are the core's (`[input.buttons]`), wherever the pointer is: the
/// chrome does nothing with them.
#[test]
fn side_buttons_are_not_the_chromes() {
    let mut chrome = chrome(800, 600, three_tabs());
    let surface = chrome.layout().chrome;
    let at = center(chrome.layout().tab_strip);
    for side in [PointerButton::Back, PointerButton::Forward] {
        for state in [ButtonState::Pressed, ButtonState::Released] {
            assert_eq!(chrome.pointer(surface, &button(at, side, state)), []);
        }
    }
}

/// The address line shows a committed address decoded where that is safe, keeps the committed
/// one apart, and opens what it shows as the same address.
#[test]
fn the_address_line_reads_decoded() {
    let committed = "https://sv.wikipedia.org/wiki/R%C3%A4ksm%C3%B6rg%C3%A5s#K%C3%A4llor%E2%80%AE";
    let mut chrome = chrome(800, 600, showing(committed, TabContent::Engine));
    let shown = "https://sv.wikipedia.org/wiki/Räksmörgås#Källor%E2%80%AE";
    assert_eq!(address(&chrome), shown);
    assert!(!chrome.state().address.is_modified());
    // The page's accessible value is the committed address, as it was.
    let tree = chrome.accessibility();
    assert_eq!(
        tree.node(NodeId::Page).unwrap().value.as_deref(),
        Some(committed)
    );
    // An unchanged snapshot changes nothing; Escape after focusing shows the same again.
    chrome.take_damage();
    chrome.set_snapshot(chrome.snapshot().clone());
    assert!(!chrome.take_damage().any());
    chrome.window_focus(FocusEvent::Activated);
    chrome.set_focus(KeyboardFocus::Chrome);
    assert_eq!(chrome.state().address.selected_text(), Some(shown));
    chrome.key(&named("Escape", Modifiers::default()));
    assert_eq!(address(&chrome), shown);
    // Enter opens what it shows, which the core parses back to the committed address.
    chrome.set_focus(KeyboardFocus::Chrome);
    let actions = chrome.key(&named("Enter", Modifiers::default()));
    assert_eq!(commands(&actions), [Command::PageOpen(shown.into())]);
}

/// The tab list on the left scrolls by whole rows with the wheel or a touchpad, only over the
/// list, repainting the list alone; a switch to a tab out of view scrolls it into view.
#[test]
fn the_tab_list_scrolls_and_follows_the_active_tab() {
    let left = LayoutOptions {
        tabs: Tabs::Left,
        ..LayoutOptions::default()
    };
    let mut snapshot = three_tabs();
    snapshot.tabs = (0..40)
        .map(|i| {
            tab(
                &format!("Tab {i}"),
                "https://a.example/",
                TabContent::Engine,
            )
        })
        .collect();
    snapshot.tabs[0].active = true;
    // 15 rows of 36 px fit below the toolbar (44 px) and above the status strip (20 px).
    let mut chrome = arranged(800, 44 + 556 + 20, snapshot, Colors::default(), left);
    let ids: Vec<_> = chrome.snapshot().tabs.iter().map(|tab| tab.id).collect();
    let list = chrome.layout().tab_list;
    assert_eq!(chrome.layout().rows, 15);
    let wheel = |y| PointerEvent::Scroll {
        delta: sighurt_api::input::ScrollDelta::Wheel { x: 0, y },
        position: SurfacePoint { x: 50.0, y: 50.0 },
        modifiers: Modifiers::default(),
    };
    let first_row = |chrome: &Model| {
        let layout = chrome.layout();
        let shown = layout
            .tabs
            .iter()
            .position(|tab| !tab.rect.size().is_empty());
        shown.map(|i| layout.tabs[i].id)
    };
    // A notch scrolls three rows, and repaints the list alone.
    chrome.take_damage();
    chrome.pointer(list, &wheel(120));
    assert_eq!(first_row(&chrome), Some(ids[3]));
    assert_eq!(
        chrome.take_damage(),
        Damage {
            tabs: true,
            ..Damage::default()
        }
    );
    // A click on its first row shown selects that tab, by identity.
    let top_row = WindowPoint {
        x: 50.0,
        y: 4.0 + 16.0,
    };
    let local = |at: WindowPoint| WindowPoint { x: at.x, y: at.y };
    let press = chrome.pointer(
        list,
        &button(local(top_row), PointerButton::Left, ButtonState::Pressed),
    );
    let release = chrome.pointer(
        list,
        &button(local(top_row), PointerButton::Left, ButtonState::Released),
    );
    assert_eq!([press, release].concat(), [ChromeAction::SelectTab(ids[3])]);
    // A touchpad's pixels move a row for each row's height; half a row waits for more.
    chrome.pointer(
        list,
        &PointerEvent::Scroll {
            delta: sighurt_api::input::ScrollDelta::Pixels { x: 0.0, y: 54.0 },
            position: SurfacePoint { x: 50.0, y: 50.0 },
            modifiers: Modifiers::default(),
        },
    );
    assert_eq!(first_row(&chrome), Some(ids[4]));
    chrome.pointer(
        list,
        &PointerEvent::Scroll {
            delta: sighurt_api::input::ScrollDelta::Pixels { x: 0.0, y: 18.0 },
            position: SurfacePoint { x: 50.0, y: 50.0 },
            modifiers: Modifiers::default(),
        },
    );
    assert_eq!(first_row(&chrome), Some(ids[5]));
    // Past either end it stops: the last rows, then the first.
    for _ in 0..20 {
        chrome.pointer(list, &wheel(120));
    }
    assert_eq!(chrome.layout().first_row, 41 - 15);
    for _ in 0..20 {
        chrome.pointer(list, &wheel(-120));
    }
    assert_eq!(first_row(&chrome), Some(ids[0]));
    // The wheel over the toolbar scrolls nothing.
    let toolbar = chrome.layout().chrome;
    let over_toolbar = PointerEvent::Scroll {
        delta: sighurt_api::input::ScrollDelta::Wheel { x: 0, y: 120 },
        position: SurfacePoint { x: 50.0, y: 20.0 },
        modifiers: Modifiers::default(),
    };
    chrome.pointer(toolbar, &over_toolbar);
    assert_eq!(first_row(&chrome), Some(ids[0]));
    // Switching to tab 30 (tab.select 30) brings it into view as the last row; back to the
    // first brings that back as the first row.
    let mut switched = chrome.snapshot().clone();
    for (i, tab) in switched.tabs.iter_mut().enumerate() {
        tab.active = i == 29;
    }
    chrome.set_snapshot(switched.clone());
    let layout = chrome.layout();
    assert_eq!(layout.first_row, 15);
    assert!(!layout.tabs[29].rect.size().is_empty());
    for (i, tab) in switched.tabs.iter_mut().enumerate() {
        tab.active = i == 0;
    }
    chrome.set_snapshot(switched);
    assert_eq!(chrome.layout().first_row, 0);
}

/// Where buffer pixel `(x, y)` of a surface placed at `surface`, painted as `image`, is in the
/// window: its centre.
fn window_point(image: &Image, surface: WindowRect, x: u32, y: u32) -> WindowPoint {
    WindowPoint {
        x: f64::from(surface.x)
            + (f64::from(x) + 0.5) * f64::from(surface.width) / f64::from(image.width),
        y: f64::from(surface.y)
            + (f64::from(y) + 0.5) * f64::from(surface.height) / f64::from(image.height),
    }
}

/// The pixels where `a` and `b` differ, as buffer coordinates.
fn differing(a: &Image, b: &Image) -> Vec<(u32, u32)> {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let mut pixels = Vec::new();
    for y in 0..a.height {
        for x in 0..a.width {
            if a.pixel(x, y) != b.pixel(x, y) {
                pixels.push((x, y));
            }
        }
    }
    pixels
}

/// Presses and releases the primary button at window position `at` on the surface placed at
/// `surface`.
fn click_on(chrome: &mut Model, surface: WindowRect, at: WindowPoint) -> Vec<ChromeAction> {
    let local = WindowPoint {
        x: at.x - f64::from(surface.x),
        y: at.y - f64::from(surface.y),
    };
    let mut actions = Vec::new();
    for state in [ButtonState::Pressed, ButtonState::Released] {
        actions.extend(chrome.pointer(surface, &button(local, PointerButton::Left, state)));
    }
    actions
}

/// Three tabs with short titles, so a label never cuts one: a Servo page (active), a WPE page
/// and the home page, which names an engine the chrome must not show.
fn short_titled() -> Snapshot {
    let mut snapshot = labelled(three_tabs(), &["servo", "wpe", "servo"]);
    snapshot.tabs[0].title = "A".into();
    snapshot.tabs[1].title = "B".into();
    snapshot
}

/// The engine label lies inside its tab, where a press selects the tab as anywhere else on it,
/// and off the close button, at 100 %, 125 %, 150 % and 200 %, with the tabs at the top and on
/// the left, compact and at 18 px. The close button's drawing still hits the close button, and
/// a click on the label selects its tab and one on the close button closes it.
#[test]
fn engine_labels_stay_on_their_tabs_at_every_scale() {
    let colors = Colors {
        text_dim: Rgba::rgb(0xff00ff),
        ..Colors::default()
    };
    let close_color = [0xff, 0x00, 0xff, 0xff];
    let default = LayoutOptions::default();
    let left = LayoutOptions {
        tabs: Tabs::Left,
        ..default
    };
    let variants = [
        default,
        LayoutOptions {
            compact: true,
            ..default
        },
        LayoutOptions {
            font_size: 18,
            ..default
        },
        left,
        LayoutOptions {
            compact: true,
            font_size: 18,
            ..left
        },
    ];
    for options in variants {
        for scale in SCALES {
            let what = format!("at {scale}/120, {options:?}");
            let without = LayoutOptions {
                engine_label: false,
                ..options
            };
            let mut chrome = arranged(800, 600, short_titled(), colors, options);
            let mut bare = arranged(800, 600, short_titled(), colors, without);
            let surface = match options.tabs {
                Tabs::Left => TABS,
                _ => CHROME,
            };
            let rect = rect_of(&chrome, surface);
            let layout = chrome.layout().clone();
            let ids: Vec<_> = layout.tabs.iter().map(|tab| tab.id).collect();
            let image = paint(&mut chrome, surface, scale);
            let unlabelled = paint(&mut bare, surface, scale);
            // Each label's pixels, by the tab whose label they are: the bounds in the buffer.
            let mut labels: Vec<Option<[u32; 4]>> = vec![None; ids.len()];
            for (x, y) in differing(&image, &unlabelled) {
                let point = window_point(&image, rect, x, y);
                if layout.address.contains(point) {
                    continue;
                }
                let Some(Target::Tab(id)) = hit_test(&layout, point) else {
                    panic!(
                        "{what}: label pixel ({x}, {y}) hits {:?}",
                        hit_test(&layout, point)
                    );
                };
                let i = ids.iter().position(|&tab| tab == id).unwrap();
                let [l, t, r, b] = labels[i].get_or_insert([x, y, x, y]);
                (*l, *t, *r, *b) = ((*l).min(x), (*t).min(y), (*r).max(x), (*b).max(y));
            }
            assert!(
                labels[0].is_some() && labels[1].is_some(),
                "{what}: {labels:?}"
            );
            assert_eq!(labels[2], None, "{what}: the home page's tab has no label");
            // The close buttons are drawn where they are hit (a disabled back button in the
            // toolbar has their color too).
            let mut crosses = 0;
            let tab_area = match options.tabs {
                Tabs::Left => layout.tab_list,
                _ => layout.tab_strip,
            };
            for y in 0..image.height {
                for x in 0..image.width {
                    let point = window_point(&image, rect, x, y);
                    if image.pixel(x, y) == close_color && tab_area.contains(point) {
                        let hit = hit_test(&layout, point);
                        assert!(
                            matches!(hit, Some(Target::CloseTab(_))),
                            "{what}: close button pixel ({x}, {y}) hits {hit:?}"
                        );
                        crosses += 1;
                    }
                }
            }
            assert!(crosses > 0, "{what}: no close button drawn");
            // A click on the second tab's label selects it; one on its close button closes it.
            let [l, t, r, b] = labels[1].unwrap();
            let label = window_point(&image, rect, (l + r) / 2, (t + b) / 2);
            let place = rect_of(&chrome, surface);
            assert_eq!(
                click_on(&mut chrome, place, label),
                [ChromeAction::SelectTab(ids[1])],
                "{what}"
            );
            let close = center(layout.tabs[1].close.unwrap());
            assert_eq!(
                click_on(&mut chrome, place, close),
                [ChromeAction::CloseTab(ids[1])],
                "{what}"
            );
        }
    }
}

/// Whether a tab shows its engine label depends on the tab's room, not on its title: of six
/// equally wide Servo tabs with long and one-letter titles, all are labelled, or in a narrow
/// window none is, at every scale.
#[test]
fn tabs_of_one_width_agree_on_their_labels() {
    let options = LayoutOptions::default();
    let without = LayoutOptions {
        engine_label: false,
        ..options
    };
    let mut snapshot = three_tabs();
    snapshot.tabs = (0..6)
        .map(|i| {
            let title = ["Sighurt — a small browser", "A"][i % 2];
            let mut tab = tab(title, "https://sighurt.example/", TabContent::Engine);
            tab.engine = Some("servo".into());
            tab.active = i == 0;
            tab
        })
        .collect();
    for (width, shown) in [(600, false), (1400, true)] {
        for scale in SCALES {
            let colors = Colors::default();
            let mut chrome = arranged(width, 400, snapshot.clone(), colors, options);
            let mut bare = arranged(width, 400, snapshot.clone(), colors, without);
            let rect = rect_of(&chrome, CHROME);
            let layout = chrome.layout().clone();
            let image = paint(&mut chrome, CHROME, scale);
            let unlabelled = paint(&mut bare, CHROME, scale);
            let mut shows = vec![false; layout.tabs.len()];
            for (x, y) in differing(&image, &unlabelled) {
                let point = window_point(&image, rect, x, y);
                if let Some(Target::Tab(id)) = hit_test(&layout, point) {
                    shows[layout.tabs.iter().position(|tab| tab.id == id).unwrap()] = true;
                }
            }
            assert_eq!(shows, [shown; 6], "{width} px at {scale}/120");
        }
    }
}

/// The engine label at the address field's right end: the field's text ends before it at
/// every scale, compact, at 18 px and beside a keymap's indicator, and it hides while the line
/// is being edited.
#[test]
fn the_address_fields_engine_label_never_overlaps_its_text() {
    let default = LayoutOptions::default();
    let variants = [
        (default, None),
        (
            LayoutOptions {
                compact: true,
                ..default
            },
            Some("NORMAL"),
        ),
        (
            LayoutOptions {
                font_size: 18,
                ..default
            },
            None,
        ),
    ];
    let long = "https://sighurt.example/a/very/long/address/that/runs/under/the/label/if/it/could";
    for (options, keymap) in variants {
        for scale in SCALES {
            let what = format!("at {scale}/120, {options:?}, {keymap:?}");
            let snapshot = |url: &str| {
                let mut snapshot = labelled(three_tabs(), &["servo"]);
                snapshot.tabs[0].url = url.into();
                snapshot.keymap = keymap.map(Into::into);
                snapshot
            };
            let without = LayoutOptions {
                engine_label: false,
                ..options
            };
            let colors = Colors::default();
            let mut full = arranged(500, 300, snapshot(long), colors, options);
            let mut label_only = arranged(500, 300, snapshot(""), colors, options);
            let mut neither = arranged(500, 300, snapshot(""), colors, without);
            let rect = full.layout().chrome;
            let field = full.layout().address;
            let toolbar = full.layout().toolbar;
            let [full_image, label_image, bare_image] = [&mut full, &mut label_only, &mut neither]
                .map(|chrome| paint(chrome, CHROME, scale));
            let in_field = |pixels: Vec<(u32, u32)>| -> Vec<(u32, u32)> {
                let inside = |&(x, y): &(u32, u32)| {
                    let point = window_point(&full_image, rect, x, y);
                    // Every changed pixel of the toolbar is inside the field.
                    if toolbar.contains(point) {
                        assert!(
                            field.contains(point),
                            "{what}: ({x}, {y}) outside the field"
                        );
                    }
                    field.contains(point)
                };
                pixels.into_iter().filter(inside).collect()
            };
            let text = in_field(differing(&full_image, &label_image));
            let label = in_field(differing(&label_image, &bare_image));
            assert!(!text.is_empty() && !label.is_empty(), "{what}");
            let text_right = text.iter().map(|&(x, _)| x).max().unwrap();
            let label_left = label.iter().map(|&(x, _)| x).min().unwrap();
            assert!(
                text_right < label_left,
                "{what}: the text reaches {text_right}, the label starts at {label_left}"
            );
            // While the line is being edited, the label hides.
            for chrome in [&mut full, &mut neither] {
                chrome.window_focus(FocusEvent::Activated);
                chrome.set_focus(KeyboardFocus::Chrome);
                type_text(chrome, long);
            }
            let typing = paint(&mut full, CHROME, scale);
            let bare_typing = paint(&mut neither, CHROME, scale);
            let typed = in_field(differing(&typing, &bare_typing));
            assert!(typed.is_empty(), "{what}: {} pixels differ", typed.len());
        }
    }
}

/// The tree describes each engine page's tab by its engine, while the chrome labels tabs.
#[test]
fn the_tree_names_each_tabs_engine() {
    let mut chrome = chrome(800, 600, short_titled());
    let ids: Vec<_> = chrome.snapshot().tabs.iter().map(|tab| tab.id).collect();
    let descriptions = |chrome: &mut Model| {
        let tree = chrome.accessibility();
        let ids = ids.iter();
        ids.map(|&id| tree.node(NodeId::Tab(id)).unwrap().description.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        descriptions(&mut chrome),
        [
            Some("Engine: servo".into()),
            Some("Engine: wpe".into()),
            None
        ]
    );
    chrome.set_options(LayoutOptions {
        engine_label: false,
        ..LayoutOptions::default()
    });
    assert_eq!(descriptions(&mut chrome), [None, None, None]);
}

/// A click on the engine label at the address field's right end runs `page.switch-engine`.
/// The hit is the text that is drawn: beside it the click is an address click, hovering it is
/// hovering the address line, and releasing elsewhere runs nothing.
#[test]
fn a_click_on_the_address_fields_engine_label_switches_engine() {
    let options = LayoutOptions::default();
    let without = LayoutOptions {
        engine_label: false,
        ..options
    };
    let mut snapshot = labelled(three_tabs(), &["servo"]);
    snapshot.tabs[0].url = "https://a.test/".into();
    let colors = Colors::default();
    let mut chrome = arranged(800, 600, snapshot.clone(), colors, options);
    let mut bare = arranged(800, 600, snapshot.clone(), colors, without);
    let rect = chrome.layout().chrome;
    let field = chrome.layout().address;
    let image = paint(&mut chrome, CHROME, 120);
    let unlabelled = paint(&mut bare, CHROME, 120);
    let label: Vec<(u32, u32)> = differing(&image, &unlabelled)
        .into_iter()
        .filter(|&(x, y)| field.contains(window_point(&image, rect, x, y)))
        .collect();
    assert!(!label.is_empty(), "the address field shows no engine label");
    let (left, right, top, bottom) = label
        .iter()
        .fold((u32::MAX, 0, u32::MAX, 0), |(l, r, t, b), &(x, y)| {
            (l.min(x), r.max(x), t.min(y), b.max(y))
        });
    let (cx, cy) = ((left + right) / 2, (top + bottom) / 2);
    let (x, y) = label
        .iter()
        .copied()
        .min_by_key(|&(px, py)| {
            let (dx, dy) = (px as i32 - cx as i32, py as i32 - cy as i32);
            dx * dx + dy * dy
        })
        .unwrap();
    let at = window_point(&image, rect, x, y);

    let actions = click(&mut chrome, at);
    assert_eq!(commands(&actions), [Command::PageSwitchEngine]);
    assert!(!chrome.state().focused);
    assert!(actions
        .iter()
        .all(|action| !matches!(action, ChromeAction::Focus(_))));

    // Just left of the drawn label is still the address line.
    let beside_x = left.saturating_sub(4);
    assert!(label.iter().all(|&(px, _)| px != beside_x));
    let beside = window_point(&image, rect, beside_x, cy);
    assert!(
        field.contains(beside),
        "{beside:?} is outside the field {field:?}"
    );
    let actions = click(&mut chrome, beside);
    assert!(commands(&actions).is_empty());
    assert!(chrome.state().focused);
    assert!(actions
        .iter()
        .any(|action| { matches!(action, ChromeAction::Focus(KeyboardFocus::Chrome)) }));
    // While the line is focused the label is hidden, so that point places the caret.
    let actions = click(&mut chrome, at);
    assert!(commands(&actions).is_empty());
    assert!(chrome.state().focused);

    // With the label turned off, the same point is an address click.
    let actions = click(&mut bare, at);
    assert!(commands(&actions).is_empty());
    assert!(bare.state().focused);

    let button_at = |chrome: &mut Model, at: WindowPoint, state: ButtonState| {
        let surface = chrome.layout().chrome;
        chrome.pointer(
            surface,
            &PointerEvent::Button {
                button: PointerButton::Left,
                state,
                position: surface.to_surface(at),
                modifiers: Modifiers::default(),
            },
        )
    };
    // Hovering the label is hovering the address line, and a second move changes nothing.
    let mut chrome = arranged(800, 600, snapshot.clone(), colors, options);
    let surface = chrome.layout().chrome;
    let motion = |chrome: &mut Model| {
        chrome.pointer(
            surface,
            &PointerEvent::Motion {
                position: surface.to_surface(at),
                modifiers: Modifiers::default(),
            },
        )
    };
    let actions = motion(&mut chrome);
    assert!(commands(&actions).is_empty());
    assert_eq!(chrome.state().hover, Some(Target::Address));
    assert!(actions
        .iter()
        .any(|action| matches!(action, ChromeAction::Cursor(Cursor::Text))));
    let again = motion(&mut chrome);
    assert!(again
        .iter()
        .all(|action| { !matches!(action, ChromeAction::Cursor(_) | ChromeAction::Command(_)) }));

    // Press on the label and release elsewhere: no command, and the line stays unfocused.
    let mut actions = button_at(&mut chrome, at, ButtonState::Pressed);
    let outside = WindowPoint { x: 0.0, y: 0.0 };
    actions.extend(button_at(&mut chrome, outside, ButtonState::Released));
    assert!(commands(&actions).is_empty());
    assert!(!chrome.state().focused);

    // A drag that starts in the address text and ends on the label does not switch.
    let mut chrome = arranged(800, 600, snapshot.clone(), colors, options);
    let mut actions = button_at(&mut chrome, beside, ButtonState::Pressed);
    actions.extend(button_at(&mut chrome, at, ButtonState::Released));
    assert!(commands(&actions).is_empty());
    assert!(chrome.state().focused);

    // Losing the window between the press and the release cancels the click.
    let mut chrome = arranged(800, 600, snapshot, colors, options);
    let mut actions = button_at(&mut chrome, at, ButtonState::Pressed);
    actions.extend(chrome.window_focus(FocusEvent::Deactivated));
    actions.extend(button_at(&mut chrome, at, ButtonState::Released));
    assert!(commands(&actions).is_empty());
    assert!(!chrome.state().focused);
}
