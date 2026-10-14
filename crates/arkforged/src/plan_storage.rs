//! Immutable private plan files, committed before a materialization is returned.

use arkforge_core::digest::sha256;
use arkforge_engine::{EngineError, PlanStorage, StoredPlan};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

fn failure(error: impl std::fmt::Display) -> EngineError {
    EngineError::PlanStorage(error.to_string())
}

fn require_plain(path: &Path, directory: bool) -> Result<(), EngineError> {
    let metadata = fs::symlink_metadata(path).map_err(failure)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(failure("private plan storage refuses reparse points"));
        }
    }
    if metadata.file_type().is_symlink()
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        return Err(failure(
            "private plan storage requires plain files and directories",
        ));
    }
    Ok(())
}

pub(crate) fn filename(plan: &StoredPlan) -> String {
    format!(
        "{}.cbor",
        sha256(plan.envelope.plan_id.as_str().as_bytes()).to_hex()
    )
}

pub(crate) fn open(root: &Path) -> Result<Vec<StoredPlan>, EngineError> {
    fs::create_dir_all(root).map_err(failure)?;
    require_plain(root, true)?;
    arkforge_platform::protect_path(root, true).map_err(failure)?;
    let mut plans = Vec::new();
    for entry in fs::read_dir(root).map_err(failure)? {
        let path = entry.map_err(failure)?.path();
        require_plain(&path, false)?;
        if path
            .extension()
            .is_some_and(|extension| extension == "pending")
        {
            // A crash before the immutable filename was published. It was
            // never returned to a caller and cannot have authorized a job.
            continue;
        }
        if path.extension().is_none_or(|extension| extension != "cbor") {
            return Err(failure("unrecognized file in private plan storage"));
        }
        let bytes = read(&path)?;
        let plan = StoredPlan::from_storage_bytes(&bytes)?;
        if path
            .file_name()
            .is_none_or(|name| name != filename(&plan).as_str())
        {
            return Err(failure(
                "private plan filename does not match its sealed identifier",
            ));
        }
        arkforge_platform::protect_path(&path, false).map_err(failure)?;
        plans.push(plan);
    }
    Ok(plans)
}

fn read(path: &Path) -> Result<Vec<u8>, EngineError> {
    require_plain(path, false)?;
    let file = File::open(path).map_err(failure)?;
    if file.metadata().map_err(failure)?.len() > StoredPlan::MAX_STORAGE_BYTES {
        return Err(failure("private plan exceeds the storage size bound"));
    }
    let mut bytes = Vec::new();
    file.take(StoredPlan::MAX_STORAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(failure)?;
    if bytes.len() as u64 > StoredPlan::MAX_STORAGE_BYTES {
        return Err(failure("private plan grew beyond the storage size bound"));
    }
    Ok(bytes)
}

pub(crate) fn persist(root: &Path, plan: &StoredPlan, bytes: &[u8]) -> Result<(), EngineError> {
    require_plain(root, true)?;
    let target = root.join(filename(plan));
    if target.try_exists().map_err(failure)? {
        return verify_existing(&target, bytes);
    }
    let mut nonce = [0; 32];
    arkforge_platform::fill_random(&mut nonce).map_err(failure)?;
    let temporary: PathBuf = root.join(format!("{}.pending", sha256(&nonce).to_hex()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).map_err(failure)?;
    let result = (|| {
        arkforge_platform::protect_path(&temporary, false).map_err(failure)?;
        file.write_all(bytes).map_err(failure)?;
        file.sync_all().map_err(failure)?;
        // A same-directory hard link publishes the fully synced bytes without
        // ever replacing an existing plan, even if another writer races us.
        match fs::hard_link(&temporary, &target) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                verify_existing(&target, bytes)?;
            }
            Err(error) => return Err(failure(error)),
        }
        arkforge_platform::sync_directory(root).map_err(failure)
    })();
    drop(file);
    // An abandoned staging file is safe to leave after a failure. Never delete
    // the committed name: it may already be referenced by a durable journal.
    let _ = fs::remove_file(temporary);
    result
}

fn verify_existing(path: &Path, bytes: &[u8]) -> Result<(), EngineError> {
    if read(path)? != bytes {
        return Err(failure(
            "a different sealed plan already owns this identifier",
        ));
    }
    Ok(())
}

/// Filesystem implementation of the engine's immutable private-plan port.
#[derive(Debug)]
pub struct FilePlanStorage {
    root: PathBuf,
}
impl FilePlanStorage {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}
impl PlanStorage for FilePlanStorage {
    fn load(&self) -> Result<Vec<StoredPlan>, EngineError> {
        open(&self.root)
    }
    fn persist(&self, plan: &StoredPlan, bytes: &[u8]) -> Result<(), EngineError> {
        persist(&self.root, plan, bytes)
    }
}
#[cfg(test)]
#[path = "plan_storage_tests.rs"]
mod tests;
