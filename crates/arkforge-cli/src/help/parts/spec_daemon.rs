HelpSpec {
        command: "daemon",
        summary: "Run and manage one paired local ArkForge runtime.",
        usage: "arkforge daemon <run|start|stop> [options]",
        effect: "Service lifecycle. The supervisor owns pairing authority; lifecycle commands do not flash a device.",
        requires: &[
            "arkforged installed beside the canonical arkforge executable.",
            "The bindings stored by config, which lifecycle commands read and explicit arguments override.",
        ],
        produces: &[
            "Typed two-process runtime status with protocol, authority epoch, readiness, active jobs, and blockers.",
        ],
        options: &[],
        examples: &["arkforge daemon start", "arkforge status"],
        next: &["arkforge device list"],
        exits: &[
            (0, "Requested lifecycle operation completed."),
            (3, "An exact executable or signing binding was refused."),
            (5, "The runtime or mechanics daemon is unavailable."),
            (6, "The runtime is already running or has active jobs."),
            (10, "Supervisor or local IPC failed."),
        ],
    }
