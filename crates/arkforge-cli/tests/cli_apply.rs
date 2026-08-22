//! Process-level contract for the promoted consent, follow, and stop verbs
//! (WF-AC-13..15).

use std::path::PathBuf;
use std::process::{Command, Output};

struct TempRuntime {
    root: PathBuf,
}

impl TempRuntime {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("arkforge-cli-apply-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
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

fn help(path: &[&str]) -> String {
    let mut arguments = vec!["help"];
    arguments.extend(path.iter().copied());
    arguments.extend(["--format", "json"]);
    let output = Command::new(env!("CARGO_BIN_EXE_arkforge"))
        .args(&arguments)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    stdout(&output)
}

#[test]
fn a_rescue_plan_is_refused_before_any_authority_store_is_read() {
    let runtime = TempRuntime::new("domain");
    let refused = runtime.json(&[
        "apply",
        "--plan",
        &format!("rescue-plan:{}", "0".repeat(64)),
        "--expect-plan-sha256",
        &"0".repeat(64),
        "--ack",
        "rescue:native-rockusb",
    ]);
    // Exit 2, not 5: the id is refused on its shape, so no runtime lookup ever
    // happens and "no runtime is listening" is not the answer.
    assert_eq!(refused.status.code(), Some(2), "{refused:?}");
    let document = stdout(&refused);
    assert!(document.contains("\"code\":\"RESCUE_PLAN_DOMAIN\""));
    assert!(document.contains("arkforge rescue apply"), "{document}");
}

#[test]
fn apply_keeps_every_gate_the_flash_leaf_had() {
    let runtime = TempRuntime::new("gates");
    let plan = "PLAN-EXAMPLE";
    let digest = "0".repeat(64);

    // The acknowledgement set is not optional and the digest is not inferable.
    for arguments in [
        vec!["apply", "--plan", plan, "--expect-plan-sha256", &digest],
        vec!["apply", "--plan", plan, "--ack", "data-loss:userdata"],
        vec![
            "apply",
            "--expect-plan-sha256",
            &digest,
            "--ack",
            "data-loss:userdata",
        ],
    ] {
        let refused = runtime.json(&arguments);
        assert_eq!(
            refused.status.code(),
            Some(2),
            "{arguments:?} -> {refused:?}"
        );
        assert!(stdout(&refused).contains("\"code\":\"INVALID_ARGUMENT\""));
    }

    // No broad consent flag was introduced with the promotion.
    let contract = help(&["apply"]);
    assert!(!contract.contains("\"--yes\""));
    assert!(!contract.contains("\"--force\""));
    assert!(contract.contains("\"name\":\"--ack\",\"type\":\"acknowledgement-token\",\"required\":true,\"repeatable\":true"));
    assert!(contract.contains(
        "{\"kind\":\"exactAcknowledgementSet\",\"plan\":\"--plan\",\"digest\":\"--expect-plan-sha256\",\"tokens\":\"--ack\"}"
    ));
    assert!(contract.contains("\"effect\":\"destructive\""));
    // A recovery plan is applied through the same verb.
    let recovery = help(&["job", "recover"]);
    assert!(recovery.contains("apply_command"), "{recovery}");
    assert!(recovery.contains("recovery:supersedes-job"), "{recovery}");
}

#[test]
fn watch_and_cancel_are_top_level_with_their_semantics_unchanged() {
    let runtime = TempRuntime::new("follow");

    // A bare watch resolves a default job, so it needs a runtime to ask.
    let refused = runtime.json(&["watch"]);
    assert_eq!(refused.status.code(), Some(5), "{refused:?}");
    assert!(stdout(&refused).contains("\"code\":\"DAEMON_UNAVAILABLE\""));

    let contract = help(&["watch"]);
    assert!(contract.contains("\"name\":\"--job\",\"type\":\"job-id\",\"required\":false"));
    assert!(contract.contains("arkforge.job-watch/v1"));

    // Cancel keeps its optimistic cursor as a required input.
    let missing = runtime.json(&["cancel", "--job", "JOB-EXAMPLE"]);
    assert_eq!(missing.status.code(), Some(2), "{missing:?}");
    assert!(stdout(&missing).contains("\"code\":\"INVALID_ARGUMENT\""));
    let contract = help(&["cancel"]);
    assert!(
        contract.contains("\"name\":\"--expect-sequence\",\"type\":\"uint64\",\"required\":true")
    );
    assert!(contract.contains("\"effect\":\"mutating-control\""));
}

#[test]
fn the_sealed_plan_points_at_the_promoted_verb() {
    // The plan document's own apply command must name the command that exists.
    let contract = help(&["flash", "plan"]);
    assert!(contract.contains("apply_command"), "{contract}");
    let index = help(&["--all"]);
    assert!(index.contains("\"command\":\"apply\""));
    assert!(index.contains("\"command\":\"watch\""));
    assert!(index.contains("\"command\":\"cancel\""));
    assert!(!index.contains("arkforge flash apply"));
    assert!(!index.contains("arkforge job watch"));
    assert!(!index.contains("arkforge job cancel"));
}
