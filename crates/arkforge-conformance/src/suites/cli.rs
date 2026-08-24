//! Process-level CLI vectors. These are also executed against the built
//! `arkforge` binary by crates/arkforge-cli/tests/conformance_blackbox.rs.

use crate::json::Json;
use crate::suites::case_id;
use crate::tree::{Case, Tree};

const SUITE: &str = "cli";

const VERSION_STDOUT: &str = "arkforge 0.1.0\n";
const BAD_FORMAT_STDERR: &str = "arkforge: INVALID_ARGUMENT: --format accepts exactly 'human' or 'json', not \"xml\".\nRemediation: Read the machine help for the exact command and option constraints.\nNext: arkforge help --format json\n";
const UNKNOWN_COMMAND_JSON: &str = "{\"schema\":\"arkforge.command-result/v1\",\"ok\":false,\"command\":[],\"error\":{\"code\":\"INVALID_ARGUMENT\",\"message\":\"Unknown subcommand \\\"frobnicate\\\"; it accepts status, device, artifact, flash, apply, watch, cancel, job, rescue, daemon, config, signing, completion, help.\",\"remediation\":\"Read the machine help for the exact command and option constraints.\",\"retryable\":false,\"required_acknowledgements\":[],\"next_commands\":[\"arkforge help --format json\"],\"facts\":null}}\n";
const CANCEL_HELP_STDOUT: &str = "{\"schema\":\"arkforge.command-help/v1\",\"path\":[\"cancel\"],\"command\":\"cancel\",\"summary\":\"Ask the authority to stop one job at a safe boundary.\",\"usage\":\"arkforge cancel --job <job-id> --expect-sequence <u64>\",\"effect\":\"mutating-control\",\"effect_detail\":\"Mutating control only. It requests a safe stop at an optimistic sequence; it never edits the journal, replays an action, or reclassifies an outcome.\",\"runtime_effect\":\"may-start-service\",\"interactive\":false,\"availability\":{\"platforms\":[\"macos\",\"windows\"],\"requires_daemon\":true,\"requires_controller\":true},\"subcommands\":[],\"requires\":[\"A durable job and the exact expected last sequence.\"],\"outputs\":[\"arkforge.job-cancellation/v1\"],\"output_descriptions\":[\"arkforge.job-cancellation/v1 with one of four typed dispositions and no automatic replay.\"],\"options\":[{\"name\":\"--job\",\"type\":\"job-id\",\"required\":true,\"repeatable\":false,\"enum_values\":[],\"sensitive\":false,\"effect_relevant\":true,\"requires\":[],\"conflicts\":[],\"description\":\"Exact durable job; required.\"},{\"name\":\"--expect-sequence\",\"type\":\"uint64\",\"required\":true,\"repeatable\":false,\"enum_values\":[],\"sensitive\":false,\"effect_relevant\":true,\"requires\":[],\"conflicts\":[],\"description\":\"Optimistic concurrency cursor; required.\"}],\"constraints\":[],\"facts_projections\":[],\"examples\":[\"arkforge --output json cancel --job JOB-EXAMPLE --expect-sequence 4\"],\"next_commands\":[\"arkforge job show --job <job-id>\"],\"exit_codes\":[{\"code\":0,\"meaning\":\"Cancelled safely or already terminal.\"},{\"code\":2,\"meaning\":\"Inputs are invalid.\"},{\"code\":5,\"meaning\":\"Runtime or job was not found.\"},{\"code\":6,\"meaning\":\"Expected sequence is stale.\"},{\"code\":8,\"meaning\":\"Outcome is unknown.\"},{\"code\":9,\"meaning\":\"Cancellation is queued at a safe boundary.\"},{\"code\":10,\"meaning\":\"Controller or supervisor failed.\"}]}\n";

struct ProcessExpectation<'a> {
    exit_code: u64,
    stdout: &'a str,
    stderr: &'a str,
}

fn emit(
    tree: &mut Tree,
    number: u32,
    title: &str,
    requirements: Vec<&'static str>,
    argv: &[&str],
    expectation: ProcessExpectation<'_>,
) {
    let mut files = Vec::new();
    if !expectation.stdout.is_empty() {
        files.push(("stdout.txt", expectation.stdout.as_bytes().to_vec()));
    }
    if !expectation.stderr.is_empty() {
        files.push(("stderr.txt", expectation.stderr.as_bytes().to_vec()));
    }
    tree.case(
        &Case {
            id: case_id("CLI", number),
            suite: SUITE,
            title: title.to_string(),
            requirements,
            kind: "process",
            description: "Run the ArkForge CLI as a subprocess with exactly this argv, no daemon and a clean temporary runtime directory. Compare exit status and stdout/stderr byte-for-byte.".to_string(),
            input: Json::object(vec![("argv", Json::strs(argv.iter().copied()))]),
            expected: Json::object(vec![
                ("exitCode", Json::Unsigned(expectation.exit_code)),
                (
                    "stdoutFile",
                    if expectation.stdout.is_empty() {
                        Json::Null
                    } else {
                        Json::str("stdout.txt")
                    },
                ),
                (
                    "stderrFile",
                    if expectation.stderr.is_empty() {
                        Json::Null
                    } else {
                        Json::str("stderr.txt")
                    },
                ),
            ]),
        },
        files,
    );
}

pub fn populate(tree: &mut Tree) {
    emit(
        tree,
        1,
        "version is a stable one-line contract",
        vec!["AF-CLI-001"],
        &["--version"],
        ProcessExpectation {
            exit_code: 0,
            stdout: VERSION_STDOUT,
            stderr: "",
        },
    );
    emit(
        tree,
        2,
        "machine help describes cancel without contacting a daemon",
        vec!["AF-CLI-001", "AF-CLI-002"],
        &["help", "cancel", "--format", "json"],
        ProcessExpectation {
            exit_code: 0,
            stdout: CANCEL_HELP_STDOUT,
            stderr: "",
        },
    );
    emit(
        tree,
        3,
        "an unsupported help format is a typed usage error",
        vec!["AF-CLI-001"],
        &["help", "cancel", "--format", "xml"],
        ProcessExpectation {
            exit_code: 2,
            stdout: "",
            stderr: BAD_FORMAT_STDERR,
        },
    );
    emit(
        tree,
        4,
        "an unknown command is a structured error before side effects",
        vec!["AF-CLI-003"],
        &["--output", "json", "frobnicate"],
        ProcessExpectation {
            exit_code: 2,
            stdout: UNKNOWN_COMMAND_JSON,
            stderr: "",
        },
    );
}
