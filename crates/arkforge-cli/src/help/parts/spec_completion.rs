HelpSpec {
        command: "completion",
        summary: "Generate shell completion from the canonical command tree.",
        usage: "arkforge completion --shell <bash|zsh|fish>",
        effect: "Read-only and offline. Completion text is generated from the same typed command definitions used by parsing and help.",
        requires: &["An explicit supported shell."],
        produces: &[
            "Shell completion source on stdout, or arkforge.completion/v1 for structured output.",
        ],
        options: &[("--shell <bash|zsh|fish>", "Target shell; required.")],
        examples: &["arkforge completion --shell zsh"],
        next: &[
            "Evaluate or install the generated source using the selected shell's normal mechanism.",
        ],
        exits: &[
            (0, "Completion source produced."),
            (2, "The shell or option is invalid."),
        ],
    }
