HelpSpec {
        command: "daemon start",
        summary: "Start the same paired runtime under a background supervisor.",
        usage: "arkforge daemon start [--profile-file <file>]... [--hdc <absolute-path> --expect-hdc-sha256 <sha256>] [--hardware-campaign <campaign-id>] [--require-release-signing]",
        effect: "Service lifecycle. Starts a background supervisor and mechanics daemon; it does not access or mutate a device.",
        requires: &["The same exact bindings as daemon run."],
        produces: &["arkforge.daemon-status/v1 after the public protocol handshake succeeds."],
        options: &[
            (
                "--profile-file <file>",
                "Additional explicit profile; repeatable.",
            ),
            (
                "--hdc <absolute-path>",
                "Exact managed-control executable; requires its expected digest.",
            ),
            (
                "--expect-hdc-sha256 <sha256>",
                "Caller expectation for HDC bytes; requires --hdc.",
            ),
            (
                "--require-release-signing",
                "Require the daemon release signing contract.",
            ),
            (
                "--hardware-campaign <campaign-id>",
                "Explicit mechanics and CLI-authority acceptance campaign; receipts remain campaign evidence and never publish production support.",
            ),
        ],
        examples: &["arkforge --output json daemon start"],
        next: &["arkforge device list"],
        exits: &[
            (0, "Runtime is ready for commands."),
            (2, "Options are invalid."),
            (3, "A tool or signing binding was refused."),
            (5, "The mechanics daemon could not start."),
            (6, "The runtime is already running."),
            (10, "Supervisor startup failed."),
        ],
    }
