HelpSpec {
        command: "config remove",
        summary: "Remove one configured profile by its exact digest.",
        usage: "arkforge config remove profile-file.sha256=<sha256>",
        effect: "Owner-only host write, committed atomically. Removal is by digest, so a moved or renamed file cannot be unbound by accident.",
        requires: &["A digest that is currently configured."],
        produces: &["arkforge.config/v1 after the transaction commits."],
        options: &[],
        examples: &["arkforge config remove profile-file.sha256=<64-lowercase-hex>"],
        next: &["arkforge config show"],
        exits: &[
            (0, "Profile removed."),
            (2, "The setting is unknown or malformed."),
            (5, "No configured profile has that digest."),
            (10, "The durable configuration boundary failed."),
        ],
    }
