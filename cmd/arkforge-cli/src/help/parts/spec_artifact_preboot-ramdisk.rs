HelpSpec {
        command: "artifact preboot-ramdisk",
        summary: "Derive the pinned temporary DAYU200 acceptance ramdisk on the host.",
        usage: "arkforge artifact preboot-ramdisk --file <original-ramdisk.img> --support-archive <pinned.cpio.gz> --out <new-image> --restore-out <new-image>",
        effect: "Host-write only. Preserves the pinned base, adds the closed pre-mount verifier and a random nonce, and saves a content-addressed derivative. Does not access the device.",
        requires: &[
            "The exact pinned original DAYU200 ramdisk, its closed compressed crypto dependency archive, and new output paths.",
        ],
        produces: &[
            "arkforge.preboot-ramdisk/v1 with derivative digest, original digest, nonce and size.",
        ],
        options: &[
            ("--file <file>", "Required. Pinned original ramdisk."),
            (
                "--support-archive <file>",
                "Required. Exact pinned gzip/newc dependency archive; arbitrary libraries and scripts are refused.",
            ),
            (
                "--restore-out <new-file>",
                "Required. Original ramdisk padded with zeros to its full partition extent, clearing every temporary overlay byte.",
            ),
            (
                "--out <new-file>",
                "Required. New host output; never overwritten.",
            ),
        ],
        examples: &[
            "arkforge artifact preboot-ramdisk --file ramdisk.img --support-archive preboot-support.cpio.gz --out acceptance-ramdisk.img --restore-out restore-ramdisk.img",
        ],
        next: &[
            "Build a derived firmware bundle preserving all other members, then seal an ordinary full-restore plan.",
        ],
        exits: &[
            (0, "Derivative saved; no device accessed."),
            (2, "Invalid arguments."),
            (3, "Pinned input or output refused."),
        ],
    }
