HelpSpec {
        command: "job list",
        summary: "List point-in-time durable status for every job in this runtime.",
        usage: "arkforge job list [--hardware-campaign <campaign-id>]",
        effect: "Read-only. It does not watch, resume, cancel, reconcile, or mutate a job.",
        requires: &["A running ArkForge runtime."],
        produces: &["arkforge.job-list/v1, including an empty jobs array."],
        options: &[(
            "--hardware-campaign <campaign-id>",
            "Exact campaign served by the running runtime; required for a campaign runtime and never inherited.",
        )],
        examples: &["arkforge --output json job list"],
        next: &["arkforge job show --job <job-id>"],
        exits: &[
            (0, "Job list produced."),
            (5, "The runtime is not available."),
            (10, "Job query or IPC failed."),
        ],
    }
