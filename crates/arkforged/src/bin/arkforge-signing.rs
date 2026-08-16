//! `arkforge-signing` — read a Mach-O binary's signing facts and check them
//! against the macOS packaging contract.
//!
//! Read-only and offline (architecture.md 15.1). It exists so the question
//! "will `arkforged` accept this tool?" can be asked before starting a daemon,
//! and so the packager can check its own output with the same code the daemon
//! uses rather than with a second, drifting implementation.
//!
//! It is a *second* opinion, not the only one. `codesign` remains the
//! independent check the packager runs first; this reads the same bytes without
//! going through the system's assessment, which is what makes the two worth
//! having together (AFD-0003).

use arkforged::packaging::{self, ContractMode};
use std::path::PathBuf;

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match run(&arguments) {
        Ok(()) => {}
        Err(message) => {
            eprintln!("arkforge-signing: {message}");
            std::process::exit(1);
        }
    }
}

fn usage() -> String {
    concat!(
        "usage: arkforge-signing <binary> [--release]\n",
        "\n",
        "  <binary>   a Mach-O file, thin or universal\n",
        "  --release  hold it to the shipped signing shape as well as the empty\n",
        "             entitlement dictionary, which is required either way\n",
        "\n",
        "Exit status is 0 when the binary meets the contract in the mode asked for.\n",
        "Nothing is written, and no Gatekeeper assessment is performed.\n"
    )
    .to_string()
}

fn run(arguments: &[String]) -> Result<(), String> {
    let mut path: Option<PathBuf> = None;
    let mut mode = ContractMode::Development;
    for argument in arguments {
        match argument.as_str() {
            "--release" => mode = ContractMode::Release,
            "--help" | "-h" => {
                print!("{}", usage());
                return Ok(());
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown argument {other:?}\n\n{}", usage()))
            }
            other => path = Some(PathBuf::from(other)),
        }
    }
    let path = path.ok_or_else(usage)?;

    let code = packaging::read_file(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    println!("{}", path.display());
    println!("  {}", code.summary());

    let violations = code.violations(mode);
    if violations.is_empty() {
        println!("  {mode:?}: meets the contract");
        return Ok(());
    }
    for violation in &violations {
        println!("  {}: {violation}", violation.code());
    }
    Err(format!(
        "{} does not meet the contract in {mode:?} mode ({})",
        path.display(),
        packaging::CONTRACT_DOC
    ))
}
