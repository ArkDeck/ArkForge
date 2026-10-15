use std::process::Command;

#[test]
fn verification_help_exposes_only_job_budget_and_campaign() {
    let output = Command::new(env!("CARGO_BIN_EXE_arkforge"))
        .args(["help", "flash", "verify", "--format", "json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for value in [
        "arkforge.flash-verification/v1",
        "read-device-and-host-write",
        "--job",
        "--budget-ms",
        "--hardware-campaign",
        "Partial verification",
    ] {
        assert!(help.contains(value), "missing {value}");
    }
    assert!(!help.contains("--partition"));
    assert!(!help.contains("--expect-sha256"));
}

#[test]
fn verification_invalid_inputs_are_rejected_before_runtime_access() {
    for arguments in [
        vec![],
        vec!["--job", "JOB-TEST", "--budget-ms", "999"],
        vec!["--job", "JOB-TEST", "--budget-ms", "600001"],
        vec!["--job", "JOB-TEST", "--budget-ms", "-1"],
        vec!["--job", "JOB-TEST", "--partition", "userdata"],
        vec!["--job", "JOB-TEST", "--shell", "reboot"],
        vec!["--job", "JOB-TEST", "--expect-sha256", &"a".repeat(64)],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_arkforge"))
            .args([
                "--no-auto-start",
                "--no-input",
                "--output",
                "json",
                "flash",
                "verify",
            ])
            .args(arguments)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        let document = String::from_utf8(output.stdout).unwrap();
        assert!(document.contains("\"code\":\"INVALID_ARGUMENT\""));
        assert!(!document.contains("DAEMON_UNAVAILABLE"));
    }
}
