HelpSpec {
        command: "apply",
        summary: "Execute one sealed plan after exact digest and acknowledgement equality.",
        usage: "arkforge apply --plan <plan-id> --expect-plan-sha256 <sha256> --ack <token> [--ack <token>...] [--hardware-campaign <campaign-id>] [--detach]",
        effect: "Destructive. Starts only the exact sealed plan after digest and acknowledgement equality; the supervisor mints one durable single-use permit per admitted step. There is no broad --yes or --force.",
        requires: &[
            "A live paired authority supervisor and fresh exact target continuity.",
            "The exact plan digest and exactly every returned acknowledgement token, with no extras.",
            "The runtime's hardware campaign named for this call, when it serves one.",
        ],
        produces: &[
            "arkforge.job-event/v1 and arkforge.command-result/v1 with a durable job id, ordered events, and terminal classification; --detach returns arkforge.apply/v1 after durable job creation while authority continues.",
        ],
        options: &[
            (
                "--plan <plan-id>",
                "Exact sealed normal-flash or recovery plan; required. A rescue-plan id is refused and directed to rescue apply.",
            ),
            (
                "--expect-plan-sha256 <sha256>",
                "Caller expectation for sealed plan bytes; required.",
            ),
            (
                "--ack <token>",
                "Exact required effect token; repeat exactly as returned.",
            ),
            (
                "--hardware-campaign <campaign-id>",
                "Named acceptance campaign the running runtime already serves; never inherited and never restarted to match.",
            ),
            (
                "--detach",
                "Return after job creation; does not cancel or transfer authority.",
            ),
        ],
        examples: &[
            "arkforge apply --plan PLAN-EXAMPLE --expect-plan-sha256 <64-lowercase-hex> --ack data-loss:userdata",
        ],
        next: &["arkforge watch --job <job-id>"],
        exits: &[
            (0, "Detached job created or watched job succeeded."),
            (
                2,
                "Inputs are invalid, or the plan belongs to the rescue domain.",
            ),
            (
                3,
                "Plan, target, authority, campaign, or freshness precondition refused.",
            ),
            (4, "Plan digest or acknowledgement set is not exact."),
            (5, "Runtime or plan was not found."),
            (6, "The runtime serves another hardware campaign."),
            (7, "Operation ended with a known non-success outcome."),
            (8, "Outcome is unknown; never retry automatically."),
            (9, "Watching ended without a terminal result."),
            (10, "Controller, supervisor, or journal failed."),
        ],
    }
