//! `arkforge-conformance generate [DIR]` writes the fixtures;
//! `arkforge-conformance check [DIR]` reports drift and exits non-zero on any;
//! `arkforge-conformance validate [REPO]` validates published model instances.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("help");
    let root: PathBuf = args
        .get(1)
        .map(PathBuf::from)
        .unwrap_or_else(arkforge_conformance::committed_root);
    match command {
        "generate" => {
            let tree = arkforge_conformance::generate();
            if let Err(error) = tree.write_to(&root) {
                eprintln!("write failed: {error}");
                return ExitCode::FAILURE;
            }
            println!("wrote {} files to {}", tree.len(), root.display());
            ExitCode::SUCCESS
        }
        "check" => {
            let tree = arkforge_conformance::generate();
            let problems = tree.diff_against(&root);
            if problems.is_empty() {
                println!(
                    "{} fixture files under {} are current",
                    tree.len(),
                    root.display()
                );
                ExitCode::SUCCESS
            } else {
                for problem in &problems {
                    eprintln!("{problem}");
                }
                eprintln!(
                    "{} problem(s). Regenerate with `cargo run -p arkforge-conformance -- generate` \
                     and review the diff as a spec change.",
                    problems.len()
                );
                ExitCode::FAILURE
            }
        }
        "validate" => {
            let repository = args.get(1).map(PathBuf::from).unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("..")
                    .join("..")
            });
            let report = arkforge_conformance::schema::validate_repository(&repository);
            if report.is_valid() {
                println!(
                    "validated {} schemas, {} profiles, {} transcripts and {} digest domains",
                    report.schemas, report.profiles, report.transcripts, report.domains
                );
                ExitCode::SUCCESS
            } else {
                for problem in report.problems {
                    eprintln!("{problem}");
                }
                ExitCode::FAILURE
            }
        }
        _ => {
            eprintln!("usage: arkforge-conformance (generate|check) [DIR] | validate [REPO]");
            ExitCode::FAILURE
        }
    }
}
