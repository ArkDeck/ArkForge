//! Process-level contract for canonical offline artifact operations.

use arkforge_artifact::fixture;
use std::path::PathBuf;
use std::process::Command;

struct TempRuntime {
    root: PathBuf,
}

impl TempRuntime {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "arkforge-cli-artifact-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn command(&self, arguments: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_arkforge"))
            .arg("--runtime-dir")
            .arg(&self.root)
            .arg("--output")
            .arg("json")
            // Import reports which connected devices could take the firmware;
            // with no runtime that section is unknown, and these tests assert
            // exactly that rather than starting one.
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

#[test]
fn import_answers_the_whole_staging_question_in_one_call() {
    let runtime = TempRuntime::new();
    let input = runtime.root.join("firmware.tar.gz");
    std::fs::write(&input, fixture::dayu200_archive()).unwrap();
    let input = input.to_str().unwrap();

    let imported = runtime.command(&["artifact", "import", "--file", input]);
    assert!(imported.status.success(), "{imported:?}");
    assert!(imported.stderr.is_empty(), "{imported:?}");
    let imported = String::from_utf8(imported.stdout).unwrap();
    assert!(imported.contains("\"schema\":\"arkforge.artifact-import/v1\""));
    assert!(imported.contains("\"device_accessed\":false"));
    assert!(imported.contains("\"deduplicated\":false"));
    // CAS facts, what the bytes parse as, and which profiles declare that
    // format, without a second command.
    assert!(
        imported.contains("\"manifest_summary\":{\"format_id\":\"rockchip-images-targz\""),
        "{imported}"
    );
    assert!(imported.contains("\"compatible_profiles\":[\"org.openharmony.dayu200@1.0.0\"]"));
    // With no runtime paired, which devices are present is unknown — not none.
    assert!(
        imported.contains(
            "\"present_devices\":{\"available\":false,\"complete\":false,\"reason\":\"DAEMON_UNAVAILABLE\",\"items\":null}"
        ),
        "{imported}"
    );
    let artifact_id = json_string(&imported, "artifact_id");
    assert_eq!(artifact_id.len(), 64);

    let deduplicated = runtime.command(&["artifact", "import", "--file", input]);
    assert!(deduplicated.status.success(), "{deduplicated:?}");
    assert!(
        String::from_utf8(deduplicated.stdout)
            .unwrap()
            .contains("\"deduplicated\":true")
    );

    let listed = runtime.command(&["artifact", "list"]);
    assert!(listed.status.success(), "{listed:?}");
    let listed = String::from_utf8(listed.stdout).unwrap();
    assert!(listed.contains("\"schema\":\"arkforge.artifact-list/v1\""));
    assert!(listed.contains(&artifact_id));
    assert!(listed.contains("\"format\":\"rockchip-images-targz\""));
    assert!(listed.contains("\"compatible_profiles\":[\"org.openharmony.dayu200@1.0.0\"]"));

    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let profile = repo.join("profiles/dayu200.yaml");
    let shown = runtime.command(&[
        "artifact",
        "show",
        "--artifact",
        &artifact_id,
        "--profile-file",
        profile.to_str().unwrap(),
    ]);
    assert!(shown.status.success(), "{shown:?}");
    let shown = String::from_utf8(shown.stdout).unwrap();
    assert!(shown.contains("\"schema\":\"arkforge.artifact-inspection/v1\""));
    assert!(shown.contains("\"format_id\":\"rockchip-images-targz\""));
    assert!(shown.contains("\"complete\":true"));
    assert!(shown.contains("\"targets\":["));
    assert!(shown.contains("\"device_accessed\":false"));
    assert!(shown.contains("\"compatible_profiles\":[\"org.openharmony.dayu200@1.0.0\"]"));

    // The absorbed leaf is gone, with no alias period.
    let removed = runtime.command(&["artifact", "inspect", "--artifact", &artifact_id]);
    assert_eq!(removed.status.code(), Some(2), "{removed:?}");
}

#[test]
fn an_independent_digest_mismatch_refuses_before_publication() {
    let runtime = TempRuntime::new();
    let input = runtime.root.join("firmware.tar.gz");
    std::fs::write(&input, fixture::dayu200_archive()).unwrap();
    let output = runtime.command(&[
        "artifact",
        "import",
        "--file",
        input.to_str().unwrap(),
        "--expect-sha256",
        &"0".repeat(64),
    ]);
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stderr.is_empty());
    let error = String::from_utf8(output.stdout).unwrap();
    assert!(error.contains("\"code\":\"ARTIFACT_IMPORT_REFUSED\""));

    let listed = runtime.command(&["artifact", "list"]);
    assert!(listed.status.success());
    assert!(
        String::from_utf8(listed.stdout)
            .unwrap()
            .contains("\"artifacts\":[]")
    );
}

fn json_string(document: &str, key: &str) -> String {
    let prefix = format!("\"{key}\":\"");
    let rest = document
        .split_once(&prefix)
        .unwrap_or_else(|| panic!("missing {key} in {document}"))
        .1;
    rest.split_once('"').unwrap().0.to_string()
}
