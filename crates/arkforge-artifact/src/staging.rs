//! Extracting the members a plan will write, out of a hashed archive.
//!
//! The native dispatcher consumes verified staged files, and the archive is one
//! compressed stream, so the images have to land on disk before a write can
//! happen. This is the only place in ArkForge that turns archive content into a
//! file.
//!
//! Three rules the shape enforces:
//!
//! 1. One pass. Nine members are extracted from one traversal of the stream,
//!    not nine traversals — on the real archive that is the difference between
//!    seconds and a minute of decompression.
//! 2. Hashed on the way out, against the manifest the parser produced from the
//!    same bytes. A member whose extracted digest disagrees is a staging
//!    failure, not a file to be written to a device.
//! 3. Names come from the manifest, never from the tar header alone. The tar
//!    reader already rejects absolute paths, `..`, links and device nodes; this
//!    adds that a member nobody asked for is never written at all.

use crate::inflate::GzipReader;
use crate::manifest::ArtifactManifest;
use crate::tar::{ArchiveError, TarReader};
use arkforge_core::digest::{Sha256, Sha256Digest};
use core::fmt;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// One member, extracted and verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedMember {
    pub member: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub sha256: Sha256Digest,
}

/// What a staging run produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagingReport {
    pub members: BTreeMap<String, StagedMember>,
    pub bytes_written: u64,
    pub directory: PathBuf,
}

/// Extracts `wanted` from the archive `source` into `directory`.
///
/// `directory` must already exist and should be job-owned: this writes into it
/// and the caller removes it. Every extracted member is checked against
/// `manifest`, which the parser derived from the same archive.
pub fn stage_members<R: Read>(
    source: R,
    manifest: &ArtifactManifest,
    wanted: &BTreeSet<String>,
    directory: &Path,
) -> Result<StagingReport, StagingError> {
    for name in wanted {
        if manifest.member(name).is_none() {
            return Err(StagingError::NotInManifest(name.clone()));
        }
    }

    let gzip = GzipReader::new(source)
        .map_err(|error| StagingError::Archive(ArchiveError::ArchiveInvalid(error.to_string())))?;
    let mut reader = TarReader::new(gzip);
    let mut staged: BTreeMap<String, StagedMember> = BTreeMap::new();
    let mut bytes_written = 0u64;

    while let Some(header) = reader.next_member().map_err(StagingError::Archive)? {
        if !wanted.contains(&header.path) {
            // Still read the body: the stream is sequential, and skipping means
            // consuming, not seeking.
            reader
                .read_member_body(&header, |_| {})
                .map_err(StagingError::Archive)?;
            continue;
        }

        let path = directory.join(&header.path);
        // The tar reader has already refused absolute paths, `..`, links and
        // device nodes, and the name came from the manifest. This is the last
        // check before a create: the join must not have escaped.
        if path.parent() != Some(directory) {
            return Err(StagingError::NameEscapesDirectory(header.path.clone()));
        }

        let mut file = std::fs::File::create(&path)
            .map_err(|error| StagingError::Io(format!("{}: {error}", path.display())))?;
        let mut hasher = Sha256::new();
        let mut written = 0u64;
        let mut write_error: Option<String> = None;

        let observation = reader
            .read_member_body(&header, |chunk| {
                if write_error.is_some() {
                    return;
                }
                if let Err(error) = file.write_all(chunk) {
                    write_error = Some(format!("{}: {error}", path.display()));
                    return;
                }
                hasher.update(chunk);
                written += chunk.len() as u64;
            })
            .map_err(StagingError::Archive)?;

        if let Some(message) = write_error {
            return Err(StagingError::Io(message));
        }
        file.sync_all()
            .map_err(|error| StagingError::Io(format!("{}: {error}", path.display())))?;
        drop(file);

        let digest = hasher.finalize();
        let expected = manifest
            .member(&header.path)
            .expect("membership checked above");
        if digest != expected.sha256 || written != expected.size_bytes {
            return Err(StagingError::MemberDisagrees {
                member: header.path.clone(),
                expected_sha256: expected.sha256,
                observed_sha256: digest,
                expected_bytes: expected.size_bytes,
                observed_bytes: written,
            });
        }
        // The tar reader's own observation is a third witness to the same bytes.
        debug_assert_eq!(observation.sha256, digest);

        bytes_written += written;
        staged.insert(
            header.path.clone(),
            StagedMember {
                member: header.path.clone(),
                path,
                size_bytes: written,
                sha256: digest,
            },
        );
    }

    let missing: Vec<String> = wanted
        .iter()
        .filter(|name| !staged.contains_key(*name))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(StagingError::MembersMissing(missing));
    }

    Ok(StagingReport {
        members: staged,
        bytes_written,
        directory: directory.to_path_buf(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StagingError {
    NotInManifest(String),
    MembersMissing(Vec<String>),
    NameEscapesDirectory(String),
    MemberDisagrees {
        member: String,
        expected_sha256: Sha256Digest,
        observed_sha256: Sha256Digest,
        expected_bytes: u64,
        observed_bytes: u64,
    },
    Archive(ArchiveError),
    Io(String),
}

impl fmt::Display for StagingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StagingError::NotInManifest(member) => {
                write!(f, "{member} is not a member of this archive's manifest")
            }
            StagingError::MembersMissing(members) => write!(
                f,
                "the archive ended without these members: {}",
                members.join(", ")
            ),
            StagingError::NameEscapesDirectory(member) => {
                write!(f, "member name {member:?} does not stay inside the staging directory")
            }
            StagingError::MemberDisagrees {
                member,
                expected_sha256,
                observed_sha256,
                expected_bytes,
                observed_bytes,
            } => write!(
                f,
                "{member} extracted as {observed_bytes} bytes hashing to {observed_sha256}; the \
                 manifest says {expected_bytes} bytes hashing to {expected_sha256}"
            ),
            StagingError::Archive(error) => write!(f, "{error}"),
            StagingError::Io(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for StagingError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dayu200;
    use crate::fixture;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "arkforge-staging-{label}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn wanted(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn staging_extracts_only_what_was_asked_for_and_verifies_it() {
        let dir = TempDir::new("subset");
        let archive = fixture::dayu200_archive();
        let manifest = dayu200::inspect(archive.as_slice()).unwrap();

        let report = stage_members(
            archive.as_slice(),
            &manifest,
            &wanted(&["uboot.img", "system.img"]),
            &dir.0,
        )
        .unwrap();

        assert_eq!(report.members.len(), 2);
        for (name, staged) in &report.members {
            let bytes = std::fs::read(&staged.path).unwrap();
            let expected = manifest.member(name).unwrap();
            assert_eq!(bytes.len() as u64, expected.size_bytes);
            assert_eq!(arkforge_core::digest::sha256(&bytes), expected.sha256);
        }
        // Nothing else landed in the directory.
        let entries: BTreeSet<String> = std::fs::read_dir(&dir.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(entries, wanted(&["uboot.img", "system.img"]));
    }

    #[test]
    fn asking_for_a_member_the_manifest_does_not_have_fails_before_any_write() {
        let dir = TempDir::new("absent");
        let archive = fixture::dayu200_archive();
        let manifest = dayu200::inspect(archive.as_slice()).unwrap();

        let error = stage_members(
            archive.as_slice(),
            &manifest,
            &wanted(&["not-in-the-archive.img"]),
            &dir.0,
        )
        .unwrap_err();
        assert_eq!(
            error,
            StagingError::NotInManifest("not-in-the-archive.img".into())
        );
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 0);
    }

    /// The manifest is the authority on what a member should be. If the bytes
    /// coming out of the stream disagree with it, the file that was just
    /// written is not a file to hand to a device.
    #[test]
    fn a_member_whose_extracted_bytes_disagree_with_the_manifest_is_refused() {
        let dir = TempDir::new("tampered");
        let archive = fixture::dayu200_archive();
        let mut manifest = dayu200::inspect(archive.as_slice()).unwrap();
        let member = manifest
            .members
            .iter_mut()
            .find(|member| member.path == "uboot.img")
            .unwrap();
        member.sha256 = arkforge_core::digest::sha256(b"a digest from somewhere else");

        let error = stage_members(
            archive.as_slice(),
            &manifest,
            &wanted(&["uboot.img"]),
            &dir.0,
        )
        .unwrap_err();
        assert!(matches!(error, StagingError::MemberDisagrees { .. }));
    }
}
