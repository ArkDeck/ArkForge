# Verify completed image ranges through Normal mode
status: proposed
scope: ArkForge standalone authority and CLI

The Windows DAYU200 full flash completed all nine writes, but Loader readback
verified only `uboot` and `resource`; seven ranges received an honest read-domain
skip. Rockchip U-Boot `rkusb_read_sector` returns `0xcc` when a request crosses
`RKUSB_READ_LIMIT_ADDR`, defined as 65536 sectors (32 MiB). Smaller chunks,
Windows driver changes and the alternate host read syntax do not remove this
firmware rule.

Add `flash verify --job` as an independent, read-only operation using the same
Normal-mode identity and hash-bound HDC port. Obtain all image ranges and
digests from the completed job, measure the current block mapping and mount
state, hash exact image bytes and persist a separate result. Keep existing
Loader observations and receipts intact. The new domain can complete six of
the seven missing image hashes on the examined board. The already mounted
writable userdata cannot prove its original post-write image hash: Normal
startup changes filesystem metadata/data. Bootloader or recovery firmware
changes are a different operation and are not introduced by this proposal.

No mechanics wire, plan digest, published profile, Loader read limit, recovery
budget or hardware maturity is changed. Partial verification stays partial.
The implementation and its tests are reviewed together; this file does not
declare approval or hardware acceptance.

Primary firmware references (pinned upstream source; the board's restricted
read behavior was separately observed):

- [read callback](https://github.com/rockchip-linux/u-boot/blob/1c535d65b8509f388d09e49fb6961f49fda35a1d/cmd/rockusb.c#L21)
- [read limit](https://github.com/rockchip-linux/u-boot/blob/1c535d65b8509f388d09e49fb6961f49fda35a1d/include/rockusb.h#L121)

The pinned source explains the observed result; it is not a claim that the
board's installed U-Boot was built from that exact commit.

Offline inspection of this firmware's `updater.img` (SHA-256
`62bb45c770d7083ee7c95e2fc9a7668684b57721d45af2f5f192a1b25ed40ea2`)
also rules out treating its existing updater environment as an established
nonmutating alternative: `etc/fstab.updater` configures userdata at `/data`
with f2fs check/quota/encryption options and no `ro`; init services create HDC
and log directories under `/data`. A separate pre-mount verification environment
would need its own proven boot and filesystem behavior. No such environment
is executed or inferred from the Normal-mode results here.
