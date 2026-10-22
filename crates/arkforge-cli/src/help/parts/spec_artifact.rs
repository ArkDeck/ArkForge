HelpSpec {
        command: "artifact",
        summary: "Import, list, and show content-addressed firmware artifacts.",
        usage: "arkforge artifact <import|list|show> [options]",
        effect: "Import writes only the local content-addressed store; list and show are offline reads of it. No artifact command mutates a device.",
        requires: &["An explicit runtime directory or the per-user default."],
        produces: &["Artifact IDs, stored-object lists, or complete inspected manifests."],
        options: &[],
        examples: &[
            "arkforge artifact import --file <firmware-file>",
            "arkforge help artifact show --format json",
        ],
        next: &["arkforge artifact import --file <firmware-file>"],
        exits: &[
            (0, "Artifact query completed."),
            (2, "Command or option is invalid."),
            (3, "Artifact inspection was refused."),
            (5, "The runtime or artifact was not found."),
            (10, "Inspection or IPC failed."),
        ],
    }
