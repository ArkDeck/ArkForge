HelpSpec {
        command: "help",
        summary: "Describe commands for humans or Agents without runtime access.",
        usage: "arkforge help [<command> [<subcommand>...]] [--all] [--format <human|json>]",
        effect: "Read-only and offline. Help is generated from the canonical typed command tree.",
        requires: &[],
        produces: &[
            "One arkforge.command-help/v1 leaf for a topic path, or the whole tree as arkforge.command-help-index/v1 for --all and for structured help without a path.",
        ],
        options: &[
            (
                "--all",
                "Describe the whole command tree in one document; takes no topic path.",
            ),
            (
                "--format <human|json>",
                "Help presentation format; default follows --output.",
            ),
        ],
        examples: &[
            "arkforge help --all --format json",
            "arkforge help apply --format json",
        ],
        next: &["Follow one returned next_commands entry with exact identifiers."],
        exits: &[
            (0, "Help produced."),
            (2, "The topic or format is invalid."),
        ],
    }
