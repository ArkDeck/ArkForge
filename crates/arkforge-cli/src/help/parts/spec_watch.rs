HelpSpec {
        command: "watch",
        summary: "Follow one job's durable events, defaulting to the job that is running.",
        usage: "arkforge watch [--job <job-id>] [--after-sequence <u64>] [--timeout-ms <u64>] [--hardware-campaign <campaign-id>]",
        effect: "Read-only polling of durable events and point-in-time status. Timeout ends only this observation; it never cancels or changes the job.",
        requires: &[
            "A running ArkForge runtime, and a resume sequence no greater than the durable last_sequence.",
        ],
        produces: &[
            "arkforge.job-watch/v1, arkforge.job-event/v1, and arkforge.command-result/v1 with strictly ordered typed events, terminal/timed-out status, and an exact resume command.",
        ],
        options: &[
            (
                "--hardware-campaign <campaign-id>",
                "Exact campaign served by the running runtime; required for a campaign runtime and never inherited.",
            ),
            (
                "--job <job-id>",
                "Exact durable job; defaults to the single running job, or the most recently active one when none is running.",
            ),
            (
                "--after-sequence <u64>",
                "Return events strictly after this cursor; optional, default 0.",
            ),
            (
                "--timeout-ms <u64>",
                "Bounded observation; optional, default 30000.",
            ),
        ],
        examples: &[
            "arkforge --output json watch",
            "arkforge --output json watch --job <job-id> --after-sequence 0 --timeout-ms 30000",
        ],
        next: &[
            "If non-terminal, repeat next_commands[0]; stopping the watch never cancels the job.",
        ],
        exits: &[
            (
                0,
                "Events and status produced, including a timed-out observation.",
            ),
            (2, "Options are invalid."),
            (
                5,
                "The runtime or job was not found, or no job is recorded.",
            ),
            (
                6,
                "Several jobs are running, or the resume cursor is ahead of the durable sequence.",
            ),
            (10, "Job query or IPC failed."),
        ],
    }
