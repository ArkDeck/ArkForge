//! Process-level contract for the aggregate `status` document and the
//! whole-tree help index (WF-AC-01..03).

use arkforge_artifact::fixture;
use std::path::PathBuf;
use std::process::{Command, Output};

struct TempRuntime {
    root: PathBuf,
}

impl TempRuntime {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("arkforge-cli-status-{}-{name}", std::process::id()));
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

    fn human(&self, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_arkforge"))
            .arg("--runtime-dir")
            .arg(&self.root)
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

fn offline(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_arkforge"))
        .args(arguments)
        .output()
        .expect("canonical arkforge CLI should start")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

#[test]
fn status_reports_unknown_empty_and_partial_sections_without_starting_a_runtime() {
    let runtime = TempRuntime::new("sections");

    let snapshot = runtime.json(&["status"]);
    assert!(snapshot.status.success(), "{snapshot:?}");
    assert!(snapshot.stderr.is_empty(), "{snapshot:?}");
    let document = stdout(&snapshot);
    assert!(document.contains("\"schema\":\"arkforge.status/v1\""));
    assert!(document.contains("\"captured_at_epoch_ms\":"));
    assert!(document.contains("\"host\":{\"platform_supported\":"));
    assert!(document.contains("\"runtime\":{\"running\":false}"));

    // A section that could not be observed reports null items and a typed
    // reason. It must never be rendered as the empty list that a completed
    // enumeration of zero earns.
    let unobservable = "{\"available\":false,\"complete\":false,\"reason\":\"RUNTIME_NOT_RUNNING\",\"items\":null}";
    assert!(
        document.contains(&format!("\"devices\":{unobservable}")),
        "{document}"
    );
    assert!(
        document.contains(&format!("\"jobs\":{unobservable}")),
        "{document}"
    );
    // The content store is host-local, so it stays a complete answer even with
    // no runtime paired.
    assert!(
        document.contains(
            "\"artifacts\":{\"available\":true,\"complete\":true,\"reason\":null,\"items\":[]}"
        ),
        "{document}"
    );
    assert!(document.contains("\"complete\":false"), "{document}");
    assert!(
        document.contains(
            "{\"code\":\"RUNTIME_NOT_RUNNING\",\"remediation\":\"arkforge daemon start\"}"
        ),
        "{document}"
    );
    assert!(document.contains("\"next_commands\":[\"arkforge daemon start\"]"));
    assert!(!document.contains("\u{1b}["));
    assert!(!document.contains(runtime.root.to_str().unwrap()));

    // Reading the snapshot must not bring a runtime, socket, or store into
    // existence: asking the question may not change the answer.
    let entries = std::fs::read_dir(&runtime.root).unwrap().count();
    assert_eq!(entries, 0, "status created state in the runtime directory");

    let bare = runtime.json(&[]);
    assert!(bare.status.success(), "{bare:?}");
    assert!(stdout(&bare).contains("\"schema\":\"arkforge.status/v1\""));

    let readable = runtime.human(&["status"]);
    assert!(readable.status.success(), "{readable:?}");
    let readable = stdout(&readable);
    assert!(readable.contains("runtime"));
    assert!(readable.contains("running:            false"));
    assert!(readable.contains("not observable (RUNTIME_NOT_RUNNING)"));
    assert!(readable.contains("Next: arkforge daemon start"));
}

#[test]
fn status_reports_a_stored_artifact_as_a_completed_enumeration() {
    let runtime = TempRuntime::new("artifacts");
    let input = runtime.root.join("firmware.tar.gz");
    std::fs::write(&input, fixture::dayu200_archive()).unwrap();
    let imported = runtime.json(&["artifact", "import", "--file", input.to_str().unwrap()]);
    assert!(imported.status.success(), "{imported:?}");

    let document = stdout(&runtime.json(&["status"]));
    assert!(document.contains("\"artifacts\":{\"available\":true,\"complete\":true,\"reason\":null,\"items\":[{\"artifact_id\":\""));
    assert!(document.contains("\"size_bytes\":"));
    // Devices and jobs stay unknown; one readable section does not make the
    // whole snapshot complete.
    assert!(document.contains("\"complete\":false"));
}

#[test]
fn help_all_is_the_whole_tree_and_agrees_with_every_per_path_leaf() {
    let index = offline(&["help", "--all", "--format", "json"]);
    assert!(index.status.success(), "{index:?}");
    let index = stdout(&index);
    assert!(index.starts_with("{\"schema\":\"arkforge.command-help-index/v1\","));

    let count: usize = index
        .split_once("\"command_count\":")
        .unwrap()
        .1
        .split_once(',')
        .unwrap()
        .0
        .parse()
        .unwrap();
    let leaves = index
        .matches("{\"schema\":\"arkforge.command-help/v1\",")
        .count();
    assert_eq!(count, leaves, "command_count must equal the array length");
    assert_eq!(
        index.matches("\"runtime_effect\":").count(),
        leaves,
        "every leaf declares runtime_effect"
    );
    assert_eq!(
        index.matches("\"facts_projections\":").count(),
        leaves,
        "every leaf declares facts_projections"
    );
    assert!(index.contains("\"command\":\"status\""));
    assert!(index.contains("\"runtime_effect\":\"may-start-service\""));

    // Structured help without a path is the same whole-tree answer, however it
    // was asked for.
    for spelling in [
        vec!["--output", "json", "help"],
        vec!["--output", "json", "--help"],
    ] {
        let implied = offline(&spelling);
        assert!(implied.status.success(), "{implied:?}");
        assert_eq!(stdout(&implied), index, "{spelling:?}");
    }

    // Every leaf must be byte-identical to its own per-path query, or an Agent
    // reading the index would be reading a second, drifting contract.
    for path in [
        vec!["apply"],
        vec!["status"],
        vec!["job", "recovery", "plan"],
        vec!["rescue", "read"],
    ] {
        let mut arguments = vec!["help"];
        arguments.extend(path.iter().copied());
        arguments.extend(["--format", "json"]);
        let leaf = offline(&arguments);
        assert!(leaf.status.success(), "{leaf:?}");
        let leaf = stdout(&leaf);
        assert!(
            index.contains(leaf.trim_end()),
            "index is missing the exact leaf for {path:?}"
        );
    }

    // --all describes the tree, so a topic path alongside it is a contradiction.
    let contradiction = offline(&["help", "--all", "flash", "--format", "json"]);
    assert_eq!(contradiction.status.code(), Some(2));
}

#[test]
fn removed_leaves_are_absent_from_the_parser_help_and_completion() {
    for removed in [
        vec!["doctor"],
        vec!["daemon", "status"],
        vec!["device", "show"],
        vec!["device", "probe"],
        vec!["artifact", "inspect"],
        vec!["job", "recovery", "guide"],
        vec!["flash", "apply"],
        vec!["job", "watch"],
        vec!["job", "cancel"],
    ] {
        let invoked = offline(&removed);
        assert_eq!(
            invoked.status.code(),
            Some(2),
            "{removed:?} must not be a command"
        );

        let mut topic = vec!["help"];
        topic.extend(removed.iter().copied());
        let described = offline(&topic);
        assert_eq!(
            described.status.code(),
            Some(2),
            "{removed:?} must not be a help topic"
        );
    }

    let index = stdout(&offline(&["help", "--all", "--format", "json"]));
    for command in [
        "doctor",
        "daemon status",
        "device show",
        "device probe",
        "artifact inspect",
        "job recovery guide",
        "flash assess",
        "flash apply",
        "job watch",
        "job cancel",
    ] {
        assert!(
            !index.contains(&format!("\"command\":\"{command}\"")),
            "{command} is still in the help index"
        );
    }
    assert!(!index.contains("arkforge.doctor/v1"));
    assert!(!index.contains("arkforge.device-probe/v1"));
    assert!(!index.contains("arkforge.device-observation/v1"));
    assert!(!index.contains("arkforge.recovery-guide/v1"));

    for shell in ["bash", "zsh", "fish"] {
        let script = stdout(&offline(&["completion", "--shell", shell]));
        for word in ["doctor", "probe", "guide"] {
            assert!(
                !script.contains(word),
                "{shell} completion still offers {word}"
            );
        }
        assert!(
            script.contains("status"),
            "{shell} completion must offer status"
        );
        assert!(
            script.contains("--deep"),
            "{shell} completion must offer --deep"
        );
    }
}

#[test]
fn device_list_absorbs_show_and_probe_behind_one_surface() {
    // Without a runtime every device query is a typed refusal, but the option
    // shape that replaced two commands must already be the published one.
    let index = stdout(&offline(&["help", "device", "list", "--format", "json"]));
    assert!(index.contains("\"name\":\"--device\""));
    assert!(index.contains("\"name\":\"--deep\",\"type\":\"boolean\""));
    assert!(index.contains("identification block"));

    let runtime = TempRuntime::new("device");
    let refused = runtime.json(&["device", "list", "--deep"]);
    assert_eq!(refused.status.code(), Some(5), "{refused:?}");
    assert!(stdout(&refused).contains("\"code\":\"DAEMON_UNAVAILABLE\""));
}
