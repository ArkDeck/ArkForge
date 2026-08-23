//! A minimal JSON writer.
//!
//! The workspace carries no third-party dependencies (AFD-0001), and a fixture
//! generator needs only to *emit* JSON deterministically — insertion-ordered
//! objects, escaped strings, integers, booleans, arrays. Nothing is parsed.

use core::fmt::Write as _;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Json {
    Null,
    Bool(bool),
    Unsigned(u64),
    Signed(i64),
    Str(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub fn str(value: impl Into<String>) -> Json {
        Json::Str(value.into())
    }

    pub fn strs<I, S>(values: I) -> Json
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Json::Array(values.into_iter().map(|s| Json::Str(s.into())).collect())
    }

    pub fn object(entries: Vec<(&str, Json)>) -> Json {
        Json::Object(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
    }

    /// Appends a key to an object; a no-op on any other variant.
    pub fn push(&mut self, key: &str, value: Json) {
        if let Json::Object(entries) = self {
            entries.push((key.to_string(), value));
        }
    }

    pub fn to_pretty(&self) -> String {
        let mut out = String::new();
        self.write_pretty(&mut out, 0);
        out.push('\n');
        out
    }

    fn write_pretty(&self, out: &mut String, indent: usize) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            Json::Unsigned(value) => write!(out, "{value}").unwrap(),
            Json::Signed(value) => write!(out, "{value}").unwrap(),
            Json::Str(value) => write_escaped(out, value),
            Json::Array(values) => {
                if values.is_empty() {
                    out.push_str("[]");
                    return;
                }
                let simple = values.iter().all(|v| {
                    matches!(
                        v,
                        Json::Null | Json::Bool(_) | Json::Unsigned(_) | Json::Signed(_)
                    ) || matches!(v, Json::Str(s) if s.len() <= 40)
                }) && values.len() <= 8;
                if simple {
                    out.push('[');
                    for (index, value) in values.iter().enumerate() {
                        if index > 0 {
                            out.push_str(", ");
                        }
                        value.write_pretty(out, indent + 1);
                    }
                    out.push(']');
                    return;
                }
                out.push_str("[\n");
                for (index, value) in values.iter().enumerate() {
                    pad(out, indent + 1);
                    value.write_pretty(out, indent + 1);
                    if index + 1 < values.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                pad(out, indent);
                out.push(']');
            }
            Json::Object(entries) => {
                if entries.is_empty() {
                    out.push_str("{}");
                    return;
                }
                out.push_str("{\n");
                for (index, (key, value)) in entries.iter().enumerate() {
                    pad(out, indent + 1);
                    write_escaped(out, key);
                    out.push_str(": ");
                    value.write_pretty(out, indent + 1);
                    if index + 1 < entries.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                pad(out, indent);
                out.push('}');
            }
        }
    }
}

fn pad(out: &mut String, indent: usize) {
    for _ in 0..indent {
        out.push_str("  ");
    }
}

fn write_escaped(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => write!(out, "\\u{:04x}", c as u32).unwrap(),
            c => out.push(c),
        }
    }
    out.push('"');
}

pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(out, "{byte:02x}").unwrap();
    }
    out
}

pub fn unhex(text: &str) -> Vec<u8> {
    assert!(text.len().is_multiple_of(2), "odd hex length");
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let s = core::str::from_utf8(pair).expect("ascii");
            u8::from_str_radix(s, 16).expect("hex")
        })
        .collect()
}
