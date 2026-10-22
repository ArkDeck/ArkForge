HelpSpec {
        command: "signing",
        summary: "Inspect a Mach-O file against the ArkForge signing contract.",
        usage: "arkforge signing verify --file <mach-o> --mode <development|release>",
        effect: "Read-only and offline. It reads one local Mach-O file and does not modify the file, host configuration, or a device.",
        requires: &["A thin or universal Mach-O file no larger than 64 MiB."],
        produces: &[
            "Observed signing facts and a typed contract verdict for every architecture slice.",
        ],
        options: &[],
        examples: &[
            "arkforge signing verify --file ./arkforged --mode development",
            "arkforge help signing verify --format json",
        ],
        next: &["arkforge signing verify --file <mach-o> --mode <development|release>"],
        exits: &[
            (
                0,
                "Signing verification completed and the file is compliant.",
            ),
            (2, "Command or option is invalid."),
            (
                3,
                "The file cannot be inspected or violates the selected contract.",
            ),
        ],
    }
