//! Every normative/draft requirement heading must point at a Rust symbol, and
//! every mapping entry must name a real requirement. Ranges are expanded so a
//! new AF-* heading cannot hide between two broad-looking entries.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("md"))
        .collect::<Vec<_>>();
    out.sort();
    out
}

fn heading_id(line: &str) -> Option<String> {
    let text = line.strip_prefix("### AF-")?;
    let tail = text
        .split_once(' ')
        .map(|(id, _)| id)
        .unwrap_or(text)
        .trim_end_matches(|character: char| !character.is_ascii_alphanumeric());
    Some(format!("AF-{tail}"))
}

fn expand(expression: &str) -> Vec<String> {
    expression
        .split(',')
        .flat_map(|part| part.split_whitespace())
        .filter(|part| part.starts_with("AF-"))
        .flat_map(|part| {
            let part = part
                .trim_matches(|character: char| matches!(character, '[' | ']' | '"' | '\'' | ';'));
            let Some((first, last)) = part.split_once("..") else {
                return vec![part.to_string()];
            };
            let Some(prefix) = first.get(..first.len().saturating_sub(3)) else {
                return vec![part.to_string()];
            };
            let Some(start) = first.get(first.len().saturating_sub(3)..) else {
                return vec![part.to_string()];
            };
            let Ok(start) = start.parse::<u32>() else {
                return vec![part.to_string()];
            };
            let Ok(end) = last.parse::<u32>() else {
                return vec![part.to_string()];
            };
            (start..=end)
                .map(|number| format!("{prefix}{number:03}"))
                .collect()
        })
        .collect()
}

#[test]
fn rust_mapping_covers_every_requirement_exactly_by_id() {
    let spec = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec");
    let mut defined = BTreeSet::new();
    for file in files(&spec.join("requirements")) {
        let source = std::fs::read_to_string(file).unwrap();
        defined.extend(source.lines().filter_map(heading_id));
    }
    // The strict-YAML grammar is both a model definition and a normative
    // requirement source, so it participates in the same closed trace set.
    let strict_yaml = std::fs::read_to_string(spec.join("model/strict-yaml.md")).unwrap();
    defined.extend(strict_yaml.lines().filter_map(heading_id));
    assert!(!defined.is_empty());

    let mapping = std::fs::read_to_string(spec.join("mappings/rust.yaml")).unwrap();
    let mut mapped = BTreeSet::new();
    for line in mapping.lines() {
        if let Some(expression) = line.trim_start().strip_prefix("- req:") {
            mapped.extend(expand(expression.trim()));
        }
    }

    let missing = defined.difference(&mapped).cloned().collect::<Vec<_>>();
    let ghosts = mapped.difference(&defined).cloned().collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "requirements absent from rust.yaml: {missing:?}"
    );
    assert!(
        ghosts.is_empty(),
        "rust.yaml maps undefined requirements: {ghosts:?}"
    );
}

#[test]
fn range_expansion_preserves_multi_segment_prefixes() {
    assert_eq!(
        expand("AF-CRASH-002, AF-CRASH-R-001..003"),
        [
            "AF-CRASH-002",
            "AF-CRASH-R-001",
            "AF-CRASH-R-002",
            "AF-CRASH-R-003",
        ]
    );
}
