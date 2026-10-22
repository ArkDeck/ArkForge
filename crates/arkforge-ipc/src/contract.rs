//! Consumer contract for `proto/arkforge.proto`, this crate, and
//! `arkforge-arkdeck-adapter`.
//!
//! The handshake is [`handshake_refusal`]. Compatibility, the git-rev pin, and
//! the recovery surface ArkDeck shows to an operator are in
//! `docs/arkdeck-contract.md`.

use crate::{PROTOCOL_MAJOR, PROTOCOL_MINOR};

/// Dotted IPC protocol, `{major}.{minor}`.
///
/// `Hello.protocol_major` must equal [`PROTOCOL_MAJOR`]. Any minor is
/// accepted. This string is the handshake version. Cargo package `0.1.0`
/// (`HelloAck.daemon_version`) and spec `1.0.0-draft.4` are other identifiers.
pub const PROTOCOL_VERSION: &str = "1.0";

/// In-process identity string for this protocol, the adapter tables reviewed
/// with it, and the recovery surface below.
///
/// During development ArkDeck pins a full 40-hex commit with Cargo `rev`.
/// `CONTRACT_TAG` names the contract revision; it is not a git tag to create.
/// ArkDeck's `check-arkforge-pin.py` accepts only `{ git, rev }`.
pub const CONTRACT_TAG: &str = "arkforge-contract-v1.0.0";

/// `arkforge flash verify` stdout schema once verification runs.
pub const FLASH_VERIFICATION_SCHEMA: &str = "arkforge.flash-verification/v1";

/// Process exit when every image range matched (`failed == 0`, `skipped == 0`).
pub const FLASH_VERIFY_EXIT_MATCHED: i32 = 0;
/// Process exit when at least one range failed. A failure wins over a skip.
pub const FLASH_VERIFY_EXIT_FAILED: i32 = 7;
/// Process exit when nothing failed and at least one range was a typed skip.
pub const FLASH_VERIFY_EXIT_PARTIAL: i32 = 9;

/// Document `outcome` when every range matched.
pub const OUTCOME_VERIFIED: &str = "verified";
/// Document `outcome` when nothing failed and at least one range was skipped.
pub const OUTCOME_PARTIAL: &str = "partial";
/// Document `outcome` when at least one range failed.
pub const OUTCOME_FAILED: &str = "failed";

/// Per-target `outcome` for a complete hash match.
pub const TARGET_VERDICT_VERIFIED: &str = "verified";
/// Per-target `outcome` for a typed skip.
pub const TARGET_VERDICT_TYPED_SKIP: &str = "typedSkip";
/// Per-target `outcome` for a failed range.
pub const TARGET_VERDICT_FAILED: &str = "failed";
/// `verification_strength` on a matched range.
pub const STRENGTH_FULL_HASH: &str = "fullHash";
/// Typed-skip `reason` for a partition that is mounted writable after boot.
pub const SKIP_MOUNTED_WRITABLE: &str = "mounted-writable-after-boot";

/// Summary schema written by `scripts/restore-boot-loop.py` on success.
pub const BOOT_LOOP_RESTORATION_SCHEMA: &str = "arkforge.boot-loop-restoration/v1";

/// `HelloAck.refusal` when `peer_major` differs from [`PROTOCOL_MAJOR`].
///
/// `None` accepts the peer. Minor is part of the signature because the
/// handshake carries it; a minor difference is compatible in both directions.
pub fn handshake_refusal(peer_major: u32, peer_minor: u32) -> Option<String> {
    crate::negotiate(peer_major, peer_minor).err()
}

/// Exit code of `arkforge flash verify` after a verification document exists.
///
/// Exits 2, 3, 5, and 10 happen before that document and are described in
/// `docs/arkdeck-contract.md`.
pub fn flash_verify_exit_code(failed: u64, skipped: u64) -> i32 {
    if failed != 0 {
        FLASH_VERIFY_EXIT_FAILED
    } else if skipped != 0 {
        FLASH_VERIFY_EXIT_PARTIAL
    } else {
        FLASH_VERIFY_EXIT_MATCHED
    }
}

/// `outcome` field of `arkforge.flash-verification/v1`.
pub fn flash_verify_outcome(failed: u64, skipped: u64) -> &'static str {
    if failed != 0 {
        OUTCOME_FAILED
    } else if skipped != 0 {
        OUTCOME_PARTIAL
    } else {
        OUTCOME_VERIFIED
    }
}

/// `PROTOCOL_VERSION` must stay equal to the numeric handshake constants.
pub fn protocol_version_matches_constants() -> bool {
    format!("{PROTOCOL_MAJOR}.{PROTOCOL_MINOR}") == PROTOCOL_VERSION
}
