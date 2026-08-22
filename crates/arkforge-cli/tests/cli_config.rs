//! Process-level contract for reusable bindings and runtime auto-ensure
//! (WF-AC-23..25).

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long any single autostart command may take before this test gives up.
///
/// Generous next to the work — starting a supervisor and a daemon — and tiny
/// next to the workflow ceiling, which is the point: a wedged autostart should
/// fail this test with a reason, not consume the whole Windows job.
const COMMAND_BUDGET: Duration = Duration::from_secs(90);

/// Runs one CLI command under [`COMMAND_BUDGET`], reporting *which* wait failed.
///
/// `Command::output` cannot express this: it waits for exit and for the output
/// pipes to close, and when it hangs it never says which. The two mean very
/// different things once a command starts a background service. A process that
/// never exits is stuck in its own work; a process that exited while its pipes
/// stay open has left a child holding the inherited write end, which on Windows
/// outlives it by default. Naming the difference is the whole value here.
fn output_within(command: &mut Command, label: &str) -> Output {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("canonical arkforge CLI should start");

    let deadline = Instant::now() + COMMAND_BUDGET;
    loop {
        match child.try_wait().expect("observe the command") {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{label}: the command never exited within {COMMAND_BUDGET:?}");
            }
            None => std::thread::sleep(Duration::from_millis(25)),
        }
    }

    // Exited. Collecting the output must not be able to hang either.
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(child.wait_with_output());
    });
    match receiver.recv_timeout(COMMAND_BUDGET) {
        Ok(collected) => collected.expect("collect the command output"),
        Err(_) => panic!(
            "{label}: the command exited but its output pipes never closed, \
             so something it started is still holding them open"
        ),
    }
}

struct TempRuntime {
    root: PathBuf,
}

impl TempRuntime {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("arkforge-cli-config-{}-{name}", std::process::id()));
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
            .arg("--no-auto-start")
            .args(arguments)
            .output()
            .expect("canonical arkforge CLI should start")
    }

    /// A file inside the runtime directory, and the digest the tool reports for
    /// it. The digest comes from the tool rather than from a constant so the
    /// test never asserts against a hash it computed a second way.
    fn pinned_file(&self, name: &str, contents: &str) -> (String, String) {
        let path = self.root.join(name);
        std::fs::write(&path, contents).unwrap();
        let path = std::fs::canonicalize(&path).unwrap();
        let imported = self.json(&["artifact", "import", "--file", path.to_str().unwrap()]);
        assert!(imported.status.success(), "{imported:?}");
        let document = stdout(&imported);
        let digest = document
            .split_once("\"sha256\":\"")
            .unwrap()
            .1
            .split_once('"')
            .unwrap()
            .0
            .to_string();
        (path.to_str().unwrap().to_string(), digest)
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
fn a_binding_names_a_path_and_its_bytes_in_one_transaction() {
    let runtime = TempRuntime::new("bind");
    let (path, digest) = runtime.pinned_file("hdc", "pretend-tool");

    // One half of a pair is not a binding.
    let half = runtime.json(&["config", "set", &format!("hdc.path={path}")]);
    assert_eq!(half.status.code(), Some(2), "{half:?}");

    // A digest that does not match the bytes stores nothing.
    let wrong = runtime.json(&[
        "config",
        "set",
        &format!("hdc.path={path}"),
        &format!("hdc.sha256={}", "0".repeat(64)),
    ]);
    assert_eq!(wrong.status.code(), Some(3), "{wrong:?}");
    assert!(stdout(&wrong).contains("\"code\":\"CONFIG_DIGEST_MISMATCH\""));
    assert!(stdout(&runtime.json(&["config", "show"])).contains("\"bound\":false"));

    // A relative path is never resolved against wherever the command ran.
    let relative = runtime.json(&[
        "config",
        "set",
        "hdc.path=bin/hdc",
        &format!("hdc.sha256={digest}"),
    ]);
    assert_eq!(relative.status.code(), Some(2), "{relative:?}");

    let bound = runtime.json(&[
        "config",
        "set",
        &format!("hdc.path={path}"),
        &format!("hdc.sha256={digest}"),
    ]);
    assert!(bound.status.success(), "{bound:?}");
    let document = stdout(&bound);
    assert!(document.contains("\"schema\":\"arkforge.config/v1\""));
    assert!(document.contains(&format!("\"bound\":true,\"sha256\":\"{digest}\"")));
    // Structured output carries the binding, never where it lives.
    assert!(!document.contains(&path), "{document}");
    assert!(
        !document.contains(runtime.root.to_str().unwrap()),
        "{document}"
    );

    // Clearing takes the path and the digest together.
    let cleared = runtime.json(&["config", "unset", "hdc"]);
    assert!(cleared.status.success(), "{cleared:?}");
    assert!(stdout(&cleared).contains("\"bound\":false,\"sha256\":null"));
}

#[test]
fn profiles_are_added_and_removed_by_digest() {
    let runtime = TempRuntime::new("profiles");
    let (path, digest) = runtime.pinned_file("dev-profile.yaml", "schemaVersion: 1\n");

    let added = runtime.json(&[
        "config",
        "add",
        &format!("profile-file.path={path}"),
        &format!("profile-file.sha256={digest}"),
    ]);
    assert!(added.status.success(), "{added:?}");
    assert!(stdout(&added).contains("\"profile_file_count\":1"));

    // The same bytes are already bound; adding them twice is a conflict.
    let again = runtime.json(&[
        "config",
        "add",
        &format!("profile-file.path={path}"),
        &format!("profile-file.sha256={digest}"),
    ]);
    assert_eq!(again.status.code(), Some(6), "{again:?}");

    // Removal is by digest, so a moved file cannot be unbound by accident.
    let missing = runtime.json(&[
        "config",
        "remove",
        &format!("profile-file.sha256={}", "0".repeat(64)),
    ]);
    assert_eq!(missing.status.code(), Some(5), "{missing:?}");

    let removed = runtime.json(&["config", "remove", &format!("profile-file.sha256={digest}")]);
    assert!(removed.status.success(), "{removed:?}");
    assert!(stdout(&removed).contains("\"profile_file_count\":0"));
}

#[test]
fn a_campaign_is_refused_by_name_rather_than_stored() {
    let runtime = TempRuntime::new("campaign");
    for arguments in [
        vec!["config", "set", "campaign=ACC-2026-01"],
        vec!["config", "add", "campaign.id=ACC-2026-01"],
    ] {
        let refused = runtime.json(&arguments);
        assert_eq!(
            refused.status.code(),
            Some(2),
            "{arguments:?} -> {refused:?}"
        );
        let document = stdout(&refused);
        assert!(document.contains("\"code\":\"CAMPAIGN_NOT_PERSISTABLE\""));
        assert!(document.contains("--hardware-campaign"), "{document}");
    }
    assert!(stdout(&runtime.json(&["config", "show"])).contains("\"campaign_persistable\":false"));
}

#[test]
fn the_stored_configuration_is_owner_only_and_survives_a_refused_write() {
    let runtime = TempRuntime::new("durable");
    let set = runtime.json(&["config", "set", "daemon.require-release-signing=true"]);
    assert!(set.status.success(), "{set:?}");
    assert!(stdout(&set).contains("\"require_release_signing\":true"));

    let stored = runtime.root.join("config");
    assert!(stored.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&stored).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "the configuration is readable by others");
    }

    // A refused command leaves the committed value exactly as it was.
    let refused = runtime.json(&["config", "set", "hdc.path=/nowhere/hdc"]);
    assert_eq!(refused.status.code(), Some(2), "{refused:?}");
    assert!(
        stdout(&runtime.json(&["config", "show"])).contains("\"require_release_signing\":true")
    );
}

#[test]
fn a_command_that_needs_a_runtime_says_so_in_its_published_contract() {
    let describe = |path: &[&str]| {
        let mut arguments = vec!["help"];
        arguments.extend(path.iter().copied());
        arguments.extend(["--format", "json"]);
        let output = Command::new(env!("CARGO_BIN_EXE_arkforge"))
            .args(&arguments)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        stdout(&output)
    };
    for path in [
        vec!["device", "list"],
        vec!["flash", "plan"],
        vec!["apply"],
        vec!["watch"],
        vec!["cancel"],
        vec!["job", "list"],
    ] {
        assert!(
            describe(&path).contains("\"runtime_effect\":\"may-start-service\""),
            "{path:?} may start a runtime and must say so"
        );
    }
    // A snapshot must be able to report "nothing is running" without changing
    // that answer, and offline commands touch nothing at all.
    for path in [
        vec!["status"],
        vec!["config", "show"],
        vec!["signing", "verify"],
    ] {
        assert!(
            describe(&path).contains("\"runtime_effect\":\"none\""),
            "{path:?} must not start a runtime"
        );
    }
}

#[test]
fn auto_start_is_opt_out_and_disclosed() {
    let runtime = TempRuntime::new("autostart");

    // Opted out: the previous refusal is exactly what comes back.
    let refused = runtime.json(&["device", "list"]);
    assert_eq!(refused.status.code(), Some(5), "{refused:?}");
    assert!(stdout(&refused).contains("\"code\":\"DAEMON_UNAVAILABLE\""));

    // The sibling mechanics daemon is only built by a workspace-wide build, so
    // the live half of this contract runs where that has happened.
    let sibling = Path::new(env!("CARGO_BIN_EXE_arkforge"))
        .parent()
        .unwrap()
        .join(if cfg!(windows) {
            "arkforged.exe"
        } else {
            "arkforged"
        });
    if !sibling.exists() {
        eprintln!("skipping the live half: {} is not built", sibling.display());
        return;
    }

    let started = output_within(
        Command::new(env!("CARGO_BIN_EXE_arkforge"))
            .arg("--runtime-dir")
            .arg(&runtime.root)
            .arg("--output")
            .arg("json")
            .args(["device", "list"]),
        "the autostarting device list",
    );
    let document = stdout(&started);
    assert!(started.status.success(), "{started:?}");
    assert!(
        document.contains("\"runtime_autostarted\":true"),
        "a command that started a service must disclose it: {document}"
    );

    // A second command attaches to the runtime the first one created rather
    // than starting another, and says nothing about starting anything.
    let attached = output_within(
        Command::new(env!("CARGO_BIN_EXE_arkforge"))
            .arg("--runtime-dir")
            .arg(&runtime.root)
            .arg("--output")
            .arg("json")
            .args(["job", "list"]),
        "the attaching job list",
    );
    assert!(attached.status.success(), "{attached:?}");
    assert!(!stdout(&attached).contains("\"runtime_autostarted\""));

    let stopped = output_within(
        Command::new(env!("CARGO_BIN_EXE_arkforge"))
            .arg("--runtime-dir")
            .arg(&runtime.root)
            .args(["daemon", "stop"]),
        "the daemon stop",
    );
    assert!(stopped.status.success(), "{stopped:?}");
}
