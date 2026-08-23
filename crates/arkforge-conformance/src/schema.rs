//! In-tree validation for the committed JSON Schemas and their published YAML
//! instances. This implements exactly the Draft 2020-12 keyword subset used by
//! `spec/model/*.schema.json`; an unknown regex is an error, not a silent pass.

use crate::json::{Json, parse as parse_json};
use arkforge_core::digest::Domain;
use arkforge_core::yaml::{self, YamlValue};
use std::path::Path;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ValidationReport {
    pub schemas: usize,
    pub profiles: usize,
    pub transcripts: usize,
    pub domains: usize,
    pub problems: Vec<String>,
}

impl ValidationReport {
    pub fn is_valid(&self) -> bool {
        self.problems.is_empty()
    }
}

pub fn validate_repository(root: &Path) -> ValidationReport {
    let mut report = ValidationReport::default();
    let profile_schema = read_schema(root, "spec/model/profile.schema.json", &mut report);
    let transcript_schema = read_schema(root, "spec/model/transcript.schema.json", &mut report);

    if let Some(schema) = &profile_schema {
        validate_yaml_directory(
            root,
            "profiles",
            schema,
            |source| {
                arkforge_core::profile::load(source)
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            },
            &mut report.profiles,
            &mut report.problems,
        );
    }
    if let Some(schema) = &transcript_schema {
        validate_yaml_directory(
            root,
            "transcripts",
            schema,
            |source| {
                arkforge_transport::transcript::parse(source)
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            },
            &mut report.transcripts,
            &mut report.problems,
        );
    }

    validate_cddl_and_domains(root, &mut report);
    report
}

fn read_schema(root: &Path, relative: &str, report: &mut ValidationReport) -> Option<Json> {
    let path = root.join(relative);
    match std::fs::read_to_string(&path) {
        Ok(source) => match parse_json(&source) {
            Ok(schema) => {
                if schema.get("$schema").and_then(Json::as_str)
                    != Some("https://json-schema.org/draft/2020-12/schema")
                {
                    report
                        .problems
                        .push(format!("{relative}: missing Draft 2020-12 $schema"));
                }
                report.schemas += 1;
                Some(schema)
            }
            Err(error) => {
                report.problems.push(format!("{relative}: {error}"));
                None
            }
        },
        Err(error) => {
            report.problems.push(format!("{relative}: {error}"));
            None
        }
    }
}

fn validate_yaml_directory(
    root: &Path,
    relative: &str,
    schema: &Json,
    semantic: impl Fn(&str) -> Result<(), String>,
    count: &mut usize,
    problems: &mut Vec<String>,
) {
    let directory = root.join(relative);
    let Ok(entries) = std::fs::read_dir(&directory) else {
        problems.push(format!("{relative}: directory is missing"));
        return;
    };
    let mut paths = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("yaml"))
        .collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        let name = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let source = match std::fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                problems.push(format!("{name}: {error}"));
                continue;
            }
        };
        let model = match yaml::parse(&source) {
            Ok(value) => yaml_json(&value),
            Err(error) => {
                problems.push(format!("{name}: strict YAML: {error}"));
                continue;
            }
        };
        let mut schema_problems = Vec::new();
        validate(schema, schema, &model, "$", &mut schema_problems);
        problems.extend(
            schema_problems
                .into_iter()
                .map(|problem| format!("{name}: {problem}")),
        );
        if let Err(error) = semantic(&source) {
            problems.push(format!("{name}: semantic loader: {error}"));
        }
        *count += 1;
    }
}

fn yaml_json(value: &YamlValue) -> Json {
    match value {
        YamlValue::Null => Json::Null,
        YamlValue::Scalar(value) => Json::str(value),
        YamlValue::Sequence(values) => Json::Array(values.iter().map(yaml_json).collect()),
        YamlValue::Mapping(entries) => Json::Object(
            entries
                .iter()
                .map(|(key, value)| (key.clone(), yaml_json(value)))
                .collect(),
        ),
    }
}

fn validate(root: &Json, schema: &Json, value: &Json, path: &str, out: &mut Vec<String>) {
    let Some(object) = schema.as_object() else {
        out.push(format!("{path}: schema node is not an object"));
        return;
    };

    if let Some(reference) = schema.get("$ref").and_then(Json::as_str) {
        match resolve_ref(root, reference) {
            Some(target) => validate(root, target, value, path, out),
            None => out.push(format!("{path}: unresolved schema reference {reference}")),
        }
    }

    if let Some(expected) = schema.get("type") {
        let matches = match expected {
            Json::Str(kind) => type_matches(kind, value),
            Json::Array(kinds) => kinds
                .iter()
                .filter_map(Json::as_str)
                .any(|kind| type_matches(kind, value)),
            _ => false,
        };
        if !matches {
            out.push(format!("{path}: value does not match schema type"));
            return;
        }
    }
    if let Some(expected) = schema.get("const")
        && expected != value
    {
        out.push(format!("{path}: value differs from const"));
    }
    if let Some(values) = schema.get("enum").and_then(Json::as_array)
        && !values.contains(value)
    {
        out.push(format!("{path}: value is outside enum"));
    }

    if let Some(branches) = schema.get("allOf").and_then(Json::as_array) {
        for branch in branches {
            validate(root, branch, value, path, out);
        }
    }
    if let Some(branches) = schema.get("oneOf").and_then(Json::as_array) {
        let matches = branches
            .iter()
            .filter(|branch| {
                let mut branch_problems = Vec::new();
                validate(root, branch, value, path, &mut branch_problems);
                branch_problems.is_empty()
            })
            .count();
        if matches != 1 {
            out.push(format!(
                "{path}: oneOf matched {matches} branches, expected 1"
            ));
        }
    }
    if let Some(negative) = schema.get("not") {
        let mut negative_problems = Vec::new();
        validate(root, negative, value, path, &mut negative_problems);
        if negative_problems.is_empty() {
            out.push(format!("{path}: value matches forbidden `not` schema"));
        }
    }

    if let Some(text) = value.as_str() {
        if let Some(minimum) = schema.get("minLength").and_then(Json::as_u64)
            && text.chars().count() < minimum as usize
        {
            out.push(format!(
                "{path}: string is shorter than minLength {minimum}"
            ));
        }
        if let Some(maximum) = schema.get("maxLength").and_then(Json::as_u64)
            && text.chars().count() > maximum as usize
        {
            out.push(format!("{path}: string is longer than maxLength {maximum}"));
        }
        if let Some(pattern) = schema.get("pattern").and_then(Json::as_str) {
            match known_pattern(pattern, text) {
                Some(true) => {}
                Some(false) => out.push(format!("{path}: string does not match {pattern}")),
                None => out.push(format!(
                    "{path}: validator does not implement pattern {pattern}"
                )),
            }
        }
    }

    if let Some(values) = value.as_array() {
        if let Some(minimum) = schema.get("minItems").and_then(Json::as_u64)
            && values.len() < minimum as usize
        {
            out.push(format!("{path}: array has fewer than {minimum} items"));
        }
        if let Some(items) = schema.get("items") {
            for (index, item) in values.iter().enumerate() {
                validate(root, items, item, &format!("{path}[{index}]"), out);
            }
        }
    }

    if let Some(entries) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(Json::as_array) {
            for key in required.iter().filter_map(Json::as_str) {
                if !entries.iter().any(|(candidate, _)| candidate == key) {
                    out.push(format!("{path}: missing required property {key}"));
                }
            }
        }
        if let Some(properties) = schema.get("properties").and_then(Json::as_object) {
            for (key, property_schema) in properties {
                if let Some((_, property_value)) =
                    entries.iter().find(|(candidate, _)| candidate == key)
                {
                    validate(
                        root,
                        property_schema,
                        property_value,
                        &format!("{path}.{key}"),
                        out,
                    );
                }
            }
            if schema.get("additionalProperties") == Some(&Json::Bool(false)) {
                for (key, _) in entries {
                    if !properties.iter().any(|(candidate, _)| candidate == key) {
                        out.push(format!("{path}: additional property {key} is forbidden"));
                    }
                }
            }
        }
    }

    // Make unused schema object binding explicit: walking keywords above is
    // intentional; unknown annotation keywords are permitted by JSON Schema.
    let _ = object;
}

fn resolve_ref<'a>(root: &'a Json, reference: &str) -> Option<&'a Json> {
    let path = reference.strip_prefix("#/")?;
    let mut current = root;
    for segment in path.split('/') {
        let key = segment.replace("~1", "/").replace("~0", "~");
        current = current.get(&key)?;
    }
    Some(current)
}

fn type_matches(kind: &str, value: &Json) -> bool {
    match kind {
        "null" => matches!(value, Json::Null),
        "boolean" => matches!(value, Json::Bool(_)),
        "integer" | "number" => matches!(value, Json::Unsigned(_) | Json::Signed(_)),
        "string" => matches!(value, Json::Str(_)),
        "array" => matches!(value, Json::Array(_)),
        "object" => matches!(value, Json::Object(_)),
        _ => false,
    }
}

fn known_pattern(pattern: &str, value: &str) -> Option<bool> {
    let result = match pattern {
        "^[A-Za-z0-9._:-]{1,128}$" => {
            !value.is_empty()
                && value.len() <= 128
                && value.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-')
                })
        }
        "^[a-z0-9-]{1,64}$" => {
            !value.is_empty()
                && value.len() <= 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        }
        "^[0-9]+\\.[0-9]+\\.[0-9]+$" => {
            let parts = value.split('.').collect::<Vec<_>>();
            parts.len() == 3
                && parts
                    .iter()
                    .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        }
        "^(0[xX][0-9A-Fa-f_]+|[0-9_]+)$" => {
            let digits = value
                .strip_prefix("0x")
                .or_else(|| value.strip_prefix("0X"));
            match digits {
                Some(digits) => {
                    !digits.is_empty()
                        && digits
                            .bytes()
                            .all(|byte| byte.is_ascii_hexdigit() || byte == b'_')
                }
                None => {
                    !value.is_empty()
                        && value
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || byte == b'_')
                }
            }
        }
        "^(sha256:)?[0-9a-f]{64}$" => {
            let digest = value.strip_prefix("sha256:").unwrap_or(value);
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        }
        "^[0-9a-f]{64}$" => {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        }
        "^(?:\\S|\\S(?:[^\\u0000-\\u001F\\u007F]*\\S)?)$" => {
            !value.is_empty() && value.trim() == value && !value.chars().any(char::is_control)
        }
        _ => return None,
    };
    Some(result)
}

fn validate_cddl_and_domains(root: &Path, report: &mut ValidationReport) {
    let relative = "spec/model/digest-bodies.cddl";
    let source = match std::fs::read_to_string(root.join(relative)) {
        Ok(source) => source,
        Err(error) => {
            report.problems.push(format!("{relative}: {error}"));
            return;
        }
    };
    if let Err(error) = balanced_cddl(&source) {
        report.problems.push(format!("{relative}: {error}"));
    }
    for domain in Domain::ALL {
        let bytes = domain.as_bytes();
        let text = String::from_utf8_lossy(&bytes[..bytes.len() - 1]);
        if !source.contains(text.as_ref()) {
            report
                .problems
                .push(format!("{relative}: digest domain {text} is not declared"));
        }
        report.domains += 1;
    }
}

fn balanced_cddl(source: &str) -> Result<(), String> {
    let mut stack: Vec<(char, usize)> = Vec::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut comment = false;
    for (offset, character) in source.char_indices() {
        if comment {
            if character == '\n' {
                comment = false;
            }
            continue;
        }
        if quoted {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = false;
            }
            continue;
        }
        match character {
            ';' => comment = true,
            '"' => quoted = true,
            '{' | '[' | '(' => stack.push((character, offset)),
            '}' | ']' | ')' => {
                let expected = match character {
                    '}' => '{',
                    ']' => '[',
                    ')' => '(',
                    _ => unreachable!(),
                };
                match stack.pop() {
                    Some((found, _)) if found == expected => {}
                    Some((found, start)) => {
                        return Err(format!(
                            "delimiter {found} at byte {start} closed by {character} at byte {offset}"
                        ));
                    }
                    None => return Err(format!("unmatched {character} at byte {offset}")),
                }
            }
            _ => {}
        }
    }
    if quoted {
        return Err("unterminated quoted string".into());
    }
    if let Some((delimiter, offset)) = stack.pop() {
        return Err(format!("unclosed {delimiter} at byte {offset}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_published_model_validates_against_the_committed_schema() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let report = validate_repository(&root);
        assert!(report.is_valid(), "{:#?}", report.problems);
        assert_eq!(report.schemas, 2);
        assert_eq!(report.profiles, 2);
        assert_eq!(report.transcripts, 3);
        assert_eq!(report.domains, Domain::ALL.len());
    }
}
