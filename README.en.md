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

The executable runtime and release package currently target macOS. The repository pins Rust 1.97.1 and Edition 2024. Build the workspace, then ask ArkForge what the current host can do:

```bash
cargo build --workspace
target/debug/arkforge help --format json
target/debug/arkforge --runtime-dir /tmp/arkforge doctor
```

Start a local runtime and list observed devices:

```bash
target/debug/arkforge --runtime-dir /tmp/arkforge daemon start
target/debug/arkforge --runtime-dir /tmp/arkforge device list
```

Firmware enters the content-addressed store before it is inspected offline by artifact ID:

```bash
target/debug/arkforge --runtime-dir /tmp/arkforge artifact import --file ./firmware.tar.gz
target/debug/arkforge --runtime-dir /tmp/arkforge artifact inspect \
  --artifact <artifact-id> \
  --profile-file profiles/dayu200.yaml
```

Normal flashing always follows `assess → plan → apply`:

```bash
target/debug/arkforge --runtime-dir /tmp/arkforge flash assess \
  --artifact <artifact-id> \
  --profile org.openharmony.dayu200@1.0.0 \
  --device <observation-id> \
  --intent full-restore

target/debug/arkforge --runtime-dir /tmp/arkforge flash plan \
  --artifact <artifact-id> \
  --profile org.openharmony.dayu200@1.0.0 \
  --device <observation-id> \
  --intent full-restore
```

`plan` does not mutate the device. Execution requires the returned plan ID, SHA-256 digest, and every acknowledgement token to be supplied unchanged. Direct CLI flashing also requires a runtime bound to an HDC binary with the exact expected digest. Until production support is published, execution is available only inside an explicitly authorized hardware campaign.

Do not guess options or reuse historical command names. Ask the current build for its complete contract:

```bash
target/debug/arkforge help flash apply --format json
target/debug/arkforge completion --shell zsh
```

## Core capabilities

### One Agent-native CLI

`arkforge` covers the complete lifecycle:

```text
doctor
device      list / show / probe / wait
artifact    import / inspect / list / show
flash       assess / plan / apply
job         list / show / watch / cancel / reconcile / recovery
rescue      list / inspect / read / plan / apply
daemon      run / start / stop / status
signing     verify
completion
help
```

Every command level has stable human-readable help and an `arkforge.command-help/v1` JSON description. Structured output never mixes in colors, progress bars, or prompts. Errors include a stable code, remediation, and executable next commands.

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

A macOS release consists of separately signed sibling `arkforge` and `arkforged` binaries. [`packaging/macos/package-arkforge.sh`](packaging/macos/package-arkforge.sh) is the packaging entry point, and the release bundle contains no vendor RockUSB tool.

## Learn more

- [Architecture and safety boundaries](docs/architecture.md)
- [Agent-native CLI proposal](docs/openspec/chg-agent-native-cli/proposal.md)
- [CLI acceptance matrix](docs/openspec/chg-agent-native-cli/verification.md)
- [Implementation tracker](TASKS.md)
- [Hardware and acceptance evidence](docs/evidence/)
- [Architecture decision records](docs/decisions/)
