//! The CLI authority's own record of who accepted what.
//!
//! This is an audit domain, not an execution one. It records that a named
//! operator interface accepted an exact plan and an exact set of destructive
//! effect tokens, and how that acceptance arrived — through a confirmation
//! screen or through argv. It never becomes a permit, never enters the
//! mechanics journal, and never counts as mechanics evidence; `arkforged`
//! produces byte-identical journals and receipts whether a run was approved on
//! a terminal or in a script.
//!
//! Its one hard rule: the record is durable *before* anything is dispatched. An
//! execution nobody can prove was approved is worse than an execution that did
//! not happen, so a failed write means zero dispatch.

use crate::StandaloneError;
use arkforge_core::digest::sha256;
use arkforge_ipc::wire;
use arkforge_platform::{protect_path, replace_file, sync_directory};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// The directory this module owns inside a runtime directory.
const APPROVAL_DIR: &str = "cli-approvals";
/// The directory recording which (board, profile) pairs this host has flashed.
const FIRST_FLASH_DIR: &str = "cli-first-flash";

/// Domain separation, so an approval digest can never collide with a plan,
/// permit, or receipt digest computed over similar bytes.
const APPROVAL_DOMAIN: &[u8] = b"arkforge.cli-approval/v1\0";
const FIRST_FLASH_DOMAIN: &[u8] = b"arkforge.cli-first-flash/v1\0";

/// How the acceptance reached the authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// A confirmation screen on a terminal the operator was sitting at.
    InteractiveTty,
    /// Exact `--ack` tokens on the command line.
    Argv,
}

impl Provenance {
    pub fn as_str(self) -> &'static str {
        match self {
            Provenance::InteractiveTty => "interactive-tty",
            Provenance::Argv => "argv",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRecord {
    pub plan_id: String,
    pub plan_sha256: String,
    /// Exactly the tokens accepted, in the order the plan declared them.
    pub tokens: Vec<String>,
    pub provenance: Provenance,
    /// The product model the operator typed, when the confirmation screen asked
    /// for it. It is recorded as an assertion, never as evidence: a human
    /// naming a board does not make the machine's identification stronger.
    pub model_assertion: Option<String>,
    pub hardware_campaign: Option<String>,
    pub recorded_at_epoch_ms: u64,
}

impl ApprovalRecord {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_string(&mut out, 1, &self.plan_id);
        wire::write_string(&mut out, 2, &self.plan_sha256);
        for token in &self.tokens {
            wire::write_string(&mut out, 3, token);
        }
        wire::write_string(&mut out, 4, self.provenance.as_str());
        if let Some(model) = &self.model_assertion {
            wire::write_string(&mut out, 5, model);
        }
        if let Some(campaign) = &self.hardware_campaign {
            wire::write_string(&mut out, 6, campaign);
        }
        wire::write_uint64(&mut out, 7, self.recorded_at_epoch_ms);
        out
    }

    /// The identifier these exact bytes have.
    ///
    /// Content-addressed so that a retry of the same acceptance is the same
    /// record, and a different acceptance can never quietly overwrite one.
    pub fn approval_id(&self) -> String {
        let mut payload = APPROVAL_DOMAIN.to_vec();
        payload.extend_from_slice(&self.encode());
        format!("cli-approval:{}", sha256(&payload).to_hex())
    }
}

/// Writes the record durably, returning its identifier.
///
/// A byte-identical record already present is success: a retried command must
/// not be blocked by its own earlier attempt. A different record under the same
/// identifier is impossible by construction, and is reported as corruption
/// rather than overwritten.
pub fn record(runtime_dir: &Path, approval: &ApprovalRecord) -> Result<String, StandaloneError> {
    let directory = runtime_dir.join(APPROVAL_DIR);
    std::fs::create_dir_all(&directory)
        .map_err(|error| io_failure("create the approval store", error))?;
    protect_path(&directory, true)
        .map_err(|error| io_failure("protect the approval store", error))?;

    let approval_id = approval.approval_id();
    let encoded = approval.encode();
    let target = directory.join(file_name(&approval_id));
    if let Ok(mut existing) = std::fs::File::open(&target) {
        let mut bytes = Vec::new();
        existing
            .read_to_end(&mut bytes)
            .map_err(|error| io_failure("read the existing approval", error))?;
        if bytes == encoded {
            return Ok(approval_id);
        }
        return Err(StandaloneError::new(
            "APPROVAL_CONFLICT",
            format!("A different approval is already stored as {approval_id}."),
            6,
            false,
        ));
    }

    let temporary = directory.join(format!("{}.next", file_name(&approval_id).display()));
    {
        let mut file = std::fs::File::create(&temporary)
            .map_err(|error| io_failure("create the approval transaction", error))?;
        protect_path(&temporary, false)
            .map_err(|error| io_failure("protect the approval transaction", error))?;
        file.write_all(&encoded)
            .and_then(|_| file.sync_all())
            .map_err(|error| io_failure("write the approval transaction", error))?;
    }
    replace_file(&temporary, &target).map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        io_failure("commit the approval", error)
    })?;
    sync_directory(&directory).map_err(|error| io_failure("sync the approval store", error))?;
    Ok(approval_id)
}

/// The key under which a (physical board, exact profile) pair is remembered.
///
/// Both halves must be facts that survive re-observation. An observation id or
/// a bus position would make every replug look like a new board and every
/// confirmation the first one, which would train the operator to type past the
/// screen that exists to stop them.
pub fn first_flash_key(physical_identity_digest: &str, profile_digest: &str) -> String {
    let mut payload = FIRST_FLASH_DOMAIN.to_vec();
    payload.extend_from_slice(physical_identity_digest.as_bytes());
    payload.push(0);
    payload.extend_from_slice(profile_digest.as_bytes());
    sha256(&payload).to_hex()
}

/// Whether this host has ever completed a flash for that pair.
pub fn is_first_flash(runtime_dir: &Path, key: &str) -> bool {
    !runtime_dir
        .join(FIRST_FLASH_DIR)
        .join(file_name(key))
        .exists()
}

/// Records a completed flash for that pair.
///
/// Called only after a job reaches a successful terminal state, so a failed or
/// interrupted attempt never spends the first confirmation.
pub fn record_first_flash(runtime_dir: &Path, key: &str) -> Result<(), StandaloneError> {
    let directory = runtime_dir.join(FIRST_FLASH_DIR);
    std::fs::create_dir_all(&directory)
        .map_err(|error| io_failure("create the first-flash store", error))?;
    protect_path(&directory, true)
        .map_err(|error| io_failure("protect the first-flash store", error))?;
    let target = directory.join(file_name(key));
    let mut file = std::fs::File::create(&target)
        .map_err(|error| io_failure("record the completed flash", error))?;
    protect_path(&target, false)
        .map_err(|error| io_failure("protect the completed flash record", error))?;
    file.write_all(key.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|error| io_failure("write the completed flash record", error))?;
    sync_directory(&directory).map_err(|error| io_failure("sync the first-flash store", error))?;
    Ok(())
}

/// A file name that cannot escape its directory whatever the identifier holds.
fn file_name(identifier: &str) -> PathBuf {
    PathBuf::from(
        identifier
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character
                } else {
                    '-'
                }
            })
            .collect::<String>(),
    )
}

fn io_failure(action: &str, error: std::io::Error) -> StandaloneError {
    StandaloneError::new(
        "APPROVAL_IO_FAILED",
        format!("Cannot {action}: {error}"),
        10,
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("arkforge-approval-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn approval() -> ApprovalRecord {
        ApprovalRecord {
            plan_id: "PLAN-1".into(),
            plan_sha256: "a".repeat(64),
            tokens: vec!["data-loss:userdata".into()],
            provenance: Provenance::Argv,
            model_assertion: None,
            hardware_campaign: None,
            recorded_at_epoch_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn the_same_acceptance_records_once_and_retries_cleanly() {
        let root = temp_dir("idempotent");
        let approval = approval();
        let first = record(&root, &approval).unwrap();
        let second = record(&root, &approval).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            std::fs::read_dir(root.join(APPROVAL_DIR)).unwrap().count(),
            1
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_different_acceptance_gets_a_different_identifier() {
        let root = temp_dir("distinct");
        let argv = approval();
        let interactive = ApprovalRecord {
            provenance: Provenance::InteractiveTty,
            model_assertion: Some("DAYU200".into()),
            ..approval()
        };
        assert_ne!(argv.approval_id(), interactive.approval_id());
        record(&root, &argv).unwrap();
        record(&root, &interactive).unwrap();
        assert_eq!(
            std::fs::read_dir(root.join(APPROVAL_DIR)).unwrap().count(),
            2
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_conflicting_record_is_reported_rather_than_overwritten() {
        let root = temp_dir("conflict");
        let approval = approval();
        let approval_id = record(&root, &approval).unwrap();
        let stored = root.join(APPROVAL_DIR).join(file_name(&approval_id));
        std::fs::write(&stored, b"someone else's bytes").unwrap();
        let conflict = record(&root, &approval).unwrap_err();
        assert_eq!(conflict.code, "APPROVAL_CONFLICT");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_pair_is_first_until_a_flash_completes() {
        let root = temp_dir("first-flash");
        let key = first_flash_key(&"d".repeat(64), &"e".repeat(64));
        assert!(is_first_flash(&root, &key));
        record_first_flash(&root, &key).unwrap();
        assert!(!is_first_flash(&root, &key));

        // A different profile on the same board is its own first flash.
        let other = first_flash_key(&"d".repeat(64), &"f".repeat(64));
        assert!(is_first_flash(&root, &other));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
