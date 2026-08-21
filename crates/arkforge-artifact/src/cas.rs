//! Content-addressed artifact store.
//!
//! architecture.md 10.1/10.2. A destructive plan never names a host path: it
//! names an artifact id and a digest, and the bytes behind that digest were
//! hashed by this store on the way in. Import is atomic (staged then renamed),
//! leases are files so a crash cannot lose a reference count, and GC only
//! removes objects that no lease and no in-flight import points at.

use arkforge_core::digest::{Sha256, Sha256Digest};
use core::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const OBJECTS_DIR: &str = "objects";
const LEASES_DIR: &str = "leases";
const STAGING_DIR: &str = "staging";

/// Staged files older than this are crash residue, not live imports.
const STAGING_GRACE: Duration = Duration::from_secs(6 * 60 * 60);

/// Import limits (architecture.md 10.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CasQuota {
    /// Ceiling on the total bytes the store may hold.
    pub max_total_bytes: u64,
    /// Free space that must remain on the volume *after* an import.
    pub min_free_bytes: u64,
    /// Ceiling on a single artifact.
    pub max_artifact_bytes: u64,
}

impl CasQuota {
    /// Sized for the DAYU200 vertical: the pinned archive is ~730 MiB and a
    /// working set of a few builds plus one recovery copy must fit.
    pub fn dayu200_default() -> Self {
        CasQuota {
            max_total_bytes: 16 * 1024 * 1024 * 1024,
            min_free_bytes: 4 * 1024 * 1024 * 1024,
            max_artifact_bytes: 8 * 1024 * 1024 * 1024,
        }
    }
}

/// Reads free space for a volume.
///
/// Behind a trait so the quota logic is testable without filling a disk, and
/// so a host without the probe still fails closed rather than importing blind.
pub trait VolumeSpaceProbe: fmt::Debug + Send + Sync {
    fn available_bytes(&self, path: &Path) -> io::Result<u64>;
}

/// Reads free space from `df -Pk`, the POSIX-specified output format.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemVolumeSpaceProbe;

impl VolumeSpaceProbe for SystemVolumeSpaceProbe {
    fn available_bytes(&self, path: &Path) -> io::Result<u64> {
        let output = std::process::Command::new("df")
            .arg("-Pk")
            .arg(path)
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other("df reported a failure"));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        // POSIX `df -P`: header line, then one line per filesystem with
        // "Filesystem 1024-blocks Used Available Capacity Mounted-on".
        let line = text
            .lines()
            .nth(1)
            .ok_or_else(|| io::Error::other("df produced no data line"))?;
        let available_kb: u64 = line
            .split_whitespace()
            .nth(3)
            .and_then(|field| field.parse().ok())
            .ok_or_else(|| io::Error::other("df data line has no available column"))?;
        Ok(available_kb.saturating_mul(1024))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightReport {
    pub requested_bytes: u64,
    pub store_bytes_in_use: u64,
    pub volume_available_bytes: u64,
    pub accepted: bool,
    pub blocker: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedObject {
    pub digest: Sha256Digest,
    pub size_bytes: u64,
    /// True when the digest was already present and the bytes were discarded.
    pub deduplicated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GcReport {
    pub objects_removed: Vec<Sha256Digest>,
    pub bytes_reclaimed: u64,
    pub staging_files_removed: usize,
}

#[derive(Debug)]
pub enum CasError {
    Io(io::Error),
    QuotaExceeded(PreflightReport),
    DigestMismatch {
        expected: Sha256Digest,
        observed: Sha256Digest,
    },
    ArtifactTooLarge {
        size_bytes: u64,
        limit: u64,
    },
    NotFound(Sha256Digest),
    LeaseHeld {
        digest: Sha256Digest,
        holders: usize,
    },
    InvalidHolder(String),
}

impl fmt::Display for CasError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CasError::Io(error) => write!(f, "content store I/O failed: {error}"),
            CasError::QuotaExceeded(report) => write!(
                f,
                "import of {} bytes refused: {}",
                report.requested_bytes,
                report.blocker.as_deref().unwrap_or("quota exceeded")
            ),
            CasError::DigestMismatch { expected, observed } => {
                write!(f, "imported bytes hash to {observed}, expected {expected}")
            }
            CasError::ArtifactTooLarge { size_bytes, limit } => write!(
                f,
                "artifact of {size_bytes} bytes exceeds the {limit}-byte per-artifact limit"
            ),
            CasError::NotFound(digest) => write!(f, "no object for {digest}"),
            CasError::LeaseHeld { digest, holders } => {
                write!(f, "object {digest} still has {holders} lease holder(s)")
            }
            CasError::InvalidHolder(holder) => {
                write!(f, "lease holder {holder:?} is not a conforming identifier")
            }
        }
    }
}

impl std::error::Error for CasError {}

impl From<io::Error> for CasError {
    fn from(error: io::Error) -> Self {
        CasError::Io(error)
    }
}

/// The store itself.
#[derive(Debug)]
pub struct ContentAddressedStore {
    root: PathBuf,
    quota: CasQuota,
    space_probe: Box<dyn VolumeSpaceProbe>,
}

impl ContentAddressedStore {
    pub fn open(root: impl Into<PathBuf>, quota: CasQuota) -> Result<Self, CasError> {
        Self::open_with_probe(root, quota, Box::new(SystemVolumeSpaceProbe))
    }

    pub fn open_with_probe(
        root: impl Into<PathBuf>,
        quota: CasQuota,
        space_probe: Box<dyn VolumeSpaceProbe>,
    ) -> Result<Self, CasError> {
        let root = root.into();
        for directory in [OBJECTS_DIR, LEASES_DIR, STAGING_DIR] {
            create_private_dir(&root.join(directory))?;
        }
        Ok(ContentAddressedStore {
            root,
            quota,
            space_probe,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn quota(&self) -> CasQuota {
        self.quota
    }

    /// Checks quota and volume space before a byte is copied.
    ///
    /// A store that discovers it is full mid-import leaves partial state and a
    /// caller that already streamed a 700 MiB archive across an IPC boundary.
    pub fn preflight(&self, requested_bytes: u64) -> Result<PreflightReport, CasError> {
        let store_bytes_in_use = self.total_bytes()?;
        let volume_available_bytes = self.space_probe.available_bytes(&self.root)?;

        let mut blocker = None;
        if requested_bytes > self.quota.max_artifact_bytes {
            blocker = Some(format!(
                "artifact of {requested_bytes} bytes exceeds the per-artifact limit of {}",
                self.quota.max_artifact_bytes
            ));
        } else if store_bytes_in_use.saturating_add(requested_bytes) > self.quota.max_total_bytes {
            blocker = Some(format!(
                "store holds {store_bytes_in_use} bytes; adding {requested_bytes} would exceed the {}-byte ceiling",
                self.quota.max_total_bytes
            ));
        } else if volume_available_bytes < requested_bytes.saturating_add(self.quota.min_free_bytes)
        {
            blocker = Some(format!(
                "volume has {volume_available_bytes} bytes free; {requested_bytes} plus the {}-byte reserve does not fit",
                self.quota.min_free_bytes
            ));
        }

        Ok(PreflightReport {
            requested_bytes,
            store_bytes_in_use,
            volume_available_bytes,
            accepted: blocker.is_none(),
            blocker,
        })
    }

    /// Streams `source` into the store.
    ///
    /// `expected_size` drives preflight; `expected_digest`, when supplied, is
    /// checked before the object is published, so a lease can never point at
    /// bytes the authority did not authorize.
    pub fn import<R: Read>(
        &self,
        mut source: R,
        expected_size: u64,
        expected_digest: Option<Sha256Digest>,
    ) -> Result<ImportedObject, CasError> {
        let report = self.preflight(expected_size)?;
        if !report.accepted {
            return Err(CasError::QuotaExceeded(report));
        }

        let staging_path = self.root.join(STAGING_DIR).join(format!(
            "import-{}-{}.part",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|delta| delta.as_nanos())
                .unwrap_or(0)
        ));
        let mut staged = create_private_file(&staging_path)?;

        let mut hasher = Sha256::new();
        let mut written = 0u64;
        let mut buffer = vec![0u8; 1024 * 1024];
        let result = (|| -> Result<(), CasError> {
            loop {
                let count = source.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                written += count as u64;
                if written > self.quota.max_artifact_bytes {
                    return Err(CasError::ArtifactTooLarge {
                        size_bytes: written,
                        limit: self.quota.max_artifact_bytes,
                    });
                }
                hasher.update(&buffer[..count]);
                staged.write_all(&buffer[..count])?;
            }
            staged.flush()?;
            // Durability before publication: a rename that beats the data to
            // disk would publish a valid-looking object with missing bytes.
            staged.sync_all()?;
            Ok(())
        })();

        if let Err(error) = result {
            let _ = fs::remove_file(&staging_path);
            return Err(error);
        }
        drop(staged);

        let digest = hasher.finalize();
        if let Some(expected) = expected_digest
            && expected != digest
        {
            let _ = fs::remove_file(&staging_path);
            return Err(CasError::DigestMismatch {
                expected,
                observed: digest,
            });
        }

        let object_path = self.object_path(&digest);
        if object_path.exists() {
            let _ = fs::remove_file(&staging_path);
            return Ok(ImportedObject {
                digest,
                size_bytes: written,
                deduplicated: true,
            });
        }
        create_private_dir(object_path.parent().expect("object path has a parent"))?;
        // Published CAS bytes are immutable inputs. Keeping the object
        // read-only lets higher-level, content-bound indexes distinguish a
        // sealed object from one that may have been edited since import.
        set_private_permissions(&staging_path, 0o400)?;
        fs::rename(&staging_path, &object_path)?;
        Ok(ImportedObject {
            digest,
            size_bytes: written,
            deduplicated: false,
        })
    }

    pub fn contains(&self, digest: &Sha256Digest) -> bool {
        self.object_path(digest).exists()
    }

    pub fn open_object(&self, digest: &Sha256Digest) -> Result<File, CasError> {
        File::open(self.object_path(digest)).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                CasError::NotFound(*digest)
            } else {
                CasError::Io(error)
            }
        })
    }

    pub fn object_size(&self, digest: &Sha256Digest) -> Result<u64, CasError> {
        Ok(fs::metadata(self.object_path(digest))
            .map_err(|_| CasError::NotFound(*digest))?
            .len())
    }

    /// Makes a fully verified object immutable to ordinary writes.
    ///
    /// Older stores may contain objects imported before publication started
    /// sealing them. A parser may call this only after it has re-hashed the
    /// complete object and matched its content address.
    pub fn seal_object(&self, digest: &Sha256Digest) -> Result<(), CasError> {
        let path = self.object_path(digest);
        fs::metadata(&path).map_err(|_| CasError::NotFound(*digest))?;
        set_private_permissions(&path, 0o400)
    }

    /// Whether the object is a regular, non-symlink, read-only file.
    pub fn object_is_sealed(&self, digest: &Sha256Digest) -> Result<bool, CasError> {
        let metadata = fs::symlink_metadata(self.object_path(digest))
            .map_err(|_| CasError::NotFound(*digest))?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Ok(false);
        }
        Ok(is_read_only(&metadata))
    }

    /// Re-hashes an object and compares it with its own address.
    ///
    /// Store corruption must not be discovered by a device write
    /// (architecture.md 6.3).
    pub fn verify_object(&self, digest: &Sha256Digest) -> Result<bool, CasError> {
        let mut file = self.open_object(digest)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 1024 * 1024];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
        Ok(hasher.finalize() == *digest)
    }

    /// Takes a lease so GC cannot reclaim the object.
    pub fn acquire_lease(&self, digest: &Sha256Digest, holder: &str) -> Result<(), CasError> {
        if !self.contains(digest) {
            return Err(CasError::NotFound(*digest));
        }
        let holder = sanitize_holder(holder)?;
        let directory = self.lease_dir(digest);
        create_private_dir(&directory)?;
        let mut file = create_private_file(&directory.join(holder))?;
        file.write_all(digest.to_hex().as_bytes())?;
        file.sync_all()?;
        Ok(())
    }

    pub fn release_lease(&self, digest: &Sha256Digest, holder: &str) -> Result<(), CasError> {
        let holder = sanitize_holder(holder)?;
        let path = self.lease_dir(digest).join(holder);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(CasError::Io(error)),
        }
    }

    pub fn lease_holders(&self, digest: &Sha256Digest) -> Result<Vec<String>, CasError> {
        let directory = self.lease_dir(digest);
        if !directory.exists() {
            return Ok(Vec::new());
        }
        let mut holders = Vec::new();
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            if let Some(name) = entry.file_name().to_str() {
                holders.push(name.to_string());
            }
        }
        holders.sort();
        Ok(holders)
    }

    /// Removes unleased objects and abandoned staging files.
    ///
    /// Crash-safe by construction: a staged file is only removed once it is
    /// older than the grace window, so an import in flight in another process
    /// is never pulled out from under it.
    pub fn collect_garbage(&self) -> Result<GcReport, CasError> {
        let mut report = GcReport::default();

        for digest in self.list_objects()? {
            if !self.lease_holders(&digest)?.is_empty() {
                continue;
            }
            let path = self.object_path(&digest);
            let size = fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
            fs::remove_file(&path)?;
            let _ = fs::remove_dir(self.lease_dir(&digest));
            report.bytes_reclaimed += size;
            report.objects_removed.push(digest);
        }

        let staging = self.root.join(STAGING_DIR);
        if staging.exists() {
            let now = SystemTime::now();
            for entry in fs::read_dir(&staging)? {
                let entry = entry?;
                let metadata = entry.metadata()?;
                let age = metadata
                    .modified()
                    .ok()
                    .and_then(|modified| now.duration_since(modified).ok())
                    .unwrap_or_default();
                if age >= STAGING_GRACE {
                    fs::remove_file(entry.path())?;
                    report.staging_files_removed += 1;
                }
            }
        }
        Ok(report)
    }

    pub fn list_objects(&self) -> Result<Vec<Sha256Digest>, CasError> {
        let mut digests = Vec::new();
        let objects = self.root.join(OBJECTS_DIR);
        if !objects.exists() {
            return Ok(digests);
        }
        for prefix_entry in fs::read_dir(&objects)? {
            let prefix_entry = prefix_entry?;
            if !prefix_entry.file_type()?.is_dir() {
                continue;
            }
            for object_entry in fs::read_dir(prefix_entry.path())? {
                let object_entry = object_entry?;
                if let Some(name) = object_entry.file_name().to_str()
                    && let Ok(digest) = Sha256Digest::parse_hex(name)
                {
                    digests.push(digest);
                }
            }
        }
        digests.sort();
        Ok(digests)
    }

    pub fn total_bytes(&self) -> Result<u64, CasError> {
        let mut total = 0u64;
        for digest in self.list_objects()? {
            total += fs::metadata(self.object_path(&digest))
                .map(|meta| meta.len())
                .unwrap_or(0);
        }
        Ok(total)
    }

    fn object_path(&self, digest: &Sha256Digest) -> PathBuf {
        let hex = digest.to_hex();
        self.root.join(OBJECTS_DIR).join(&hex[..2]).join(&hex)
    }

    fn lease_dir(&self, digest: &Sha256Digest) -> PathBuf {
        self.root.join(LEASES_DIR).join(digest.to_hex())
    }
}

/// Lease holders become filenames, so the character set is constrained the same
/// way identifiers are.
fn sanitize_holder(holder: &str) -> Result<&str, CasError> {
    let conforming = !holder.is_empty()
        && holder.len() <= 128
        && holder
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'));
    if conforming {
        Ok(holder)
    } else {
        Err(CasError::InvalidHolder(holder.to_string()))
    }
}

fn create_private_dir(path: &Path) -> Result<(), CasError> {
    if !path.exists() {
        fs::create_dir_all(path)?;
    }
    set_private_permissions(path, 0o700)?;
    Ok(())
}

fn create_private_file(path: &Path) -> Result<File, CasError> {
    let file = File::create(path)?;
    set_private_permissions(path, 0o600)?;
    Ok(file)
}

#[cfg(unix)]
fn set_private_permissions(path: &Path, mode: u32) -> Result<(), CasError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path, _mode: u32) -> Result<(), CasError> {
    // Windows ACLs are a Stage B item; see architecture.md 15.2, which keeps
    // the Windows transport surface out of AF-V1/AF-V2 acceptance.
    Ok(())
}

#[cfg(unix)]
fn is_read_only(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o222 == 0
}

#[cfg(not(unix))]
fn is_read_only(metadata: &fs::Metadata) -> bool {
    metadata.permissions().readonly()
}

#[cfg(test)]
mod tests {
    use super::*;
    use arkforge_core::digest::sha256;

    #[derive(Debug)]
    struct FixedSpace(u64);

    impl VolumeSpaceProbe for FixedSpace {
        fn available_bytes(&self, _path: &Path) -> io::Result<u64> {
            Ok(self.0)
        }
    }

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "arkforge-cas-{}-{}-{:?}",
                name,
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            TempRoot(path)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn store(root: &TempRoot, quota: CasQuota, free: u64) -> ContentAddressedStore {
        ContentAddressedStore::open_with_probe(&root.0, quota, Box::new(FixedSpace(free))).unwrap()
    }

    fn quota() -> CasQuota {
        CasQuota {
            max_total_bytes: 1_000_000,
            min_free_bytes: 10_000,
            max_artifact_bytes: 500_000,
        }
    }

    #[test]
    fn imports_are_addressed_by_their_own_hash() {
        let root = TempRoot::new("import");
        let store = store(&root, quota(), 10_000_000);
        let payload = b"firmware bytes".repeat(100);
        let imported = store
            .import(payload.as_slice(), payload.len() as u64, None)
            .unwrap();
        assert_eq!(imported.digest, sha256(&payload));
        assert_eq!(imported.size_bytes, payload.len() as u64);
        assert!(!imported.deduplicated);
        assert!(store.contains(&imported.digest));
        assert!(store.verify_object(&imported.digest).unwrap());
    }

    #[test]
    fn a_second_import_of_the_same_bytes_deduplicates() {
        let root = TempRoot::new("dedup");
        let store = store(&root, quota(), 10_000_000);
        let payload = b"same bytes".to_vec();
        store
            .import(payload.as_slice(), payload.len() as u64, None)
            .unwrap();
        let second = store
            .import(payload.as_slice(), payload.len() as u64, None)
            .unwrap();
        assert!(second.deduplicated);
        assert_eq!(store.list_objects().unwrap().len(), 1);
    }

    #[test]
    fn a_digest_mismatch_never_publishes_an_object() {
        let root = TempRoot::new("mismatch");
        let store = store(&root, quota(), 10_000_000);
        let payload = b"actual bytes".to_vec();
        let error = store
            .import(
                payload.as_slice(),
                payload.len() as u64,
                Some(sha256(b"different bytes")),
            )
            .unwrap_err();
        assert!(matches!(error, CasError::DigestMismatch { .. }));
        assert!(store.list_objects().unwrap().is_empty());
        // And nothing is left staged.
        let staged: Vec<_> = fs::read_dir(root.0.join(STAGING_DIR))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(staged.is_empty(), "staging must be clean after a rejection");
    }

    #[test]
    fn preflight_refuses_when_the_volume_would_drop_below_the_reserve() {
        let root = TempRoot::new("space");
        // 20_000 free, 10_000 reserved: a 15_000-byte import does not fit.
        let store = store(&root, quota(), 20_000);
        let report = store.preflight(15_000).unwrap();
        assert!(!report.accepted);
        assert!(report.blocker.unwrap().contains("does not fit"));

        let accepted = store.preflight(5_000).unwrap();
        assert!(accepted.accepted, "{accepted:?}");
    }

    #[test]
    fn preflight_refuses_an_artifact_over_the_per_artifact_limit() {
        let root = TempRoot::new("toobig");
        let store = store(&root, quota(), 10_000_000);
        let report = store.preflight(600_000).unwrap();
        assert!(!report.accepted);
        assert!(report.blocker.unwrap().contains("per-artifact limit"));
    }

    #[test]
    fn the_store_ceiling_counts_what_is_already_held() {
        let root = TempRoot::new("ceiling");
        let store = store(&root, quota(), 10_000_000);
        for filler in [7u8, 9u8] {
            let payload = vec![filler; 400_000];
            store
                .import(payload.as_slice(), payload.len() as u64, None)
                .unwrap();
        }
        assert_eq!(store.total_bytes().unwrap(), 800_000);
        // 800_000 held + 150_000 fits under the 1_000_000 ceiling; 300_000 does
        // not — and 300_000 is well inside the per-artifact limit, so the
        // ceiling is what refuses it.
        assert!(store.preflight(150_000).unwrap().accepted);
        let refused = store.preflight(300_000).unwrap();
        assert!(!refused.accepted);
        assert!(
            refused.blocker.as_deref().unwrap().contains("ceiling"),
            "{refused:?}"
        );
    }

    #[test]
    fn gc_reclaims_only_unleased_objects() {
        let root = TempRoot::new("gc");
        let store = store(&root, quota(), 10_000_000);
        let leased = store.import(&b"leased"[..], 6, None).unwrap();
        let orphan = store.import(&b"orphan"[..], 6, None).unwrap();
        store.acquire_lease(&leased.digest, "JOB-1").unwrap();

        let report = store.collect_garbage().unwrap();
        assert_eq!(report.objects_removed, vec![orphan.digest]);
        assert!(store.contains(&leased.digest));
        assert!(!store.contains(&orphan.digest));

        store.release_lease(&leased.digest, "JOB-1").unwrap();
        let second = store.collect_garbage().unwrap();
        assert_eq!(second.objects_removed, vec![leased.digest]);
    }

    #[test]
    fn two_holders_both_have_to_release_before_gc_reclaims() {
        let root = TempRoot::new("two-holders");
        let store = store(&root, quota(), 10_000_000);
        let object = store.import(&b"shared"[..], 6, None).unwrap();
        store.acquire_lease(&object.digest, "JOB-1").unwrap();
        store.acquire_lease(&object.digest, "PLAN-9").unwrap();
        assert_eq!(store.lease_holders(&object.digest).unwrap().len(), 2);

        store.release_lease(&object.digest, "JOB-1").unwrap();
        assert!(store.collect_garbage().unwrap().objects_removed.is_empty());
        assert!(store.contains(&object.digest));

        store.release_lease(&object.digest, "PLAN-9").unwrap();
        assert_eq!(
            store.collect_garbage().unwrap().objects_removed,
            vec![object.digest]
        );
    }

    #[test]
    fn gc_leaves_a_fresh_staging_file_alone() {
        let root = TempRoot::new("staging");
        let store = store(&root, quota(), 10_000_000);
        let staged = root.0.join(STAGING_DIR).join("import-in-flight.part");
        fs::write(&staged, b"partial").unwrap();
        let report = store.collect_garbage().unwrap();
        assert_eq!(report.staging_files_removed, 0);
        assert!(staged.exists(), "an in-flight import must survive GC");
    }

    #[test]
    fn a_lease_on_a_missing_object_is_refused() {
        let root = TempRoot::new("nolease");
        let store = store(&root, quota(), 10_000_000);
        assert!(matches!(
            store.acquire_lease(&sha256(b"absent"), "JOB-1"),
            Err(CasError::NotFound(_))
        ));
    }

    #[test]
    fn a_holder_name_cannot_escape_the_lease_directory() {
        let root = TempRoot::new("holder");
        let store = store(&root, quota(), 10_000_000);
        let object = store.import(&b"x"[..], 1, None).unwrap();
        for holder in ["../escape", "with/slash", "with space", ""] {
            assert!(
                matches!(
                    store.acquire_lease(&object.digest, holder),
                    Err(CasError::InvalidHolder(_))
                ),
                "{holder:?} should be refused"
            );
        }
    }

    #[test]
    fn corruption_is_detected_by_verify_rather_than_by_a_device_write() {
        let root = TempRoot::new("corrupt");
        let store = store(&root, quota(), 10_000_000);
        let object = store.import(&b"good bytes"[..], 10, None).unwrap();
        let hex = object.digest.to_hex();
        let path = root.0.join(OBJECTS_DIR).join(&hex[..2]).join(&hex);
        set_private_permissions(&path, 0o600).unwrap();
        fs::write(&path, b"tampered!!").unwrap();
        assert!(!store.verify_object(&object.digest).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn objects_are_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let root = TempRoot::new("perms");
        let store = store(&root, quota(), 10_000_000);
        let object = store.import(&b"secret firmware"[..], 15, None).unwrap();
        let hex = object.digest.to_hex();
        let path = root.0.join(OBJECTS_DIR).join(&hex[..2]).join(&hex);
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o400, "published object mode {mode:o} is not sealed");
        assert!(store.object_is_sealed(&object.digest).unwrap());
    }
}
