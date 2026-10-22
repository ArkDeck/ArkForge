HelpSpec {
        command: "signing verify",
        summary: "Verify every Mach-O slice against one explicit signing mode.",
        usage: "arkforge signing verify --file <mach-o> --mode <development|release>",
        effect: "Read-only and offline. Development mode permits clean local ad-hoc signatures; release mode requires the complete shipping contract. Both modes require an empty entitlement dictionary.",
        requires: &[
            "An explicit file path.",
            "An explicit development or release mode; no mode is inferred.",
        ],
        produces: &[
            "arkforge.signing-verification/v1 with compliant, signing facts, stable violation codes, remediation, and the contract reference.",
        ],
        options: &[
            (
                "--file <mach-o>",
                "Thin or universal Mach-O file; required.",
            ),
            (
                "--mode <development|release>",
                "Signing contract strength; required.",
            ),
        ],
        examples: &[
            "arkforge --output json signing verify --file ./target/debug/arkforged --mode development",
            "arkforge signing verify --file ./target/release/arkforged --mode release",
        ],
        next: &[
            "If compliant is false, correct every violations[] item and repeat the exact command.",
        ],
        exits: &[
            (0, "The file meets the selected contract."),
            (2, "Required inputs are missing or invalid."),
            (
                3,
                "The file is unreadable as Mach-O or violates the selected contract.",
            ),
        ],
    }
