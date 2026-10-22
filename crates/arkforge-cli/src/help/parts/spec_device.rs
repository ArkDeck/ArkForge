HelpSpec {
        command: "device",
        summary: "Discover, identify, and wait for exact device observations.",
        usage: "arkforge device <list|wait> [options]",
        effect: "Read-only. Device commands cannot select a target, materialize authority, or mutate a device.",
        requires: &["A running ArkForge runtime for the selected --runtime-dir."],
        produces: &["Current observations with their identification evidence."],
        options: &[],
        examples: &[
            "arkforge device list",
            "arkforge help device list --format json",
        ],
        next: &["arkforge device list"],
        exits: &[
            (0, "Query completed, including an empty list."),
            (2, "Command or option is invalid."),
            (3, "The public runtime refused the query."),
            (5, "The runtime or requested observation was not found."),
            (10, "The runtime response or local IPC failed."),
        ],
    }
