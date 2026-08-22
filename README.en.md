# ArkForge

[简体中文](README.md) · English

**Safe, auditable, and recoverable firmware flashing for OpenHarmony development boards.**

ArkForge is built for development boards that support OpenHarmony. It brings firmware formats, chipset download protocols, and USB transports into one consistent workflow that developers and Agents can use in the same way:

```text
check host → identify device and firmware → assess effects → plan → acknowledge → execute → verify and recover
```

It provides a single `arkforge` CLI and can also serve as the mechanics layer behind ArkDeck. Whichever entry point you use, ArkForge never “finds a device and starts flashing.” It seals the exact device, firmware, profile, data effects, and execution environment into a plan, executes that plan step by step, and records auditable receipts.

## Why ArkForge

OpenHarmony development boards span different chipset platforms, while their firmware operations are often split across vendor tools, scripts, USB protocols, and product-specific code. ArkForge gives those moving parts a clear boundary:

- **One entry point**: discover devices, import firmware, create plans, execute jobs, inspect results, and enter rescue through `arkforge`;
- **Native execution**: DAYU200 uses the in-repository RockUSB implementation, with no vendor flashing tool to install or invoke;
- **Plan before execution**: destructive work requires an exact plan digest and the complete acknowledgement-token set;
- **No blind retries**: a durable journal records every step, and an outcome-unknown write is never replayed automatically after restart;
- **Agent-friendly by design**: one command tree generates human help, stable JSON/JSONL, actionable errors, and shell completion;
- **Device-neutral architecture**: new hardware is added through artifact parsers, Providers, Transports, and data-driven Device Profiles instead of model branches in the product layer;
- **Faster without weaker validation**: the latest pipeline caches sealed artifact manifests, validates staged images in parallel, and avoids full readback of ranges known to be unreadable.

## Current support

ArkForge is currently in the **hardware-qualification stage**. It is not yet a one-click flasher for general users.

| Device | Available capabilities | Status |
| --- | --- | --- |
| **DAYU200** (RK3568 / RockUSB) | Firmware import and inspection, device observation, complete nine-partition overwrite, per-partition verification, durable recovery, and native RockUSB rescue | Native mechanics have completed multiple full-device hardware runs. The standalone CLI authority and rescue software surfaces are complete, but each still requires its own controlled hardware campaign and maintainer review |
| **DAYU600** (uis7885 / PAC) | PAC structure observation, profile candidates, and non-executable PlanAssessment | Research and assessment only. None of the 18 execution evidence gates currently pass, so no flashing entry point is available |

The production support registry is intentionally empty. `--hardware-campaign` opens a named, controlled acceptance run; it is not a `--force` escape hatch and never publishes production support automatically.

See the [implementation tracker](TASKS.md) and [evidence ledger](docs/evidence/ledger.md) for the detailed status.

## Quick start

The runtime supports macOS and Windows x64; Windows signing and physical-device evidence remain a separately accepted maturity combination. The repository pins Rust 1.97.1 and Edition 2024. Build the workspace, then ask ArkForge what the current host can do:

```bash
cargo build --workspace
target/debug/arkforge help --all --format json
target/debug/arkforge --runtime-dir /tmp/arkforge status
```

`status` — which is also what a bare `arkforge` runs — aggregates host, runtime,
devices, artifacts, jobs, and blockers into one `arkforge.status/v1` document. A
section that could not be observed reports `items: null` with a typed `reason`;
only a completed enumeration of zero is `items: []`. It never starts a runtime on
the way to answering.

A command that needs the runtime **brings one up** rather than refusing:

```bash
target/debug/arkforge --runtime-dir /tmp/arkforge device list --deep
```

Auto-start reads the bindings from `config`, is concurrency-idempotent under an
owner-only startup lock (competing commands produce one runtime, and the rest
attach after checking it matches), and is **disclosed**: human output prints a
line, and structured documents carry `runtime_autostarted: true`.
`--no-auto-start` restores the previous typed refusal. A runtime already paired
with ArkDeck is attached to, never taken over. `status` — and a bare `arkforge` —
deliberately never starts one: it has to be able to answer "nothing is running"
without changing that answer.

Reusable local bindings are configured once, owner-only and committed atomically:

```bash
target/debug/arkforge --runtime-dir /tmp/arkforge config set \
  hdc.path=/usr/local/bin/hdc hdc.sha256=<64hex>
target/debug/arkforge --runtime-dir /tmp/arkforge config show
```

A path and the digest of its bytes are **one transaction**, so a configuration
can never name an executable it has not pinned; a relative path is refused; every
pin is re-hashed before the runtime starts and byte drift is a typed refusal; and
a failed write leaves the previous configuration exactly as it was.
`config show --output json` reports binding state, digests, and counts — never a
host or HDC path. `campaign` is not a configuration key: it returns
`CAMPAIGN_NOT_PERSISTABLE`, because a campaign that could be left switched on in
a file would stop meaning that the run was reviewed.

`device list` reports an identification block per device that keeps **compatible
profile** and **physical model** apart, each with its evidence and strength. A USB
vendor/product pair proves a protocol personality, never the board, so a device in
Loader reports `model: null`. `--deep` additionally probes every candidate profile
and reports the facts it returned.

Firmware enters the content-addressed store first; one import returns every staging
fact — CAS, a manifest summary, the profiles declaring that format, and the connected
devices those profiles could flash:

```bash
target/debug/arkforge --runtime-dir /tmp/arkforge artifact import --file ./firmware.tar.gz
target/debug/arkforge --runtime-dir /tmp/arkforge artifact show \
  --artifact <artifact-id> \
  --profile-file profiles/dayu200.yaml
```

Normal flashing is two commands, `plan` then `apply`. One `flash plan` call
imports, identifies, assesses, and seals:

```bash
target/debug/arkforge --runtime-dir /tmp/arkforge flash plan \
  --file ./firmware.tar.gz \
  --profile org.openharmony.dayu200@1.0.0 \
  --device <observation-id>
```

`--file` is an **implicit import**: the bytes enter the content store first, the
hash is sealed into the plan, and the result reports the artifact id — a caller
path never appears in a plan. `--profile` and `--intent` may be omitted when they
can be inferred: the profile is the intersection of the formats the firmware
declares and the USB identities the device matches, adopted only when that
intersection has exactly one member; the intent is defaulted when the combination
admits exactly one. Several candidate devices are narrowed with `--device`
(exact) or `--target` (serial digest, unique prefix of at least four characters,
or a proven model name); the two are mutually exclusive, and ambiguity is always a
typed refusal rather than a default pick.

**Inference never crosses the identity gate.** When this build cannot prove which
board the target is — in Loader or Maskrom there is only a VID/PID and a mode —
sealing a plan requires both an explicit `--profile` and an exact `--device`, or
the call returns `IDENTITY_CONFIRMATION_REQUIRED`. A human assertion does not
raise `strength` to `strong`. `--assess-only` produces the assessment without
materializing a plan and exits 0 even when `executable` is false.

The result is one `arkforge.flash-plan/v2` document: `resolved` (each part with
how it was decided and the evidence behind it), `assessment`, `plan`, and a
directly executable `apply_command`. When a gate does not pass the call exits 3
and the same document appears under `error.facts.flash_plan` with `plan: null` —
the failure path carries what the success path would have.

`plan` does not mutate the device. Execution is the top-level `apply` — the
consent verb shared by a normal flash plan and a recovery plan, because what it
asks of the operator is the same act in both cases: accepting a named set of
destructive effects.

```bash
target/debug/arkforge --runtime-dir /tmp/arkforge apply \
  --plan <plan-id> \
  --expect-plan-sha256 <sha256> \
  --ack <token>
```

The returned plan ID, SHA-256 digest, and every acknowledgement token must be
supplied unchanged: one extra token is `UNEXPECTED_ACKNOWLEDGEMENT`, one missing
token does not pass, and no broad `--yes` or `--force` exists. A rescue plan
(`rescue-plan:<sha256>`) is refused on its identifier shape *before* any
authority store is read and directed to `rescue apply` — rescue is a separate
consent domain. When the runtime serves a hardware campaign, the same id must be
named for this call with `--hardware-campaign`: a campaign is never inherited and
the runtime is never restarted to match an argument, and a mismatch costs zero
dispatch.

`watch` with no arguments follows the single running job; with none running it
reports the most recently active one; several running is a real ambiguity, listed
and refused. `cancel --job --expect-sequence` keeps its semantics.

Direct CLI flashing also requires a runtime bound to an HDC binary with the exact expected digest. Until production support is published, execution is available only inside an explicitly authorized hardware campaign.

Do not guess options or reuse historical command names. Ask the current build for its complete contract:

```bash
target/debug/arkforge help apply --format json
target/debug/arkforge completion --shell zsh
```

## Core capabilities

### One Agent-native CLI

`arkforge` covers the complete lifecycle:

```text
status      aggregate host / runtime / device / artifact / job / blocker snapshot
device      list [--device] [--deep] / wait
artifact    import / list / show
flash       plan [--assess-only]
apply       execute a sealed plan (shared by normal and recovery)
watch       [--job] follows the running job by default
cancel      --job --expect-sequence
job         list / show / reconcile / recovery
rescue      list / inspect / read / plan / apply
config      show / set / unset / add / remove
daemon      run / start / stop
signing     verify
completion
help        [<command path>] / --all
```

The query surface is cut at **decision points** rather than internal resources:
`device list` covers what show and probe used to, `artifact show` absorbed the offline
inspect, and `job show` embeds the event tail, every action receipt, and the no-replay
recovery block. Every embedded section in a composite document reports its own
availability — an unobservable one is `items: null` with a typed reason, never an empty
set wearing a plausible shape.

Every command level has stable human-readable help and an `arkforge.command-help/v1` JSON description. `help --all` — and structured `help` without a path — returns the whole tree as one `arkforge.command-help-index/v1`, whose leaves are byte-identical to the per-path queries and each declare `runtime_effect` and `facts_projections`. Structured output never mixes in colors, progress bars, or prompts. Errors include a stable code, remediation, and executable next commands.

### An auditable safety model

- Device selection starts from an exact observation; zero matches, multiple matches, or identity changes are refused before mutation;
- Firmware is stored by content digest, and plans bind the artifact, profile, device, toolchain, effects, and authority;
- Every mutating or destructive step requires a single-use StepPermit;
- Apply requires the complete plan digest and acknowledgement set—there is no broad `--yes` or `--force`;
- Job journals remain queryable across process restarts, and `outcomeUnknown` is never replayed automatically;
- Normal flash and rescue use separate plan, receipt, and evidence domains. A normal flash failure never falls back to rescue automatically.

### Native RockUSB and explicit rescue

DAYU200 enumeration, Loader transitions, partition I/O, reset, and read-domain-aware verification are implemented by `arkforged` over native RockUSB. Rescue reuses the same typed protocol surface, but it is available only through an explicit `arkforge rescue ...` workflow. It does not expose arbitrary USB requests, raw-LBA writes, a shell, or vendor arguments.

### Standalone or integrated with ArkDeck

In standalone mode, the local `arkforge` supervisor acts as the authority: it binds the target, issues exact permits, and uses typed HDC control for mode transitions and postflight checks. `arkforged` remains responsible only for firmware parsing, protocols, USB, writes, verification, and durable state.

When integrated, ArkDeck can provide the authority through `arkforge-arkdeck-adapter`. The ArkDeck and standalone runtimes use separate namespaces and cannot take over each other's paired daemon. ArkForge Core has no dependency on ArkDeck types.

## Development and verification

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
```

The workspace has no third-party Rust runtime dependencies. SHA-256, deterministic CBOR, DEFLATE, tar, and the Protobuf wire codec are implemented in the repository and checked against published test vectors. See [AFD-0001](docs/decisions/AFD-0001-zero-dependency-core.md) for the rationale.

A macOS release is one `ArkForge.bundle`: separately signed `arkforge` and `arkforged` binaries plus published profiles are bound member-by-member by `Contents/Resources/arkforge-bundle.json`. [`packaging/macos/package-arkforge.sh`](packaging/macos/package-arkforge.sh) is the packaging entry point, and the bundle contains no vendor RockUSB tool. Swift consumers use `ArkForgeProtocol` and `ArkForgeClient` from [`swift/ArkForgeSDK`](swift/ArkForgeSDK) instead of copying the IPC codec.

The Windows release surface uses local-only Named Pipes, current-user ACLs,
WinUSB, and Authenticode. See
[`packaging/windows/README.md`](packaging/windows/README.md) for packaging,
driver binding, installation, and tiered acceptance. A production package must
use a catalog returned by the Windows Hardware Developer Program; an
application certificate is not treated as a production driver signature.

## Learn more

- [Architecture and safety boundaries](docs/architecture.md)
- [Agent-native CLI proposal](docs/openspec/chg-agent-native-cli/proposal.md)
- [CLI acceptance matrix](docs/openspec/chg-agent-native-cli/verification.md)
- [Implementation tracker](TASKS.md)
- [Hardware and acceptance evidence](docs/evidence/)
- [Architecture decision records](docs/decisions/)
