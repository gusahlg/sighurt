//! What `vte`, `termwiz` and our parser do with the bytes a terminal sends.
//!
//! `crossterm` has no public byte parser. Its coverage is the `--worker crossterm`
//! child in `pty.rs`, which is the only way to make it read a pty it does not own.

use termwiz::input::{InputEvent, InputParser, KeyCode, KeyEvent, Modifiers, MouseButtons};
use tty_io::{Input, Key, KeyCode as Evdev, MouseButton, Parser, Reply, Wheel};

fn vte_events(bytes: &[u8]) -> Vec<VteEvent> {
    let mut performer = VtePerformer::default();
    vte::Parser::new().advance(&mut performer, bytes);
    performer.events
}

#[derive(Default)]
struct VtePerformer {
    events: Vec<VteEvent>,
}

#[derive(Debug, PartialEq, Eq)]
enum VteEvent {
    Print(char),
    Execute(u8),
    Osc(Vec<Vec<u8>>, bool),
    Csi(Vec<Vec<u16>>, Vec<u8>, char),
    Hook(Vec<Vec<u16>>, Vec<u8>, char),
    Put(u8),
    Unhook,
    Esc(Vec<u8>, u8),
}

impl vte::Perform for VtePerformer {
    fn print(&mut self, c: char) {
        self.events.push(VteEvent::Print(c));
    }

    fn execute(&mut self, byte: u8) {
        self.events.push(VteEvent::Execute(byte));
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        let params = params.iter().map(|param| param.to_vec()).collect();
        self.events.push(VteEvent::Osc(params, bell_terminated));
    }

    fn csi_dispatch(&mut self, params: &vte::Params, intermediates: &[u8], _ignore: bool, c: char) {
        let params = params.iter().map(|sub| sub.to_vec()).collect();
        self.events
            .push(VteEvent::Csi(params, intermediates.to_vec(), c));
    }

    fn hook(&mut self, params: &vte::Params, intermediates: &[u8], _ignore: bool, c: char) {
        let params = params.iter().map(|sub| sub.to_vec()).collect();
        self.events
            .push(VteEvent::Hook(params, intermediates.to_vec(), c));
    }

    fn put(&mut self, byte: u8) {
        self.events.push(VteEvent::Put(byte));
    }

    fn unhook(&mut self) {
        self.events.push(VteEvent::Unhook);
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        self.events
            .push(VteEvent::Esc(intermediates.to_vec(), byte));
    }
}

#[test]
fn vte_discards_apc_and_saturates_params_at_u16() {
    let apc = vte_events(b"\x1b_Gi=1;OK\x1b\\");
    let printed: String = apc
        .iter()
        .filter_map(|event| match event {
            VteEvent::Print(ch) => Some(*ch),
            _ => None,
        })
        .collect();
    let put: Vec<u8> = apc
        .iter()
        .filter_map(|event| match event {
            VteEvent::Put(byte) => Some(*byte),
            _ => None,
        })
        .collect();
    assert!(
        !printed.contains("OK") && !printed.contains('G') && put.is_empty(),
        "APC payload was delivered: {apc:?}"
    );
    assert!(
        apc.iter()
            .all(|event| !matches!(event, VteEvent::Osc(_, _))),
        "{apc:?}"
    );

    match vte_events(b"\x1b[128512u").as_slice() {
        [VteEvent::Csi(params, _, 'u')] => {
            assert_eq!(
                params.first().map(Vec::as_slice),
                Some([u16::MAX].as_slice())
            );
        }
        other => panic!("expected one saturated CSI, got {other:?}"),
    }
}

#[test]
fn vte_delivers_osc_dcs_and_da1_as_raw_sequences() {
    match vte_events(b"\x1b]10;rgb:1111/2222/3333\x07").as_slice() {
        [VteEvent::Osc(params, true)] => {
            assert_eq!(params[0], b"10");
            assert_eq!(params[1], b"rgb:1111/2222/3333");
        }
        other => panic!("{other:?}"),
    }

    let dcs = vte_events(b"\x1bP1+r524742=726762\x1b\\");
    assert!(
        dcs.iter()
            .any(|event| matches!(event, VteEvent::Hook(_, inter, 'r') if inter.contains(&b'+'))),
        "{dcs:?}"
    );
    let payload: Vec<u8> = dcs
        .iter()
        .filter_map(|event| match event {
            VteEvent::Put(byte) => Some(*byte),
            _ => None,
        })
        .collect();
    assert_eq!(payload, b"524742=726762");
    assert!(dcs.iter().any(|event| matches!(event, VteEvent::Unhook)));

    match vte_events(b"\x1b[?62;4c").as_slice() {
        [VteEvent::Csi(params, intermediates, 'c')] => {
            assert!(intermediates.contains(&b'?'));
            assert!(params.iter().any(|param| param.first() == Some(&4)));
        }
        other => panic!("{other:?}"),
    }

    match vte_events(b"\x1b[6;20;10t").as_slice() {
        [VteEvent::Csi(params, _, 't')] => {
            let flat: Vec<u16> = params
                .iter()
                .filter_map(|param| param.first().copied())
                .collect();
            assert_eq!(flat, vec![6, 20, 10]);
        }
        other => panic!("{other:?}"),
    }
    match vte_events(b"\x1b[4;1080;1920t").as_slice() {
        [VteEvent::Csi(params, _, 't')] => {
            let flat: Vec<u16> = params
                .iter()
                .filter_map(|param| param.first().copied())
                .collect();
            assert_eq!(flat, vec![4, 1080, 1920]);
        }
        other => panic!("{other:?}"),
    }
    // ST-terminated OSC arrives, and the `\` of ST is also reported as an
    // escape. A caller that turns escapes into keys can see a spurious key.
    let osc_st = vte_events(b"\x1b]11;rgb:aaaa/bbbb/cccc\x1b\\");
    assert!(
        osc_st.iter().any(|event| matches!(
            event,
            VteEvent::Osc(params, false)
                if params.first().map(Vec::as_slice) == Some(&b"11"[..])
                    && params.get(1).map(Vec::as_slice) == Some(&b"rgb:aaaa/bbbb/cccc"[..])
        )),
        "{osc_st:?}"
    );
    assert!(
        osc_st
            .iter()
            .any(|event| matches!(event, VteEvent::Esc(_, b'\\'))),
        "ST backslash was consumed: {osc_st:?}"
    );
}

#[test]
fn termwiz_legacy_bytes_are_keys_and_not_replies() {
    for (label, bytes) in [
        ("arrow", &b"\x1b[A"[..]),
        ("f5", &b"\x1b[15~"[..]),
        ("ss3-f1", &b"\x1bOP"[..]),
        ("alt-a", &b"\x1ba"[..]),
        ("ctrl-a", &b"\x01"[..]),
        ("tab", &b"\t"[..]),
        ("enter", &b"\r"[..]),
        ("escape", &b"\x1b"[..]),
    ] {
        let events = termwiz(bytes);
        assert!(
            !events.is_empty()
                && events
                    .iter()
                    .all(|event| matches!(event, InputEvent::Key(_))),
            "{label} was not a key: {events:?}"
        );
    }
}

fn termwiz(bytes: &[u8]) -> Vec<InputEvent> {
    InputParser::new().parse_as_vec(bytes, false)
}

fn termwiz_keys_only(label: &str, bytes: &[u8]) -> Vec<InputEvent> {
    let events = termwiz(bytes);
    assert!(
        events
            .iter()
            .all(|event| matches!(event, InputEvent::Key(_))),
        "{label} was not left unstructured: {events:?}"
    );
    events
}

#[test]
fn termwiz_input_parser_handles_plain_kitty_mouse_and_paste() {
    assert_eq!(
        termwiz(b"\x1b[97u"),
        vec![InputEvent::Key(KeyEvent {
            key: KeyCode::Char('a'),
            modifiers: Modifiers::NONE,
        })]
    );
    assert_eq!(
        termwiz(b"\x1b[A"),
        vec![InputEvent::Key(KeyEvent {
            key: KeyCode::UpArrow,
            modifiers: Modifiers::NONE,
        })]
    );
    match termwiz(b"\x1b[<0;3;4M").as_slice() {
        [InputEvent::Mouse(mouse)] => {
            assert_eq!((mouse.x, mouse.y), (3, 4));
            assert!(mouse.mouse_buttons.contains(MouseButtons::LEFT));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        termwiz(b"\x1b[200~hello\x1b[201~"),
        vec![InputEvent::Paste("hello".to_string())]
    );
}

#[test]
fn termwiz_input_parser_drops_kitty_extensions_and_query_replies() {
    let shifted = termwiz(b"\x1b[97:65:97;2:1;65u");
    assert_ne!(
        shifted,
        vec![InputEvent::Key(KeyEvent {
            key: KeyCode::Char('A'),
            modifiers: Modifiers::NONE,
        })],
        "colon alternates and associated text were treated as one key: {shifted:?}"
    );
    let emoji = termwiz(b"\x1b[128512u");
    assert!(
        !matches!(
            emoji.as_slice(),
            [InputEvent::Key(KeyEvent {
                key: KeyCode::Char('😀'),
                ..
            })]
        ),
        "non-ASCII CSI-u was a single key: {emoji:?}"
    );
    // No event-type field. The `:3` release is not a key-up; ESC `[` is Alt+[
    // and the rest is typed as characters.
    assert_eq!(
        termwiz(b"\x1b[97;1:3u"),
        vec![
            InputEvent::Key(KeyEvent {
                key: KeyCode::Char('['),
                modifiers: Modifiers::ALT,
            }),
            InputEvent::Key(KeyEvent {
                key: KeyCode::Char('9'),
                modifiers: Modifiers::NONE,
            }),
            InputEvent::Key(KeyEvent {
                key: KeyCode::Char('7'),
                modifiers: Modifiers::NONE,
            }),
            InputEvent::Key(KeyEvent {
                key: KeyCode::Char(';'),
                modifiers: Modifiers::NONE,
            }),
            InputEvent::Key(KeyEvent {
                key: KeyCode::Char('1'),
                modifiers: Modifiers::NONE,
            }),
            InputEvent::Key(KeyEvent {
                key: KeyCode::Char(':'),
                modifiers: Modifiers::NONE,
            }),
            InputEvent::Key(KeyEvent {
                key: KeyCode::Char('3'),
                modifiers: Modifiers::NONE,
            }),
            InputEvent::Key(KeyEvent {
                key: KeyCode::Char('u'),
                modifiers: Modifiers::NONE,
            }),
        ]
    );

    termwiz_keys_only("DA1", b"\x1b[?62;4c");
    termwiz_keys_only("kitty keyboard query", b"\x1b[?31u");
    termwiz_keys_only("kitty graphics", b"\x1b_Gi=1;OK\x1b\\");
    termwiz_keys_only("XTGETTCAP", b"\x1bP1+r524742=726762\x1b\\");
    termwiz_keys_only("OSC 10", b"\x1b]10;rgb:1111/2222/3333\x07");
    termwiz_keys_only("OSC 11", b"\x1b]11;rgb:aaaa/bbbb/cccc\x1b\\");
    termwiz_keys_only("cell size", b"\x1b[6;20;10t");
    termwiz_keys_only("text area", b"\x1b[4;1080;1920t");
}

fn one_reply(bytes: &[u8]) -> Reply {
    match Parser::new().feed(bytes).as_slice() {
        [Input::Reply(reply)] => reply.clone(),
        other => panic!("expected one reply, got {other:?}"),
    }
}

#[test]
fn our_parser_structures_kitty_and_the_replies_the_others_lose() {
    let keys = Parser::new().feed(b"\x1b[97:65:97;2:1;65u");
    match keys.as_slice() {
        [Input::Key(key)] => {
            assert_eq!(key.code, Evdev(30));
            assert_eq!(key.key, Key::Character("A".into()));
            assert_eq!(key.text.as_deref(), Some("A"));
            assert!(key.modifiers.shift && key.consumed.shift);
        }
        other => panic!("{other:?}"),
    }
    match Parser::new().feed(b"\x1b[128512u").as_slice() {
        [Input::Key(key)] => {
            assert_eq!(key.key, Key::Character("😀".into()));
            assert_eq!(key.code, Evdev(0));
        }
        other => panic!("{other:?}"),
    }
    match Parser::new().feed(b"\x1b[97;1:3u").as_slice() {
        [Input::Key(key)] => {
            assert_eq!(key.state, tty_io::ButtonState::Released);
            assert!(key.text.is_none());
        }
        other => panic!("{other:?}"),
    }
    match one_reply(b"\x1b[?62;4c") {
        Reply::Da1 {
            sixel: true,
            params,
        } => assert!(params.contains(&4)),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        one_reply(b"\x1b[?31u"),
        Reply::KittyKeyboard { flags: 31 }
    ));
    assert!(matches!(
        one_reply(b"\x1b_Gi=1;OK\x1b\\"),
        Reply::KittyGraphics { ok: true, .. }
    ));
    match one_reply(b"\x1bP1+r524742=726762\x1b\\") {
        Reply::XtGetTcap {
            found: true,
            entries,
        } => {
            assert_eq!(entries, vec![("RGB".into(), Some("rgb".into()))]);
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        one_reply(b"\x1b]10;rgb:1111/2222/3333\x07"),
        Reply::OscColor {
            index: 10,
            bell: true,
            ..
        }
    ));
    assert!(matches!(
        one_reply(b"\x1b[6;20;10t"),
        Reply::CellPixels {
            height: 20,
            width: 10
        }
    ));
    match Parser::new().feed(b"\x1b[<65;1;1M").as_slice() {
        [Input::Mouse(mouse)] => {
            assert_eq!(mouse.button, MouseButton::None);
            assert_eq!(mouse.wheel, Some(Wheel::Down));
        }
        other => panic!("{other:?}"),
    }
}
