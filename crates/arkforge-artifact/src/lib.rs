//! # arkforge-artifact
//!
//! Artifact import, content-addressed storage and firmware-container parsing.
//!
//! The boundary architecture.md 10.3 draws: a parser has no USB, no network,
//! no process execution, decides no authority, and emits no vendor options. It
//! emits facts, unknowns and a confidence level. The store above it never lets
//! a plan name a host path — only an artifact id and a digest it hashed itself.

#![forbid(unsafe_code)]

pub mod cas;
pub mod dayu200;
pub mod fixture;
pub mod inflate;
pub mod manifest;
pub mod tar;

pub use cas::{
    CasError, CasQuota, ContentAddressedStore, GcReport, ImportedObject, PreflightReport,
    SystemVolumeSpaceProbe, VolumeSpaceProbe,
};
pub use manifest::{
    ArchiveMemberFact, ArtifactManifest, GrammarBranch, ManifestError, MemberRole,
    ParserConfidence, PartitionAttribute, PartitionEntryFact, PartitionTableFact,
};
pub use tar::{ArchiveError, TarMemberHeader, TarMemberObservation, TarReader};
