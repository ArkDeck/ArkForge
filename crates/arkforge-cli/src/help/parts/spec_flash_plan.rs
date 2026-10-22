HelpSpec {
        command: "flash plan",
        summary: "Import, identify, assess, and seal one normal-flash plan in one call.",
        usage: "arkforge flash plan (--file <firmware-file> | --artifact <artifact-id>) [--device <observation-id> | --target <selector>] [--profile <id@version>] [--intent <full-restore>] [--hardware-campaign <campaign-id>] [--wait-device <u64>] [--assess-only]",
        effect: "Imports bytes into the content store, reads the exact device through the paired runtime, and stores a sealed plan. It does not mutate the device. --assess-only stops before sealing and materializes nothing.",
        requires: &[
            "A running paired CLI authority supervisor.",
            "Exact mechanics maturity and independent authority support, for a sealed plan.",
            "An explicit --profile and exact --device when this build cannot prove which physical board the target is.",
        ],
        produces: &[
            "arkforge.flash-plan/v2 carrying the resolved artifact, device identification, profile and intent with how each was decided, the assessment, the sealed plan, and the exact apply command. A refused plan returns the same document under error.facts.flash_plan with plan null.",
        ],
        options: &[
            (
                "--file <firmware-file>",
                "Firmware container imported into the content store before the plan binds it.",
            ),
            (
                "--artifact <artifact-id>",
                "Exact already-imported content id; conflicts with --file.",
            ),
            (
                "--device <observation-id>",
                "Exact current observation; conflicts with --target.",
            ),
            (
                "--target <selector>",
                "Serial digest, unique identifier prefix of at least four characters, or proven product model; conflicts with --device.",
            ),
            (
                "--profile <id@version>",
                "Exact loaded profile identity; inferred when the compatible set has exactly one member.",
            ),
            (
                "--intent <full-restore>",
                "Semantic intent; defaulted when the profile and format admit exactly one.",
            ),
            (
                "--hardware-campaign <campaign-id>",
                "Named acceptance campaign the running runtime must already serve; it is never restarted to match.",
            ),
            (
                "--wait-device <u64>",
                "Bounded wait in milliseconds for a matching device to appear.",
            ),
            (
                "--assess-only",
                "Produce the assessment and stop; no plan is materialized.",
            ),
        ],
        examples: &[
            "arkforge --output json flash plan --file <firmware-file> --profile org.openharmony.dayu200@1.0.0 --device OBS-PREFLIGHT",
            "arkforge --output json flash plan --artifact <artifact-id> --assess-only",
        ],
        next: &[
            "Use the returned apply_command verbatim after reviewing required_acknowledgements.",
        ],
        exits: &[
            (
                0,
                "Plan sealed, or assessment produced under --assess-only.",
            ),
            (2, "Inputs are invalid, or no firmware was named."),
            (
                3,
                "Mechanics or authority support is unavailable, or the physical identity was not established.",
            ),
            (5, "A named resource, device, or runtime is unavailable."),
            (
                6,
                "The device or profile could not be narrowed to exactly one, or the runtime serves another campaign.",
            ),
            (10, "Controller, store, or supervisor failed."),
        ],
    }
