HelpSpec {
        command: "",
        summary: "ArkForge plans, executes, verifies, and recovers device firmware operations.",
        usage: "arkforge [global options] [<command> [<subcommand>]] [options]",
        effect: "With no command the root reports the aggregate host and runtime status. It never starts a runtime and never accesses a device for mutation.",
        requires: &[],
        produces: &[
            "arkforge.status/v1 for a bare invocation, or human help and arkforge.command-help-index/v1 for help.",
        ],
        options: &[
            ("--runtime-dir <dir>", "Per-user ArkForge state directory."),
            (
                "--output <human|json|jsonl>",
                "Stable presentation format; default: human.",
            ),
            (
                "--no-color",
                "Disable color; accepted for deterministic scripts.",
            ),
            ("--quiet", "Print only the final human result."),
            (
                "--no-auto-start",
                "Refuse instead of starting a runtime that is not already listening.",
            ),
            (
                "--no-input",
                "Never ask; treat every missing decision as a typed refusal.",
            ),
            (
                "--verbose",
                "Include diagnostic evidence; never include secrets.",
            ),
            ("-h, --help", "Show help for the current command."),
            ("-V, --version", "Print this build's ArkForge version."),
        ],
        examples: &[
            "arkforge help --all --format json",
            "arkforge help flash plan --format json",
        ],
        next: &["arkforge status"],
        exits: &[
            (0, "Status, help, or version produced."),
            (2, "Command or option is invalid."),
        ],
    }
