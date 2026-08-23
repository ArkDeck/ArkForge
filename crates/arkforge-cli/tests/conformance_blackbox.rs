//! The committed CLI conformance vectors are process contracts, not snapshots
//! that merely exist on disk. Execute each one against the built binary.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture(case: &str, file: &str) -> Vec<u8> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("spec")
        .join("conformance")
        .join("v1")
        .join("cli")
        .join(case);
    std::fs::read(root.join(file)).unwrap_or_default()
}

fn run(case: &str, args: &[&str], expected_code: i32) {
    let runtime: PathBuf = std::env::temp_dir().join(format!(
        "arkforge-cli-conformance-{}-{}",
        std::process::id(),
        case
    ));
    let output = Command::new(env!("CARGO_BIN_EXE_arkforge"))
        .args(args)
        .env("ARKFORGE_RUNTIME_DIR", &runtime)
        .output()
        .expect("run arkforge CLI");
    assert_eq!(output.status.code(), Some(expected_code), "{case}");
    assert_eq!(output.stdout, fixture(case, "stdout.txt"), "{case} stdout");
    assert_eq!(output.stderr, fixture(case, "stderr.txt"), "{case} stderr");
}

#[test]
fn committed_cli_vectors_match_the_real_process() {
    run("AF-CONF-CLI-001", &["--version"], 0);
    run(
        "AF-CONF-CLI-002",
        &["help", "cancel", "--format", "json"],
        0,
    );
    run("AF-CONF-CLI-003", &["help", "cancel", "--format", "xml"], 2);
    run("AF-CONF-CLI-004", &["--output", "json", "frobnicate"], 2);
}
