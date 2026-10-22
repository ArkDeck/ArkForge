HelpSpec {
        command: "job reconcile",
        summary: "Assess possible effects without replaying an unresolved job.",
        usage: "arkforge job reconcile --job <job-id> [--hardware-campaign <campaign-id>]",
        effect: "Read-only controller assessment. It never dispatches, resumes, edits, or reclassifies the original job optimistically.",
        requires: &["A durable job in the selected paired runtime."],
        produces: &[
            "arkforge.job-reconciliation/v1 with immutable original state, possible-effect completeness, effects, and verdict.",
        ],
        options: &[
            (
                "--hardware-campaign <campaign-id>",
                "Exact campaign served by the running runtime; required for a campaign runtime and never inherited.",
            ),
            ("--job <job-id>", "Exact durable job; required."),
        ],
        examples: &["arkforge --output json job reconcile --job JOB-EXAMPLE"],
        next: &["arkforge job show --job <job-id>"],
        exits: &[
            (0, "No unresolved outcome remains."),
            (5, "Runtime or job was not found."),
            (
                8,
                "Original outcome remains unknown; never retry automatically.",
            ),
            (10, "Controller or supervisor failed."),
        ],
    }
