//! Process-level contract for the composite staging surface (WF-AC-07..12).
//!
//! No runtime is paired in these tests, so every path that needs a device stops
//! at a typed refusal. That is the point: the refusal must already carry what
//! the call established before it stopped.

use arkforge_artifact::fixture;
use std::path::PathBuf;
use std::process::{Command, Output};

struct TempRuntime {
    root: PathBuf,
}

impl TempRuntime {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("arkforge-cli-flash-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn firmware(&self) -> String {
        let input = self.root.join("firmware.tar.gz");
        std::fs::write(&input, fixture::dayu200_archive()).unwrap();
        input.to_str().unwrap().to_string()
    }

    fn json(&self, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_arkforge"))
            .arg("--runtime-dir")
            .arg(&self.root)
            .arg("--output")
            .arg("json")
            // These tests assert what happens with no runtime, so they must say
            // they do not want one started for them.
            .arg("--no-auto-start")
            .args(arguments)
            .output()
            .expect("canonical arkforge CLI should start")
    }
}

impl Drop for TempRuntime {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

#[test]
fn firmware_must_be_named_and_is_never_taken_positionally() {
    let runtime = TempRuntime::new("content");

    let missing = runtime.json(&["flash", "plan"]);
    assert_eq!(missing.status.code(), Some(2), "{missing:?}");
    assert!(stdout(&missing).contains("\"code\":\"CONTENT_REQUIRED\""));

    // A structured caller never gets a guessed positional argument.
    let positional = runtime.json(&["flash", "plan", &runtime.firmware()]);
    assert_eq!(positional.status.code(), Some(2), "{positional:?}");
    assert!(stdout(&positional).contains("\"code\":\"INVALID_ARGUMENT\""));
}

#[test]
fn a_plan_refuses_before_importing_anything_when_it_cannot_reach_a_runtime() {
    let runtime = TempRuntime::new("facts");
    let firmware = runtime.firmware();

    // Runtime first, content second (design.md 3.2): a call that cannot reach a
    // device must not spend a 200 MiB import discovering that.
    let refused = runtime.json(&["flash", "plan", "--file", &firmware]);
    assert_eq!(refused.status.code(), Some(5), "{refused:?}");
    let document = stdout(&refused);
    assert!(document.contains("\"code\":\"DAEMON_UNAVAILABLE\""));
    let listed = stdout(&runtime.json(&["artifact", "list"]));
    assert!(listed.contains("\"artifacts\":[]"), "{listed}");
}

#[test]
fn exact_and_searching_selectors_are_mutually_exclusive() {
    let runtime = TempRuntime::new("selectors");
    let firmware = runtime.firmware();

    for pair in [
        vec!["--file", "PLACEHOLDER", "--artifact", &"0".repeat(64)],
        vec![
            "--file",
            "PLACEHOLDER",
            "--device",
            "OBS-1",
            "--target",
            "OBS",
        ],
    ] {
        let arguments = std::iter::once("flash")
            .chain(std::iter::once("plan"))
            .chain(pair.iter().map(|value| {
                if *value == "PLACEHOLDER" {
                    firmware.as_str()
                } else {
                    value
                }
            }))
            .collect::<Vec<_>>();
        let refused = runtime.json(&arguments);
        assert_eq!(
            refused.status.code(),
            Some(2),
            "{arguments:?} -> {refused:?}"
        );
        assert!(stdout(&refused).contains("\"code\":\"INVALID_ARGUMENT\""));
    }
}

#[test]
fn the_help_contract_publishes_the_composite_shape_and_its_refusal_facts() {
    let help = Command::new(env!("CARGO_BIN_EXE_arkforge"))
        .args(["help", "flash", "plan", "--format", "json"])
        .output()
        .unwrap();
    assert!(help.status.success(), "{help:?}");
    let help = stdout(&help);
    assert!(help.contains("arkforge.flash-plan/v2"));
    // The declared refusal projections and their caps.
    assert!(help.contains(
        "\"facts_projections\":[{\"name\":\"flash_plan\",\"schema\":\"arkforge.flash-plan/v2\",\"max_items\":1},{\"name\":\"device_candidates\",\"schema\":\"arkforge.resolved-device/v1\",\"max_items\":32}]"
    ), "{help}");
    // The mutual exclusions are in the typed tree, not only in prose.
    assert!(help.contains("{\"kind\":\"conflicts\",\"left\":\"--artifact\",\"right\":\"--file\"}"));
    assert!(help.contains("{\"kind\":\"conflicts\",\"left\":\"--device\",\"right\":\"--target\"}"));
    assert!(help.contains("{\"kind\":\"conflicts\",\"left\":\"--target\",\"right\":\"--device\"}"));
    // Neither content option is required on its own; one of them is.
    assert!(help.contains("\"name\":\"--file\",\"type\":\"path\",\"required\":false"));
    assert!(help.contains(
        "{\"kind\":\"exactlyOneOf\",\"options\":[\"--file\",\"--artifact\"],\"required\":true}"
    ));
    assert!(help.contains("\"name\":\"--assess-only\",\"type\":\"boolean\""));

    let index = Command::new(env!("CARGO_BIN_EXE_arkforge"))
        .args(["help", "--all", "--format", "json"])
        .output()
        .unwrap();
    let index = stdout(&index);
    assert!(!index.contains("\"command\":\"flash assess\""));
    assert!(!index.contains("arkforge.flash-assessment/v1"));
}

#[test]
fn the_absorbed_assess_leaf_is_gone_with_no_alias() {
    let runtime = TempRuntime::new("removed");
    let removed = runtime.json(&[
        "flash",
        "assess",
        "--artifact",
        &"0".repeat(64),
        "--profile",
        "org.openharmony.dayu200@1.0.0",
        "--device",
        "OBS-1",
        "--intent",
        "full-restore",
    ]);
    assert_eq!(removed.status.code(), Some(2), "{removed:?}");
    assert!(stdout(&removed).contains("\"code\":\"INVALID_ARGUMENT\""));
}
