HelpSpec {
        command: "artifact show",
        summary: "Parse one stored artifact offline and optionally compare profile coverage.",
        usage: "arkforge artifact show --artifact <artifact-id> [--profile-file <file>]",
        effect: "Read-only artifact parsing after opening bytes by content digest. It never reparses a caller path, never contacts the runtime, and never accesses a device.",
        requires: &["One exact artifact id already present in this runtime store."],
        produces: &[
            "arkforge.artifact-inspection/v1 with the complete manifest, the profiles declaring its format, and optional ordered profile target coverage.",
        ],
        options: &[
            (
                "--artifact <artifact-id>",
                "Exact stored content SHA-256; required.",
            ),
            (
                "--profile-file <file>",
                "Optional DeviceProfile used only for coverage comparison.",
            ),
        ],
        examples: &[
            "arkforge --output json artifact show --artifact <64-lowercase-hex> --profile-file profiles/dayu200.yaml",
        ],
        next: &[
            "arkforge flash plan --artifact <artifact-id> --profile <profile-id> --device <observation-id> --intent full-restore",
        ],
        exits: &[
            (0, "Manifest and optional coverage produced."),
            (2, "The artifact id or options are invalid."),
            (3, "Artifact or profile parsing was refused."),
            (
                5,
                "The artifact store, object, or profile file was not found.",
            ),
            (10, "The content store failed."),
        ],
    }
