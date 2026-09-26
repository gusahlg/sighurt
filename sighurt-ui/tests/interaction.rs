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
    ButtonState, FocusEvent, Key, KeyCode, KeyEvent, Modifiers, PointerButton, PointerEvent,
    Preedit, TextInputEvent,
};
use sighurt_ui::accessibility::NodeId;
use sighurt_ui::hit_test::{hit_test, Target};
use sighurt_ui::{Chrome, Colors, Damage, Focus, Rgba, Snapshot, TabContent, UiAction};

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

fn type_text(chrome: &mut Chrome, text: &str) {
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
fn click(chrome: &mut Chrome, at: WindowPoint) -> Vec<UiAction> {
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

fn commands(actions: &[UiAction]) -> Vec<Command> {
    actions
        .iter()
        .filter_map(|action| match action {
            UiAction::Command(command) => Some(command.clone()),
            _ => None,
        })
        .collect()
}

/// A chrome with the address line focused (as the core does for `location.focus`) and `text`
/// typed.
fn editing(text: &str) -> Chrome {
    let mut chrome = chrome(800, 600, three_tabs());
    chrome.window_focus(FocusEvent::Activated);
    chrome.set_focus(Focus::Chrome);
    assert!(chrome.state().focused);
    type_text(&mut chrome, text);
    chrome
}

/// The clipboard request among `actions`.
fn paste_request(actions: &[UiAction]) -> RequestId {
    let request = actions.iter().find_map(|action| match action {
        UiAction::Paste(request) => Some(*request),
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

fn address(chrome: &Chrome) -> &str {
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
        actions.contains(&UiAction::Copy("blåbär".into())),
        "{actions:?}"
    );
    // Ctrl+V asks for the clipboard; its text replaces the selection without line breaks.
    let request = paste_request(&chrome.key(&ctrl("v", 47)));
    chrome.paste(request, "lingon\nsylt\r\n");
    assert_eq!(address(&chrome), "https://exempel.se/lingonsylt");
    // Shift+Home selects to the start; Ctrl+X cuts.
    chrome.key(&named("Home", SHIFT));
    let actions = chrome.key(&ctrl("x", 45));
    assert!(actions.contains(&UiAction::Copy("https://exempel.se/lingonsylt".into())));
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
        actions.contains(&UiAction::Focus(Focus::Page)),
        "{actions:?}"
    );
    assert!(actions.contains(&UiAction::TextInput(None)), "{actions:?}");
    assert_eq!(address(&chrome), "https://sighurt.example/");
    assert!(!chrome.state().focused);

    // An empty line opens nothing.
    let mut chrome = editing("");
    chrome.key(&ctrl("a", 30));
    chrome.key(&named("Delete", Modifiers::default()));
    let actions = chrome.key(&named("Enter", Modifiers::default()));
    assert_eq!(commands(&actions), []);
    assert!(actions.contains(&UiAction::Focus(Focus::Page)));
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
    chrome.set_focus(Focus::Page);
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
    chrome.set_focus(Focus::Page);
    assert_eq!(address(&chrome), "sylt");
    snapshot.tabs[0].loading = false;
    chrome.set_snapshot(snapshot);
    assert_eq!(address(&chrome), "https://sighurt.example/");

    // So does a load that never started, at the next snapshot, even an unchanged one.
    let mut chrome = editing("sylt");
    chrome.key(&enter);
    chrome.set_focus(Focus::Page);
    assert_eq!(address(&chrome), "sylt");
    chrome.set_snapshot(chrome.snapshot().clone());
    assert_eq!(address(&chrome), "https://sighurt.example/");

    // Switching tabs drops it.
    let mut chrome = editing("sylt");
    chrome.key(&enter);
    chrome.set_focus(Focus::Page);
    let mut snapshot = chrome.snapshot().clone();
    snapshot.tabs[0].loading = true;
    chrome.set_snapshot(snapshot.clone());
    chrome.set_snapshot(switched(&snapshot, 1));
    assert_eq!(address(&chrome), "https://recept.example/sylt");

    // Typed into again, it is an edit: a commit meanwhile doesn't replace it, and the page
    // taking focus drops it.
    let mut chrome = editing("sylt");
    chrome.key(&enter);
    chrome.set_focus(Focus::Page);
    let mut snapshot = chrome.snapshot().clone();
    snapshot.tabs[0].loading = true;
    chrome.set_snapshot(snapshot.clone());
    chrome.set_focus(Focus::Chrome);
    assert_eq!(chrome.state().address.selected_text(), Some("sylt"));
    type_text(&mut chrome, "lingon");
    snapshot.tabs[0].url = "https://sighurt.example/sylt".into();
    snapshot.tabs[0].loading = false;
    chrome.set_snapshot(snapshot);
    assert_eq!(address(&chrome), "lingon");
    chrome.set_focus(Focus::Page);
    assert_eq!(address(&chrome), "https://sighurt.example/sylt");
}

/// The core decides where focus goes after a command or a click in the page, and the chrome
/// follows without reporting the move back.
#[test]
fn focus_follows_the_core() {
    let mut chrome = chrome(800, 600, three_tabs());
    chrome.window_focus(FocusEvent::Activated);
    let actions = chrome.set_focus(Focus::Chrome);
    assert!(chrome.state().focused);
    assert!(
        matches!(actions[..], [UiAction::TextInput(Some(_))]),
        "{actions:?}"
    );
    assert_eq!(
        chrome.state().address.selected_text(),
        Some("https://sighurt.example/")
    );
    // The page takes focus (a tab switch, back, a click in the page): what was typed is
    // dropped.
    type_text(&mut chrome, "halv");
    assert_eq!(chrome.set_focus(Focus::Page), [UiAction::TextInput(None)]);
    assert!(!chrome.state().focused);
    assert_eq!(address(&chrome), "https://sighurt.example/");
    chrome.take_damage();
    assert_eq!(chrome.set_focus(Focus::Page), []);
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
    chrome.set_focus(Focus::Chrome);
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
    assert!(actions.contains(&UiAction::Focus(Focus::Page)));
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
    chrome.set_focus(Focus::Chrome);
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
        actions.contains(&UiAction::Focus(Focus::Chrome)),
        "{actions:?}"
    );
    let caret = actions.iter().find_map(|action| match action {
        UiAction::TextInput(Some(state)) => Some(state.clone()),
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
    let actions = click(&mut chrome, center(layout.tabs[1].rect));
    assert_eq!(commands(&actions), [Command::TabSelect(n(2))]);
    let actions = click(&mut chrome, center(layout.tabs[2].close.unwrap()));
    assert_eq!(commands(&actions), [Command::TabClose(Some(n(3)))]);
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
    assert_eq!(commands(&actions), []);
    // A release without a press does nothing either.
    let actions = chrome.pointer(
        surface,
        &button(
            center(layout.tabs[1].rect),
            PointerButton::Left,
            ButtonState::Released,
        ),
    );
    assert_eq!(commands(&actions), []);
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
    // The mouse's back and forward buttons work anywhere on the chrome.
    let surface = layout.chrome;
    let at = center(layout.tab_strip);
    let actions = chrome.pointer(
        surface,
        &button(at, PointerButton::Forward, ButtonState::Pressed),
    );
    assert_eq!(commands(&actions), [Command::PageForward]);
    let actions = chrome.pointer(
        surface,
        &button(at, PointerButton::Back, ButtonState::Pressed),
    );
    assert_eq!(commands(&actions), []);
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
        actions.contains(&UiAction::Cursor(Cursor::Text)),
        "{actions:?}"
    );
    let damage = chrome.take_damage();
    assert_eq!(
        damage,
        Damage {
            chrome: true,
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
        actions.contains(&UiAction::Cursor(Cursor::Pointer)),
        "{actions:?}"
    );
    let actions = chrome.pointer(surface, &PointerEvent::Leave);
    assert_eq!(actions, [UiAction::Cursor(Cursor::Default)]);
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
            UiAction::TextInput(Some(state)) => Some(state.clone()),
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

/// The input method's caret rectangle is where the caret is drawn, at every scale.
#[test]
fn the_input_method_caret_is_where_the_caret_is_drawn() {
    for scale in SCALES {
        let mut chrome = editing("");
        chrome.key(&ctrl("a", 30));
        type_text(&mut chrome, "blåbär");
        // A trailing space keeps the caret clear of the last glyph.
        let state = chrome
            .key(&typed(" "))
            .into_iter()
            .find_map(|action| match action {
                UiAction::TextInput(Some(state)) => Some(state),
                _ => None,
            })
            .expect("typing moves the caret");
        let rect = chrome.layout().chrome;
        let image = paint(&mut chrome, rect, scale);
        let ratio = f64::from(image.width) / f64::from(rect.width);
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
            "at {scale}/120: caret reported at {reported} px, drawn from {left} px"
        );
        // It is as tall as reported.
        for y in top..bottom {
            assert_eq!(image.pixel(right, y), text, "at {scale}/120, row {y}");
        }
    }
}

/// Hit targets stay on the drawn controls at 100 %, 125 %, 150 % and 200 %: every pixel painted
/// in a control's own color hits that control, and every pixel that hits a control lies within
/// what is drawn for it. Distinct colors make each control's drawing identifiable.
#[test]
fn hit_targets_stay_on_drawn_controls_at_every_scale() {
    let colors = Colors {
        hover: Rgba::rgb(0xff0000),
        tab: Rgba::rgb(0x00ff00),
        surface: Rgba::rgb(0x0000ff),
        ..Colors::default()
    };
    for scale in SCALES {
        for (width, height) in [(800, 600), (1001, 777), (333, 250)] {
            let snapshot = three_tabs();
            let mut chrome = chrome_with(width, height, snapshot, colors);
            let layout = chrome.layout().clone();
            let what = format!("{width}×{height} at {scale}/120");
            // The hovered button, the active tab and the address field, each in its own color.
            let hovered = [Target::Forward, Target::ReloadOrStop];
            for target in hovered {
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
                let expected = [
                    (Target::Tab(chrome.snapshot().tabs[0].id), colors.tab),
                    (target, colors.hover),
                    (Target::Address, colors.surface),
                ];
                let image = paint(&mut chrome, layout.chrome, scale);
                let ratio = (
                    f64::from(image.width) / f64::from(layout.chrome.width),
                    f64::from(image.height) / f64::from(layout.chrome.height),
                );
                for (target, color) in expected {
                    check_target(&image, ratio, &layout, target, color, &what);
                }
            }
        }
    }
}

fn check_target(
    image: &Image,
    ratio: (f64, f64),
    layout: &sighurt_ui::Layout,
    target: Target,
    color: Rgba,
    what: &str,
) {
    let color = [color.r, color.g, color.b, 255];
    // A tab's close button sits on the tab, drawn over its background.
    let part_of = |hit: Target| match (hit, target) {
        (Target::CloseTab(close), Target::Tab(tab)) => close == tab,
        _ => hit == target,
    };
    let hits = |x: u32, y: u32| {
        let point = WindowPoint {
            x: (f64::from(x) + 0.5) / ratio.0,
            y: (f64::from(y) + 0.5) / ratio.1,
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
    chrome.set_focus(Focus::Chrome);
    let page = chrome.layout().page;
    let local = WindowPoint { x: 10.0, y: 10.0 };
    let actions = chrome.pointer(
        page,
        &button(local, PointerButton::Left, ButtonState::Pressed),
    );
    assert!(
        actions.contains(&UiAction::Focus(Focus::Page)),
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
    chrome.set_focus(Focus::Chrome);
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
    chrome.set_focus(Focus::Chrome);
    type_text(&mut chrome, "a");
    assert_eq!(address(&chrome), "a");
    let actions = chrome.key(&named("Enter", Modifiers::default()));
    assert_eq!(commands(&actions), [Command::PageOpen("a".into())]);
}
