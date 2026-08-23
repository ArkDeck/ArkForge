//! The fixture tree: every file the generator produces, keyed by its path
//! relative to `spec/conformance/v1`.
//!
//! Generation is pure — no clocks, no randomness, no host paths — so the tree
//! is a function of the reference implementation alone, and "the committed
//! fixtures are current" is a byte comparison.

use crate::json::{Json, hex};
use arkforge_core::digest::sha256;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone)]
pub struct Tree {
    files: BTreeMap<String, Vec<u8>>,
}

/// Metadata every case directory carries in `case.json`.
#[derive(Debug, Clone)]
pub struct Case {
    pub id: String,
    pub suite: &'static str,
    pub title: String,
    /// Requirement IDs (`AF-…`) this case is evidence for.
    pub requirements: Vec<&'static str>,
    /// `encode` | `decode` | `digest` | `verify` | `replay` | `derive` | `table`.
    pub kind: &'static str,
    pub description: String,
    /// Inputs that are small enough to inline. Large/binary inputs go in files
    /// and are referenced from `files`.
    pub input: Json,
    pub expected: Json,
}

impl Tree {
    pub fn new() -> Self {
        Tree::default()
    }

    pub fn put(&mut self, path: impl Into<String>, bytes: impl Into<Vec<u8>>) {
        let path = path.into();
        assert!(
            self.files.insert(path.clone(), bytes.into()).is_none(),
            "duplicate fixture path {path}"
        );
    }

    pub fn put_text(&mut self, path: impl Into<String>, text: impl AsRef<str>) {
        self.put(path, text.as_ref().as_bytes().to_vec());
    }

    /// Writes a case directory: `case.json` plus the given files. Binary files
    /// get a `.hex` twin so a reviewer can read a diff.
    pub fn case(&mut self, case: &Case, files: Vec<(&str, Vec<u8>)>) {
        let dir = format!("{}/{}", case.suite, case.id);
        let mut file_index = Vec::new();
        for (name, bytes) in files {
            let path = format!("{dir}/{name}");
            file_index.push((
                name.to_string(),
                Json::object(vec![
                    ("bytes", Json::Unsigned(bytes.len() as u64)),
                    ("sha256", Json::str(sha256(&bytes).to_hex())),
                ]),
            ));
            if is_binary_name(name) {
                self.put_text(format!("{path}.hex"), wrap_hex(&bytes));
            }
            self.put(path, bytes);
        }
        let mut meta = Json::object(vec![
            ("id", Json::str(&case.id)),
            ("suite", Json::str(case.suite)),
            ("title", Json::str(&case.title)),
            ("kind", Json::str(case.kind)),
            (
                "requirements",
                Json::strs(case.requirements.iter().copied()),
            ),
            ("description", Json::str(&case.description)),
            ("input", case.input.clone()),
            ("expected", case.expected.clone()),
        ]);
        meta.push("files", Json::Object(file_index));
        self.put_text(format!("{dir}/case.json"), meta.to_pretty());
    }

    pub fn files(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.files
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// The suite manifest: every file with its digest, so a port can check it
    /// holds the same fixture set before it trusts a single expected value.
    pub fn manifest(&self, spec_version: &str) -> Json {
        let mut suites: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for (path, bytes) in &self.files {
            let suite = path.split('/').next().unwrap_or("").to_string();
            suites
                .entry(suite)
                .or_default()
                .push((path.clone(), sha256(bytes).to_hex()));
        }
        let mut suite_entries = Vec::new();
        for (suite, files) in suites {
            let cases: std::collections::BTreeSet<String> = files
                .iter()
                .filter_map(|(path, _)| path.split('/').nth(1).map(|s| s.to_string()))
                .collect();
            suite_entries.push((
                suite,
                Json::object(vec![
                    ("cases", Json::strs(cases)),
                    (
                        "files",
                        Json::Object(
                            files
                                .into_iter()
                                .map(|(path, digest)| (path, Json::str(digest)))
                                .collect(),
                        ),
                    ),
                ]),
            ));
        }
        Json::object(vec![
            ("schema", Json::str("arkforge.conformance-manifest/v1")),
            ("specVersion", Json::str(spec_version)),
            (
                "generator",
                Json::str("crates/arkforge-conformance (Rust reference implementation as oracle)"),
            ),
            (
                "authority",
                Json::str(
                    "These bytes are normative. If a fixture and the prose in spec/ disagree, \
                     that is a spec defect to be resolved by a spec revision, never by an \
                     implementation choosing one side silently.",
                ),
            ),
            ("suites", Json::Object(suite_entries)),
        ])
    }

    pub fn write_to(&self, root: &Path) -> std::io::Result<()> {
        for (path, bytes) in &self.files {
            let full: PathBuf = root.join(path);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(full, bytes)?;
        }
        Ok(())
    }

    /// Paths whose committed bytes differ from, are missing from, or are absent
    /// in the generated tree.
    pub fn diff_against(&self, root: &Path) -> Vec<String> {
        let mut problems = Vec::new();
        for (path, bytes) in &self.files {
            match std::fs::read(root.join(path)) {
                Ok(on_disk) if &on_disk == bytes => {}
                Ok(_) => problems.push(format!("differs: {path}")),
                Err(_) => problems.push(format!("missing: {path}")),
            }
        }
        let mut on_disk = Vec::new();
        collect_files(root, root, &mut on_disk);
        for path in on_disk {
            if !self.files.contains_key(&path) {
                problems.push(format!("stale (not generated): {path}"));
            }
        }
        problems
    }
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, &path, out);
        } else if let Ok(relative) = path.strip_prefix(root) {
            out.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}

fn is_binary_name(name: &str) -> bool {
    name.ends_with(".cbor") || name.ends_with(".bin") || name.ends_with(".pb")
}

/// Hex, 32 bytes per line, so diffs point at an offset rather than a blob.
fn wrap_hex(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(32) {
        out.push_str(&hex(chunk));
        out.push('\n');
    }
    out
}
