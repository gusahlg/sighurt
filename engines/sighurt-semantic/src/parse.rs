//! JSON from the extraction script, checked against the semantic bounds.
//!
//! Owns: turning that JSON into a [`SemanticSnapshot`] and refusing a value that is too big,
//! the wrong shape, or fails [`SemanticSnapshot::check`]. Must not own: producing the JSON, or
//! trusting a document generation carried in it. The generation on the snapshot is the one the
//! caller passes.

use std::fmt;

use serde_json::{Map, Value};
use sighurt_api::ids::DocumentGeneration;
use sighurt_api::semantic::{
    Landmark, NodeId, NodeKind, SemanticNode, SemanticSnapshot, SnapshotError, MAX_NODES,
    MAX_OPTIONS, MAX_TOTAL_BYTES,
};

/// Bytes of JSON structure allowed per node, on top of the doubled text budget.
///
/// [`max_input_bytes`] is [`MAX_TOTAL_BYTES`] twice (JSON escaping can double a string) plus
/// this much for every node the snapshot is allowed to hold. A longer input is refused before
/// it is parsed.
pub const NODE_JSON_OVERHEAD: usize = 512;

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

/// Parses the extraction script's JSON into a snapshot of `document`.
///
/// The input is refused whole when it is longer than [`max_input_bytes`]. A `nodes` array
/// longer than [`MAX_NODES`] is refused before those nodes are built. A kind this crate does
/// not know becomes [`NodeKind::Container`]. A heading level outside 1..=6, or a landmark name
/// this crate does not know, does too. `document` in the JSON is ignored: the snapshot is
/// stamped with `document`. [`SemanticSnapshot::check`] then runs, and its error is returned
/// instead of a snapshot. The truncation flag is the one in the JSON (missing means false).
pub fn parse(json: &str, document: DocumentGeneration) -> Result<SemanticSnapshot, ParseError> {
    let limit = max_input_bytes();
    if json.len() > limit {
        return Err(ParseError::TooLarge {
            len: json.len(),
            limit,
        });
    }
    let value: Value =
        serde_json::from_str(json).map_err(|err| ParseError::Syntax(err.to_string()))?;
    let Some(obj) = value.as_object() else {
        return Err(type_err("snapshot", "an object"));
    };
    let truncated = match obj.get("truncated") {
        None => false,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => return Err(type_err("truncated", "a boolean")),
    };
    let root = req_u64(obj.get("root"), "root")?;
    let focus = opt_id(obj.get("focus"), "focus")?;
    let Some(nodes_value) = obj.get("nodes") else {
        return Err(type_err("nodes", "an array"));
    };
    let Some(arr) = nodes_value.as_array() else {
        return Err(type_err("nodes", "an array"));
    };
    if arr.len() > MAX_NODES {
        return Err(ParseError::Snapshot(SnapshotError::NodeCount(arr.len())));
    }
    let mut nodes = Vec::with_capacity(arr.len());
    for (index, item) in arr.iter().enumerate() {
        nodes.push(parse_node(item, &format!("nodes[{index}]"))?);
    }
    let snapshot = SemanticSnapshot {
        document,
        root: NodeId(root),
        nodes,
        focus,
        truncated,
    };
    snapshot.check().map_err(ParseError::Snapshot)?;
    Ok(snapshot)
}

fn parse_node(value: &Value, path: &str) -> Result<SemanticNode, ParseError> {
    let Some(obj) = value.as_object() else {
        return Err(type_err(path, "an object"));
    };
    let id = req_u64(obj.get("id"), &format!("{path}.id"))?;
    let kind = parse_kind(obj, path)?;
    if let Some(Value::Array(options)) = obj.get("options") {
        if options.len() > MAX_OPTIONS {
            return Err(ParseError::Snapshot(SnapshotError::Options(NodeId(id))));
        }
    }
    Ok(SemanticNode {
        id: NodeId(id),
        kind,
        text: opt_string(obj, "text", path)?,
        children: parse_ids(obj.get("children"), &format!("{path}.children"))?,
        href: opt_string(obj, "href", path)?,
        value: opt_string(obj, "value", path)?,
        label: opt_string(obj, "label", path)?,
        checked: opt_bool(obj, "checked", path)?,
        options: parse_strings(obj.get("options"), &format!("{path}.options"))?,
        selected: opt_u32(obj, "selected", path)?,
        disabled: flag(obj, "disabled", path)?,
        password: flag(obj, "password", path)?,
    })
}

fn parse_kind(obj: &Map<String, Value>, path: &str) -> Result<NodeKind, ParseError> {
    let Some(Value::String(kind)) = obj.get("kind") else {
        return Err(type_err(format!("{path}.kind"), "a string"));
    };
    Ok(match kind.as_str() {
        "document" => NodeKind::Document,
        "heading" => match heading_level(obj, path)? {
            Some(level) => NodeKind::Heading(level),
            None => NodeKind::Container,
        },
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
        "landmark" => match landmark(obj, path)? {
            Some(kind) => NodeKind::Landmark(kind),
            None => NodeKind::Container,
        },
        _ => NodeKind::Container,
    })
}

fn heading_level(obj: &Map<String, Value>, path: &str) -> Result<Option<u8>, ParseError> {
    match obj.get("level") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => match number.as_u64() {
            Some(level) if (1..=6).contains(&level) => Ok(Some(level as u8)),
            Some(_) => Ok(None),
            None => Err(type_err(format!("{path}.level"), "an integer")),
        },
        Some(_) => Err(type_err(format!("{path}.level"), "an integer")),
    }
}

fn landmark(obj: &Map<String, Value>, path: &str) -> Result<Option<Landmark>, ParseError> {
    match obj.get("landmark") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(name)) => Ok(match name.as_str() {
            "banner" => Some(Landmark::Banner),
            "navigation" => Some(Landmark::Navigation),
            "main" => Some(Landmark::Main),
            "complementary" => Some(Landmark::Complementary),
            "contentinfo" => Some(Landmark::ContentInfo),
            "search" => Some(Landmark::Search),
            "form" => Some(Landmark::Form),
            "region" => Some(Landmark::Region),
            _ => None,
        }),
        Some(_) => Err(type_err(format!("{path}.landmark"), "a string")),
    }
}

fn parse_ids(value: Option<&Value>, path: &str) -> Result<Vec<NodeId>, ParseError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let Some(arr) = value.as_array() else {
        return Err(type_err(path, "an array of node ids"));
    };
    let mut ids = Vec::with_capacity(arr.len());
    for (index, item) in arr.iter().enumerate() {
        ids.push(NodeId(req_u64(Some(item), &format!("{path}[{index}]"))?));
    }
    Ok(ids)
}

fn parse_strings(value: Option<&Value>, path: &str) -> Result<Vec<String>, ParseError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let Some(arr) = value.as_array() else {
        return Err(type_err(path, "an array of strings"));
    };
    let mut strings = Vec::with_capacity(arr.len());
    for (index, item) in arr.iter().enumerate() {
        let Some(text) = item.as_str() else {
            return Err(type_err(format!("{path}[{index}]"), "a string"));
        };
        strings.push(text.to_owned());
    }
    Ok(strings)
}

fn opt_string(
    obj: &Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<Option<String>, ParseError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(type_err(format!("{path}.{key}"), "a string")),
    }
}

fn opt_bool(obj: &Map<String, Value>, key: &str, path: &str) -> Result<Option<bool>, ParseError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(flag)) => Ok(Some(*flag)),
        Some(_) => Err(type_err(format!("{path}.{key}"), "a boolean")),
    }
}

fn flag(obj: &Map<String, Value>, key: &str, path: &str) -> Result<bool, ParseError> {
    match obj.get(key) {
        None => Ok(false),
        Some(Value::Bool(flag)) => Ok(*flag),
        Some(_) => Err(type_err(format!("{path}.{key}"), "a boolean")),
    }
}

fn opt_id(value: Option<&Value>, path: &str) -> Result<Option<NodeId>, ParseError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(_) => Ok(Some(NodeId(req_u64(value, path)?))),
    }
}

fn opt_u32(obj: &Map<String, Value>, key: &str, path: &str) -> Result<Option<u32>, ParseError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let number = req_u64(Some(value), &format!("{path}.{key}"))?;
            u32::try_from(number)
                .map(Some)
                .map_err(|_| type_err(format!("{path}.{key}"), "an integer in range"))
        }
    }
}

fn req_u64(value: Option<&Value>, path: &str) -> Result<u64, ParseError> {
    let Some(Value::Number(number)) = value else {
        return Err(type_err(path, "an integer"));
    };
    number.as_u64().ok_or_else(|| type_err(path, "an integer"))
}
