HelpSpec {
        command: "flash",
        summary: "Stage and seal normal firmware work against exact resources.",
        usage: "arkforge flash <run|plan|verify> [options]",
        effect: "Run performs the whole flash behind one consent gate; plan stops after sealing, and the top-level apply executes what it sealed. Verify independently reads completed image ranges in Normal mode.",
        requires: &[
            "Firmware content, and enough evidence to name exactly one device and profile.",
        ],
        produces: &["One composite staging document."],
        options: &[],
        examples: &["arkforge help flash run --format json"],
        next: &["arkforge flash run --file <firmware-file> --ack <token>"],
        exits: &[
            (0, "Staging document produced."),
            (2, "Command or option is invalid."),
            (3, "Materialization was refused."),
            (5, "A required runtime object was not found."),
            (
                6,
                "The device or profile could not be narrowed to exactly one.",
            ),
            (10, "Assessment or IPC failed."),
        ],
    }
