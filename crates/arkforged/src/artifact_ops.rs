//! Offline artifact operations shared by the daemon and canonical CLI.

use arkforge_artifact::cas::ContentAddressedStore;
use arkforge_artifact::manifest::{
    ArchiveMemberFact, ArtifactManifest, GrammarBranch, MemberRole, ParserConfidence,
    PartitionAttribute, PartitionEntryFact, PartitionTableFact,
};
use arkforge_artifact::{dayu200, pac};
use arkforge_core::Sha256Digest;
use arkforge_core::identity::{ArtifactFormat, Version};
use arkforge_core::ids::OpaqueId;
use arkforge_core::plan::ExecutionUnknown;
use arkforge_core::profile::DeviceProfile;
use arkforge_ipc::messages::{ArchiveMember, InspectArtifactResponse, KeyValue, PartitionEntry};
use arkforge_ipc::wire;
use arkforge_platform::{replace_file, sync_directory};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

const MANIFEST_CACHE_SCHEMA: &str = "arkforge.artifact-manifest-cache/v1";
const MANIFEST_CACHE_DIR: &str = "manifests";
const MAX_MANIFEST_CACHE_BYTES: u64 = 8 * 1024 * 1024;
static MANIFEST_CACHE_TEMPORARY: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileTargetCoverage {
    pub write_order: u32,
    pub partition: String,
    pub source_member: Option<String>,
    pub source_size_bytes: Option<u64>,
    pub present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileCoverage {
    pub profile_id: String,
    pub profile_version: String,
    pub profile_sha256: String,
    pub complete: bool,
    pub targets: Vec<ProfileTargetCoverage>,
}

/// Reads a container with the parser its framing indicates.
///
/// Gzip framing selects the measured DAYU200 archive parser. Other input goes
/// to the PAC research observer, which remains non-production and carries its
/// unknowns in the returned manifest.
pub fn inspect_container<R: Read>(mut source: R) -> Result<ArtifactManifest, String> {
    let mut magic = [0u8; 2];
    let mut filled = 0usize;
    while filled < magic.len() {
        match source.read(&mut magic[filled..]) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(error) => return Err(error.to_string()),
        }
    }
    let head = magic[..filled].to_vec();
    let rejoined = std::io::Read::chain(std::io::Cursor::new(head), source);

    if filled == 2 && magic[0] == 0x1f && magic[1] == 0x8b {
        dayu200::inspect(rejoined).map_err(|error| error.to_string())
    } else {
        pac::inspect(rejoined)
            .map(|(manifest, _)| manifest)
            .map_err(|error| error.to_string())
    }
}

/// Returns a parser manifest without re-inflating and re-hashing an immutable
/// content-addressed artifact on every daemon restart.
///
/// The first inspection still parses every byte, verifies that the parser's
/// content digest equals the object's address, seals the object read-only and
/// atomically publishes a manifest whose own canonical digest is checked on
/// every load. A missing, writable, malformed or stale cache is only a miss:
/// ArkForge falls back to the full parser before it can materialize a plan.
pub fn inspect_stored_container(
    store: &ContentAddressedStore,
    digest: &Sha256Digest,
) -> Result<ArtifactManifest, String> {
    if store.object_is_sealed(digest).unwrap_or(false)
        && let Ok(Some(manifest)) = load_manifest_cache(store, digest)
    {
        return Ok(manifest);
    }

    let object = store
        .open_object(digest)
        .map_err(|error| error.to_string())?;
    let manifest = inspect_container(object)?;
    if manifest.content_digest != *digest {
        return Err(format!(
            "artifact object address {} does not match its parsed content digest {}",
            digest, manifest.content_digest
        ));
    }
    manifest.validate().map_err(|error| error.to_string())?;
    store
        .seal_object(digest)
        .map_err(|error| error.to_string())?;
    // Cache publication is an optimization, not a correctness prerequisite.
    // A read-only filesystem may still execute safely by parsing again.
    let _ = persist_manifest_cache(store.root(), digest, &manifest);
    Ok(manifest)
}

fn load_manifest_cache(
    store: &ContentAddressedStore,
    digest: &Sha256Digest,
) -> Result<Option<ArtifactManifest>, String> {
    let path = manifest_cache_path(store.root(), digest);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || !metadata_is_read_only(&metadata)
        || metadata.len() > MAX_MANIFEST_CACHE_BYTES
    {
        return Ok(None);
    }
    let object_size = store
        .object_size(digest)
        .map_err(|error| error.to_string())?;
    let bytes = fs::read(&path).map_err(|error| error.to_string())?;
    decode_manifest_cache(&bytes, digest, object_size).map(Some)
}

fn persist_manifest_cache(
    store_root: &Path,
    digest: &Sha256Digest,
    manifest: &ArtifactManifest,
) -> Result<(), String> {
    let directory = store_root.join(MANIFEST_CACHE_DIR);
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    set_mode(&directory, 0o700)?;

    let mut encoded = Vec::new();
    wire::write_string(&mut encoded, 1, MANIFEST_CACHE_SCHEMA);
    wire::write_message(&mut encoded, 2, &manifest_response(manifest).encode());
    wire::write_string(&mut encoded, 3, &manifest.format.version.to_string());
    if let Some(table) = &manifest.partition_table {
        wire::write_bool(&mut encoded, 4, true);
        wire::write_string(&mut encoded, 5, &table.device);
        wire::write_uint32(&mut encoded, 6, table.logical_block_size);
    }

    let target = manifest_cache_path(store_root, digest);
    let temporary = directory.join(format!(
        ".{}.{}.{}.tmp",
        digest,
        std::process::id(),
        MANIFEST_CACHE_TEMPORARY.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> Result<(), String> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| error.to_string())?;
        set_mode(&temporary, 0o600)?;
        file.write_all(&encoded)
            .map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        drop(file);
        set_mode(&temporary, 0o400)?;
        prepare_manifest_cache_target(&target)?;
        replace_file(&temporary, &target).map_err(|error| error.to_string())?;
        sync_directory(&directory).map_err(|error| error.to_string())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(windows)]
fn prepare_manifest_cache_target(target: &Path) -> Result<(), String> {
    let metadata = match fs::symlink_metadata(target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err("manifest cache replacement target is not a regular file".into());
    }
    // MoveFileExW refuses to replace a read-only destination. Clearing only
    // this cache file's attribute keeps replacement atomic; a crash before the
    // rename leaves a writable cache, which load_manifest_cache rejects.
    set_mode(target, 0o600)
}

#[cfg(not(windows))]
fn prepare_manifest_cache_target(_target: &Path) -> Result<(), String> {
    Ok(())
}

fn decode_manifest_cache(
    bytes: &[u8],
    expected_digest: &Sha256Digest,
    object_size: u64,
) -> Result<ArtifactManifest, String> {
    let mut schema = String::new();
    let mut response = None;
    let mut version = String::new();
    let mut has_partition_table = false;
    let mut partition_device = String::new();
    let mut logical_block_size = 0u32;
    let mut reader = wire::Reader::new(bytes);
    while let Some((field, value)) = reader.next_field().map_err(|error| error.to_string())? {
        match field {
            1 => {
                schema = value
                    .as_str(1)
                    .map_err(|error| error.to_string())?
                    .to_string()
            }
            2 => {
                response = Some(
                    InspectArtifactResponse::decode(
                        value.as_bytes().map_err(|error| error.to_string())?,
                    )
                    .map_err(|error| error.to_string())?,
                )
            }
            3 => {
                version = value
                    .as_str(3)
                    .map_err(|error| error.to_string())?
                    .to_string()
            }
            4 => has_partition_table = value.as_bool().map_err(|error| error.to_string())?,
            5 => {
                partition_device = value
                    .as_str(5)
                    .map_err(|error| error.to_string())?
                    .to_string()
            }
            6 => {
                logical_block_size =
                    u32::try_from(value.as_u64().map_err(|error| error.to_string())?)
                        .map_err(|_| "cached logical block size exceeds u32".to_string())?
            }
            _ => {}
        }
    }
    if schema != MANIFEST_CACHE_SCHEMA {
        return Err("manifest cache schema is absent or unsupported".into());
    }
    let response = response.ok_or_else(|| "manifest cache has no response".to_string())?;
    if response.size_bytes != object_size {
        return Err("manifest cache size does not match the stored object".into());
    }
    let content_digest =
        Sha256Digest::parse_hex(&response.content_sha256).map_err(|error| error.to_string())?;
    if content_digest != *expected_digest {
        return Err("manifest cache content digest does not match its object address".into());
    }

    let manifest = ArtifactManifest {
        format: ArtifactFormat {
            id: OpaqueId::new(response.format_id).map_err(|error| error.to_string())?,
            version: Version::parse(&version)
                .ok_or_else(|| "manifest cache has an invalid format version".to_string())?,
        },
        content_digest,
        size_bytes: response.size_bytes,
        members: response
            .members
            .into_iter()
            .map(|member| {
                Ok(ArchiveMemberFact {
                    path: member.path,
                    size_bytes: member.size_bytes,
                    sha256: Sha256Digest::parse_hex(&member.sha256)
                        .map_err(|error| error.to_string())?,
                    role: parse_member_role(&member.role)?,
                })
            })
            .collect::<Result<_, String>>()?,
        partition_table: if has_partition_table {
            if partition_device.is_empty() || logical_block_size == 0 {
                return Err("manifest cache has an incomplete partition table".into());
            }
            Some(PartitionTableFact {
                device: partition_device,
                logical_block_size,
                entries: response
                    .partitions
                    .into_iter()
                    .map(|entry| {
                        Ok(PartitionEntryFact {
                            index: entry.index,
                            name: entry.name,
                            offset_sectors: entry.offset_sectors,
                            size_sectors: entry.size_sectors,
                            attribute: if entry.attribute.is_empty() {
                                None
                            } else {
                                Some(PartitionAttribute::parse(&entry.attribute).ok_or_else(
                                    || {
                                        "manifest cache has an unknown partition attribute"
                                            .to_string()
                                    },
                                )?)
                            },
                            grammar_branch: parse_grammar_branch(&entry.grammar_branch)?,
                        })
                    })
                    .collect::<Result<_, String>>()?,
            })
        } else {
            if !response.partitions.is_empty() {
                return Err("manifest cache has partitions without a table".into());
            }
            None
        },
        build_facts: response
            .build_facts
            .into_iter()
            .map(|fact| {
                Ok((
                    OpaqueId::new(fact.key).map_err(|error| error.to_string())?,
                    fact.value,
                ))
            })
            .collect::<Result<_, String>>()?,
        unclassified_members: response.unclassified_members,
        execution_relevant_unknowns: response
            .execution_relevant_unknowns
            .into_iter()
            .map(|unknown| {
                Ok(ExecutionUnknown {
                    id: OpaqueId::new(unknown.key).map_err(|error| error.to_string())?,
                    summary: unknown.value,
                })
            })
            .collect::<Result<_, String>>()?,
        confidence: match response.confidence.as_str() {
            "researchOnly" => ParserConfidence::ResearchOnly,
            "productionManifest" => ParserConfidence::ProductionManifest,
            _ => return Err("manifest cache has an unknown confidence".into()),
        },
    };
    manifest.validate().map_err(|error| error.to_string())?;
    let actual_manifest_digest = manifest.digest().map_err(|error| error.to_string())?;
    let cached_manifest_digest =
        Sha256Digest::parse_hex(&response.manifest_sha256).map_err(|error| error.to_string())?;
    if actual_manifest_digest != cached_manifest_digest {
        return Err("manifest cache canonical digest does not match its payload".into());
    }
    Ok(manifest)
}

fn parse_member_role(value: &str) -> Result<MemberRole, String> {
    match value {
        "partitionTable" => Ok(MemberRole::PartitionTable),
        "loader" => Ok(MemberRole::Loader),
        "imageCandidate" => Ok(MemberRole::ImageCandidate),
        "metadata" => Ok(MemberRole::Metadata),
        "unclassified" => Ok(MemberRole::Unclassified),
        _ => Err("manifest cache has an unknown member role".into()),
    }
}

fn parse_grammar_branch(value: &str) -> Result<GrammarBranch, String> {
    match value {
        "fixed" => Ok(GrammarBranch::Fixed),
        "fixedBootable" => Ok(GrammarBranch::FixedBootable),
        "remainderGrow" => Ok(GrammarBranch::RemainderGrow),
        _ => Err("manifest cache has an unknown grammar branch".into()),
    }
}

fn manifest_cache_path(store_root: &Path, digest: &Sha256Digest) -> std::path::PathBuf {
    store_root
        .join(MANIFEST_CACHE_DIR)
        .join(format!("{}.manifest", digest))
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|error| error.to_string())
}

#[cfg(not(unix))]
fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    let mut permissions = fs::metadata(path)
        .map_err(|error| error.to_string())?
        .permissions();
    permissions.set_readonly(mode & 0o200 == 0);
    fs::set_permissions(path, permissions).map_err(|error| error.to_string())
}

#[cfg(unix)]
fn metadata_is_read_only(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o222 == 0
}

#[cfg(not(unix))]
fn metadata_is_read_only(metadata: &fs::Metadata) -> bool {
    metadata.permissions().readonly()
}

pub fn manifest_response(manifest: &ArtifactManifest) -> InspectArtifactResponse {
    let mut response = InspectArtifactResponse {
        format_id: manifest.format.id.to_string(),
        content_sha256: manifest.content_digest.to_hex(),
        size_bytes: manifest.size_bytes,
        confidence: manifest.confidence.as_str().to_string(),
        manifest_sha256: manifest
            .digest()
            .map(|digest| digest.to_hex())
            .unwrap_or_default(),
        unclassified_members: manifest.unclassified_members.clone(),
        ..InspectArtifactResponse::default()
    };
    for member in &manifest.members {
        response.members.push(ArchiveMember {
            path: member.path.clone(),
            size_bytes: member.size_bytes,
            sha256: member.sha256.to_hex(),
            role: member.role.as_str().to_string(),
        });
    }
    if let Some(table) = &manifest.partition_table {
        for entry in &table.entries {
            response.partitions.push(PartitionEntry {
                index: entry.index,
                name: entry.name.clone(),
                offset_sectors: entry.offset_sectors,
                size_sectors: entry.size_sectors,
                attribute: entry
                    .attribute
                    .map(|attribute| attribute.as_str().to_string())
                    .unwrap_or_default(),
                grammar_branch: entry.grammar_branch.as_str().to_string(),
            });
        }
    }
    for (key, value) in &manifest.build_facts {
        response.build_facts.push(KeyValue {
            key: key.to_string(),
            value: value.clone(),
        });
    }
    for unknown in &manifest.execution_relevant_unknowns {
        response.execution_relevant_unknowns.push(KeyValue {
            key: unknown.id.to_string(),
            value: unknown.summary.clone(),
        });
    }
    response
}

pub fn profile_coverage(
    manifest: &ArtifactManifest,
    profile: &DeviceProfile,
) -> Result<ProfileCoverage, String> {
    let mut targets = profile
        .allowed_targets
        .iter()
        .map(|target| {
            let member = target
                .source_member
                .as_deref()
                .and_then(|name| manifest.member(name));
            ProfileTargetCoverage {
                write_order: target.write_order,
                partition: target.partition.to_string(),
                source_member: target.source_member.clone(),
                source_size_bytes: member.map(|member| member.size_bytes),
                present: member.is_some(),
            }
        })
        .collect::<Vec<_>>();
    targets.sort_by_key(|target| target.write_order);
    Ok(ProfileCoverage {
        profile_id: profile.id.to_string(),
        profile_version: profile.version.to_string(),
        profile_sha256: profile
            .digest()
            .map_err(|error| error.to_string())?
            .to_hex(),
        complete: targets.iter().all(|target| target.present),
        targets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use arkforge_artifact::cas::CasQuota;
    use arkforge_artifact::fixture;
    use arkforge_core::profile;

    #[test]
    fn shared_inspection_matches_the_pinned_dayu200_manifest() {
        let manifest = inspect_container(fixture::dayu200_archive().as_slice()).unwrap();
        let response = manifest_response(&manifest);
        assert_eq!(response.format_id, dayu200::FORMAT_ID);
        assert_eq!(response.members.len(), 17);
        assert_eq!(response.partitions.len(), 15);
        assert_eq!(response.content_sha256.len(), 64);
        assert_eq!(response.manifest_sha256.len(), 64);
    }

    #[test]
    fn profile_coverage_is_ordered_and_explicit_about_missing_members() {
        let manifest = inspect_container(fixture::dayu200_archive().as_slice()).unwrap();
        let profile = profile::load(include_str!("../../../profiles/dayu200.yaml")).unwrap();
        let coverage = profile_coverage(&manifest, &profile).unwrap();
        assert!(coverage.complete);
        assert_eq!(coverage.targets.len(), 9);
        assert!(
            coverage
                .targets
                .windows(2)
                .all(|pair| pair[0].write_order < pair[1].write_order)
        );
    }

    #[test]
    fn a_sealed_manifest_cache_survives_restart_and_corruption_falls_back_to_parsing() {
        let root = std::env::temp_dir().join(format!(
            "arkforge-manifest-cache-{}-{}",
            std::process::id(),
            MANIFEST_CACHE_TEMPORARY.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let store = ContentAddressedStore::open(
            &root,
            CasQuota {
                max_total_bytes: 100 * 1024 * 1024,
                min_free_bytes: 0,
                max_artifact_bytes: 100 * 1024 * 1024,
            },
        )
        .unwrap();
        let archive = fixture::dayu200_archive();
        let imported = store
            .import(archive.as_slice(), archive.len() as u64, None)
            .unwrap();

        let first = inspect_stored_container(&store, &imported.digest).unwrap();
        assert!(store.object_is_sealed(&imported.digest).unwrap());
        let cached = load_manifest_cache(&store, &imported.digest)
            .unwrap()
            .expect("published cache");
        assert_eq!(cached, first);

        let cache_path = manifest_cache_path(&root, &imported.digest);
        set_mode(&cache_path, 0o600).unwrap();
        fs::write(&cache_path, b"corrupt cache").unwrap();
        set_mode(&cache_path, 0o400).unwrap();
        let recovered = inspect_stored_container(&store, &imported.digest).unwrap();
        assert_eq!(recovered, first);
        assert_eq!(
            load_manifest_cache(&store, &imported.digest)
                .unwrap()
                .expect("repaired cache"),
            first
        );

        fs::remove_dir_all(&root).unwrap();
    }
}
