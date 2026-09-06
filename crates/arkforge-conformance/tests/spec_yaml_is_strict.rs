//! Every YAML file under `spec/` must be readable by the strict YAML subset
//! reader that loads a DeviceProfile (spec/AUTHORING.md §4). A spec table that
//! needs anchors, flow mappings or multi-line scalars would be a table the
//! reference loader cannot read.

use std::path::{Path, PathBuf};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
        .flatten()
    {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "conformance") {
                continue; // fixtures are generated, not hand-written YAML
            }
            collect(&path, out);
        } else if path
            .extension()
            .is_some_and(|ext| ext == "yaml" || ext == "yml")
        {
            out.push(path);
        }
    }
}

#[test]
fn every_spec_yaml_file_parses_with_the_strict_subset() {
    let spec = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("spec");
    let mut files = Vec::new();
    collect(&spec, &mut files);
    assert!(!files.is_empty(), "no YAML under {}", spec.display());
    let mut failures = Vec::new();
    for file in &files {
        let source = std::fs::read_to_string(file).unwrap();
        if let Err(error) = arkforge_core::yaml::parse(&source) {
            failures.push(format!("{}: {error}", file.display()));
        }
    }
    assert!(failures.is_empty(), "\n{}\n", failures.join("\n"));
}

/// Every published profile and transcript must still load, and the set is read
/// from the directories rather than listed here: a file added to `profiles/` or
/// `transcripts/` that the reference loader cannot read is exactly the drift
/// this check exists to catch, and a hand-maintained list silently misses it.
#[test]
fn published_profiles_and_transcripts_still_load() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");

    let mut profiles = Vec::new();
    collect(&root.join("profiles"), &mut profiles);
    assert!(profiles.len() >= 2, "published profiles went missing");
    for profile in &profiles {
        let source = std::fs::read_to_string(profile).unwrap();
        arkforge_core::profile::load(&source)
            .unwrap_or_else(|e| panic!("{}: {e}", profile.display()));
    }

    let mut transcripts = Vec::new();
    collect(&root.join("transcripts"), &mut transcripts);
    assert!(transcripts.len() >= 3, "published transcripts went missing");
    for transcript in &transcripts {
        let source = std::fs::read_to_string(transcript).unwrap();
        arkforge_transport::transcript::parse(&source)
            .unwrap_or_else(|e| panic!("{}: {e}", transcript.display()));
    }
}
