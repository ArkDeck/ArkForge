//! A minimal, dependency-free JSON reader/writer.
//!
//! The workspace carries no third-party dependencies (AFD-0001), and a fixture
//! The reader exists for the in-tree schema validator. It deliberately accepts
//! only integer JSON numbers: ArkForge schemas and model instances contain no
//! floats, and silently rounding one would defeat validation.

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

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(entries) => entries
                .iter()
                .find(|(candidate, _)| candidate == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(values) => Some(values),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Object(entries) => Some(entries),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Json::Unsigned(value) => Some(*value),
            Json::Signed(value) if *value >= 0 => Some(*value as u64),
            _ => None,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonParseError {
    pub offset: usize,
    pub message: String,
}

impl core::fmt::Display for JsonParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "byte {}: {}", self.offset, self.message)
    }
}

impl std::error::Error for JsonParseError {}

pub fn parse(source: &str) -> Result<Json, JsonParseError> {
    let mut parser = Parser {
        source: source.as_bytes(),
        offset: 0,
    };
    parser.whitespace();
    let value = parser.value(0)?;
    parser.whitespace();
    if parser.offset != parser.source.len() {
        return Err(parser.error("trailing bytes after the JSON value"));
    }
    Ok(value)
}

struct Parser<'a> {
    source: &'a [u8],
    offset: usize,
}

impl Parser<'_> {
    fn error(&self, message: impl Into<String>) -> JsonParseError {
        JsonParseError {
            offset: self.offset,
            message: message.into(),
        }
    }

    fn whitespace(&mut self) {
        while self
            .source
            .get(self.offset)
            .is_some_and(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.offset += 1;
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, JsonParseError> {
        if depth > 128 {
            return Err(self.error("JSON nesting exceeds 128 levels"));
        }
        match self.source.get(self.offset).copied() {
            Some(b'n') => self.literal(b"null", Json::Null),
            Some(b't') => self.literal(b"true", Json::Bool(true)),
            Some(b'f') => self.literal(b"false", Json::Bool(false)),
            Some(b'"') => self.string().map(Json::Str),
            Some(b'[') => self.array(depth + 1),
            Some(b'{') => self.object(depth + 1),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(found) => Err(self.error(format!("unexpected byte 0x{found:02x}"))),
            None => Err(self.error("unexpected end of JSON")),
        }
    }

    fn literal(&mut self, text: &[u8], value: Json) -> Result<Json, JsonParseError> {
        if self.source.get(self.offset..self.offset + text.len()) == Some(text) {
            self.offset += text.len();
            Ok(value)
        } else {
            Err(self.error("invalid JSON literal"))
        }
    }

    fn string(&mut self) -> Result<String, JsonParseError> {
        self.offset += 1;
        let mut out = String::new();
        loop {
            let Some(byte) = self.source.get(self.offset).copied() else {
                return Err(self.error("unterminated JSON string"));
            };
            match byte {
                b'"' => {
                    self.offset += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.offset += 1;
                    let Some(escape) = self.source.get(self.offset).copied() else {
                        return Err(self.error("truncated JSON escape"));
                    };
                    self.offset += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => self.unicode_escape(&mut out)?,
                        _ => return Err(self.error("unknown JSON escape")),
                    }
                }
                0x00..=0x1f => return Err(self.error("control byte in JSON string")),
                0x20..=0x7f => {
                    out.push(byte as char);
                    self.offset += 1;
                }
                _ => {
                    let remaining = core::str::from_utf8(&self.source[self.offset..])
                        .map_err(|_| self.error("invalid UTF-8 in JSON string"))?;
                    let character = remaining
                        .chars()
                        .next()
                        .ok_or_else(|| self.error("truncated UTF-8 in JSON string"))?;
                    out.push(character);
                    self.offset += character.len_utf8();
                }
            }
        }
    }

    fn unicode_escape(&mut self, out: &mut String) -> Result<(), JsonParseError> {
        let first = self.hex_quad()?;
        let scalar = if (0xd800..=0xdbff).contains(&first) {
            if self.source.get(self.offset..self.offset + 2) != Some(b"\\u") {
                return Err(self.error("high surrogate is not followed by a low surrogate"));
            }
            self.offset += 2;
            let second = self.hex_quad()?;
            if !(0xdc00..=0xdfff).contains(&second) {
                return Err(self.error("invalid low surrogate"));
            }
            0x1_0000 + (((first as u32 - 0xd800) << 10) | (second as u32 - 0xdc00))
        } else if (0xdc00..=0xdfff).contains(&first) {
            return Err(self.error("unpaired low surrogate"));
        } else {
            first as u32
        };
        out.push(char::from_u32(scalar).ok_or_else(|| self.error("invalid Unicode scalar"))?);
        Ok(())
    }

    fn hex_quad(&mut self) -> Result<u16, JsonParseError> {
        let bytes = self
            .source
            .get(self.offset..self.offset + 4)
            .ok_or_else(|| self.error("truncated Unicode escape"))?;
        let text = core::str::from_utf8(bytes).expect("ASCII slice");
        let value = u16::from_str_radix(text, 16)
            .map_err(|_| self.error("Unicode escape is not four hexadecimal digits"))?;
        self.offset += 4;
        Ok(value)
    }

    fn array(&mut self, depth: usize) -> Result<Json, JsonParseError> {
        self.offset += 1;
        self.whitespace();
        let mut values = Vec::new();
        if self.source.get(self.offset) == Some(&b']') {
            self.offset += 1;
            return Ok(Json::Array(values));
        }
        loop {
            values.push(self.value(depth)?);
            self.whitespace();
            match self.source.get(self.offset) {
                Some(b',') => {
                    self.offset += 1;
                    self.whitespace();
                }
                Some(b']') => {
                    self.offset += 1;
                    return Ok(Json::Array(values));
                }
                _ => return Err(self.error("expected ',' or ']' in JSON array")),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, JsonParseError> {
        self.offset += 1;
        self.whitespace();
        let mut entries: Vec<(String, Json)> = Vec::new();
        if self.source.get(self.offset) == Some(&b'}') {
            self.offset += 1;
            return Ok(Json::Object(entries));
        }
        loop {
            if self.source.get(self.offset) != Some(&b'"') {
                return Err(self.error("JSON object key must be a string"));
            }
            let key = self.string()?;
            if entries.iter().any(|(existing, _)| existing == &key) {
                return Err(self.error(format!("duplicate JSON object key {key:?}")));
            }
            self.whitespace();
            if self.source.get(self.offset) != Some(&b':') {
                return Err(self.error("expected ':' after JSON object key"));
            }
            self.offset += 1;
            self.whitespace();
            let value = self.value(depth)?;
            entries.push((key, value));
            self.whitespace();
            match self.source.get(self.offset) {
                Some(b',') => {
                    self.offset += 1;
                    self.whitespace();
                }
                Some(b'}') => {
                    self.offset += 1;
                    return Ok(Json::Object(entries));
                }
                _ => return Err(self.error("expected ',' or '}' in JSON object")),
            }
        }
    }

    fn number(&mut self) -> Result<Json, JsonParseError> {
        let start = self.offset;
        if self.source.get(self.offset) == Some(&b'-') {
            self.offset += 1;
        }
        if self.source.get(self.offset) == Some(&b'0') {
            self.offset += 1;
            if self.source.get(self.offset).is_some_and(u8::is_ascii_digit) {
                return Err(self.error("leading zero in JSON number"));
            }
        } else {
            let digits = self.offset;
            while self.source.get(self.offset).is_some_and(u8::is_ascii_digit) {
                self.offset += 1;
            }
            if self.offset == digits {
                return Err(self.error("JSON number has no digits"));
            }
        }
        if self
            .source
            .get(self.offset)
            .is_some_and(|byte| matches!(byte, b'.' | b'e' | b'E'))
        {
            return Err(self.error("floating-point JSON is outside the ArkForge model"));
        }
        let text = core::str::from_utf8(&self.source[start..self.offset]).unwrap();
        if text.starts_with('-') {
            text.parse::<i64>()
                .map(Json::Signed)
                .map_err(|_| self.error("signed JSON integer is out of range"))
        } else {
            text.parse::<u64>()
                .map(Json::Unsigned)
                .map_err(|_| self.error("unsigned JSON integer is out of range"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_round_trips_the_writer_and_unicode() {
        let value = Json::object(vec![
            ("text", Json::str("开发板 \"one\"")),
            (
                "values",
                Json::Array(vec![Json::Unsigned(1), Json::Bool(true), Json::Null]),
            ),
        ]);
        assert_eq!(parse(&value.to_pretty()).unwrap(), value);
        assert_eq!(parse("\"\\ud83d\\ude80\"").unwrap(), Json::str("🚀"));
    }

    #[test]
    fn reader_rejects_duplicate_keys_floats_and_trailing_bytes() {
        assert!(parse("{\"a\":1,\"a\":2}").is_err());
        assert!(parse("1.5").is_err());
        assert!(parse("true false").is_err());
    }
}
