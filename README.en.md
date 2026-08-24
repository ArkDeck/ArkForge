# ArkForge

[简体中文](README.md) · English

ArkForge is an open, device-neutral firmware operations platform. It brings artifact parsing,
device identification, flashing protocols, USB transport, verification, and recovery into one
native Rust workflow. Run it standalone or use it as the mechanics layer behind
[ArkDeck](https://github.com/ArkDeck/ArkDeck).

It never “finds a device and starts flashing.” ArkForge first seals the exact device, firmware,
profile, data effects, and execution environment into a plan. That plan runs only after explicit
consent, with an auditable record for every step.

```text
identify device and firmware → assess effects → seal plan → acknowledge → execute → verify and recover
```

## Status

ArkForge is in the **hardware-qualification stage**. The production support registry is still
empty; general users should not treat it as a one-click flasher.

| Device | Capabilities | Status |
| --- | --- | --- |
| **DAYU200** (RK3568 / RockUSB) | Firmware inspection, device observation, complete nine-partition overwrite, per-partition verification, durable recovery, and native RockUSB rescue | Native mechanics have completed multiple full-device hardware runs. The standalone CLI authority and rescue paths still need their own controlled hardware acceptance and maintainer review |
| **DAYU600** (uis7885 / PAC) | PAC structure observation, profile candidates, and non-executable PlanAssessment | Research and assessment only. None of the 18 execution evidence gates currently pass, so no flashing entry point is available |

`--hardware-campaign` opens a named, controlled acceptance run. It is not `--force`, never bypasses
safety gates, and never publishes production support automatically. See the
[implementation tracker](TASKS.md) and [evidence ledger](docs/evidence/ledger.md) for current details.

## Build

The repository pins Rust 1.98.0 and Edition 2024. The runtime currently targets macOS and Windows
x64; Windows release signing and physical-device evidence are accepted as a separate maturity
combination.

```bash
git clone https://github.com/ArkDeck/ArkForge.git
cd ArkForge
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
are implemented by `arkforged` over native RockUSB. Release packages do not install or invoke a
vendor RockUSB flashing tool.

## Embed ArkForge

| Surface | Use |
| --- | --- |
| `arkforge` | Unified CLI for developers and Agents, covering status, device, artifact, flash, apply, job, and rescue |
| `arkforged` | Local mechanics daemon responsible for parsing, protocols, USB, writes, verification, and durable state |
| [`arkforge-client`](crates/arkforge-client) | Rust application API and typed client |
| [`ArkForgeSDK`](swift/ArkForgeSDK) | `ArkForgeProtocol` and `ArkForgeClient` for Swift |
| [`arkforge-arkdeck-adapter`](adapters/arkforge-arkdeck-adapter) | Lets ArkDeck provide authority without introducing ArkDeck types into ArkForge Core |

The standalone CLI and ArkDeck use separate runtime namespaces and cannot take over each other's
paired daemon. Authority decides who may perform which operation on which device; ArkForge mechanics
only lowers an authorized plan through the selected artifact format, Provider, and Transport.

## Develop

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
```

The Rust runtime has no third-party dependencies. SHA-256, deterministic CBOR, DEFLATE, tar, and
the Protobuf wire codec are implemented in the repository and checked against published test
vectors. See [AFD-0001](docs/decisions/AFD-0001-zero-dependency-core.md) for the rationale.

## Documentation

- [Normative, language-neutral specification](spec/README.md) — state machines, digest models,
  port contracts, stable errors, and conformance fixtures
- [Architecture and safety boundaries](docs/architecture.md)
- [Agent-native CLI design](docs/openspec/chg-agent-native-cli/proposal.md)
- [CLI acceptance matrix](docs/openspec/chg-agent-native-cli/verification.md)
- [Hardware and acceptance evidence](docs/evidence/)
- [Architecture decision records](docs/decisions/)
- [Windows x64 packaging and acceptance](packaging/windows/README.md)

## License

Apache-2.0
