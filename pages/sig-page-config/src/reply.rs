//! The runtime's answers to the page's requests, read from their text. The format is
//! `sighurt-runtime`'s `page_config`, which documents it; the two change together.
//!
//! Must not own: what the page does with an answer (the page).

/// A problem or notice, with the line it is on (1-based; 0 when it isn't known).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub line: usize,
    pub text: String,
}

/// The answer to a read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Read {
    /// The file and its text. `read_only`: saves will be refused.
    File {
        path: String,
        target: String,
        text: String,
        read_only: bool,
    },
    /// There is no file: the text to start from.
    New {
        path: String,
        target: String,
        text: String,
    },
    Refused(String),
}

impl Read {
    /// What a save over this read sends as `replaces`: the file it read, and what it held or
    /// that there was none.
    pub fn replaces(target: &str, contents: Option<&str>) -> String {
        match contents {
            Some(text) => format!("file {target}\n{text}"),
            None => format!("missing {target}\n"),
        }
    }
}

/// The answer to a check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    /// Valid, with what the user should know anyway.
    Valid(Vec<Problem>),
    Invalid(Vec<Problem>),
}

/// The answer to a write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Write {
    /// Written and reloaded: what applies only when `sig` starts again, and what else the
    /// reload said.
    Saved {
        restart: Vec<String>,
        notices: Vec<String>,
    },
    /// The file changed since the page read it.
    Stale(String),
    Invalid(Vec<Problem>),
    Refused(String),
}

fn head(answer: &str) -> (&str, &str) {
    answer.split_once('\n').unwrap_or((answer, ""))
}

pub fn read(answer: &str) -> Read {
    let (first, rest) = head(answer);
    let (state, path) = first.split_once(' ').unwrap_or((first, ""));
    if state == "refused" {
        return Read::Refused(rest.to_owned());
    }
    let (second, text) = head(rest);
    let Some(target) = second.strip_prefix("target ") else {
        return Read::Refused(format!(
            "sig answered what this page doesn't read: {second:?}"
        ));
    };
    let (path, target, text) = (path.to_owned(), target.to_owned(), text.to_owned());
    match state {
        "file" => Read::File {
            path,
            target,
            text,
            read_only: false,
        },
        "read-only" => Read::File {
            path,
            target,
            text,
            read_only: true,
        },
        "new" => Read::New { path, target, text },
        _ => Read::Refused(format!(
            "sig answered what this page doesn't read: {first:?}"
        )),
    }
}

fn problems(body: &str, kind: &str) -> Vec<Problem> {
    body.lines()
        .filter_map(|line| line.strip_prefix(kind)?.strip_prefix(' '))
        .map(|rest| {
            let (n, text) = rest.split_once(' ').unwrap_or((rest, ""));
            Problem {
                line: n.parse().unwrap_or(0),
                text: text.to_owned(),
            }
        })
        .collect()
}

pub fn check(answer: &str) -> Check {
    let (first, body) = head(answer);
    match first {
        "valid" => Check::Valid(problems(body, "notice")),
        _ => Check::Invalid(problems(body, "problem")),
    }
}

pub fn write(answer: &str) -> Write {
    let (first, body) = head(answer);
    let lines = |kind: &str| -> Vec<String> {
        body.lines()
            .filter_map(|line| Some(line.strip_prefix(kind)?.strip_prefix(' ')?.to_owned()))
            .collect()
    };
    match first {
        "saved" => Write::Saved {
            restart: lines("restart"),
            notices: lines("notice"),
        },
        "stale" => Write::Stale(body.to_owned()),
        "invalid" => Write::Invalid(problems(body, "problem")),
        "refused" => Write::Refused(body.to_owned()),
        _ => Write::Refused(format!(
            "sig answered what this page doesn't read: {first:?}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_as_the_runtime_writes_them() {
        assert_eq!(
            read("file /h/.config/sighurt/config.toml\ntarget /d/sig.toml\nversion = 1\n"),
            Read::File {
                path: "/h/.config/sighurt/config.toml".into(),
                target: "/d/sig.toml".into(),
                text: "version = 1\n".into(),
                read_only: false
            }
        );
        assert!(matches!(
            read("read-only /p\ntarget /p\nx"),
            Read::File {
                read_only: true,
                ..
            }
        ));
        assert_eq!(
            read("new /a b\ntarget /a b\n# x\nversion = 1\n"),
            Read::New {
                path: "/a b".into(),
                target: "/a b".into(),
                text: "# x\nversion = 1\n".into()
            }
        );
        assert_eq!(read("refused\nno"), Read::Refused("no".into()));
        assert!(matches!(read("what"), Read::Refused(_)));
        assert!(
            matches!(read("file /p\nversion = 1\n"), Read::Refused(_)),
            "no target"
        );
        assert_eq!(Read::replaces("/t", Some("")), "file /t\n");
        assert_eq!(Read::replaces("/t", None), "missing /t\n");
        assert_eq!(check("valid\n"), Check::Valid(vec![]));
        assert_eq!(
            check("invalid\nproblem 4 [ui] renderer: no\nproblem 0 the file needs it\n"),
            Check::Invalid(vec![
                Problem {
                    line: 4,
                    text: "[ui] renderer: no".into()
                },
                Problem {
                    line: 0,
                    text: "the file needs it".into()
                },
            ])
        );
        assert_eq!(
            check("valid\nnotice 3 [chrome.x] is for\n"),
            Check::Valid(vec![Problem {
                line: 3,
                text: "[chrome.x] is for".into()
            }])
        );
        assert_eq!(
            write("saved\nrestart [ui] renderer applies when sig starts again\nnotice n\n"),
            Write::Saved {
                restart: vec!["[ui] renderer applies when sig starts again".into()],
                notices: vec!["n".into()]
            }
        );
        assert_eq!(write("stale\nchanged"), Write::Stale("changed".into()));
        assert_eq!(
            write("invalid\nproblem 2 zoom: x\n"),
            Write::Invalid(vec![Problem {
                line: 2,
                text: "zoom: x".into()
            }])
        );
        assert_eq!(
            write("refused\nread-only"),
            Write::Refused("read-only".into())
        );
    }
}
