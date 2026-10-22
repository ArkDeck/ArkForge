HelpSpec {
        command: "job show",
        summary: "Show one durable job with its event tail, receipts, and recovery block.",
        usage: "arkforge job show --job <job-id> [--hardware-campaign <campaign-id>]",
        effect: "Read-only point-in-time status. It neither waits for events nor changes the job.",
        requires: &["One exact job id from job list or a prior command result."],
        produces: &[
            "arkforge.job/v1 with plan binding, state, progress, the last 20 durable events, every action receipt, and the no-replay recovery block. Each embedded section reports its own availability, so an unreadable one is never rendered as an absent one.",
        ],
        options: &[
            (
                "--hardware-campaign <campaign-id>",
                "Exact campaign served by the running runtime; required for a campaign runtime and never inherited.",
            ),
            ("--job <job-id>", "Exact durable job id; required."),
        ],
        examples: &["arkforge --output json job show --job <job-id>"],
        next: &["If state is outcomeUnknown, run 'arkforge job reconcile --job <job-id>'."],
        exits: &[
            (
                0,
                "Job document produced, including a partially observable one.",
            ),
            (2, "The job id is missing."),
            (5, "The runtime or job was not found."),
            (10, "Job query or IPC failed."),
        ],
    }
