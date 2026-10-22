HelpSpec {
        command: "config",
        summary: "Bind reusable local tools and profiles for this runtime directory.",
        usage: "arkforge config <show|set|unset|add|remove> [<key>=<value>...]",
        effect: "Owner-only host state. Configuration names what this host may drive; it never grants consent, never opens a hardware campaign, and never touches a device.",
        requires: &["An explicit runtime directory or the per-user default."],
        produces: &[
            "arkforge.config/v1 with binding state, digests, and counts; structured output never carries a host path.",
        ],
        options: &[],
        examples: &[
            "arkforge config show",
            "arkforge help config set --format json",
        ],
        next: &["arkforge config show"],
        exits: &[
            (0, "Configuration produced or committed."),
            (2, "Command, setting, or value is invalid."),
            (3, "A digest did not match, so nothing was stored."),
            (5, "A named file or binding was not found."),
            (6, "The binding already exists."),
            (10, "The durable configuration boundary failed."),
        ],
    }
