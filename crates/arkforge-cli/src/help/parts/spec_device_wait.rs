HelpSpec {
        command: "device wait",
        summary: "Wait for exactly one observation matching an explicit profile and mode.",
        usage: "arkforge device wait --profile <profile-id> --mode <mode> [--timeout-ms <u64>] [--hardware-campaign <campaign-id>]",
        effect: "Repeated read-only discovery and probing. It never chooses the first match; multiple matches are a typed ambiguity refusal.",
        requires: &["A loaded profile id and an explicit expected mode."],
        produces: &["arkforge.device-wait/v1 with the unique probed observation and facts digest."],
        options: &[
            (
                "--hardware-campaign <campaign-id>",
                "Exact campaign served by the running runtime; required for a campaign runtime and never inherited.",
            ),
            (
                "--profile <profile-id>",
                "Explicit loaded profile; required.",
            ),
            ("--mode <mode>", "Exact declared device mode; required."),
            (
                "--timeout-ms <u64>",
                "Bounded wait; optional, default 30000.",
            ),
        ],
        examples: &[
            "arkforge --output json device wait --profile org.openharmony.dayu200@1.0.0 --mode loader --timeout-ms 30000",
        ],
        next: &[
            "arkforge flash plan --file <firmware-file> --profile <profile-id> --device <observation-id>",
        ],
        exits: &[
            (0, "Exactly one matching probed observation was produced."),
            (2, "Profile, mode, or timeout is invalid."),
            (3, "A matching provider probe was refused."),
            (
                5,
                "The runtime was unavailable or the bounded wait expired.",
            ),
            (
                6,
                "More than one observation matched; no target was selected.",
            ),
            (10, "Discovery, probe, or IPC failed."),
        ],
    }
