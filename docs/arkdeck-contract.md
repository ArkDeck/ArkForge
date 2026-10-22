# ArkDeck consumer contract

ArkDeck pins one ArkForge commit and checks the constants in `arkforge-ipc`
and `arkforge-arkdeck-adapter`. This document is the compatibility rule for
that pin, including the boot-loop and `flash verify` outcomes ArkDeck shows
to an operator.

The wire schema is `proto/arkforge.proto`. The numeric handshake is
`arkforge_ipc::PROTOCOL_MAJOR` / `PROTOCOL_MINOR`. The dotted form and the
recovery exits live in `arkforge_ipc::contract`. The adapter re-exports those
constants and adds `ARKDECK_CONTRACT_VERSION`.

## Identifiers

Development pins a full git commit. `CONTRACT_TAG` is an in-process identity
string (`arkforge-contract-v1.0.0`). It is the label of this contract revision,
and ArkDeck's `rust/scripts/check-arkforge-pin.py` rejects a Cargo `tag` or
`branch` key. This repository does not cut a git tag or a GitHub Release to
publish the contract while development continues.

| Identifier | Current value | What ArkDeck does with it |
| --- | --- | --- |
| Git pin | 40-hex `rev` | The same full commit on every `arkforge-*` crate in `rust/Cargo.toml`. |
| `contract::CONTRACT_TAG` | `arkforge-contract-v1.0.0` | Compile-time identity string for this contract revision. |
| `contract::PROTOCOL_VERSION` | `1.0` | Compile-time and `Hello` / `HelloAck` handshake. Major `1`, minor `0`. |
| `ARKDECK_CONTRACT_VERSION` | `arkforge.arkdeck-contract/1.0.0` | Compile-time assert beside `CONTRACT_TAG`. Same `1.0.0`. |
| `MAPPING_TABLE_VERSION` | `arkforge.arkdeck-step-map/v1` | Step-kind admission table. Included in contract `1.0.0`. |
| `CONTROL_TABLE_VERSION` | `arkforge.arkdeck-control-map/v1` | Managed-control table. Included in contract `1.0.0`. |
| Cargo package version | `0.1.0` | `HelloAck.daemon_version` only. A daemon build string. |
| Spec draft | `1.0.0-draft.4` | Normative spec generation. Separate from this pin. |
| Profile `recovery.version` | `1.0.0` on DAYU200 | `RecoveryGuide.recovery_contract_version` and `SupersedingRecoveryAssessment.recovery_contract_version`, with the contract id and sha256. Profile data. |

Package `arkforge.ipc.v1` tracks the handshake major.

## When the contract identity moves

The identity string is `arkforge-contract-v<MAJOR>.<MINOR>.<PATCH>`, currently
`arkforge-contract-v1.0.0`. A move is a commit on `main`. ArkDeck then advances
its `rev` to that commit. The move does not create a git tag.

| Component | Moves when |
| --- | --- |
| `MAJOR` | The handshake would refuse the previous major. Field numbers stay retired. |
| `MINOR` | Protobuf gains optional fields, or an adapter table gains a row an older peer can ignore. `handshake_refusal` accepts any minor on the same major. Unknown fields are skipped. Unknown enum values fail closed. |
| `PATCH` | Docs, tests, or clarifications leave Hello bytes, both adapter tables, flash-verify exits `0`/`7`/`9`, and the boot-loop success rule unchanged. |

Changing `MAPPING_TABLE_VERSION` or `CONTROL_TABLE_VERSION` moves this identity
even when the IPC major and minor stay. A published workflow-step spelling
change is a major contract bump. An additive unmapped-or-new row is a minor bump.

## What ArkDeck changes

Declare every ArkForge crate with the same git URL and the same 40-hex commit.
`check-arkforge-pin.py` requires the key set `{ git, rev }`, one revision, and
a lock source of `git+https://github.com/ArkDeck/ArkForge.git?rev=<commit>#<commit>`.

```toml
arkforge-ipc = { git = "https://github.com/ArkDeck/ArkForge.git", rev = "<40-hex-commit>" }
arkforge-arkdeck-adapter = { git = "https://github.com/ArkDeck/ArkForge.git", rev = "<40-hex-commit>" }
```

Both `rev` values are the same commit. Copy that SHA from the reviewed `main`
commit (`git rev-parse HEAD`). Core stays free of ArkDeck types; ArkDeck
depends on the adapter for admission tables and on `arkforge-ipc` for the
handshake codec. All ArkForge crates move together.

On a checkout of that commit, run:

```sh
cargo test -p arkforge-ipc --test consumer_contract
cargo test -p arkforge-arkdeck-adapter --test consumer_contract
```

Those tests encode and decode `Hello`, `HelloAck`, and a
`SupersedingRecoveryAssessment`, pin the public Hello bytes, and check the
flash-verify exits. They are the roundtrip a consumer can run without a device.
`check-arkforge-pin.py --run-vectors` runs them, plus `swift_sdk_vectors` and
`permit_vectors`, from the checkout Cargo fetched for the lockfile.

In ArkDeck's own build:

```rust
assert_eq!(arkforge_ipc::contract::PROTOCOL_VERSION, "1.0");
assert_eq!(arkforge_ipc::contract::CONTRACT_TAG, "arkforge-contract-v1.0.0");
assert_eq!(
    arkforge_arkdeck_adapter::ARKDECK_CONTRACT_VERSION,
    "arkforge.arkdeck-contract/1.0.0"
);
assert_eq!(
    arkforge_arkdeck_adapter::MAPPING_TABLE_VERSION,
    "arkforge.arkdeck-step-map/v1"
);
assert_eq!(
    arkforge_arkdeck_adapter::control::CONTROL_TABLE_VERSION,
    "arkforge.arkdeck-control-map/v1"
);
```

At runtime, send `Hello { protocol_major: 1, protocol_minor: 0, session_kind }`
and read `HelloAck` before any request:

- `protocol_major` in the ack is the daemon's major (`1`).
- A set `refusal` ends the session. Surface the text. The major mismatch text
  is `protocol major <peer> is not compatible with 1`. A socket mismatch text
  is `this socket serves <Public|Controller> sessions, peer announced <Public|Controller>`.
- `daemon_version` is the Cargo package version of that `arkforged` build.
- `execution_ready`, `execution_blockers`, `toolchain_id`, and
  `toolchain_sha256` are standing facts from AF-IPC-003.

Public Hello body, minor `0` omitted as the proto3 default, is the four bytes
`08 01 18 01`. Controller is `08 01 18 02`. On the socket each body is preceded
by a 4-byte big-endian length (`00 00 00 04` for the public Hello).

## `flash verify`

```sh
arkforge --output json flash verify \
  --job <job-id> --hardware-campaign <campaign-id>
```

When verification runs, stdout is one `arkforge.flash-verification/v1`
document and the process exit is `FlashVerificationResult::exit_code`. ArkDeck
maps exit 0 to a full match, exit 9 to a typed skip, and exit 7 to a
verification failure, and shows the document beside that exit. Historical job
receipts stay as they were. The command is read-only and is separate from rescue.

| Exit | Document `outcome` | Condition ArkDeck displays |
| --- | --- | --- |
| 0 | `verified` | `failed == 0` and `skipped == 0`. Every image range matched. Each target has `outcome: verified` and `verification_strength: fullHash`. |
| 9 | `partial` | `failed == 0` and `skipped > 0`. At least one target has `outcome: typedSkip`. Writable mounted `userdata` uses `reason: mounted-writable-after-boot`. |
| 7 | `failed` | `failed > 0`. A failure wins over any skip. Each failed target carries its `reason`. |

Counts `verified`, `skipped`, and `failed` in the document are the same counts
the exit code uses.

These exits mean a verification result. The process can exit earlier, with no
`arkforge.flash-verification/v1` document:

| Exit | Meaning |
| --- | --- |
| 2 | The job reference or budget is invalid. |
| 3 | Source, identity, campaign, or execution state refused verification. |
| 5 | A required job or runtime resource was not found. |
| 10 | IPC or durable recording failed. |

## Boot-loop recovery

`docs/boot-loop-restoration.md` and `scripts/restore-boot-loop.py` are the
operator workflow. ArkDeck surfaces that workflow. It is a CLI/Runtime
restoration plus a second full-hash `flash verify`. It is separate from
`startExecution`, from automatic replay, and from native rescue.

The script restores the pinned original gzip plus zeros across the full 4 MiB
ramdisk. It leaves userdata untouched, leaves the old flash unreplayed, and
leaves authority records unedited. It requires confirmed success receipts
through the final reboot, the sole active job or the latest durable job when
none is active, `--ack overwrite:partition=ramdisk`, and the named hardware
campaign.

| Script exit | What ArkDeck shows |
| --- | --- |
| 0 | The restoration job state is `succeeded`. A following `flash verify` of that job has `verified == 1`, `skipped == 0`, `failed == 0`, `outcome: verified`, and one target with `verification_strength: fullHash`. The summary file uses schema `arkforge.boot-loop-restoration/v1`. Userdata was neither written nor verified. |
| 1 | The script stopped and wrote a `stopped` record. The operator inspects the Runtime job and receipts. Another run would risk a second write, so ArkDeck does not offer a blind retry. |
| 2 | Argument parsing failed before the script wrote a record. |

When the wait budget ends with no usable USB session, the script exits 1, submits
no restoration, and tells the operator to enter Loader physically. A reboot
loop on the display does not by itself mean a USB session exists.

`getRecoveryGuide` remains the IPC recovery read. Its
`recovery_contract_version` is the profile value (DAYU200 `1.0.0` today),
together with `recovery_contract_id` and `recovery_contract_sha256`. ArkDeck
shows those three fields as the profile recovery identity. The boot-loop script
handles the ramdisk case above; it does not replace the guide.

## Consumer-owned maintenance API

`arkforge_arkdeck_adapter::maintenance` exposes `verify` and `restore` as typed,
in-process workflows over Runtime-owned ports. They do not spawn the standalone
CLI or acquire its authority. The port contract is
`spec/ports/authority-maintenance.md`. This API leaves the IPC Hello,
step/control tables, and their `1.0.0` identity strings unchanged.

Consume it by advancing the same 40-hex `rev` on every ArkForge crate to a
`main` commit that contains the API. `check-arkforge-pin.py` is the pin check,
and both `consumer_contract` tests must pass at that commit.
