HelpSpec {
        command: "flash restore-ramdisk",
        summary: "Seal a separate pinned-original-ramdisk-only DAYU200 restoration plan.",
        usage: "arkforge flash restore-ramdisk --artifact <original-artifact-id> --profile <id@version> --device <observation-id> --hardware-campaign <campaign-id> [--source-job <job-id>] [--assess-only]",
        effect: "Planning only. The sealed plan is destructive for ramdisk, includes mode transitions, verification and postflight, and never writes userdata. Apply still requires exact digest and acknowledgement tokens.",
        requires: &[
            "A named DAYU200 campaign and artifact containing the pinned original ramdisk.",
            "An explicit --profile and exact --device when board identity is unproven.",
        ],
        produces: &[
            "arkforge.flash-plan/v2 with one persistent ramdisk effect and an exact apply command.",
        ],
        options: &[
            (
                "--artifact <artifact-id>",
                "Required. Original firmware artifact.",
            ),
            (
                "--source-job <job-id>",
                "Require the fresh board to match the source job's durable identity and confirmed full-flash reboot receipts, including after Runtime restart.",
            ),
            (
                "--profile <id@version>",
                "Exact loaded DAYU200 profile identity; required when board identity is unproven.",
            ),
            (
                "--device <observation-id>",
                "Required. Exact current observation.",
            ),
            (
                "--hardware-campaign <campaign-id>",
                "Required. Named acceptance campaign.",
            ),
            ("--assess-only", "Return only the assessment."),
        ],
        examples: &[
            "arkforge flash restore-ramdisk --artifact <artifact-id> --profile org.openharmony.dayu200@1.0.0 --device <observation-id> --hardware-campaign <campaign-id>",
        ],
        next: &[
            "Review and execute the returned apply command, then flash verify the restoration job.",
        ],
        exits: &[
            (0, "Plan or assessment returned."),
            (2, "Invalid inputs."),
            (3, "Safety or campaign preconditions refused."),
            (5, "Runtime or target unavailable."),
        ],
    }
