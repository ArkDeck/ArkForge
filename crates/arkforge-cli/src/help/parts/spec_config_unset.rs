HelpSpec {
        command: "config unset",
        summary: "Clear one binding, leaving every other setting untouched.",
        usage: "arkforge config unset <hdc|daemon.require-release-signing>",
        effect: "Owner-only host write, committed atomically. Clearing the HDC binding clears its path and digest together.",
        requires: &["One setting name to clear."],
        produces: &["arkforge.config/v1 after the transaction commits."],
        options: &[],
        examples: &["arkforge config unset hdc"],
        next: &["arkforge config show"],
        exits: &[
            (0, "Binding cleared."),
            (2, "The setting name is unknown or carries a value."),
            (10, "The durable configuration boundary failed."),
        ],
    }
