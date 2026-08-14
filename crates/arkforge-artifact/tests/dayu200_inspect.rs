//! End-to-end artifact vertical: import → inspect → manifest facts.
//!
//! AF-V1 acceptance: current DAYU200 archive facts parity, unknown member fail
//! closed, no device mutation (nothing here can reach a device — the parser has
//! no transport at all).

use arkforge_artifact::cas::{CasQuota, ContentAddressedStore, VolumeSpaceProbe};
use arkforge_artifact::dayu200::{self, Dayu200ParseError};
use arkforge_artifact::fixture;
use arkforge_artifact::manifest::{MemberRole, ParserConfidence};
use arkforge_core::digest::sha256;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug)]
struct AmpleSpace;

impl VolumeSpaceProbe for AmpleSpace {
    fn available_bytes(&self, _path: &Path) -> io::Result<u64> {
        Ok(64 * 1024 * 1024 * 1024)
    }
}

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "arkforge-it-{}-{}-{:?}",
            name,
            std::process::id(),
            std::thread::current().id()
        ));
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

fn store(root: &TempRoot) -> ContentAddressedStore {
    ContentAddressedStore::open_with_probe(
        &root.0,
        CasQuota::dayu200_default(),
        Box::new(AmpleSpace),
    )
    .unwrap()
}

#[test]
fn import_then_inspect_reproduces_the_pinned_archive_facts() {
    let root = TempRoot::new("inspect");
    let store = store(&root);
    let archive = fixture::dayu200_archive();

    let imported = store
        .import(archive.as_slice(), archive.len() as u64, None)
        .unwrap();
    assert_eq!(imported.digest, sha256(&archive));

    // Inspection reads the stored object, never the caller's path
    // (architecture.md 10.1).
    let object = store.open_object(&imported.digest).unwrap();
    let manifest = dayu200::inspect(object).unwrap();

    assert_eq!(manifest.content_digest, imported.digest);
    assert_eq!(manifest.size_bytes, archive.len() as u64);
    assert_eq!(manifest.format.id.as_str(), dayu200::FORMAT_ID);

    // 17 members, exactly the pinned inventory.
    let observed: Vec<&str> = manifest.members.iter().map(|m| m.path.as_str()).collect();
    let mut expected: Vec<&str> = fixture::PINNED_MEMBER_NAMES.to_vec();
    let mut sorted_observed = observed.clone();
    sorted_observed.sort_unstable();
    expected.sort_unstable();
    assert_eq!(sorted_observed, expected);

    // Roles follow the container format's rules, not a device allowlist.
    let role = |path: &str| manifest.member(path).unwrap().role;
    assert_eq!(role("parameter.txt"), MemberRole::PartitionTable);
    assert_eq!(role("MiniLoaderAll.bin"), MemberRole::Loader);
    assert_eq!(role("system.img"), MemberRole::ImageCandidate);
    assert_eq!(role("userdata.img"), MemberRole::ImageCandidate);
    assert_eq!(role("config.cfg"), MemberRole::Metadata);
    assert_eq!(role("daily_build.log"), MemberRole::Metadata);
    assert_eq!(role("manifest_tag.xml"), MemberRole::Metadata);

    // `updater_binary` has no extension the format recognizes. It is reported
    // as unclassified, which is an execution-relevant unknown — the pinned
    // ArkDeck profile classifies it out of band, as a Profile fact.
    assert_eq!(role("updater_binary"), MemberRole::Unclassified);
    assert_eq!(manifest.unclassified_members, vec!["updater_binary"]);
    assert_eq!(manifest.confidence, ParserConfidence::ResearchOnly);
    assert!(manifest
        .execution_relevant_unknowns
        .iter()
        .any(|unknown| unknown.summary.contains("updater_binary")));

    // Every member hash is the hash of the bytes the fixture put there.
    assert_eq!(
        manifest.member("vendor.img").unwrap().sha256,
        sha256(&fixture::fixture_body("vendor.img", 2048))
    );

    // The partition table decodes to ArkDeck's pinned values.
    let table = manifest.partition_table.as_ref().unwrap();
    assert_eq!(table.device, "rk29xxnand");
    assert_eq!(table.entries.len(), 15);
    assert_eq!(table.entry("uboot").unwrap().offset_sectors, 8192);
    assert_eq!(table.entry("system").unwrap().offset_sectors, 245_760);
    assert_eq!(table.entry("vendor").unwrap().offset_sectors, 4_440_064);
    assert_eq!(table.entry("chip_ckm").unwrap().offset_sectors, 6_938_624);
    assert_eq!(table.entry("userdata").unwrap().offset_sectors, 19_955_712);
    assert_eq!(table.entry("userdata").unwrap().size_sectors, None);

    // Build facts come from inside a hashed image, not from a filename.
    let fact = |key: &str| {
        manifest
            .build_facts
            .iter()
            .find(|(id, _)| id.as_str() == key)
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(fact("const.ohos.fullname"), Some(fixture::FIXTURE_BUILD_VERSION));
    assert_eq!(fact("const.product.model"), Some(fixture::FIXTURE_PRODUCT_MODEL));
}

#[test]
fn the_manifest_digest_is_stable_across_two_inspections() {
    let archive = fixture::dayu200_archive();
    let first = dayu200::inspect(archive.as_slice()).unwrap();
    let second = dayu200::inspect(archive.as_slice()).unwrap();
    assert_eq!(first.digest().unwrap(), second.digest().unwrap());
}

#[test]
fn an_extra_unknown_member_is_reported_rather_than_ignored() {
    let mut tar = arkforge_artifact::fixture::TarArchiveBuilder::new();
    for name in fixture::PINNED_MEMBER_NAMES {
        let body = if name == "parameter.txt" {
            format!("{}\n", fixture::PINNED_CMDLINE).into_bytes()
        } else {
            fixture::fixture_body(name, 512)
        };
        tar = tar.add_file(name, &body);
    }
    tar = tar.add_file("surprise.payload", b"unexpected");
    let archive = fixture::gzip_stored(&tar.finish());

    let manifest = dayu200::inspect(archive.as_slice()).unwrap();
    assert!(manifest
        .unclassified_members
        .contains(&"surprise.payload".to_string()));
    assert_eq!(manifest.confidence, ParserConfidence::ResearchOnly);
}

#[test]
fn a_corrupted_partition_table_fails_closed() {
    let archive = fixture::dayu200_archive_with(|name, body| {
        if name == "parameter.txt" {
            b"CMDLINE:mtdparts=rk29xxnand:0x00002000@0x00002000(uboot:secure)".to_vec()
        } else {
            body
        }
    });
    let error = dayu200::inspect(archive.as_slice()).unwrap_err();
    assert!(
        matches!(error, Dayu200ParseError::Parameter(_)),
        "unexpected error {error}"
    );
}

#[test]
fn a_truncated_archive_fails_closed() {
    let archive = fixture::dayu200_archive();
    let truncated = &archive[..archive.len() * 2 / 3];
    assert!(dayu200::inspect(truncated).is_err());
}

#[test]
fn contradictory_build_facts_are_an_error_not_a_preference() {
    // Two image members claim different builds. Choosing one would silently
    // pin postflight to a value the device may never answer.
    let archive = fixture::dayu200_archive_with(|name, body| {
        if name == "vendor.img" {
            let mut body = body;
            body.extend_from_slice(b"\nconst.ohos.fullname=OpenHarmony-6.0.0.1\n");
            body
        } else {
            body
        }
    });
    let error = dayu200::inspect(archive.as_slice()).unwrap_err();
    assert!(
        matches!(error, Dayu200ParseError::ContradictoryBuildFact { .. }),
        "unexpected error {error}"
    );
}

#[test]
fn a_lease_keeps_the_imported_archive_alive_across_a_gc() {
    let root = TempRoot::new("lease");
    let store = store(&root);
    let archive = fixture::dayu200_archive();
    let imported = store
        .import(archive.as_slice(), archive.len() as u64, None)
        .unwrap();
    store.acquire_lease(&imported.digest, "PLAN-001").unwrap();

    let report = store.collect_garbage().unwrap();
    assert!(report.objects_removed.is_empty());
    assert!(store.verify_object(&imported.digest).unwrap());

    store.release_lease(&imported.digest, "PLAN-001").unwrap();
    let report = store.collect_garbage().unwrap();
    assert_eq!(report.objects_removed, vec![imported.digest]);
}
