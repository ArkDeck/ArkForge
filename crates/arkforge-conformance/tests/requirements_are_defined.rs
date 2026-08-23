//! Every requirement id a fixture cites must be defined somewhere under
//! `spec/` (a `### AF-…` heading in requirements/model docs, or an `id:` in a
//! state-machine table). A fixture that cites a ghost requirement is a fixture
//! nobody can trace.

use std::collections::BTreeSet;
use std::path::Path;

fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn ids_in(text: &str) -> BTreeSet<String> {
    // AF-<AREA>-<NNN>, AF-<AREA>-<L>-<NNN>, AF-ENG-T-<NNN>
    let mut out = BTreeSet::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 3 <= bytes.len() {
        if &bytes[i..i + 3] == b"AF-" {
            let mut j = i + 3;
            while j < bytes.len()
                && (bytes[j].is_ascii_uppercase() || bytes[j].is_ascii_digit() || bytes[j] == b'-')
            {
                j += 1;
            }
            let candidate = &text[i..j];
            let candidate = candidate.trim_end_matches('-');
            if candidate
                .rsplit('-')
                .next()
                .is_some_and(|tail| tail.len() == 3 && tail.chars().all(|c| c.is_ascii_digit()))
            {
                out.insert(candidate.to_string());
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

#[test]
fn fixture_requirements_are_defined_in_the_spec() {
    let spec = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("spec");
    let mut defined = BTreeSet::new();
    let mut files = Vec::new();
    walk(&spec, &mut files);
    for file in &files {
        let relative = file
            .strip_prefix(&spec)
            .unwrap()
            .to_string_lossy()
            .to_string();
        if relative.starts_with("conformance/") || relative.starts_with("mappings/") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        for line in text.lines() {
            let heading = line.strip_prefix("### ").map(|rest| rest.to_string());
            let id_line = line
                .trim_start()
                .strip_prefix("id: ")
                .map(|rest| rest.to_string());
            if let Some(candidate) = heading.or(id_line) {
                defined.extend(ids_in(&candidate));
            }
        }
    }
    assert!(!defined.is_empty());

    let mut referenced = BTreeSet::new();
    for file in &files {
        if file.file_name().is_some_and(|n| n == "case.json") {
            let text = std::fs::read_to_string(file).unwrap();
            // the "requirements" array is the only place AF- ids appear in a case
            if let Some(start) = text.find("\"requirements\"") {
                let end = text[start..]
                    .find(']')
                    .map(|e| start + e)
                    .unwrap_or(text.len());
                referenced.extend(ids_in(&text[start..end]));
            }
        }
    }
    let missing: Vec<_> = referenced.difference(&defined).cloned().collect();
    assert!(
        missing.is_empty(),
        "fixtures cite requirement ids the spec never defines: {missing:?}"
    );
}
