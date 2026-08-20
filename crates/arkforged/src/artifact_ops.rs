//! Offline artifact operations shared by the daemon and canonical CLI.

use arkforge_artifact::manifest::ArtifactManifest;
use arkforge_artifact::{dayu200, pac};
use arkforge_core::profile::DeviceProfile;
use arkforge_ipc::messages::{ArchiveMember, InspectArtifactResponse, KeyValue, PartitionEntry};
use std::io::Read;

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
}
