HelpSpec {
        command: "config set",
        summary: "Bind the exact managed-control executable or the signing requirement.",
        usage: "arkforge config set hdc.path=<absolute-path> hdc.sha256=<sha256> | daemon.require-release-signing=<true|false>",
        effect: "Owner-only host write, committed atomically. A failed write leaves the previous configuration exactly as it was.",
        requires: &[
            "An absolute path whose current bytes have the supplied digest.",
            "hdc.path and hdc.sha256 supplied together; one alone is refused.",
        ],
        produces: &["arkforge.config/v1 after the transaction commits."],
        options: &[],
        examples: &[
            "arkforge config set daemon.require-release-signing=true",
            HDC_BINDING_EXAMPLE,
        ],
        next: &["arkforge config show"],
        exits: &[
            (0, "Binding committed."),
            (
                2,
                "A setting is unknown, malformed, relative, or a campaign key.",
            ),
            (3, "The file does not have the expected digest."),
            (5, "The named file was not found."),
            (10, "The durable configuration boundary failed."),
        ],
    }
