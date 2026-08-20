//! Process-level contract for the canonical signing command.

#![cfg(target_os = "macos")]

use std::process::Command;

#[test]
fn canonical_signing_help_is_agent_discoverable() {
    let output = Command::new(env!("CARGO_BIN_EXE_arkforge"))
        .args(["help", "--format", "json"])
        .output()
        .expect("run canonical CLI help");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"schema_version\":\"arkforge.command-help/v1\""));
    assert!(stdout.contains("\"command\":\"rescue\""));
    assert!(stdout.contains("\"command\":\"signing\""));

    let output = Command::new(env!("CARGO_BIN_EXE_arkforge"))
        .args(["help", "signing", "verify", "--format", "json"])
        .output()
        .expect("run signing help");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"command\":\"signing verify\""));
    assert!(stdout.contains("--mode <development|release>"));
}

#[test]
fn development_verification_returns_stable_json() {
    let output = Command::new(env!("CARGO_BIN_EXE_arkforge"))
        .args([
            "--output",
            "json",
            "signing",
            "verify",
            "--file",
            env!("CARGO_BIN_EXE_arkforged"),
            "--mode",
            "development",
        ])
        .output()
        .expect("verify the test daemon");
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"schema_version\":\"arkforge.signing-verification/v1\""));
    assert!(stdout.contains("\"mode\":\"development\""));
    assert!(stdout.contains("\"compliant\":true"));
    assert!(stdout.contains("\"violations\":[]"));
}

#[test]
fn removed_release_flag_is_not_a_compatibility_alias() {
    let output = Command::new(env!("CARGO_BIN_EXE_arkforge"))
        .args([
            "--output",
            "json",
            "signing",
            "verify",
            "--file",
            env!("CARGO_BIN_EXE_arkforged"),
            "--release",
            "true",
        ])
        .output()
        .expect("run invalid historical syntax");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty(), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("\"schema_version\":\"arkforge.error/v1\""));
    assert!(stderr.contains("\"code\":\"INVALID_ARGUMENT\""));
}
