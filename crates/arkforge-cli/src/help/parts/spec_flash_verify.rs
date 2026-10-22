HelpSpec {
        command: "flash verify",
        summary: "Verify a completed DAYU200 job's full image ranges in Normal mode.",
        usage: "arkforge flash verify --job <job-id> [--budget-ms <u64>] [--hardware-campaign <campaign-id>] [--preboot-ramdisk <sha256>]",
        effect: "Read-only device access; persists a separate verification record. Exact ranges and expected digests come only from the completed job's sealed plan. Never rewrites historical job receipts or promotes hardware support.",
        requires: &[
            "A successful complete DAYU200 job owned by this standalone runtime; no active jobs.",
            "The same strongly identified board in Normal mode and a hash-bound HDC executable.",
        ],
        produces: &[
            "arkforge.flash-verification/v1 with fullHash, typedSkip, or failed per image range and an independent durable record id.",
        ],
        options: &[
            (
                "--preboot-ramdisk <sha256>",
                "Use the exact host derivative matching the source job's ramdisk write to validate its current-boot pre-mount userdata report; failures are never skips.",
            ),
            (
                "--job <job-id>",
                "Required. Exact completed job reference; no caller paths, ranges, or expected hashes are accepted.",
            ),
            (
                "--budget-ms <u64>",
                "Total device read budget in milliseconds, 1000..600000; defaults to 300000.",
            ),
            (
                "--hardware-campaign <campaign-id>",
                "Exact named campaign of the original job and current runtime; never inherited.",
            ),
        ],
        examples: &[
            "arkforge --output json flash verify --job <job-id> --hardware-campaign <campaign-id>",
        ],
        next: &["arkforge job show --job <job-id>"],
        exits: &[
            (0, "Every image range matched its complete SHA-256 digest."),
            (2, "The job reference or budget is invalid."),
            (
                3,
                "Source, identity, campaign, or execution state refused verification.",
            ),
            (5, "A required job or runtime resource was not found."),
            (
                7,
                "At least one range failed verification; inspect its explicit reason.",
            ),
            (
                9,
                "Partial verification: no failures, but at least one typed skip, such as writable userdata.",
            ),
            (10, "IPC or durable recording failed."),
        ],
    }
