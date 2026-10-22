# ArkDeck authority-owned maintenance API

Status: proposed; implementation available for review, consumer ports not composed.

ArkDeck cannot execute standalone verification or restore-boot-loop.py against
its own jobs: the standalone supervisor owns a different authority and durable
source store. The additive adapter module provides typed orchestration while
keeping source loading, exact observation, bounded block reads, durable intent,
RuntimeCapability reservation/consumption and StepPermit execution in ArkDeck's
trusted Runtime. It owns no CLI process or transport and cannot mint authority.

The result contract remains protocol 1.0, adapter tables 1.0.0, flash verification
0/9/7 and restoration 0/1/2. No existing wire or conformance bytes change. The new
port is draft, documented in docs/spec/ports/authority-maintenance.md. It needs a
consumer implementation and review before any Catalog operation is enabled.

Fault injection covers successful full hashes, typed writable skips, mismatches,
missing/unknown source receipts, wrong authority/campaign/plan, drift before/after
reads, persistence failure, invalid parameters before any record, exact ramdisk
scope, single reservation consumption, unknown execution and no retry. These are
fake port tests and cannot establish hardware support or verify a real device.

Full testing also exposed a macOS IPC defect: accepted Unix sockets could inherit
O_NONBLOCK from a polling listener, making valid framed requests fail depending
on timing. Accept now returns a blocking session; supervisor request reads remain
bounded. Campaign status read errors propagate instead of becoming a no-campaign
classification. Host IPC timeout and CLI campaign tests cover these fixes.

## Local targeted checks

2026-10-16, macOS host, no device writes:

- cargo fmt --all -- --check: exit 0.
- cargo clippy --workspace --all-targets --all-features -- -D warnings: exit 0;
  `/private/tmp/arkforge-maintenance-clippy.log`.
- cargo test --workspace --all-targets: exit 0 after IPC fix;
  `/private/tmp/arkforge-maintenance-tests.log`.
- cargo test -p arkforge-conformance: exit 0; no fixture generation or edits;
  `/private/tmp/arkforge-maintenance-conformance.log`.
- Additional public/controller Hello refusal test: exit 0 (3 session_bounds
  tests), `/private/tmp/arkforge-maintenance-handshake.log`.
- Adapter maintenance: 8 fake-port tests passed. Original consumer contracts pass.

The initial sandboxed full test could not start the temporary supervisor. The
controlled host run then exposed and reproduced the IPC defect above. Neither
failure was reported as a pass; full tests passed after the fix.

## CI

Pending the companion PR's Rust/macOS and Windows checks. Local macOS checks do
not establish Windows or hardware support. No tag or release has been created.
