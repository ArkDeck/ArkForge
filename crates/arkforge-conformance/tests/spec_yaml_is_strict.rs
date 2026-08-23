//! Every YAML file under `spec/` must be readable by the strict YAML subset
//! reader that loads a DeviceProfile (spec/AUTHORING.md §4). A spec table that
//! needs anchors, flow mappings or multi-line scalars would be a table the
//! reference loader cannot read.

use std::path::{Path, PathBuf};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir)
        .expect("spec directory exists")
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

#[test]
fn published_profiles_and_transcripts_still_load() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    for profile in ["profiles/dayu200.yaml", "profiles/dayu600.yaml"] {
        let source = std::fs::read_to_string(root.join(profile)).unwrap();
        arkforge_core::profile::load(&source).unwrap_or_else(|e| panic!("{profile}: {e}"));
    }
    for transcript in [
        "transcripts/dayu200-gj4-ecamp-96effff15.yaml",
        "transcripts/dayu200-gj4-ecamp-31e041bc.yaml",
        "transcripts/dayu600-research-synthetic.yaml",
    ] {
        let source = std::fs::read_to_string(root.join(transcript)).unwrap();
        arkforge_transport::transcript::parse(&source)
            .unwrap_or_else(|e| panic!("{transcript}: {e}"));
    }
}
