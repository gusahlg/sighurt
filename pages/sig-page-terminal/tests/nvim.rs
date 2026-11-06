//! nvim's output, as captured from nvim 0.11 in a pty of 80 by 24 with `TERM=xterm-256color`:
//! started with `nvim -u NONE -i NONE --clean f.txt` on a file of two lines, the queries the
//! page answers answered as it answers them, then `O`, "hello from sig" and Escape typed. The
//! emulator must show what nvim drew.

use sig_page_terminal::term::Term;

#[test]
fn nvims_screen_is_what_it_drew() {
    let mut term = Term::new(24, 80);
    let replies = term.feed(include_bytes!("nvim-startup.bin"));
    // nvim waits for DA1 (and asks for the device status), twice in this capture: both
    // answered, and nothing else.
    let replies = String::from_utf8(replies).unwrap();
    assert!(replies.contains("\x1b[?62;22c") && replies.contains("\x1b[0n"));
    assert_eq!(
        replies.replace("\x1b[?62;22c", "").replace("\x1b[0n", ""),
        ""
    );
    let screen = term.screen();
    assert!(screen.alternate_screen());
    let contents = screen.contents();
    let lines: Vec<_> = contents.lines().collect();
    // The last row, nvim's command line, is empty.
    assert_eq!(lines.len(), 23, "{contents}");
    assert_eq!(lines[0], "hello from sig");
    assert_eq!(lines[1], "a".repeat(40));
    assert_eq!(lines[2], "=".repeat(40));
    assert!(lines[3..22].iter().all(|line| *line == "~"), "{contents}");
    assert!(lines[22].starts_with("f.txt [+]") && lines[22].ends_with("1,14           All"));
    // After Escape the cursor is on the last character typed.
    assert_eq!(screen.cursor_position(), (0, 13));
}
