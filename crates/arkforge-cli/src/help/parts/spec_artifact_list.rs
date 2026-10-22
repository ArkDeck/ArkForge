HelpSpec {
        command: "artifact list",
        summary: "List every stored object with the format and profiles it parses as.",
        usage: "arkforge artifact list",
        effect: "Read-only when a store exists. An absent store is reported as an empty list and is not created. Each stored object is parsed to report its format, so listing costs one container read per object.",
        requires: &[],
        produces: &[
            "arkforge.artifact-list/v1 with artifact ids, byte sizes, the parsed format, and the profiles declaring it; an object that cannot be parsed carries a null format and a typed reason.",
        ],
        options: &[],
        examples: &["arkforge --output json artifact list"],
        next: &["arkforge artifact show --artifact <artifact-id>"],
        exits: &[
            (0, "Artifact list produced, including an empty list."),
            (10, "The content store could not be read."),
        ],
    }
