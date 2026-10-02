//! Settings: the values of one implementation's own table of the configuration.
//!
//! The core reads the configuration file and hands a keymap or a chrome its own table
//! (`[keys]`, `[keymap.<name>]`, `[chrome.<name>]`) as these values: first to the
//! implementation's check, then, once the whole file is valid, to the implementation itself. An
//! implementation never sees the rest of the file, and doesn't depend on the TOML crate the
//! core reads it with.
//!
//! Must not own: reading or parsing the file (the executable and the core), or what any key
//! means (the implementation whose table it is).

use std::collections::BTreeMap;

/// One value of a setting: TOML's types without dates, which the core refuses.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// A string.
    String(String),
    /// A whole number.
    Integer(i64),
    /// A floating-point number.
    Float(f64),
    /// `true` or `false`.
    Boolean(bool),
    /// A list of values.
    Array(Vec<Value>),
    /// A table of settings.
    Table(Table),
}

/// A table of settings, by key.
pub type Table = BTreeMap<String, Value>;

impl Value {
    /// The string, if it is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(text) => Some(text),
            _ => None,
        }
    }

    /// The boolean, if it is one.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Boolean(value) => Some(*value),
            _ => None,
        }
    }

    /// The table, if it is one.
    pub fn as_table(&self) -> Option<&Table> {
        match self {
            Self::Table(table) => Some(table),
            _ => None,
        }
    }

    /// What kind of value it is, for messages: "a string", "a table", ...
    pub fn kind(&self) -> &'static str {
        match self {
            Self::String(_) => "a string",
            Self::Integer(_) => "a whole number",
            Self::Float(_) => "a number",
            Self::Boolean(_) => "true or false",
            Self::Array(_) => "a list",
            Self::Table(_) => "a table",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_say_what_they_are() {
        let text = Value::String("insert".into());
        let table = Value::Table(Table::from([("pass".into(), Value::Boolean(true))]));
        assert_eq!(text.as_str(), Some("insert"));
        assert_eq!(text.as_table(), None);
        assert_eq!(table.as_table().unwrap()["pass"].as_bool(), Some(true));
        assert_eq!(Value::Integer(3).as_str(), None);
        let kinds = [Value::Float(1.5), Value::Array(Vec::new()), table].map(|v| v.kind());
        assert_eq!(kinds, ["a number", "a list", "a table"]);
    }
}
