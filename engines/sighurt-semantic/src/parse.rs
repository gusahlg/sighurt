//! JSON from the extraction script, checked against the semantic bounds.
//!
//! Owns: turning that JSON into a [`SemanticSnapshot`] and refusing a value that is too big,
//! the wrong shape, or fails [`SemanticSnapshot::check`]. Must not own: producing the JSON, or
//! trusting a document generation carried in it. The generation on the snapshot is the one the
//! caller passes.
//!
//! Parsing is a single forward scan. It stops at [`MAX_NODES`], [`MAX_STRING_BYTES`],
//! [`MAX_TOTAL_BYTES`] and a nesting depth without building a `serde_json::Value` tree. Input
//! longer than [`max_input_bytes`] is refused before the scan. [`MAX_TOTAL_BYTES`] is the text
//! budget, which is smaller than the JSON that carries it, so the raw-byte ceiling stays
//! [`max_input_bytes`].

use std::fmt;

use sighurt_api::ids::DocumentGeneration;
use sighurt_api::semantic::{
    Landmark, NodeId, NodeKind, SemanticNode, SemanticSnapshot, SnapshotError, MAX_NODES,
    MAX_OPTIONS, MAX_STRING_BYTES, MAX_TOTAL_BYTES,
};

/// Bytes of JSON structure allowed per node, on top of the doubled text budget.
///
/// [`max_input_bytes`] is [`MAX_TOTAL_BYTES`] twice (JSON escaping can double a string) plus
/// this much for every node the snapshot is allowed to hold. A longer input is refused before
/// it is parsed.
pub const NODE_JSON_OVERHEAD: usize = 512;

/// Nesting of `{` and `[` the scan will enter. A snapshot's own shape is a handful of levels;
/// anything deeper is refused without walking the rest.
const MAX_JSON_DEPTH: usize = 64;

/// The most bytes of JSON [`parse`] will read.
pub fn max_input_bytes() -> usize {
    MAX_TOTAL_BYTES
        .saturating_mul(2)
        .saturating_add(MAX_NODES.saturating_mul(NODE_JSON_OVERHEAD))
}

/// Why [`parse`] refused a string.
#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    /// The JSON text is longer than [`max_input_bytes`]. It was not parsed.
    TooLarge {
        /// The text's length.
        len: usize,
        /// The limit it crossed.
        limit: usize,
    },
    /// The text is not JSON.
    Syntax(String),
    /// A field is present but is not the type a snapshot uses.
    Type {
        /// Where in the JSON, such as `nodes[0].text`.
        path: String,
        /// What was required there.
        expected: &'static str,
    },
    /// [`SemanticSnapshot::check`] refused it, or it named more than [`MAX_NODES`] nodes.
    ///
    /// No snapshot is returned: a failure is the whole value.
    Snapshot(SnapshotError),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { len, limit } => {
                write!(
                    f,
                    "semantic JSON is {len} bytes, over the {limit} byte limit"
                )
            }
            Self::Syntax(message) => write!(f, "semantic JSON is malformed: {message}"),
            Self::Type { path, expected } => {
                write!(f, "semantic JSON field {path} should be {expected}")
            }
            Self::Snapshot(error) => write!(f, "semantic snapshot is malformed: {error}"),
        }
    }
}

impl std::error::Error for ParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Snapshot(error) => Some(error),
            _ => None,
        }
    }
}

fn type_err(path: impl Into<String>, expected: &'static str) -> ParseError {
    ParseError::Type {
        path: path.into(),
        expected,
    }
}

fn syntax(message: impl Into<String>) -> ParseError {
    ParseError::Syntax(message.into())
}

struct Cur<'a> {
    src: &'a str,
    i: usize,
    depth: usize,
    /// Bytes of snapshot text stored so far (`text`, `href`, `value`, `label`, options).
    total: usize,
}

impl<'a> Cur<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            i: 0,
            depth: 0,
            total: 0,
        }
    }

    fn rest(&self) -> &str {
        // A mid-character index is a bug in the scanner. Refuse it instead of panicking.
        self.src.get(self.i..).unwrap_or("")
    }

    fn peek(&self) -> Option<u8> {
        self.src.as_bytes().get(self.i).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.i += 1;
        Some(byte)
    }

    fn skip_ws(&mut self) {
        while let Some(b @ (b' ' | b'\t' | b'\n' | b'\r')) = self.peek() {
            self.i += 1;
            let _ = b;
        }
    }

    fn enter(&mut self) -> Result<(), ParseError> {
        if self.depth >= MAX_JSON_DEPTH {
            return Err(syntax("semantic JSON nests too deeply"));
        }
        self.depth += 1;
        Ok(())
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    fn eat(&mut self, lit: &str) -> Result<(), ParseError> {
        if let Some(rest) = self.rest().strip_prefix(lit) {
            let boundary = rest
                .as_bytes()
                .first()
                .is_none_or(|c| !c.is_ascii_alphanumeric() && *c != b'_');
            if boundary {
                self.i += lit.len();
                return Ok(());
            }
        }
        Err(syntax(format!("expected {lit}")))
    }

    /// A JSON string. `count` adds its decoded bytes to the text budget.
    ///
    /// Stops once the decoded length passes [`MAX_STRING_BYTES`], or the text budget when
    /// `count` is set, without keeping the tail.
    fn string(&mut self, count: bool) -> Result<String, ParseError> {
        if self.bump() != Some(b'"') {
            return Err(syntax("expected a string"));
        }
        let mut out = String::new();
        loop {
            let Some(byte) = self.peek() else {
                return Err(syntax("unterminated string"));
            };
            if byte == b'"' {
                self.i += 1;
                break;
            }
            let ch = if byte == b'\\' {
                self.escape()?
            } else if byte < 0x20 {
                return Err(syntax("unescaped control character in a string"));
            } else {
                self.raw_char()?
            };
            let next = out.len().saturating_add(ch.len_utf8());
            if next > MAX_STRING_BYTES {
                return Err(ParseError::Snapshot(SnapshotError::TooMuchText));
            }
            if count && self.total.saturating_add(next) > MAX_TOTAL_BYTES {
                return Err(ParseError::Snapshot(SnapshotError::TooMuchText));
            }
            out.push(ch);
        }
        if count {
            self.total = self.total.saturating_add(out.len());
        }
        Ok(out)
    }

    fn raw_char(&mut self) -> Result<char, ParseError> {
        let rest = self.rest();
        let ch = rest
            .chars()
            .next()
            .ok_or_else(|| syntax("truncated string"))?;
        self.i += ch.len_utf8();
        Ok(ch)
    }

    fn escape(&mut self) -> Result<char, ParseError> {
        self.i += 1; // '\\'
        let Some(kind) = self.bump() else {
            return Err(syntax("truncated escape"));
        };
        Ok(match kind {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{0008}',
            b'f' => '\u{000c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => self.unicode_escape()?,
            _ => return Err(syntax("bad escape")),
        })
    }

    fn unicode_escape(&mut self) -> Result<char, ParseError> {
        let unit = self.hex4()?;
        if (0xD800..=0xDBFF).contains(&unit) {
            if self.rest().starts_with("\\u") {
                self.i += 2;
                let low = self.hex4()?;
                if (0xDC00..=0xDFFF).contains(&low) {
                    let cp =
                        0x1_0000 + (((u32::from(unit) - 0xD800) << 10) | (u32::from(low) - 0xDC00));
                    return char::from_u32(cp).ok_or_else(|| syntax("bad unicode escape"));
                }
            }
            return Err(syntax("lone surrogate"));
        }
        if (0xDC00..=0xDFFF).contains(&unit) {
            return Err(syntax("lone surrogate"));
        }
        char::from_u32(u32::from(unit)).ok_or_else(|| syntax("bad unicode escape"))
    }

    /// Exactly four ASCII hex bytes. A multibyte character is rejected before any `str` slice:
    /// `self.i + 4` can land inside `é` while still being inside the byte length.
    fn hex4(&mut self) -> Result<u16, ParseError> {
        let bytes = self.src.as_bytes();
        if self.i.saturating_add(4) > bytes.len() {
            return Err(syntax("short unicode escape"));
        }
        let mut value = 0u16;
        for offset in 0..4 {
            let digit = match bytes[self.i + offset] {
                digit @ b'0'..=b'9' => digit - b'0',
                digit @ b'a'..=b'f' => digit - b'a' + 10,
                digit @ b'A'..=b'F' => digit - b'A' + 10,
                _ => return Err(syntax("bad unicode escape")),
            };
            value = (value << 4) | u16::from(digit);
        }
        self.i += 4;
        Ok(value)
    }

    fn skip_string(&mut self) -> Result<(), ParseError> {
        // Enforce the string cap even for fields the snapshot does not keep.
        let _ = self.string(false)?;
        Ok(())
    }

    fn scan_number(&mut self) -> Result<&'a str, ParseError> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        match self.peek() {
            Some(b'0') => {
                self.i += 1;
                if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    return Err(syntax("leading zero"));
                }
            }
            Some(b'1'..=b'9') => {
                while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.i += 1;
                }
            }
            _ => return Err(syntax("expected a number")),
        }
        if self.peek() == Some(b'.') {
            self.i += 1;
            if !self.peek().is_some_and(|c| c.is_ascii_digit()) {
                return Err(syntax("expected a digit after the decimal point"));
            }
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.i += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if !self.peek().is_some_and(|c| c.is_ascii_digit()) {
                return Err(syntax("expected an exponent"));
            }
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.i += 1;
            }
        }
        if self.i - start > 64 {
            return Err(syntax("number is too long"));
        }
        self.src
            .get(start..self.i)
            .ok_or_else(|| syntax("bad number"))
    }

    fn number(&mut self) -> Result<serde_json::Number, ParseError> {
        let token = self.scan_number()?;
        serde_json::from_str(token).map_err(|_| syntax("bad number"))
    }

    fn u64_at(&mut self, path: &str) -> Result<u64, ParseError> {
        self.skip_ws();
        let number = self.number().map_err(|_| type_err(path, "an integer"))?;
        number.as_u64().ok_or_else(|| type_err(path, "an integer"))
    }

    fn skip_value(&mut self) -> Result<(), ParseError> {
        self.enter()?;
        let result = self.skip_value_in();
        self.leave();
        result
    }

    fn skip_value_in(&mut self) -> Result<(), ParseError> {
        self.skip_ws();
        match self.peek() {
            Some(b'"') => self.skip_string(),
            Some(b'{') => self.skip_object(),
            Some(b'[') => self.skip_array(),
            Some(b't') => self.eat("true"),
            Some(b'f') => self.eat("false"),
            Some(b'n') => self.eat("null"),
            Some(b'-' | b'0'..=b'9') => {
                let _ = self.scan_number()?;
                Ok(())
            }
            _ => Err(syntax("expected a value")),
        }
    }

    fn skip_object(&mut self) -> Result<(), ParseError> {
        self.bump(); // '{'
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(());
        }
        loop {
            self.skip_ws();
            self.skip_string()?;
            self.skip_ws();
            if self.bump() != Some(b':') {
                return Err(syntax("expected ':'"));
            }
            self.skip_value()?;
            self.skip_ws();
            match self.bump() {
                Some(b',') => continue,
                Some(b'}') => return Ok(()),
                _ => return Err(syntax("expected ',' or '}'")),
            }
        }
    }

    fn skip_array(&mut self) -> Result<(), ParseError> {
        self.bump(); // '['
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(());
        }
        loop {
            self.skip_value()?;
            self.skip_ws();
            match self.bump() {
                Some(b',') => continue,
                Some(b']') => return Ok(()),
                _ => return Err(syntax("expected ',' or ']'")),
            }
        }
    }
}

/// Parses the extraction script's JSON into a snapshot of `document`.
///
/// The input is refused whole when it is longer than [`max_input_bytes`]. The scan then
/// builds nodes directly and stops when [`MAX_NODES`], [`MAX_STRING_BYTES`],
/// [`MAX_TOTAL_BYTES`] or the JSON nesting depth is crossed, instead of materializing a
/// `serde_json::Value` first. A kind this crate does not know becomes [`NodeKind::Container`].
/// A heading level outside 1..=6, or a landmark name this crate does not know, does too.
/// `document` in the JSON is ignored: the snapshot is stamped with `document`.
/// [`SemanticSnapshot::check`] then runs, and its error is returned instead of a snapshot.
/// The truncation flag is the one in the JSON (missing means false).
pub fn parse(json: &str, document: DocumentGeneration) -> Result<SemanticSnapshot, ParseError> {
    let limit = max_input_bytes();
    if json.len() > limit {
        return Err(ParseError::TooLarge {
            len: json.len(),
            limit,
        });
    }
    let mut cur = Cur::new(json);
    if cur.rest().starts_with('\u{feff}') {
        cur.i += '\u{feff}'.len_utf8();
    }
    cur.skip_ws();
    if cur.peek() != Some(b'{') {
        // A finished non-object is the wrong type. Anything that is not finished JSON is
        // syntax, including trailing junk (`true,`).
        cur.skip_value()?;
        cur.skip_ws();
        if cur.i != cur.src.len() {
            return Err(syntax("trailing data after the snapshot"));
        }
        return Err(type_err("snapshot", "an object"));
    }
    let snapshot = read_snapshot(&mut cur, document)?;
    cur.skip_ws();
    if cur.i != cur.src.len() {
        return Err(syntax("trailing data after the snapshot"));
    }
    snapshot.check().map_err(ParseError::Snapshot)?;
    Ok(snapshot)
}

fn read_snapshot(
    cur: &mut Cur<'_>,
    document: DocumentGeneration,
) -> Result<SemanticSnapshot, ParseError> {
    cur.skip_ws();
    if cur.bump() != Some(b'{') {
        return Err(type_err("snapshot", "an object"));
    }
    cur.enter()?;
    let mut truncated = false;
    let mut root = None;
    let mut focus = None;
    let mut nodes = None;
    cur.skip_ws();
    if cur.peek() != Some(b'}') {
        loop {
            let key = cur.string(false)?;
            cur.skip_ws();
            if cur.bump() != Some(b':') {
                return Err(syntax("expected ':'"));
            }
            cur.skip_ws();
            match key.as_str() {
                "truncated" => truncated = expect_bool(cur, "truncated")?,
                "root" => root = Some(cur.u64_at("root")?),
                "focus" => focus = optional_id(cur, "focus")?,
                "nodes" => nodes = Some(read_nodes(cur)?),
                "document" => cur.skip_value()?,
                _ => cur.skip_value()?,
            }
            cur.skip_ws();
            match cur.bump() {
                Some(b'}') => break,
                Some(b',') => {
                    cur.skip_ws();
                    if cur.peek() == Some(b'}') {
                        return Err(syntax("trailing comma"));
                    }
                }
                _ => return Err(syntax("expected ',' or '}'")),
            }
        }
    } else {
        cur.i += 1;
    }
    cur.leave();
    let root = root.ok_or_else(|| type_err("root", "an integer"))?;
    let nodes = nodes.ok_or_else(|| type_err("nodes", "an array"))?;
    Ok(SemanticSnapshot {
        document,
        root: NodeId(root),
        nodes,
        focus,
        truncated,
    })
}

fn expect_bool(cur: &mut Cur<'_>, path: &str) -> Result<bool, ParseError> {
    cur.skip_ws();
    if cur.rest().starts_with("true") {
        cur.eat("true")?;
        Ok(true)
    } else if cur.rest().starts_with("false") {
        cur.eat("false")?;
        Ok(false)
    } else {
        Err(type_err(path, "a boolean"))
    }
}

fn optional_id(cur: &mut Cur<'_>, path: &str) -> Result<Option<NodeId>, ParseError> {
    cur.skip_ws();
    if cur.rest().starts_with("null") {
        cur.eat("null")?;
        Ok(None)
    } else {
        Ok(Some(NodeId(cur.u64_at(path)?)))
    }
}

fn read_nodes(cur: &mut Cur<'_>) -> Result<Vec<SemanticNode>, ParseError> {
    cur.skip_ws();
    if cur.bump() != Some(b'[') {
        return Err(type_err("nodes", "an array"));
    }
    cur.enter()?;
    let mut nodes = Vec::new();
    cur.skip_ws();
    if cur.peek() == Some(b']') {
        cur.i += 1;
        cur.leave();
        return Ok(nodes);
    }
    loop {
        if nodes.len() >= MAX_NODES {
            // One more value is enough to know the array is over the cap. The tail is not read.
            cur.skip_ws();
            if cur.peek() == Some(b']') {
                return Err(syntax("trailing comma"));
            }
            cur.leave();
            return Err(ParseError::Snapshot(SnapshotError::NodeCount(
                nodes.len() + 1,
            )));
        }
        let index = nodes.len();
        nodes.push(read_node(cur, index)?);
        cur.skip_ws();
        match cur.bump() {
            Some(b']') => break,
            Some(b',') => {
                cur.skip_ws();
                if cur.peek() == Some(b']') {
                    return Err(syntax("trailing comma"));
                }
            }
            _ => return Err(syntax("expected ',' or ']'")),
        }
    }
    cur.leave();
    Ok(nodes)
}

struct RawNode {
    id: Option<u64>,
    kind: Option<String>,
    level: Option<u64>,
    /// Set when `level` is present but not an integer. Only a heading reports it.
    level_bad: bool,
    landmark: Option<String>,
    /// Set when `landmark` is present but not a string. Only a landmark reports it.
    landmark_bad: bool,
    text: Option<String>,
    children: Vec<NodeId>,
    href: Option<String>,
    value: Option<String>,
    label: Option<String>,
    checked: Option<bool>,
    options: Vec<String>,
    options_overflow: bool,
    selected: Option<u32>,
    disabled: bool,
    password: bool,
}

fn read_node(cur: &mut Cur<'_>, index: usize) -> Result<SemanticNode, ParseError> {
    let path = format!("nodes[{index}]");
    cur.skip_ws();
    if cur.bump() != Some(b'{') {
        return Err(type_err(&path, "an object"));
    }
    cur.enter()?;
    let mut raw = RawNode {
        id: None,
        kind: None,
        level: None,
        level_bad: false,
        landmark: None,
        landmark_bad: false,
        text: None,
        children: Vec::new(),
        href: None,
        value: None,
        label: None,
        checked: None,
        options: Vec::new(),
        options_overflow: false,
        selected: None,
        disabled: false,
        password: false,
    };
    loop {
        cur.skip_ws();
        if cur.peek() == Some(b'}') {
            cur.i += 1;
            break;
        }
        let key = cur.string(false)?;
        cur.skip_ws();
        if cur.bump() != Some(b':') {
            return Err(syntax("expected ':'"));
        }
        cur.skip_ws();
        let field = format!("{path}.{key}");
        match key.as_str() {
            "id" => raw.id = Some(cur.u64_at(&field)?),
            "kind" => {
                cur.skip_ws();
                if cur.peek() != Some(b'"') {
                    return Err(type_err(format!("{path}.kind"), "a string"));
                }
                raw.kind = Some(cur.string(false)?);
            }
            "level" => {
                let (level, bad) = read_level(cur)?;
                raw.level = level;
                raw.level_bad = bad;
            }
            "landmark" => {
                let (name, bad) = read_landmark(cur)?;
                raw.landmark = name;
                raw.landmark_bad = bad;
            }
            "text" => {
                let previous = raw.text.take();
                raw.text = optional_counted(cur, &field, previous)?;
            }
            "children" => raw.children = read_ids(cur, &field)?,
            "href" => {
                let previous = raw.href.take();
                raw.href = optional_counted(cur, &field, previous)?;
            }
            "value" => {
                let previous = raw.value.take();
                raw.value = optional_counted(cur, &field, previous)?;
            }
            "label" => {
                let previous = raw.label.take();
                raw.label = optional_counted(cur, &field, previous)?;
            }
            "checked" => raw.checked = optional_bool(cur, &field)?,
            "options" => {
                let (options, overflow) = read_options(cur, &field, &raw.options)?;
                raw.options = options;
                raw.options_overflow = raw.options_overflow || overflow;
            }
            "selected" => raw.selected = optional_u32(cur, &field)?,
            "disabled" => raw.disabled = expect_bool(cur, &field)?,
            "password" => raw.password = expect_bool(cur, &field)?,
            _ => cur.skip_value()?,
        }
        cur.skip_ws();
        match cur.bump() {
            Some(b',') => {
                cur.skip_ws();
                if cur.peek() == Some(b'}') {
                    return Err(syntax("trailing comma"));
                }
            }
            Some(b'}') => break,
            _ => return Err(syntax("expected ',' or '}'")),
        }
    }
    cur.leave();
    let id = raw
        .id
        .ok_or_else(|| type_err(format!("{path}.id"), "an integer"))?;
    if raw.options_overflow {
        return Err(ParseError::Snapshot(SnapshotError::Options(NodeId(id))));
    }
    let kind = kind_of(&raw, &path)?;
    Ok(SemanticNode {
        id: NodeId(id),
        kind,
        text: raw.text,
        children: raw.children,
        href: raw.href,
        value: raw.value,
        label: raw.label,
        checked: raw.checked,
        options: raw.options,
        selected: raw.selected,
        disabled: raw.disabled,
        password: raw.password,
    })
}

/// Reads a counted string, giving back the previous value's bytes when a key is repeated.
fn optional_counted(
    cur: &mut Cur<'_>,
    path: &str,
    previous: Option<String>,
) -> Result<Option<String>, ParseError> {
    cur.skip_ws();
    if cur.rest().starts_with("null") {
        cur.eat("null")?;
        if let Some(old) = previous {
            cur.total = cur.total.saturating_sub(old.len());
        }
        return Ok(None);
    }
    if cur.peek() != Some(b'"') {
        return Err(type_err(path, "a string"));
    }
    if let Some(old) = &previous {
        cur.total = cur.total.saturating_sub(old.len());
    }
    cur.string(true).map(Some)
}

fn read_level(cur: &mut Cur<'_>) -> Result<(Option<u64>, bool), ParseError> {
    cur.skip_ws();
    if cur.rest().starts_with("null") {
        cur.eat("null")?;
        return Ok((None, false));
    }
    if matches!(cur.peek(), Some(b'-' | b'0'..=b'9')) {
        let number = cur.number()?;
        return Ok(match number.as_u64() {
            Some(value) => (Some(value), false),
            None => (None, true),
        });
    }
    cur.skip_value()?;
    Ok((None, true))
}

fn read_landmark(cur: &mut Cur<'_>) -> Result<(Option<String>, bool), ParseError> {
    cur.skip_ws();
    if cur.rest().starts_with("null") {
        cur.eat("null")?;
        Ok((None, false))
    } else if cur.peek() == Some(b'"') {
        Ok((Some(cur.string(false)?), false))
    } else {
        // The wrong type is reported only when this node is a landmark.
        cur.skip_value()?;
        Ok((None, true))
    }
}

fn optional_u64(cur: &mut Cur<'_>, path: &str) -> Result<Option<u64>, ParseError> {
    cur.skip_ws();
    if cur.rest().starts_with("null") {
        cur.eat("null")?;
        Ok(None)
    } else {
        Ok(Some(cur.u64_at(path)?))
    }
}

fn optional_u32(cur: &mut Cur<'_>, path: &str) -> Result<Option<u32>, ParseError> {
    match optional_u64(cur, path)? {
        None => Ok(None),
        Some(number) => u32::try_from(number)
            .map(Some)
            .map_err(|_| type_err(path, "an integer in range")),
    }
}

fn optional_bool(cur: &mut Cur<'_>, path: &str) -> Result<Option<bool>, ParseError> {
    cur.skip_ws();
    if cur.rest().starts_with("null") {
        cur.eat("null")?;
        Ok(None)
    } else if cur.peek() == Some(b't') || cur.peek() == Some(b'f') {
        Ok(Some(expect_bool(cur, path)?))
    } else {
        Err(type_err(path, "a boolean"))
    }
}

fn read_ids(cur: &mut Cur<'_>, path: &str) -> Result<Vec<NodeId>, ParseError> {
    cur.skip_ws();
    if cur.rest().starts_with("null") {
        cur.eat("null")?;
        return Ok(Vec::new());
    }
    if cur.bump() != Some(b'[') {
        return Err(type_err(path, "an array of node ids"));
    }
    cur.enter()?;
    let mut ids = Vec::new();
    cur.skip_ws();
    if cur.peek() == Some(b']') {
        cur.i += 1;
        cur.leave();
        return Ok(ids);
    }
    loop {
        if ids.len() >= MAX_NODES {
            cur.skip_ws();
            if cur.peek() == Some(b']') {
                return Err(syntax("trailing comma"));
            }
            cur.leave();
            return Err(ParseError::Snapshot(SnapshotError::NodeCount(
                ids.len() + 1,
            )));
        }
        let item = format!("{path}[{}]", ids.len());
        ids.push(NodeId(cur.u64_at(&item)?));
        cur.skip_ws();
        match cur.bump() {
            Some(b']') => break,
            Some(b',') => {
                cur.skip_ws();
                if cur.peek() == Some(b']') {
                    return Err(syntax("trailing comma"));
                }
            }
            _ => return Err(syntax("expected ',' or ']'")),
        }
    }
    cur.leave();
    Ok(ids)
}

fn read_options(
    cur: &mut Cur<'_>,
    path: &str,
    previous: &[String],
) -> Result<(Vec<String>, bool), ParseError> {
    cur.skip_ws();
    if cur.rest().starts_with("null") {
        cur.eat("null")?;
        release_options(cur, previous);
        return Ok((Vec::new(), false));
    }
    if cur.bump() != Some(b'[') {
        return Err(type_err(path, "an array of strings"));
    }
    release_options(cur, previous);
    cur.enter()?;
    let mut options = Vec::new();
    let mut overflow = false;
    cur.skip_ws();
    if cur.peek() == Some(b']') {
        cur.i += 1;
        cur.leave();
        return Ok((options, false));
    }
    loop {
        if options.len() >= MAX_OPTIONS {
            cur.skip_ws();
            if cur.peek() == Some(b']') {
                return Err(syntax("trailing comma"));
            }
            overflow = true;
            // Drop the rest of this array without storing it.
            cur.skip_value()?;
            cur.skip_ws();
            while cur.peek() == Some(b',') {
                cur.i += 1;
                cur.skip_value()?;
                cur.skip_ws();
            }
            if cur.bump() != Some(b']') {
                return Err(syntax("expected ',' or ']'"));
            }
            break;
        }
        cur.skip_ws();
        if cur.peek() != Some(b'"') {
            return Err(type_err(format!("{path}[{}]", options.len()), "a string"));
        }
        options.push(cur.string(true)?);
        cur.skip_ws();
        match cur.bump() {
            Some(b',') => {
                cur.skip_ws();
                if cur.peek() == Some(b']') {
                    return Err(syntax("trailing comma"));
                }
            }
            Some(b']') => break,
            _ => return Err(syntax("expected ',' or ']'")),
        }
    }
    cur.leave();
    Ok((options, overflow))
}

fn release_options(cur: &mut Cur<'_>, previous: &[String]) {
    for old in previous {
        cur.total = cur.total.saturating_sub(old.len());
    }
}

fn kind_of(raw: &RawNode, path: &str) -> Result<NodeKind, ParseError> {
    let Some(kind) = raw.kind.as_deref() else {
        return Err(type_err(format!("{path}.kind"), "a string"));
    };
    Ok(match kind {
        "document" => NodeKind::Document,
        "heading" => {
            if raw.level_bad {
                return Err(type_err(format!("{path}.level"), "an integer"));
            }
            match raw.level {
                Some(level) if (1..=6).contains(&level) => match u8::try_from(level) {
                    Ok(level) => NodeKind::Heading(level),
                    Err(_) => NodeKind::Container,
                },
                _ => NodeKind::Container,
            }
        }
        "paragraph" => NodeKind::Paragraph,
        "text" => NodeKind::Text,
        "link" => NodeKind::Link,
        "button" => NodeKind::Button,
        "textfield" => NodeKind::TextField,
        "textarea" => NodeKind::TextArea,
        "checkbox" => NodeKind::Checkbox,
        "radio" => NodeKind::Radio,
        "select" => NodeKind::Select,
        "image" => NodeKind::Image,
        "list" => NodeKind::List,
        "listitem" => NodeKind::ListItem,
        "table" => NodeKind::Table,
        "row" => NodeKind::Row,
        "cell" => NodeKind::Cell,
        "pre" => NodeKind::Pre,
        "code" => NodeKind::Code,
        "quote" => NodeKind::Quote,
        "separator" => NodeKind::Separator,
        "landmark" => {
            if raw.landmark_bad {
                return Err(type_err(format!("{path}.landmark"), "a string"));
            }
            match raw.landmark.as_deref() {
                Some("banner") => NodeKind::Landmark(Landmark::Banner),
                Some("navigation") => NodeKind::Landmark(Landmark::Navigation),
                Some("main") => NodeKind::Landmark(Landmark::Main),
                Some("complementary") => NodeKind::Landmark(Landmark::Complementary),
                Some("contentinfo") => NodeKind::Landmark(Landmark::ContentInfo),
                Some("search") => NodeKind::Landmark(Landmark::Search),
                Some("form") => NodeKind::Landmark(Landmark::Form),
                Some("region") => NodeKind::Landmark(Landmark::Region),
                _ => NodeKind::Container,
            }
        }
        _ => NodeKind::Container,
    })
}
