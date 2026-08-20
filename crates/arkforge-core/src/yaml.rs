//! A strict YAML subset for DeviceProfile documents.
//!
//! Profiles are the file architecture.md 18.2 sketches, and a Profile digest
//! enters the plan (18.3), so the reader has to be exact about what it accepts.
//! This is not a YAML implementation: it is a small block-structured grammar
//! that happens to be valid YAML, chosen so a profile can be reviewed by eye
//! and hashed without ambiguity.
//!
//! Accepted: block mappings, block sequences, flow sequences, plain and quoted
//! scalars, `#` comments, `---` document start.
//!
//! Rejected: anchors, aliases, tags, flow mappings, multi-line scalars, tabs,
//! duplicate keys, and inconsistent indentation. Each of those is a place where
//! two readers could disagree about the document, and a profile that two
//! readers disagree about is a profile that hashes two ways.

use core::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YamlValue {
    Scalar(String),
    Sequence(Vec<YamlValue>),
    Mapping(Vec<(String, YamlValue)>),
    /// An explicitly empty value (`key:` with nothing after it).
    Null,
}

impl YamlValue {
    pub fn as_scalar(&self) -> Option<&str> {
        match self {
            YamlValue::Scalar(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_sequence(&self) -> Option<&[YamlValue]> {
        match self {
            YamlValue::Sequence(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_mapping(&self) -> Option<&[(String, YamlValue)]> {
        match self {
            YamlValue::Mapping(entries) => Some(entries),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&YamlValue> {
        self.as_mapping()?
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }

    pub fn is_null(&self) -> bool {
        matches!(self, YamlValue::Null)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YamlError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for YamlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for YamlError {}

fn error(line: usize, message: impl Into<String>) -> YamlError {
    YamlError {
        line,
        message: message.into(),
    }
}

#[derive(Debug, Clone)]
struct Line {
    number: usize,
    indent: usize,
    content: String,
}

/// Parses a document in the accepted subset.
pub fn parse(source: &str) -> Result<YamlValue, YamlError> {
    let mut lines = Vec::new();
    for (index, raw) in source.lines().enumerate() {
        let number = index + 1;
        if raw.contains('\t') {
            return Err(error(number, "tabs are not permitted for indentation"));
        }
        let indent = raw.len() - raw.trim_start_matches(' ').len();
        let content = strip_comment(&raw[indent..]);
        let content = content.trim_end();
        if content.is_empty() || content == "---" {
            continue;
        }
        if content.starts_with('&') || content.starts_with('*') || content.starts_with('!') {
            return Err(error(
                number,
                "anchors, aliases and tags are not permitted in a profile",
            ));
        }
        lines.push(Line {
            number,
            indent,
            content: content.to_string(),
        });
    }
    if lines.is_empty() {
        return Ok(YamlValue::Null);
    }
    let mut cursor = 0usize;
    let value = parse_block(&lines, &mut cursor, lines[0].indent)?;
    if cursor != lines.len() {
        return Err(error(
            lines[cursor].number,
            "unexpected content after the document body",
        ));
    }
    Ok(value)
}

/// Removes a trailing `# comment`, respecting quoted scalars.
fn strip_comment(text: &str) -> &str {
    let bytes = text.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'#' if !in_single && !in_double
                // A `#` only starts a comment at the start or after a space.
                && (index == 0 || bytes[index - 1] == b' ') =>
            {
                return &text[..index];
            }
            _ => {}
        }
    }
    text
}

fn parse_block(lines: &[Line], cursor: &mut usize, indent: usize) -> Result<YamlValue, YamlError> {
    if *cursor >= lines.len() {
        return Ok(YamlValue::Null);
    }
    if lines[*cursor].content.starts_with("- ") || lines[*cursor].content == "-" {
        parse_sequence(lines, cursor, indent)
    } else {
        parse_mapping(lines, cursor, indent)
    }
}

fn parse_mapping(
    lines: &[Line],
    cursor: &mut usize,
    indent: usize,
) -> Result<YamlValue, YamlError> {
    let mut entries: Vec<(String, YamlValue)> = Vec::new();
    while *cursor < lines.len() {
        let line = &lines[*cursor];
        if line.indent < indent {
            break;
        }
        if line.indent > indent {
            return Err(error(
                line.number,
                "unexpected indentation inside a mapping",
            ));
        }
        if line.content.starts_with("- ") || line.content == "-" {
            return Err(error(
                line.number,
                "a sequence item cannot appear where a mapping key is expected",
            ));
        }

        let (key, rest) = split_key(&line.content, line.number)?;
        if entries.iter().any(|(existing, _)| existing == &key) {
            return Err(error(line.number, format!("duplicate key {key:?}")));
        }
        *cursor += 1;

        let value = if rest.is_empty() {
            let child_indent = lines.get(*cursor).map(|next| next.indent);
            match child_indent {
                Some(child) if child > indent => parse_block(lines, cursor, child)?,
                _ => YamlValue::Null,
            }
        } else {
            parse_inline_scalar(rest, line.number)?
        };
        entries.push((key, value));
    }
    Ok(YamlValue::Mapping(entries))
}

fn parse_sequence(
    lines: &[Line],
    cursor: &mut usize,
    indent: usize,
) -> Result<YamlValue, YamlError> {
    let mut items = Vec::new();
    while *cursor < lines.len() {
        let line = &lines[*cursor];
        if line.indent < indent {
            break;
        }
        if line.indent > indent {
            return Err(error(
                line.number,
                "unexpected indentation inside a sequence",
            ));
        }
        if !(line.content.starts_with("- ") || line.content == "-") {
            break;
        }

        let inline = line.content[1..].trim_start().to_string();
        let item_column = indent + (line.content.len() - line.content[1..].trim_start().len());
        *cursor += 1;

        if inline.is_empty() {
            let child_indent = lines.get(*cursor).map(|next| next.indent);
            let value = match child_indent {
                Some(child) if child > indent => parse_block(lines, cursor, child)?,
                _ => YamlValue::Null,
            };
            items.push(value);
            continue;
        }

        // `- key: value` starts a mapping whose first key sits on this line.
        if let Some((key, rest)) = try_split_key(&inline) {
            let mut entries: Vec<(String, YamlValue)> = Vec::new();
            let value = if rest.is_empty() {
                let child_indent = lines.get(*cursor).map(|next| next.indent);
                match child_indent {
                    Some(child) if child > item_column => parse_block(lines, cursor, child)?,
                    _ => YamlValue::Null,
                }
            } else {
                parse_inline_scalar(rest, line.number)?
            };
            entries.push((key, value));

            while *cursor < lines.len() && lines[*cursor].indent == item_column {
                let continuation = &lines[*cursor];
                if continuation.content.starts_with("- ") {
                    break;
                }
                let (key, rest) = split_key(&continuation.content, continuation.number)?;
                if entries.iter().any(|(existing, _)| existing == &key) {
                    return Err(error(continuation.number, format!("duplicate key {key:?}")));
                }
                let number = continuation.number;
                *cursor += 1;
                let value = if rest.is_empty() {
                    let child_indent = lines.get(*cursor).map(|next| next.indent);
                    match child_indent {
                        Some(child) if child > item_column => parse_block(lines, cursor, child)?,
                        _ => YamlValue::Null,
                    }
                } else {
                    parse_inline_scalar(rest, number)?
                };
                entries.push((key, value));
            }
            items.push(YamlValue::Mapping(entries));
        } else {
            items.push(parse_inline_scalar(&inline, line.number)?);
        }
    }
    Ok(YamlValue::Sequence(items))
}

fn split_key(content: &str, line: usize) -> Result<(String, &str), YamlError> {
    try_split_key(content)
        .ok_or_else(|| error(line, format!("expected `key: value`, found {content:?}")))
}

fn try_split_key(content: &str) -> Option<(String, &str)> {
    let bytes = content.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b':' if !in_single && !in_double => {
                let after = bytes.get(index + 1);
                if after.is_none() || after == Some(&b' ') {
                    let key = content[..index].trim();
                    if key.is_empty() {
                        return None;
                    }
                    let key = unquote_simple(key);
                    return Some((key, content[index + 1..].trim()));
                }
            }
            _ => {}
        }
    }
    None
}

fn unquote_simple(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.len() >= 2 {
        let bytes = trimmed.as_bytes();
        if (bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'')
        {
            return trimmed[1..trimmed.len() - 1].to_string();
        }
    }
    trimmed.to_string()
}

fn parse_inline_scalar(text: &str, line: usize) -> Result<YamlValue, YamlError> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(YamlValue::Null);
    }
    if text.starts_with('{') {
        return Err(error(
            line,
            "flow mappings are not permitted; use a block mapping",
        ));
    }
    if text.starts_with('|') || text.starts_with('>') {
        return Err(error(line, "multi-line scalars are not permitted"));
    }
    if text.starts_with('[') {
        if !text.ends_with(']') {
            return Err(error(line, "flow sequence is not closed on one line"));
        }
        let inner = &text[1..text.len() - 1];
        if inner.trim().is_empty() {
            return Ok(YamlValue::Sequence(Vec::new()));
        }
        let mut items = Vec::new();
        for part in split_flow_items(inner, line)? {
            let part = part.trim();
            if part.is_empty() {
                return Err(error(line, "empty item in flow sequence"));
            }
            items.push(YamlValue::Scalar(unquote_simple(part)));
        }
        return Ok(YamlValue::Sequence(items));
    }
    if text == "null" || text == "~" {
        return Ok(YamlValue::Null);
    }
    Ok(YamlValue::Scalar(unquote_simple(text)))
}

fn split_flow_items(inner: &str, line: usize) -> Result<Vec<&str>, YamlError> {
    let bytes = inner.as_bytes();
    let mut items = Vec::new();
    let mut start = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'[' | b'{' if !in_single && !in_double => {
                return Err(error(line, "nested flow collections are not permitted"));
            }
            b',' if !in_single && !in_double => {
                items.push(&inner[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    items.push(&inner[start..]);
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_shape_a_profile_uses() {
        let document = r#"
# a profile
schemaVersion: arkforge.device-profile/v1
profile:
  id: org.example.testboard
  version: 1.0.0
identity:
  productModels: [TESTBOARD]
  soc:
    vendor: examplevendor
    family: rk3568
providers:
  - id: arkforge.example
    backend: example-tool-fixed
    versionRange: ">=1.0.0 <2.0.0"
  - id: arkforge.example-native
    backend: download-native
allowedTargets: []
readDomain:
  write: full-disk
  read: characterize-at-runtime   # measured every execution
  erasedMediumFiller: "0xCC"
"#;
        let value = parse(document).unwrap();
        assert_eq!(
            value.get("schemaVersion").unwrap().as_scalar(),
            Some("arkforge.device-profile/v1")
        );
        assert_eq!(
            value.get("profile").unwrap().get("id").unwrap().as_scalar(),
            Some("org.example.testboard")
        );
        assert_eq!(
            value
                .get("identity")
                .unwrap()
                .get("soc")
                .unwrap()
                .get("family")
                .unwrap()
                .as_scalar(),
            Some("rk3568")
        );
        assert_eq!(
            value
                .get("identity")
                .unwrap()
                .get("productModels")
                .unwrap()
                .as_sequence()
                .unwrap()
                .len(),
            1
        );
        let providers = value.get("providers").unwrap().as_sequence().unwrap();
        assert_eq!(providers.len(), 2);
        assert_eq!(
            providers[0].get("versionRange").unwrap().as_scalar(),
            Some(">=1.0.0 <2.0.0")
        );
        assert!(providers[1].get("versionRange").is_none());
        assert!(
            value
                .get("allowedTargets")
                .unwrap()
                .as_sequence()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            value
                .get("readDomain")
                .unwrap()
                .get("read")
                .unwrap()
                .as_scalar(),
            Some("characterize-at-runtime")
        );
        assert_eq!(
            value
                .get("readDomain")
                .unwrap()
                .get("erasedMediumFiller")
                .unwrap()
                .as_scalar(),
            Some("0xCC")
        );
    }

    #[test]
    fn a_duplicate_key_is_rejected() {
        let error = parse("a: 1\na: 2\n").unwrap_err();
        assert!(error.message.contains("duplicate key"), "{error}");
    }

    #[test]
    fn tabs_are_rejected() {
        let error = parse("a:\n\tb: 1\n").unwrap_err();
        assert!(error.message.contains("tabs"), "{error}");
    }

    #[test]
    fn anchors_aliases_and_tags_are_rejected() {
        for document in ["a: 1\n&anchor\n", "*alias\n", "!!str x\n"] {
            assert!(parse(document).is_err(), "{document:?} should be rejected");
        }
    }

    #[test]
    fn flow_mappings_and_block_scalars_are_rejected() {
        assert!(parse("a: {b: 1}\n").is_err());
        assert!(parse("a: |\n  text\n").is_err());
        assert!(parse("a: >\n  text\n").is_err());
    }

    #[test]
    fn a_url_value_is_not_split_at_its_scheme_colon() {
        let value = parse("source: https://example.invalid/spec\n").unwrap();
        assert_eq!(
            value.get("source").unwrap().as_scalar(),
            Some("https://example.invalid/spec")
        );
    }

    #[test]
    fn a_hash_inside_a_quoted_scalar_is_not_a_comment() {
        let value = parse("note: \"contains # not a comment\"\n").unwrap();
        assert_eq!(
            value.get("note").unwrap().as_scalar(),
            Some("contains # not a comment")
        );
    }

    #[test]
    fn nested_sequences_of_mappings_keep_their_shape() {
        let document = r#"
modeTransitions:
  - from: hdc-normal
    to: download-loader
    rebind:
      requireDisconnect: true
      toleranceWindowMs: 20000
  - from: download-loader
    to: hdc-normal
    rebind:
      requireDisconnect: true
      toleranceWindowMs: 90000
"#;
        let value = parse(document).unwrap();
        let transitions = value.get("modeTransitions").unwrap().as_sequence().unwrap();
        assert_eq!(transitions.len(), 2);
        assert_eq!(
            transitions[0].get("from").unwrap().as_scalar(),
            Some("hdc-normal")
        );
        assert_eq!(
            transitions[1]
                .get("rebind")
                .unwrap()
                .get("toleranceWindowMs")
                .unwrap()
                .as_scalar(),
            Some("90000")
        );
    }

    #[test]
    fn an_empty_document_is_null() {
        assert_eq!(parse("").unwrap(), YamlValue::Null);
        assert_eq!(parse("# only a comment\n").unwrap(), YamlValue::Null);
    }

    #[test]
    fn misaligned_indentation_is_rejected_rather_than_guessed() {
        let document = "a:\n  b: 1\n   c: 2\n";
        assert!(parse(document).is_err());
    }
}
