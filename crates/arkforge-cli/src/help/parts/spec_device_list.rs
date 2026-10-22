HelpSpec {
        command: "device list",
        summary: "List current observations with what this build can prove about each.",
        usage: "arkforge device list [--device <observation-id>] [--deep] [--hardware-campaign <campaign-id>]",
        effect: "Read-only discovery through runtime-dir/public.sock. --deep additionally probes every candidate profile. No observation is selected and no device is mutated.",
        requires: &["A running ArkForge runtime."],
        produces: &[
            "arkforge.device-list/v1 observations, each with an identification block reporting compatible profiles and physical model separately, with evidence and strength.",
        ],
        options: &[
            (
                "--hardware-campaign <campaign-id>",
                "Exact campaign served by the running runtime; required for a campaign runtime and never inherited.",
            ),
            (
                "--device <observation-id>",
                "Report only this exact current observation; optional.",
            ),
            (
                "--deep",
                "Also probe every candidate profile and report the facts it returned.",
            ),
        ],
        examples: &[
            "arkforge --output json device list",
            "arkforge --output json device list --deep",
        ],
        next: &[
            "arkforge flash plan --artifact <artifact-id> --profile <profile-id> --device <observation-id> --intent full-restore",
        ],
        exits: &[
            (0, "Observation list produced, including an empty list."),
            (2, "Command or option is invalid."),
            (3, "Discovery or a candidate probe was refused."),
            (5, "The runtime or requested observation was not found."),
            (10, "Discovery or IPC failed."),
        ],
    }
