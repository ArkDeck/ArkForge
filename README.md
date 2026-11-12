# ArkForge

[简体中文](README.zh-CN.md) · English

[![Rust](https://github.com/ArkDeck/ArkForge/actions/workflows/rust.yml/badge.svg)](https://github.com/ArkDeck/ArkForge/actions/workflows/rust.yml)
[![Windows](https://github.com/ArkDeck/ArkForge/actions/workflows/windows.yml/badge.svg)](https://github.com/ArkDeck/ArkForge/actions/workflows/windows.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)

ArkForge is an open, device-neutral firmware operations platform. It brings artifact parsing,
device identification, flashing protocols, USB transport, verification, and recovery into one
native Rust workflow. Run it standalone or use it as the flashing layer behind
[ArkDeck](https://github.com/ArkDeck/ArkDeck), the main toolkit of the
[ArkDeck organization](https://github.com/ArkDeck). Trace tooling lives in
[ArkTrace](https://github.com/ArkDeck/ArkTrace).

It never “finds a device and starts flashing.” ArkForge first seals the exact device, firmware,
profile, data effects, and execution environment into a plan. That plan runs only after explicit
consent, with an auditable record for every step.

```text
identify device and firmware → assess effects → seal plan → acknowledge → execute → verify and recover
```

## Status

ArkForge is in development and in the **hardware-qualification stage**. There are no tagged
releases; build from source. The production support registry is still empty; general users should
not treat it as a one-click flasher.

| Device | Capabilities | Status |
| --- | --- | --- |
| **DAYU200** (RK3568 / RockUSB) | Firmware inspection, device observation, complete nine-partition overwrite, per-partition verification, durable recovery, and native RockUSB rescue | Native mechanics have completed multiple full-device hardware runs. The standalone CLI authority and rescue paths still need their own controlled hardware acceptance and maintainer review. Ledger (2026-08-19): [AF-V1](docs/archive/TASKS.md#af-v1), [AF-V2](docs/archive/TASKS.md#af-v2), [NRU](docs/archive/TASKS.md#nru) |
| **DAYU600** (uis7885 / PAC) | PAC structure observation, profile candidates, and non-executable PlanAssessment | Research and assessment only. None of the 18 execution evidence gates currently pass, so no flashing entry point is available |

`--hardware-campaign` opens a named, controlled acceptance run. It is not `--force`, never bypasses
safety gates, and never publishes production support automatically. See the
[evidence ledger](docs/evidence/ledger.md) for current details. The stage ledger is archived at [docs/archive/TASKS.md](docs/archive/TASKS.md).

## Requirements

- Rust stable via [`rust-toolchain.toml`](rust-toolchain.toml); the minimum supported Rust version
  is 1.99.1, with Edition 2024
- macOS or Windows x64 as the host. Linux has no native USB backend and is not a supported host
- Swift 6.0 or newer, only for the Swift SDK (`swift test`)
- Python 3, only for the preboot and boot-loop scripts in [`hack/`](hack/)
- [`just`](https://github.com/casey/just) (optional) for the shortcuts in the root [`justfile`](justfile)

The runtime workspace has no third-party crates. SHA-256, deterministic CBOR, DEFLATE, tar, and the
Protobuf wire codec are implemented in the repository and checked against published test vectors.
See [AFD-0001](docs/decisions/AFD-0001-zero-dependency-core.md) for the rationale.

## Build

```bash
git clone https://github.com/ArkDeck/ArkForge.git
cd ArkForge
rustup update stable
cargo build --workspace
target/debug/arkforge
```

A bare `arkforge` is equivalent to `arkforge status`: it reports host, runtime, device, artifact,
job, and blocker state without starting the runtime or mutating a device.

## Run ArkForge

Inspect the host and discover devices:

```bash
target/debug/arkforge status
target/debug/arkforge device list --deep
```

Commands that need the runtime start the local `arkforged` from configured bindings. Use
`--no-auto-start` to refuse instead. A reusable HDC binding must pin both an absolute path and the
digest of its bytes:

```bash
target/debug/arkforge config set \
  hdc.path=/usr/local/bin/hdc hdc.sha256=<64hex>
target/debug/arkforge config show
```

Firmware enters a content-addressed store first. Import reports the artifact ID, manifest summary,
candidate profiles, and matching connected devices:

```bash
target/debug/arkforge artifact import --file ./firmware.tar.gz
target/debug/arkforge artifact show \
  --artifact <artifact-id> \
  --profile-file profiles/dayu200.yaml
```

With an operator at a terminal, normal flashing has a one-command entry point:

```bash
target/debug/arkforge flash run --file ./firmware.tar.gz
```

ArkForge presents interactive consent only when stdin, stdout, and stderr are all TTYs, output is
human-readable, and `--no-input` is absent. Redirected, piped, and structured invocations remain
non-interactive and never hang waiting for input.

For staged review, seal the plan first, then return its exact ID, digest, and complete
acknowledgement-token set:

```bash
target/debug/arkforge flash plan \
  --file ./firmware.tar.gz \
  --profile org.openharmony.dayu200@1.0.0 \
  --device <observation-id>

target/debug/arkforge apply \
  --plan <plan-id> \
  --expect-plan-sha256 <sha256> \
  --ack <token>
```

Until production support is published, any real write also requires an explicitly authorized
hardware campaign and the matching device, artifact, toolchain, platform, and authority maturity.

After a DAYU200 flash completes and the board returns to Normal mode, independently
verify the original job's complete image ranges:

```bash
target/debug/arkforge --output json flash verify \
  --job <job-id> --hardware-campaign <campaign-id>
```

The runtime derives bounds and SHA-256 values from the sealed source plan, checks
current block mappings and mounts, and saves a separate verification record. This
covers static partitions beyond the Loader read window; writable `userdata` after
boot remains an explicit `typedSkip`. Exit 0 means every image range matched, 9
means partial verification with skips, and 7 means a verification failure. Original
job receipts remain unchanged.

If a temporary acceptance ramdisk leaves the board in a boot loop after the
original job confirmed its final reboot, use the
[boot-loop restoration script](docs/boot-loop-restoration.md). It checks the
same board's Normal and Loader identity, submits one restoration through the
runtime, and verifies the full hash. Without USB enumeration, the board still
has to be put into Loader mode by hand.

Do not guess options or reuse historical command names. The current binary provides complete human
help, a stable JSON contract, and shell completion:

```bash
target/debug/arkforge help --all --format json
target/debug/arkforge completion --shell zsh
```

## Safe by default

- **Exact target**: every plan binds one named device observation; zero matches, multiple matches,
  or identity drift are refused before mutation.
- **Content addressed**: firmware is stored by digest, never by a caller path in the plan. The plan
  also binds the profile, toolchain, effects, and authority.
- **Exact consent**: execution requires the complete plan digest and acknowledgement set. There is
  no broad `--yes` or `--force`.
- **No blind retries**: a durable journal records execution boundaries. An outcome-unknown write is
  never replayed automatically after restart.
- **Explicit rescue**: normal flash and rescue use separate plans, receipts, and evidence domains;
  one never silently falls back to the other.
- **Agent-native**: one command tree produces stable JSON/JSONL, typed errors, executable
  remediation, and shell completion.

DAYU200 enumeration, Loader transitions, partition I/O, reset, and read-domain-aware verification
are implemented by `arkforged` over native RockUSB. ArkForge does not install or invoke a vendor
RockUSB flashing tool.

## Embed ArkForge

| Surface | Use |
| --- | --- |
| `arkforge` | Unified CLI for developers and Agents, covering status, device, artifact, flash, apply, job, and rescue |
| `arkforged` | Local mechanics daemon responsible for parsing, protocols, USB, writes, verification, and durable state |
| [`arkforge-client`](crates/arkforge-client) | Rust application API and typed client |
| [`ArkForgeSDK`](swift/ArkForgeSDK) | `ArkForgeProtocol` and `ArkForgeClient` for Swift |
| [`arkforge-arkdeck-adapter`](crates/arkforge-arkdeck-adapter) | Lets ArkDeck provide authority without introducing ArkDeck types into ArkForge Core |

The standalone CLI and ArkDeck use separate runtime namespaces and cannot take over each other's
paired daemon. Authority decides who may perform which operation on which device; ArkForge mechanics
only lowers an authorized plan through the selected artifact format, Provider, and Transport.

## Develop

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
```

The root [`justfile`](justfile) wraps those commands:

| Recipe | What it runs |
| --- | --- |
| `just build` | `cargo build --workspace` |
| `just test` | `cargo test --workspace --all-targets` |
| `just lint` | clippy with `-D warnings` |
| `just fmt` | `cargo fmt --all` (formats in place) |
| `just ci` | the host checks from [`rust.yml`](.github/workflows/rust.yml): format check, preboot script tests, clippy, workspace tests, and `swift test` |

Windows packaging guards stay in [`windows.yml`](.github/workflows/windows.yml). `just` is a
developer convenience, not a runtime dependency.

### Continuous integration

| Workflow | Trigger | Host | What runs |
| --- | --- | --- | --- |
| [`rust.yml`](.github/workflows/rust.yml) | push, pull request | `macos-latest` | `cargo fmt`, clippy `-D warnings`, `cargo test --workspace --all-targets`, the Swift SDK, and the preboot scripts |
| [`windows.yml`](.github/workflows/windows.yml) | push, pull request | `windows-latest` | The same Rust checks, plus Windows packaging parse and no-hardware acceptance guards. The workspace tests compile `platform_windows.rs` / `windows_topology.rs` and run USB transcript replay, including `golden_transcript_parity` |
| [`fuzz.yml`](.github/workflows/fuzz.yml) | push, pull request | `ubuntu-latest` | A time-boxed `cargo-fuzz` smoke of every target in [`fuzz/`](fuzz/) |
| [`fuzz-nightly.yml`](.github/workflows/fuzz-nightly.yml) | nightly, manual | `ubuntu-latest` | A longer libFuzzer run per target |
| [`windows-acceptance.yml`](.github/workflows/windows-acceptance.yml) | manual only | self-hosted Windows x64 with a DAYU200 | Hardware acceptance; not a pull request gate |

Seeded parser mutations also run in every `cargo test`
([`crates/arkforge-artifact/tests/parser_fuzz.rs`](crates/arkforge-artifact/tests/parser_fuzz.rs)).
Fuzz scope, corpus, and the rule that every rejection is a typed error are in
[`fuzz/README.md`](fuzz/README.md).

## Repository layout

| Path | Contents |
| --- | --- |
| [`cmd/`](cmd/) | Binaries: the `arkforge` CLI and the `arkforged` daemon |
| [`crates/`](crates/) | Library crates: core, artifact, engine, provider, transport, USB, platform, IPC, client, authority API, standalone authority, ArkDeck adapter, and conformance |
| [`swift/ArkForgeSDK/`](swift/ArkForgeSDK/) | Swift SDK, built by the root [`Package.swift`](Package.swift) |
| [`profiles/`](profiles/) | Device profiles (`dayu200.yaml`, `dayu600.yaml`) |
| [`api/`](api/) | Protocol definitions |
| [`fuzz/`](fuzz/) | Separate `cargo-fuzz` workspace |
| [`test/`](test/) | USB transcript fixtures |
| [`packaging/`](packaging/) | macOS and Windows packaging scripts |
| [`hack/`](hack/) | Preboot firmware preparation and boot-loop restoration scripts |
| [`docs/`](docs/) | Specification, architecture, decisions, evidence, and the archive |

## Documentation

- [Normative, language-neutral specification](docs/spec/README.md) — state machines, digest models,
  port contracts, stable errors, and conformance fixtures
- [Architecture and safety boundaries](docs/architecture.md)
- [Hardware and acceptance evidence](docs/evidence/) and the [evidence ledger](docs/evidence/ledger.md)
- [Architecture decision records](docs/decisions/)
- [Windows x64 packaging and acceptance](packaging/windows/README.md)
- [ArkDeck consumer contract and version pin](docs/arkdeck-contract.md)
- [DAYU200 boot-loop restoration](docs/boot-loop-restoration.md)
- [Agent-native CLI design](docs/archive/openspec/chg-agent-native-cli/proposal.md) and
  [CLI acceptance matrix](docs/archive/openspec/chg-agent-native-cli/verification.md) (archived change drafts)
- [Changelog](CHANGELOG.md)

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md) first. [AGENTS.md](AGENTS.md) defines the agent and
maintainer boundary. Community expectations are in [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).

## Security

Report vulnerabilities privately, as described in [SECURITY.md](SECURITY.md). Do not open a public
issue for a security report.

## License

ArkForge is released under the [MIT License](LICENSE).
