HelpSpec {
        command: "flash run",
        summary: "Import, identify, assess, seal, accept, and write, in one command.",
        usage: "arkforge flash run [FILE] [--file <firmware-file> | --artifact <artifact-id>] [--device <observation-id> | --target <selector>] [--profile <id@version>] [--intent <full-restore>] [--hardware-campaign <campaign-id>] [--ack <token>]... [--wait-device <u64>] [--detach]",
        effect: "Destructive. Every stage before consent only reads or writes host storage; the write itself passes the same sealed plan, exact digest, and exact acknowledgement set the two-command path does. There is no broad --yes or --force.",
        requires: &[
            "A running paired CLI authority supervisor, started for this call if none is listening.",
            "Firmware content, named or selected.",
            "Consent: a confirmation screen on a terminal, or exactly the sealed tokens as --ack.",
            "A durable arkforge.cli-approval/v1 record; if it cannot be written, nothing is dispatched.",
        ],
        produces: &[
            "arkforge.job-event/v1 and arkforge.command-result/v1 with a durable job id and terminal classification; --detach returns arkforge.flash-run/v1. A missing acknowledgement returns the sealed plan and the exact apply command under error.facts, so the plan is never materialized twice.",
        ],
        options: &[
            (
                "--file <firmware-file>",
                "Firmware container imported into the content store before the plan binds it.",
            ),
            (
                "--artifact <artifact-id>",
                "Exact already-imported content id; conflicts with --file.",
            ),
            (
                "--device <observation-id>",
                "Exact current observation; conflicts with --target.",
            ),
            (
                "--target <selector>",
                "Serial digest, unique identifier prefix of at least four characters, or proven product model; conflicts with --device.",
            ),
            (
                "--profile <id@version>",
                "Exact loaded profile identity; inferred when the compatible set has exactly one member.",
            ),
            (
                "--intent <full-restore>",
                "Semantic intent; defaulted when the profile and format admit exactly one.",
            ),
            (
                "--hardware-campaign <campaign-id>",
                "Named acceptance campaign the running runtime must serve; never inherited.",
            ),
            (
                "--ack <token>",
                "Exact required effect token; repeat exactly as the plan declares. Required when no confirmation screen is available.",
            ),
            (
                "--wait-device <u64>",
                "Bounded wait in milliseconds for a matching device to appear.",
            ),
            (
                "--detach",
                "Return after job creation; does not cancel or transfer authority.",
            ),
        ],
        examples: &[
            "arkforge flash run --file <firmware-file> --profile org.openharmony.dayu200@1.0.0 --device OBS-PREFLIGHT --ack data-loss:userdata",
        ],
        next: &["arkforge watch --job <job-id>"],
        exits: &[
            (0, "The device was written and the job succeeded."),
            (2, "Inputs are invalid, or no firmware was named."),
            (
                3,
                "Mechanics, authority, campaign, or physical identity precondition refused.",
            ),
            (
                4,
                "Consent was declined, or the acknowledgement set is not exact.",
            ),
            (5, "A named resource, device, or runtime is unavailable."),
            (
                6,
                "The device or profile could not be narrowed to exactly one, or an approval conflicts.",
            ),
            (7, "Operation ended with a known non-success outcome."),
            (8, "Outcome is unknown; never retry automatically."),
            (9, "Tracking ended without a terminal result."),
            (10, "Controller, store, approval, or supervisor failed."),
        ],
    }
