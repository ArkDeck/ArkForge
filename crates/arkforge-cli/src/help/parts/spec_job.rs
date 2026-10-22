HelpSpec {
        command: "job",
        summary: "Observe, reconcile, and recover durable jobs.",
        usage: "arkforge job <list|show|reconcile|recover> [options]",
        effect: "Observation and reconciliation are read-only; recover creates a distinct superseding plan and never replays the original job. Following and stopping a job are the top-level watch and cancel commands.",
        requires: &["A running ArkForge runtime."],
        produces: &["Durable point-in-time job status or typed recovery guidance."],
        options: &[],
        examples: &["arkforge job list", "arkforge help job show --format json"],
        next: &["arkforge job list"],
        exits: &[
            (0, "Job query completed, including an empty list."),
            (2, "Command or option is invalid."),
            (3, "The runtime refused the query."),
            (5, "The runtime or requested job was not found."),
            (10, "Job query or IPC failed."),
        ],
    }
