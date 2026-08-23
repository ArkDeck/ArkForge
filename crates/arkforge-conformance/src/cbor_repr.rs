//! Human-readable renderings of a `CborValue`.
//!
//! - `diag` is RFC 8949 §8 diagnostic notation, for reviewers;
//! - `typed_json` is a JSON form a port can load without a CBOR parser, so a
//!   conformance runner can build the value and encode it itself.

use crate::json::{Json, hex};
use arkforge_core::digest::CborValue;

pub fn diag(value: &CborValue) -> String {
    match value {
        CborValue::Unsigned(v) => v.to_string(),
        CborValue::Negative(v) => v.to_string(),
        CborValue::Bytes(b) => format!("h'{}'", hex(b)),
        CborValue::Text(t) => {
            let mut out = String::from("\"");
            for ch in t.chars() {
                match ch {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    c => out.push(c),
                }
            }
            out.push('"');
            out
        }
        CborValue::Array(values) => {
            let inner: Vec<String> = values.iter().map(diag).collect();
            format!("[{}]", inner.join(", "))
        }
        CborValue::Map(entries) => {
            // Diagnostic notation shows the canonical (encoded-key) order, the
            // order a reader of the bytes will meet.
            let mut sorted: Vec<(Vec<u8>, String)> = entries
                .iter()
                .map(|(k, v)| {
                    let key_bytes = k.to_canonical_bytes().unwrap_or_default();
                    (key_bytes, format!("{}: {}", diag(k), diag(v)))
                })
                .collect();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            let inner: Vec<String> = sorted.into_iter().map(|(_, s)| s).collect();
            format!("{{{}}}", inner.join(", "))
        }
        CborValue::Bool(b) => b.to_string(),
        CborValue::Null => "null".to_string(),
    }
}

pub fn typed_json(value: &CborValue) -> Json {
    match value {
        CborValue::Unsigned(v) => Json::object(vec![("uint", Json::Unsigned(*v))]),
        CborValue::Negative(v) => Json::object(vec![("nint", Json::Signed(*v))]),
        CborValue::Bytes(b) => Json::object(vec![("bytes", Json::str(hex(b)))]),
        CborValue::Text(t) => Json::object(vec![("text", Json::str(t.clone()))]),
        CborValue::Array(values) => Json::object(vec![(
            "array",
            Json::Array(values.iter().map(typed_json).collect()),
        )]),
        CborValue::Map(entries) => Json::object(vec![(
            "map",
            Json::Array(
                entries
                    .iter()
                    .map(|(k, v)| Json::Array(vec![typed_json(k), typed_json(v)]))
                    .collect(),
            ),
        )]),
        CborValue::Bool(b) => Json::object(vec![("bool", Json::Bool(*b))]),
        CborValue::Null => Json::str("null"),
    }
}
