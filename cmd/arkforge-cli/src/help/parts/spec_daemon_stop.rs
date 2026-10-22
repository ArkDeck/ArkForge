HelpSpec {
        command: "daemon stop",
        summary: "Stop an idle paired runtime without a force path.",
        usage: "arkforge daemon stop",
        effect: "Service lifecycle. Stops both processes only when no durable job is active; it never cancels a job implicitly.",
        requires: &["A live runtime with zero active jobs."],
        produces: &["arkforge.daemon-stop/v1."],
        options: &[],
        examples: &["arkforge --output json daemon stop"],
        next: &["arkforge daemon start"],
        exits: &[
            (0, "Idle runtime stopped."),
            (5, "No runtime is listening."),
            (6, "Active jobs prevent stopping."),
            (10, "Stop IPC failed."),
        ],
    }
