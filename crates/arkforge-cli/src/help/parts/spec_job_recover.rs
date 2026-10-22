HelpSpec {
        command: "job recover",
        summary: "Seal a distinct plan covering the complete outstanding unknown set.",
        usage: "arkforge job recover --job <job-id> (--file <firmware-file> | --artifact <artifact-id>) [--device <observation-id> | --target <selector>] [--profile <id@version>] [--intent <full-restore>] [--hardware-campaign <campaign-id>] [--wait-device <u64>]",
        effect: "Reads the exact device through the paired runtime and stores a sealed host object. It never resumes, edits, replays, or reclassifies any original job; their outcomes and journals stay exactly as they are.",
        requires: &[
            "One exact anchor job in the complete outstanding unknown set; every source requires independent coverage, identity proof and unspent recovery allowance.",
            "Firmware content, and enough evidence to name exactly one device and profile.",
        ],
        produces: &[
            "arkforge.flash-plan/v2 whose plan covers the complete outstanding unknown set under a new epoch and intent. The named job anchors the recovery:supersedes-job token; the Runtime seals and rechecks every source. The assessment projects the same target's effects and gate state.",
        ],
        options: &[
            (
                "--job <job-id>",
                "Exact unresolved anchor job; does not narrow the complete outstanding set. Required.",
            ),
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
                "--wait-device <u64>",
                "Bounded wait in milliseconds for a matching device to appear.",
            ),
        ],
        examples: &[
            "arkforge --output json job recover --job <job-id> --artifact <artifact-id> --profile org.openharmony.dayu200@1.0.0 --device OBS-PREFLIGHT",
        ],
        next: &["Use the returned apply_command verbatim after reviewing the superseding effects."],
        exits: &[
            (0, "Superseding plan sealed; the original job is unchanged."),
            (2, "Inputs are invalid, or no firmware was named."),
            (
                3,
                "No superseding plan was created, or a support precondition refused.",
            ),
            (5, "The runtime, job, or a named resource was not found."),
            (
                6,
                "The device or profile could not be narrowed to exactly one, or the runtime serves another campaign.",
            ),
            (10, "Controller, store, or supervisor failed."),
        ],
    }
