//! The journal on disk.
//!
//! architecture.md 13.2/13.3. The in-memory [`Journal`] gives the chain; this
//! gives the part that survives a crash, which is the part the crash-semantics
//! table is written against.
//!
//! # What durability here does and does not claim
//!
//! `fsync` is issued before any record marked [`FsyncPolicy::Durable`] is
//! allowed to influence an external effect. That is a real ordering guarantee
//! against process death, and the truncation campaign in the tests proves the
//! reader half of it exhaustively: every possible torn tail is either replayed
//! as a shorter prefix or refused.
//!
//! It is *not* a proof that the platform honours `fsync` against power loss.
//! macOS `fsync(2)` does not flush the drive's own write cache — `F_FULLFSYNC`
//! does — and neither this module nor any test in this repository has cut power
//! to a board mid-write. `F_FULLFSYNC` is not reachable without `libc`, which
//! AFD-0001 forbids. So the honest claim is: ordered and durable against
//! process death, unproven against power loss. Evidence AD-017 records this as
//! an open limit rather than a passed gate.

use crate::journal::{FsyncPolicy, Journal, JournalError, JournalRecord, JournalRecordKind};
use arkforge_core::ids::OpaqueId;
use arkforge_core::Sha256Digest;
use core::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Leads the file so a reader knows what it opened before it decodes anything.
const MAGIC: &[u8; 8] = b"ARKFJRN1";

/// A frame longer than this is rejected without allocating for it. Records
/// carry short facts; the bound exists so a corrupt length cannot make the
/// daemon reserve a gigabyte.
const MAX_FRAME_BYTES: u32 = 1 << 20;

/// An append-only journal file with its in-memory chain.
#[derive(Debug)]
pub struct DurableJournal {
    path: PathBuf,
    file: File,
    journal: Journal,
    /// Bytes written since the last fsync, so a buffered record is not silently
    /// left behind by a reader that never sees a durable one.
    unsynced_records: usize,
}

/// What opening a journal found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Records replayed and verified.
    pub records_replayed: usize,
    /// Bytes discarded from the end because a frame was incomplete — a write
    /// that a crash interrupted. Zero on a clean open.
    pub torn_tail_bytes: u64,
    /// Whether the file existed before this open.
    pub existed: bool,
}

impl RecoveryReport {
    pub fn was_torn(&self) -> bool {
        self.torn_tail_bytes > 0
    }
}

impl DurableJournal {
    /// Opens or creates the journal at `path`, replaying and verifying it.
    ///
    /// A frame that is present but incomplete is a crash during a write: it is
    /// truncated away and reported. Anything else — a broken chain, a bad
    /// digest, a misdeclared fsync policy — is refused, because a journal that
    /// no longer proves its own history cannot be the thing recovery reasons
    /// from.
    pub fn open(path: impl AsRef<Path>) -> Result<(Self, RecoveryReport), DurableJournalError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|error| DurableJournalError::Io {
                    path: parent.to_path_buf(),
                    message: error.to_string(),
                })?;
            }
        }
        let existed = path.exists();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| DurableJournalError::Io {
                path: path.clone(),
                message: error.to_string(),
            })?;

        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|error| DurableJournalError::Io {
                path: path.clone(),
                message: error.to_string(),
            })?;

        let mut journal = Journal::new();
        let mut report = RecoveryReport {
            records_replayed: 0,
            torn_tail_bytes: 0,
            existed,
        };

        let mut cursor = if bytes.is_empty() {
            file.write_all(MAGIC)
                .and_then(|()| file.sync_all())
                .map_err(|error| DurableJournalError::Io {
                    path: path.clone(),
                    message: error.to_string(),
                })?;
            MAGIC.len()
        } else {
            if bytes.len() < MAGIC.len() || &bytes[..MAGIC.len()] != MAGIC {
                // Too short to hold a header, or holding a different one. A
                // partial header is still a file this build must not append to.
                return Err(DurableJournalError::NotAJournal { path });
            }
            MAGIC.len()
        };

        while cursor < bytes.len() {
            let Some(length_bytes) = bytes.get(cursor..cursor + 4) else {
                report.torn_tail_bytes = (bytes.len() - cursor) as u64;
                break;
            };
            let length = u32::from_be_bytes([
                length_bytes[0],
                length_bytes[1],
                length_bytes[2],
                length_bytes[3],
            ]);
            if length == 0 || length > MAX_FRAME_BYTES {
                // A zero or absurd length is not a torn write of a real record:
                // the length is written in the same call as the body, so a
                // crash leaves a short file, not a wrong number.
                return Err(DurableJournalError::FrameLengthInvalid {
                    at_offset: cursor as u64,
                    length,
                });
            }
            let body_start = cursor + 4;
            let Some(body) = bytes.get(body_start..body_start + length as usize) else {
                report.torn_tail_bytes = (bytes.len() - cursor) as u64;
                break;
            };
            let record = JournalRecord::from_canonical_bytes(body)
                .map_err(|error| DurableJournalError::Journal(Box::new(error)))?;
            journal
                .adopt(record)
                .map_err(|error| DurableJournalError::Journal(Box::new(error)))?;
            report.records_replayed += 1;
            cursor = body_start + length as usize;
        }

        if report.torn_tail_bytes > 0 {
            file.set_len(cursor as u64)
                .and_then(|()| file.sync_all())
                .map_err(|error| DurableJournalError::Io {
                    path: path.clone(),
                    message: error.to_string(),
                })?;
        }
        file.seek(SeekFrom::Start(cursor as u64))
            .map_err(|error| DurableJournalError::Io {
                path: path.clone(),
                message: error.to_string(),
            })?;

        Ok((
            DurableJournal {
                path,
                file,
                journal,
                unsynced_records: 0,
            },
            report,
        ))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn journal(&self) -> &Journal {
        &self.journal
    }

    pub fn head_digest(&self) -> Sha256Digest {
        self.journal.head_digest()
    }

    pub fn len(&self) -> usize {
        self.journal.len()
    }

    pub fn is_empty(&self) -> bool {
        self.journal.is_empty()
    }

    /// Appends a record, syncing when its kind demands it.
    ///
    /// Returns the record's digest. The caller that is about to cause an
    /// external effect holds this: if `append` returned, a `Durable` record for
    /// the intent is on stable storage, and if it did not return, no effect may
    /// follow.
    pub fn append(
        &mut self,
        kind: JournalRecordKind,
        at_epoch_ms: u64,
        job_revision: u64,
        subject: OpaqueId,
        facts: Vec<(OpaqueId, String)>,
    ) -> Result<Sha256Digest, DurableJournalError> {
        let record = self
            .journal
            .append(kind, at_epoch_ms, job_revision, subject, facts)
            .map_err(|error| DurableJournalError::Journal(Box::new(error)))?
            .clone();

        let body = record
            .to_canonical_bytes()
            .map_err(|error| DurableJournalError::Journal(Box::new(JournalError::Cbor(error))))?;
        let length = u32::try_from(body.len()).ok().filter(|length| {
            *length > 0 && *length <= MAX_FRAME_BYTES
        });
        let Some(length) = length else {
            return Err(DurableJournalError::RecordTooLarge(body.len()));
        };

        // One write call for length and body together, so a crash leaves a
        // short file rather than a valid length pointing at absent bytes.
        let mut frame = Vec::with_capacity(4 + body.len());
        frame.extend_from_slice(&length.to_be_bytes());
        frame.extend_from_slice(&body);

        self.file
            .write_all(&frame)
            .map_err(|error| DurableJournalError::Io {
                path: self.path.clone(),
                message: error.to_string(),
            })?;
        self.unsynced_records += 1;

        if record.fsync_policy == FsyncPolicy::Durable {
            self.file
                .sync_all()
                .map_err(|error| DurableJournalError::Io {
                    path: self.path.clone(),
                    message: error.to_string(),
                })?;
            self.unsynced_records = 0;
        }

        Ok(record.record_digest)
    }

    /// Flushes whatever buffered records are outstanding.
    ///
    /// Shutdown only. Correctness never depends on this being called: every
    /// record that a decision rests on is `Durable` and was synced by `append`.
    pub fn sync(&mut self) -> Result<(), DurableJournalError> {
        self.file
            .sync_all()
            .map_err(|error| DurableJournalError::Io {
                path: self.path.clone(),
                message: error.to_string(),
            })?;
        self.unsynced_records = 0;
        Ok(())
    }

    /// Records written but not yet synced. Diagnostics.
    pub fn unsynced_records(&self) -> usize {
        self.unsynced_records
    }
}

#[derive(Debug)]
pub enum DurableJournalError {
    Io { path: PathBuf, message: String },
    NotAJournal { path: PathBuf },
    FrameLengthInvalid { at_offset: u64, length: u32 },
    RecordTooLarge(usize),
    Journal(Box<JournalError>),
}

impl fmt::Display for DurableJournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DurableJournalError::Io { path, message } => {
                write!(f, "{}: {message}", path.display())
            }
            DurableJournalError::NotAJournal { path } => write!(
                f,
                "{} does not begin with an ArkForge journal header",
                path.display()
            ),
            DurableJournalError::FrameLengthInvalid { at_offset, length } => write!(
                f,
                "journal frame at offset {at_offset} declares an impossible length of {length}"
            ),
            DurableJournalError::RecordTooLarge(size) => {
                write!(f, "journal record of {size} bytes exceeds the frame bound")
            }
            DurableJournalError::Journal(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for DurableJournalError {}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "arkforge-durable-{label}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }

        fn file(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn id(value: &str) -> OpaqueId {
        OpaqueId::new(value).unwrap()
    }

    fn write_three(path: &Path) {
        let (mut journal, report) = DurableJournal::open(path).unwrap();
        assert!(!report.existed);
        journal
            .append(JournalRecordKind::PlanStored, 1_000, 1, id("PLAN-1"), vec![])
            .unwrap();
        journal
            .append(
                JournalRecordKind::PreflightObserved,
                2_000,
                1,
                id("JOB-1"),
                vec![(id("mode"), "hdc-normal".into())],
            )
            .unwrap();
        journal
            .append(
                JournalRecordKind::StepIntentRecorded,
                3_000,
                1,
                id("STEP-1"),
                vec![(id("permitId"), "PERMIT-1".into())],
            )
            .unwrap();
    }

    #[test]
    fn a_journal_reopens_with_its_chain_intact() {
        let dir = TempDir::new("reopen");
        let path = dir.file("journal.cbor");
        write_three(&path);

        let (reopened, report) = DurableJournal::open(&path).unwrap();
        assert!(report.existed);
        assert!(!report.was_torn());
        assert_eq!(report.records_replayed, 3);
        reopened.journal().verify().unwrap();
        assert_eq!(reopened.journal().records()[2].kind, JournalRecordKind::StepIntentRecorded);
    }

    #[test]
    fn appends_continue_the_chain_across_a_reopen() {
        let dir = TempDir::new("continue");
        let path = dir.file("journal.cbor");
        write_three(&path);

        let (mut reopened, _) = DurableJournal::open(&path).unwrap();
        let head_before = reopened.head_digest();
        reopened
            .append(
                JournalRecordKind::SemanticReceiptRecorded,
                4_000,
                1,
                id("STEP-1"),
                vec![],
            )
            .unwrap();

        let (again, report) = DurableJournal::open(&path).unwrap();
        assert_eq!(report.records_replayed, 4);
        again.journal().verify().unwrap();
        assert_eq!(again.journal().records()[3].previous_digest, head_before);
    }

    /// The crash campaign. Every byte-length prefix of a real journal is a
    /// state a crash could leave on disk; each one must either replay as a
    /// prefix of what was written or be refused. What must never happen is a
    /// journal that opens cleanly and is missing a record whose successor
    /// survived, because recovery reads the last record as the whole truth.
    #[test]
    fn every_torn_tail_replays_as_a_prefix_or_is_refused() {
        let dir = TempDir::new("torn");
        let source = dir.file("journal.cbor");
        write_three(&source);
        let full = std::fs::read(&source).unwrap();
        let (whole, _) = DurableJournal::open(&source).unwrap();
        let written: Vec<_> = whole
            .journal()
            .records()
            .iter()
            .map(|record| record.record_digest)
            .collect();
        drop(whole);

        for cut in 0..full.len() {
            let path = dir.file(&format!("torn-{cut}.cbor"));
            std::fs::write(&path, &full[..cut]).unwrap();

            match DurableJournal::open(&path) {
                Ok((torn, report)) => {
                    torn.journal().verify().unwrap();
                    let replayed: Vec<_> = torn
                        .journal()
                        .records()
                        .iter()
                        .map(|record| record.record_digest)
                        .collect();
                    assert_eq!(
                        replayed,
                        written[..replayed.len()],
                        "cut at {cut} replayed something other than a prefix"
                    );
                    // Every byte is accounted for: what survives on disk is
                    // exactly what was replayed, and the difference from the
                    // truncated input is exactly what the report called torn.
                    // A cut that lands on a frame boundary loses a record with
                    // nothing torn, which is a crash before that write began.
                    let remaining = std::fs::metadata(&path).unwrap().len();
                    assert_eq!(
                        remaining + report.torn_tail_bytes,
                        cut.max(MAGIC.len()) as u64,
                        "cut at {cut} left bytes unaccounted for"
                    );
                }
                // A cut inside the header leaves a file this build must not
                // append to; refusing is the correct answer.
                Err(DurableJournalError::NotAJournal { .. }) => {
                    assert!(cut < MAGIC.len(), "cut at {cut} rejected a valid header");
                }
                Err(other) => panic!("cut at {cut}: {other}"),
            }
        }
    }

    #[test]
    fn a_torn_tail_is_truncated_so_the_next_append_is_not_written_after_garbage() {
        let dir = TempDir::new("truncate");
        let path = dir.file("journal.cbor");
        write_three(&path);
        let full = std::fs::read(&path).unwrap();

        // Cut inside the last frame's body.
        std::fs::write(&path, &full[..full.len() - 5]).unwrap();
        let (mut torn, report) = DurableJournal::open(&path).unwrap();
        assert!(report.was_torn());
        assert_eq!(report.records_replayed, 2);
        torn.append(
            JournalRecordKind::CancellationRequested,
            5_000,
            1,
            id("JOB-1"),
            vec![],
        )
        .unwrap();
        drop(torn);

        let (reopened, report) = DurableJournal::open(&path).unwrap();
        assert!(!report.was_torn());
        assert_eq!(report.records_replayed, 3);
        reopened.journal().verify().unwrap();
        assert_eq!(
            reopened.journal().records()[2].kind,
            JournalRecordKind::CancellationRequested
        );
    }

    #[test]
    fn a_file_that_is_not_a_journal_is_refused_rather_than_appended_to() {
        let dir = TempDir::new("foreign");
        let path = dir.file("notes.txt");
        std::fs::write(&path, b"this is somebody else's file").unwrap();
        assert!(matches!(
            DurableJournal::open(&path),
            Err(DurableJournalError::NotAJournal { .. })
        ));
    }

    #[test]
    fn an_edited_record_is_refused_on_reopen() {
        let dir = TempDir::new("edited");
        let path = dir.file("journal.cbor");
        write_three(&path);
        let mut full = std::fs::read(&path).unwrap();

        // Flip a byte inside the first frame's body.
        let target = MAGIC.len() + 8;
        full[target] ^= 0x01;
        std::fs::write(&path, &full).unwrap();

        match DurableJournal::open(&path) {
            Err(DurableJournalError::Journal(_)) => {}
            Err(other) => panic!("expected a journal error, got {other}"),
            Ok(_) => panic!("an edited record was accepted"),
        }
    }

    #[test]
    fn a_dispatch_relevant_record_is_synced_before_append_returns() {
        let dir = TempDir::new("sync");
        let path = dir.file("journal.cbor");
        let (mut journal, _) = DurableJournal::open(&path).unwrap();

        journal
            .append(
                JournalRecordKind::PreflightObserved,
                1_000,
                1,
                id("JOB-1"),
                vec![],
            )
            .unwrap();
        assert_eq!(journal.unsynced_records(), 1, "an observation may wait");

        journal
            .append(
                JournalRecordKind::StepIntentRecorded,
                2_000,
                1,
                id("STEP-1"),
                vec![],
            )
            .unwrap();
        assert_eq!(
            journal.unsynced_records(),
            0,
            "an intent must be on stable storage before append returns"
        );
    }
}
