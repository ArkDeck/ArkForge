HelpSpec {
        command: "daemon run",
        summary: "Run the CLI authority supervisor and mechanics daemon in the foreground.",
        usage: "arkforge daemon run [--profile-file <file>]... [--hdc <absolute-path> --expect-hdc-sha256 <sha256>] [--hardware-campaign <campaign-id>] [--require-release-signing]",
        effect: "Service lifecycle. Creates owner-only local sockets and keeps both processes supervised until daemon stop is requested.",
        requires: &[
            "A runtime directory not owned by another live supervisor.",
            "HDC path and digest together when managed normal-mode control is required.",
        ],
        produces: &[
            "arkforge.daemon-status/v1 for a paired foreground runtime; the pairing secret exists only in supervisor/daemon memory and crosses an inherited stdin pipe.",
        ],
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
                "Refuse unless arkforged satisfies the release signing contract.",
            ),
            (
                "--hardware-campaign <campaign-id>",
                "Explicit mechanics and CLI-authority acceptance campaign; receipts remain campaign evidence and never publish production support.",
            ),
        ],
        examples: &[DAEMON_RUN_EXAMPLE],
        next: &["arkforge status"],
        exits: &[
            (0, "Runtime stopped cleanly."),
            (2, "Options are invalid."),
            (3, "A tool or signing binding was refused."),
            (6, "The runtime is already running."),
            (10, "A supervised process or local IPC failed."),
        ],
    }
