# Recover a DAYU200 boot loop after a temporary ramdisk

`scripts/restore-boot-loop.py` watches for the same board in Normal or Loader
mode and submits one separate original-ramdisk restoration through the running
ArkForge CLI/Runtime. It is intended for a full flash whose writes, verification
steps and final reboot all have confirmed success receipts, but whose system
does not return reliably after that reboot.

The script restores the pinned original gzip plus zeros across the complete
4 MiB ramdisk extent. It never rewrites userdata, replays the old flash, edits
authority records, invokes a raw transport or falls through to native rescue.
An unknown or partial source outcome requires its own Runtime recovery path;
this script refuses it. This workflow does not provide arbitrary-partition or
MaskROM recovery, and does not promote a device's production support status.

Use the published CLI and a running Runtime in the same explicitly authorized
hardware campaign. Import the restoration bundle produced by
`scripts/prepare-preboot-firmware.py` before starting. The source job must be the
sole active job, or the latest durable job when none is active. Both the CLI and
the Runtime continue to enforce their ordinary identity and admission gates.

```sh
python3 scripts/restore-boot-loop.py \
  --arkforge /path/to/published/arkforge \
  --runtime-dir /path/to/campaign-runtime \
  --source-job JOB-... \
  --artifact <imported-restoration-bundle-sha256> \
  --hardware-campaign <campaign-name> \
  --ack overwrite:partition=ramdisk \
  --out-dir /path/to/new-restoration-records \
  --wait-seconds 600 --job-timeout-seconds 600
```

On Windows use `python` and the published `arkforge.exe` path. Output directories
must be new. The explicit acknowledgement authorizes this single restoration;
the script still reviews the returned plan's exact digest, campaign, inputs,
steps, pinned extent and acknowledgement set before applying it.

The listener polls Runtime's read-only device list and requires one supported
device with strong identity. Its `flash restore-ramdisk --source-job` call
requires Runtime to match a fresh probe against the source's durable original
or current target binding, including serial, topology, descriptor and protocol
facts. Runtime independently checks every source action's success receipt
through the final reboot. Live admission events are extra corroboration when
available; they are not required after restart, when durable event cursors can
be sparse. No caller identity file is trusted or authority record modified.

Only after that check and plan review does the script stop a still-waiting
source at its confirmed final-reboot boundary using its exact current sequence.
If a device disappears during read-only planning, the listener may resume within
its wait budget. Runtime checks the live session again before dispatch.

The script records an apply intent locally before submitting once. An apply
error, lost reply, timeout or non-success outcome stops the script without
retrying. Inspect the retained Runtime job and receipts; do not rerun to repeat
a write. A newer restoration job prevents the original source from being
silently reused. The original Runtime journal is retained, including any safe
cancellation appended by Runtime.

Success requires a succeeded restoration job followed by a separate full-hash
verification of the entire original ramdisk, with zero skips and failures.
No userdata verification is inferred. All CLI responses remain local in the
new output directory; publishing them is a separate user decision.

A USB listener can only act when firmware actually exposes a supported USB
mode. If no usable session appears, it exits without restoration and asks for
physical Loader entry. A visual reboot loop alone does not establish USB
availability or prove which boot stage failed.
