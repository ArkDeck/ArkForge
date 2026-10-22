HelpSpec {
        command: "status",
        summary: "Report host, runtime, device, artifact, job, and blocker state in one document.",
        usage: "arkforge status",
        effect: "Read-only. It aggregates every fact this host can currently observe, never starts a runtime, and never opens a device for mutation.",
        requires: &[],
        produces: &[
            "arkforge.status/v1 with per-section availability, so a section that could not be observed is never reported as an empty one.",
        ],
        options: &[],
        examples: &["arkforge --output json status"],
        next: &["arkforge daemon start"],
        exits: &[
            (
                0,
                "A snapshot was produced, including a partial or not-ready one.",
            ),
            (2, "A global option is invalid."),
            (10, "The host root assessment itself could not be produced."),
        ],
    }
