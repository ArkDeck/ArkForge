//! Reusable local bindings, stored owner-only beside the runtime.
//!
//! Everything here is a *binding a caller can reuse*: which HDC executable this
//! host is allowed to drive, which development profiles to load, whether the
//! daemon must satisfy the release signing contract. Nothing here is consent,
//! and nothing here is a hardware campaign — a named acceptance run is given
//! per call or not at all, because a campaign that could be left switched on in
//! a config file would stop meaning "this run was reviewed".
//!
//! Two properties the format buys, both of which a caller depends on:
//!
//! * a path and the digest of the bytes at that path are written in one
//!   transaction, so a config can never name an executable it has not pinned;
//! * a failed write leaves the previous config exactly as it was, because the
//!   new bytes land in a sibling temporary file that is synced before it
//!   replaces the old one.

use crate::StandaloneError;
use arkforge_core::digest::sha256;
use arkforge_ipc::wire;
use arkforge_platform::{protect_path, replace_file, sync_directory};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// The owner-only file this module owns inside a runtime directory.
const CONFIG_FILE: &str = "config";
const TEMP_FILE: &str = "config.next";

/// An executable this host may drive, pinned to the bytes it had when bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedFile {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub hdc: Option<PinnedFile>,
    pub profile_files: Vec<PinnedFile>,
    pub require_release_signing: bool,
}

impl RuntimeConfig {
    /// Reads the stored config, treating an absent file as an empty one.
    ///
    /// Absent and empty are the same answer here on purpose: a host that has
    /// never configured anything and a host that cleared everything are in the
    /// same state, and neither is an error.
    pub fn load(runtime_dir: &Path) -> Result<Self, StandaloneError> {
        let path = runtime_dir.join(CONFIG_FILE);
        let mut bytes = Vec::new();
        match std::fs::File::open(&path) {
            Ok(mut file) => {
                file.read_to_end(&mut bytes)
                    .map_err(|error| io_failure("read the stored configuration", error))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(io_failure("open the stored configuration", error)),
        }
        Self::decode(&bytes)
    }

    /// Commits a new config, or leaves the old one untouched.
    pub fn store(&self, runtime_dir: &Path) -> Result<(), StandaloneError> {
        std::fs::create_dir_all(runtime_dir)
            .map_err(|error| io_failure("create the runtime directory", error))?;
        protect_path(runtime_dir, true)
            .map_err(|error| io_failure("protect the runtime directory", error))?;
        let temporary = runtime_dir.join(TEMP_FILE);
        let target = runtime_dir.join(CONFIG_FILE);
        let encoded = self.encode();
        {
            let mut file = std::fs::File::create(&temporary)
                .map_err(|error| io_failure("create the configuration transaction", error))?;
            protect_path(&temporary, false)
                .map_err(|error| io_failure("protect the configuration transaction", error))?;
            file.write_all(&encoded)
                .and_then(|_| file.sync_all())
                .map_err(|error| io_failure("write the configuration transaction", error))?;
        }
        replace_file(&temporary, &target).map_err(|error| {
            let _ = std::fs::remove_file(&temporary);
            io_failure("commit the configuration", error)
        })?;
        sync_directory(runtime_dir)
            .map_err(|error| io_failure("sync the runtime directory", error))?;
        Ok(())
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(hdc) = &self.hdc {
            wire::write_message(&mut out, 1, &encode_pinned(hdc));
        }
        for profile in &self.profile_files {
            wire::write_message(&mut out, 2, &encode_pinned(profile));
        }
        wire::write_bool(&mut out, 3, self.require_release_signing);
        out
    }

    fn decode(input: &[u8]) -> Result<Self, StandaloneError> {
        let mut config = Self::default();
        let mut reader = wire::Reader::new(input);
        while let Some((field, value)) = reader
            .next_field()
            .map_err(|error| corrupt(format!("field header is invalid: {error}")))?
        {
            match field {
                1 => {
                    config.hdc = Some(decode_pinned(value.as_bytes().map_err(|error| {
                        corrupt(format!("the HDC binding is invalid: {error}"))
                    })?)?)
                }
                2 => config
                    .profile_files
                    .push(decode_pinned(value.as_bytes().map_err(|error| {
                        corrupt(format!("a profile binding is invalid: {error}"))
                    })?)?),
                3 => {
                    config.require_release_signing = value.as_bool().map_err(|error| {
                        corrupt(format!("the signing requirement is invalid: {error}"))
                    })?
                }
                _ => {}
            }
        }
        Ok(config)
    }

    /// Re-hashes every pinned file and refuses on drift.
    ///
    /// Called before the bytes are used, never once at bind time only: a pin
    /// recorded yesterday says nothing about the file on disk today.
    pub fn verify_pins(&self) -> Result<(), StandaloneError> {
        for (kind, pinned) in self
            .hdc
            .iter()
            .map(|hdc| ("hdc", hdc))
            .chain(self.profile_files.iter().map(|file| ("profile-file", file)))
        {
            let observed = digest_of(&pinned.path)?;
            if observed != pinned.sha256 {
                return Err(StandaloneError::new(
                    "CONFIG_PIN_DRIFTED",
                    format!(
                        "The configured {kind} no longer has its pinned bytes; rebind it before use."
                    ),
                    3,
                    false,
                ));
            }
        }
        Ok(())
    }
}

fn encode_pinned(pinned: &PinnedFile) -> Vec<u8> {
    let mut out = Vec::new();
    wire::write_string(&mut out, 1, &pinned.path.to_string_lossy());
    wire::write_string(&mut out, 2, &pinned.sha256);
    out
}

fn decode_pinned(input: &[u8]) -> Result<PinnedFile, StandaloneError> {
    let mut path = String::new();
    let mut sha256 = String::new();
    let mut reader = wire::Reader::new(input);
    while let Some((field, value)) = reader
        .next_field()
        .map_err(|error| corrupt(format!("a binding field is invalid: {error}")))?
    {
        match field {
            1 => {
                path = value
                    .as_str(1)
                    .map_err(|error| corrupt(format!("a binding path is invalid: {error}")))?
                    .to_string()
            }
            2 => {
                sha256 = value
                    .as_str(2)
                    .map_err(|error| corrupt(format!("a binding digest is invalid: {error}")))?
                    .to_string()
            }
            _ => {}
        }
    }
    if path.is_empty() || sha256.len() != 64 {
        return Err(corrupt(
            "a binding is missing its path or its digest".to_string(),
        ));
    }
    Ok(PinnedFile {
        path: PathBuf::from(path),
        sha256,
    })
}

/// The canonical absolute path and current digest of a file being bound.
///
/// A relative path is refused rather than resolved: the directory a command
/// happened to run from is not part of a durable binding.
pub fn pin(path: &Path) -> Result<PinnedFile, StandaloneError> {
    if !path.is_absolute() {
        return Err(StandaloneError::invalid(
            "A configured path must be absolute; the working directory is not part of a binding.",
        ));
    }
    let canonical = std::fs::canonicalize(path).map_err(|error| {
        StandaloneError::new(
            "CONFIG_FILE_NOT_FOUND",
            format!("Cannot resolve the configured path: {error}"),
            5,
            false,
        )
    })?;
    let sha256 = digest_of(&canonical)?;
    Ok(PinnedFile {
        path: canonical,
        sha256,
    })
}

fn digest_of(path: &Path) -> Result<String, StandaloneError> {
    let mut file = std::fs::File::open(path).map_err(|error| {
        StandaloneError::new(
            "CONFIG_FILE_NOT_FOUND",
            format!("Cannot read the configured file: {error}"),
            5,
            false,
        )
    })?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| io_failure("read the configured file", error))?;
    Ok(sha256(&bytes).to_hex())
}

fn io_failure(action: &str, error: std::io::Error) -> StandaloneError {
    StandaloneError::new(
        "CONFIG_IO_FAILED",
        format!("Cannot {action}: {error}"),
        10,
        true,
    )
}

fn corrupt(detail: String) -> StandaloneError {
    StandaloneError::new(
        "CONFIG_REJECTED",
        format!("The stored configuration cannot be read: {detail}"),
        3,
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("arkforge-config-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn an_absent_configuration_reads_as_an_empty_one() {
        let root = temp_dir("absent");
        assert_eq!(
            RuntimeConfig::load(&root).unwrap(),
            RuntimeConfig::default()
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_stored_configuration_round_trips_every_binding() {
        let root = temp_dir("roundtrip");
        let config = RuntimeConfig {
            hdc: Some(PinnedFile {
                path: PathBuf::from("/usr/local/bin/hdc"),
                sha256: "a".repeat(64),
            }),
            profile_files: vec![
                PinnedFile {
                    path: PathBuf::from("/opt/one.yaml"),
                    sha256: "b".repeat(64),
                },
                PinnedFile {
                    path: PathBuf::from("/opt/two.yaml"),
                    sha256: "c".repeat(64),
                },
            ],
            require_release_signing: true,
        };
        config.store(&root).unwrap();
        assert_eq!(RuntimeConfig::load(&root).unwrap(), config);

        // The commit is atomic, so nothing is left behind to be read next time.
        assert!(!root.join(TEMP_FILE).exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_failed_commit_leaves_the_previous_configuration_readable() {
        let root = temp_dir("durable");
        let first = RuntimeConfig {
            require_release_signing: true,
            ..RuntimeConfig::default()
        };
        first.store(&root).unwrap();

        // A directory where the transaction file must go makes the write fail
        // at its durable boundary.
        std::fs::create_dir(root.join(TEMP_FILE)).unwrap();
        let second = RuntimeConfig::default();
        assert!(second.store(&root).is_err());
        assert_eq!(RuntimeConfig::load(&root).unwrap(), first);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_relative_path_is_never_bound() {
        assert!(pin(Path::new("relative/hdc")).is_err());
    }

    #[test]
    fn a_pin_is_verified_against_the_bytes_on_disk_not_the_bytes_when_bound() {
        let root = temp_dir("drift");
        let target = root.join("tool");
        std::fs::write(&target, b"first").unwrap();
        let pinned = pin(&std::fs::canonicalize(&target).unwrap()).unwrap();
        let config = RuntimeConfig {
            hdc: Some(pinned),
            ..RuntimeConfig::default()
        };
        config.verify_pins().unwrap();

        std::fs::write(&target, b"second").unwrap();
        let drift = config.verify_pins().unwrap_err();
        assert_eq!(drift.code, "CONFIG_PIN_DRIFTED");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
