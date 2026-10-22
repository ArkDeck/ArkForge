HelpSpec {
        command: "config show",
        summary: "Show which tools and profiles this runtime directory has bound.",
        usage: "arkforge config show",
        effect: "Read-only. It does not create a runtime directory it did not find.",
        requires: &[],
        produces: &[
            "arkforge.config/v1 with HDC binding state and digest, profile digests and count, and the release signing requirement. Host and HDC paths are shown only to the owner in human output.",
        ],
        options: &[],
        examples: &["arkforge --output json config show"],
        next: &["arkforge status"],
        exits: &[
            (0, "Configuration produced, including an empty one."),
            (3, "The stored configuration could not be read."),
            (10, "The configuration store failed."),
        ],
    }
