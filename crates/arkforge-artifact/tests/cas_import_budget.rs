//! CAS import budget for a full DAYU200 bundle.
//!
//! AF-V1 acceptance (architecture.md 22, 10.2): "DAYU200 整包 CAS 导入在声明预算内"
//! and "实测 available-space preflight".
//!
//! The budget is declared here and measured here. What is measured is *this
//! implementation's* import path at the real bundle size — stream, hash, stage,
//! fsync, publish — using a synthetic stream of the pinned archive's exact byte
//! count. It is not a measurement of the vendor archive, which lives in neither
//! repository; the shape of the work (a full streaming read plus a full SHA-256)
//! is identical either way, because content addressing has to hash every byte.
//!
//! Run the measurement with:
//!
//! ```text
//! cargo test -p arkforge-artifact --release --test cas_import_budget -- --ignored --nocapture
//! ```

use arkforge_artifact::cas::{CasQuota, ContentAddressedStore, VolumeSpaceProbe};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// The pinned DAYU200 `images.tar.gz` size, from ArkDeck
/// `RockchipFlashProfile.dayu200.archiveSizeBytes`.
const DAYU200_ARCHIVE_BYTES: u64 = 730_769_584;

/// The declared budget.
///
/// architecture.md 10.2 anchors streaming import at roughly 10 GB/min from the
/// in-repo #1003 measurement. A ~730 MiB bundle at that rate is about 4.4 s, so
/// 60 s is a generous ceiling that still fails loudly if the import path grows a
/// per-byte cost — an extra copy, a second hash, a small-chunk read loop.
const IMPORT_BUDGET_SECONDS: f64 = 60.0;

#[derive(Debug)]
struct AmpleSpace;

impl VolumeSpaceProbe for AmpleSpace {
    fn available_bytes(&self, _path: &Path) -> io::Result<u64> {
        Ok(256 * 1024 * 1024 * 1024)
    }
}

#[derive(Debug)]
struct TightSpace(u64);

impl VolumeSpaceProbe for TightSpace {
    fn available_bytes(&self, _path: &Path) -> io::Result<u64> {
        Ok(self.0)
    }
}

/// A deterministic byte source of a given length, without materializing it.
struct SyntheticArchive {
    remaining: u64,
    state: u32,
}

impl SyntheticArchive {
    fn new(length: u64) -> Self {
        SyntheticArchive {
            remaining: length,
            state: 0x1234_5678,
        }
    }
}

impl Read for SyntheticArchive {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let count = out.len().min(self.remaining as usize);
        for slot in out[..count].iter_mut() {
            self.state = self
                .state
                .wrapping_mul(1_664_525)
                .wrapping_add(1_013_904_223);
            *slot = (self.state >> 24) as u8;
        }
        self.remaining -= count as u64;
        Ok(count)
    }
}

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("arkforge-budget-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        TempRoot(path)
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
#[ignore = "writes ~730 MiB; run explicitly with --ignored"]
fn a_full_dayu200_bundle_imports_within_the_declared_budget() {
    let root = TempRoot::new("full");
    let store = ContentAddressedStore::open_with_probe(
        &root.0,
        CasQuota::dayu200_default(),
        Box::new(AmpleSpace),
    )
    .unwrap();

    let started = Instant::now();
    let imported = store
        .import(
            SyntheticArchive::new(DAYU200_ARCHIVE_BYTES),
            DAYU200_ARCHIVE_BYTES,
            None,
        )
        .unwrap();
    let elapsed = started.elapsed().as_secs_f64();

    assert_eq!(imported.size_bytes, DAYU200_ARCHIVE_BYTES);
    let throughput_mb_s = DAYU200_ARCHIVE_BYTES as f64 / 1_048_576.0 / elapsed;
    println!(
        "import: {} bytes in {elapsed:.2}s ({throughput_mb_s:.1} MiB/s), budget {IMPORT_BUDGET_SECONDS:.0}s",
        DAYU200_ARCHIVE_BYTES
    );
    assert!(
        elapsed < IMPORT_BUDGET_SECONDS,
        "import took {elapsed:.2}s, over the declared {IMPORT_BUDGET_SECONDS:.0}s budget"
    );

    // Verification re-reads and re-hashes the whole object; it is the check a
    // plan runs before trusting a stored artifact, so its cost is budgeted too.
    let started = Instant::now();
    assert!(store.verify_object(&imported.digest).unwrap());
    let verify_elapsed = started.elapsed().as_secs_f64();
    println!("verify: {verify_elapsed:.2}s");
    assert!(verify_elapsed < IMPORT_BUDGET_SECONDS);
}

#[test]
fn the_available_space_preflight_refuses_a_bundle_that_would_not_fit() {
    // Measured against the real preflight path, at the real bundle size, with
    // no bytes copied — which is the point: preflight answers before the
    // caller streams 730 MiB across an IPC boundary.
    let root = TempRoot::new("preflight");
    let quota = CasQuota::dayu200_default();

    // Just enough for the bundle but not for the reserve.
    let tight = ContentAddressedStore::open_with_probe(
        &root.0,
        quota,
        Box::new(TightSpace(DAYU200_ARCHIVE_BYTES + quota.min_free_bytes - 1)),
    )
    .unwrap();
    let report = tight.preflight(DAYU200_ARCHIVE_BYTES).unwrap();
    assert!(!report.accepted, "{report:?}");
    assert!(report.blocker.unwrap().contains("does not fit"));

    // One byte more and it fits.
    let ample = ContentAddressedStore::open_with_probe(
        &root.0,
        quota,
        Box::new(TightSpace(DAYU200_ARCHIVE_BYTES + quota.min_free_bytes)),
    )
    .unwrap();
    assert!(ample.preflight(DAYU200_ARCHIVE_BYTES).unwrap().accepted);
}

#[test]
fn the_default_quota_admits_a_dayu200_bundle_and_bounds_the_store() {
    let quota = CasQuota::dayu200_default();
    assert!(
        DAYU200_ARCHIVE_BYTES < quota.max_artifact_bytes,
        "the per-artifact limit must admit a full bundle"
    );
    // Room for several builds plus a recovery copy, and a hard ceiling.
    assert!(quota.max_total_bytes >= DAYU200_ARCHIVE_BYTES * 8);
    assert!(quota.max_total_bytes < u64::MAX);
}
