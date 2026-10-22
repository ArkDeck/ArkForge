HelpSpec {
        command: "rescue list",
        summary: "List exact DAYU200 Loader observations reachable by native RockUSB.",
        usage: "arkforge rescue list",
        effect: "Read-only USB enumeration and readiness classification. The device is not mutated.",
        requires: &[],
        produces: &[
            "arkforge.rescue-device-list/v1 with opaque rescue device IDs; raw serial values are not printed.",
        ],
        options: &[],
        examples: &["arkforge --output json rescue list"],
        next: &["arkforge rescue inspect --device <device-id>"],
        exits: &[
            (0, "List produced, including an empty list."),
            (3, "Native USB enumeration was refused."),
        ],
    }
