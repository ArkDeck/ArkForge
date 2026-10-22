HelpSpec {
        command: "cancel",
        summary: "Ask the authority to stop one job at a safe boundary.",
        usage: "arkforge cancel --job <job-id> --expect-sequence <u64> [--hardware-campaign <campaign-id>]",
        effect: "Mutating control only. It requests a safe stop at an optimistic sequence; it never edits the journal, replays an action, or reclassifies an outcome.",
        requires: &["A durable job and the exact expected last sequence."],
        produces: &[
            "arkforge.job-cancellation/v1 with one of four typed dispositions and no automatic replay.",
        ],
        options: &[
            (
                "--hardware-campaign <campaign-id>",
                "Exact campaign served by the running runtime; required for a campaign runtime and never inherited.",
            ),
            ("--job <job-id>", "Exact durable job; required."),
            (
                "--expect-sequence <u64>",
                "Optimistic concurrency cursor; required.",
            ),
        ],
        examples: &["arkforge --output json cancel --job JOB-EXAMPLE --expect-sequence 4"],
        next: &["arkforge job show --job <job-id>"],
        exits: &[
            (0, "Cancelled safely or already terminal."),
            (2, "Inputs are invalid."),
            (5, "Runtime or job was not found."),
            (6, "Expected sequence is stale."),
            (8, "Outcome is unknown."),
            (9, "Cancellation is queued at a safe boundary."),
            (10, "Controller or supervisor failed."),
        ],
    }
