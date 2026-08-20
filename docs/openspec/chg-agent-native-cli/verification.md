# Verification — CHG-2026-CLI

> Status: native-rescue software checks and legacy read-only command consolidation are
> passing; remaining command/authority work and hardware checks are planned. Software tests do not authorize real writes.
> DAYU200 normal CLI authority and native rescue require separate real-device evidence.

## Acceptance matrix

| ID | Verification | Expected result |
|---|---|---|
| CLI-AC-01 | Walk the typed command tree | Every leaf has effect, inputs, constraints, outputs, exits, example and next command |
| CLI-AC-02 | Substitute typed fixtures and parse every generated example without I/O | All canonical examples parse; placeholders and ellipses are never accepted as real hashes/IDs |
| CLI-AC-03 | Snapshot human and JSON help | Both derive from one tree; JSON validates as `arkforge.command-help/v1` |
| CLI-AC-04 | Run JSON/JSONL secret and ANSI scan | Structured stdout contains no ANSI/progress text, path secrets, endpoints, connect keys or permit secret |
| CLI-AC-05 | Compare old and new discover/inspect/assess/job results | Same IPC request semantics and typed payload facts |
| CLI-AC-06 | Compare offline import/inspect and signing results | Same artifact digests/manifests and signing violations |
| CLI-AC-07 | Inspect packaged binaries and canonical handlers | Old wrapper binaries are absent; each retained behavior has one canonical handler |
| CLI-AC-08 | Architecture dependency guard | `arkforged` cannot reference authority permit minting; only the CLI authority supervisor can |
| CLI-AC-09 | Pair CLI runtime and attempt ArkDeck runtime takeover | Dedicated runtime succeeds; paired ArkDeck runtime returns `AUTHORITY_ALREADY_PAIRED`; no takeover flag exists |
| CLI-AC-10 | Permit cross-vectors | CLI canonical CBOR/tag exactly match `arkforge-authority-api` vectors |
| CLI-AC-11 | Permit tamper matrix | Wrong plan/action/effect/device/session/attempt/time/tag/epoch is refused with zero dispatch |
| CLI-AC-12 | Retry submission, then restart the authority supervisor | Same-epoch retry replays exact stored bytes; restart rotates epoch and never first-consumes an old unsent permit; no uncertain action is dispatched twice |
| CLI-AC-13 | Managed-control secret scan | Receipt/journal/event omit HDC path, endpoint, connect key, argv, shell and lifecycle details |
| CLI-AC-14 | Normal-to-Loader identity cases | Success requires accepted command, exact detach and unique allowed rebind; zero/multiple/replacement refuses |
| CLI-AC-15 | Plan with file instead of artifact ID | Parser refuses and points to `artifact import` |
| CLI-AC-16 | Apply without digest, with wrong digest, or missing/extra token | No job starts; typed exact remediation is returned |
| CLI-AC-17 | Apply a sealed full-restore plan | Only sealed effects dispatch, one permit per step, ordered receipts and terminal result |
| CLI-AC-18 | Disconnect the frontend, then issue `job cancel` during a non-interruptible write | Disconnect only ends watching; explicit cancellation queues at a safe boundary and does not kill the write |
| CLI-AC-19 | Kill command frontend/supervisor/daemon at each durable boundary | Frontend loss only ends watching; supervisor/daemon crash table holds and outcome-unknown action is never replayed |
| CLI-AC-20 | Scan dependency graph, device-tool strings and process-spawn calls | Rescue has no rkdeveloptool/vendor executable dependency or device subprocess path |
| CLI-AC-21 | Rebuild ArkForge after creating a plan | Build digest mismatch refuses apply before device mutation |
| CLI-AC-22 | Fuzz rescue command/protocol surface | Only typed discover/GPT read/READ_LBA/WRITE_LBA/DEVICE_RESET actions are reachable; raw USB/vendor argv never reaches execution |
| CLI-AC-23 | Rescue with zero/multiple/replaced devices | Refused before mutation; no first-match behavior |
| CLI-AC-24 | Plan write to missing/protected partition or mismatched image hash | Refused before mutation |
| CLI-AC-25 | Apply rescue write/reset without exact plan digest/tokens | Refused before native mutating USB I/O |
| CLI-AC-26 | Decode RescueReceipt as normal receipt | Type/schema/store boundary rejects it; no normal success or maturity is produced |
| CLI-AC-27 | Force normal flash failure | Returns typed blocker/recovery guide and never invokes rescue automatically |
| CLI-AC-28 | Real DAYU200 CLI-authority full restore | Nine partitions, userdata impact, native RockUSB receipts, reset and exact build postflight succeed |
| CLI-AC-29 | Real read-domain verification | Readable targets are Verified/Failed; unreachable targets are TypedSkip; TypedSkip adds no verified strength |
| CLI-AC-30 | Real multi-device and cable-detach campaign | Exact selection holds; ambiguity/detach cannot migrate the plan to another board |
| CLI-AC-31 | Real native rescue campaign | Each closed action records native build/device/effect/evidence and only its limited RescueReceipt claim |
| CLI-AC-32 | Mechanics + authority gate review | Existing mechanics maturity remains seven-axis; only the reviewed CLI `AuthoritySupportKey` permits it to execute; ArkDeck support, rebuilt supervisors, replay and rescue do not inherit the claim |

## Agent comprehension scenarios

Run these from an empty runtime using only data returned by help/results; the test
driver must not contain hard-coded command-specific recovery logic.

1. Ask `help --format json`, start daemon, discover one device, import firmware,
   assess, plan and apply by following `next_commands`.
2. Omit `data-loss:userdata`, read `required_acknowledgements`, then reissue the
   exact suggested command with the plan digest unchanged.
3. Request DAYU600 execute, receive a maturity/evidence blocker, and stop without
   attempting rescue.
4. Ask for rescue help, select an exact observation, and create/apply a plan without
   installing or locating any external tool.
5. Observe `OUTCOME_UNKNOWN`, follow only `job reconcile` and recovery guidance;
   never retry `flash apply` or `rescue apply` automatically.

## Real-device evidence requirements

Each hardware run records:

- full command manifest version and invocation with secrets redacted;
- CLI frontend, authority supervisor, daemon, native build, HDC, profile, artifact
  and host digests;
- exact device binding and mode lineage;
- plan/effect/permit/receipt/journal digests;
- ordered state/events and terminal classification;
- expected and observed postflight facts;
- whether the run is normal campaign evidence or limited rescue evidence.

A process exit code or vendor stdout marker alone is never sufficient evidence.
