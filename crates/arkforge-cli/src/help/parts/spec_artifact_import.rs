HelpSpec {
        command: "artifact import",
        summary: "Import one firmware file and report what it is and what it fits.",
        usage: "arkforge artifact import --file <firmware-file> [--expect-sha256 <sha256>]",
        effect: "Host write only. It creates or deduplicates one content-addressed object after quota, size, and optional digest checks, then reads the stored bytes and the current observations; no device is mutated.",
        requires: &[
            "One regular input file.",
            "Enough store quota and volume reserve for the complete input.",
        ],
        produces: &[
            "arkforge.artifact-import/v1 with CAS facts, a bounded manifest summary, the profiles that declare the parsed format, and the connected devices those profiles could flash. The present-device section reports availability, so a runtime that is not running is unknown rather than empty.",
        ],
        options: &[
            (
                "--file <firmware-file>",
                "Regular firmware container file; required.",
            ),
            (
                "--expect-sha256 <sha256>",
                "Independent expected lowercase SHA-256; optional.",
            ),
        ],
        examples: &["arkforge --output json artifact import --file ./firmware.tar.gz"],
        next: &["arkforge artifact show --artifact <returned-artifact-id>"],
        exits: &[
            (0, "Artifact imported or deduplicated and synced."),
            (2, "Input options or digest syntax are invalid."),
            (
                3,
                "Digest, size, quota, or volume precondition refused import.",
            ),
            (5, "The input file was not found."),
            (10, "The durable content store failed."),
        ],
    }
