HelpSpec {
        command: "config add",
        summary: "Add one additional development profile by absolute path and digest.",
        usage: "arkforge config add profile-file.path=<absolute-path> profile-file.sha256=<sha256>",
        effect: "Owner-only host write, committed atomically. The profile is re-hashed before every runtime start, and byte drift is a typed refusal.",
        requires: &[
            "An absolute path whose current bytes have the supplied digest.",
            "A digest not already configured.",
        ],
        produces: &["arkforge.config/v1 after the transaction commits."],
        examples: &[
            "arkforge config add profile-file.path=<absolute-path> profile-file.sha256=<64-lowercase-hex>",
        ],
        options: &[],
        next: &["arkforge config show"],
        exits: &[
            (0, "Profile added."),
            (2, "A setting is unknown, malformed, or relative."),
            (3, "The file does not have the expected digest."),
            (5, "The named file was not found."),
            (6, "That digest is already configured."),
            (10, "The durable configuration boundary failed."),
        ],
    }
