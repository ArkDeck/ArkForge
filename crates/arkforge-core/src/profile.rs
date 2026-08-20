//! DeviceProfile — the device facts a plan is built against.
//!
//! architecture.md 18. The schema is device-neutral; the device is the *data*
//! in `profiles/`. Two facts are first-class here because the DAYU200 campaign
//! proved they cannot live anywhere else:
//!
//! - `read_domain`, because the read and write faces of a loader are not
//!   necessarily the same size (AD-006), and a verification claim that ignores
//!   that is a claim about bytes nobody looked at;
//! - mode aliases and rebind transient tolerance, because a Transport that
//!   improvises them turns a normal re-enumeration into a fatal error
//!   (architecture.md 11.3).

use crate::digest::{CanonicalCbor, CborError, CborValue, Domain, Sha256Digest, digest_canonical};
use crate::effect::{DataImpact, DataImpactState, DeviceMode};
use crate::identity::{DeviceProfileIdentity, Version};
use crate::ids::{OpaqueId, PartitionId};
use crate::verification::{
    ReadDomainDeclaration, TargetVerificationDeclaration, VerificationFallback,
    VerificationStrength,
};
use crate::yaml::{self, YamlValue};
use core::fmt;
use std::collections::BTreeSet;

pub const SCHEMA_VERSION: &str = "arkforge.device-profile/v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocIdentity {
    pub vendor: OpaqueId,
    pub family: OpaqueId,
}

impl CanonicalCbor for SocIdentity {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("vendor", self.vendor.to_cbor()),
            ("family", self.family.to_cbor()),
        ])
    }
}

/// Which hardware revisions the profile covers.
///
/// A production profile may not use a wildcard unless accepted evidence proves
/// revision independence, and that evidence is named in the profile
/// (architecture.md 18.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HardwareRevisionPolicy {
    Allow(Vec<String>),
    AnyRevision { evidence_ref: OpaqueId },
}

impl CanonicalCbor for HardwareRevisionPolicy {
    fn to_cbor(&self) -> CborValue {
        match self {
            HardwareRevisionPolicy::Allow(revisions) => CborValue::map(vec![(
                "allow",
                CborValue::array(
                    revisions
                        .iter()
                        .map(|revision| CborValue::text(revision.clone()))
                        .collect(),
                ),
            )]),
            HardwareRevisionPolicy::AnyRevision { evidence_ref } => {
                CborValue::map(vec![("anyRevisionEvidence", evidence_ref.to_cbor())])
            }
        }
    }
}

/// A provider/backend pairing this profile permits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCombination {
    pub provider_id: OpaqueId,
    pub backend: OpaqueId,
    pub minimum_version: Version,
    pub maximum_version_exclusive: Version,
}

impl ProviderCombination {
    pub fn accepts(&self, provider_id: &OpaqueId, backend: &OpaqueId, version: Version) -> bool {
        self.provider_id == *provider_id
            && self.backend == *backend
            && version >= self.minimum_version
            && version < self.maximum_version_exclusive
    }
}

impl CanonicalCbor for ProviderCombination {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("providerId", self.provider_id.to_cbor()),
            ("backend", self.backend.to_cbor()),
            ("minimumVersion", self.minimum_version.to_cbor()),
            (
                "maximumVersionExclusive",
                self.maximum_version_exclusive.to_cbor(),
            ),
        ])
    }
}

/// A USB identity that indicates a mode.
///
/// architecture.md 11.2 permits VID/PID inside a Profile or Transport but
/// forbids them from forming a stable target on their own — so this maps an
/// identity to a *mode*, never to a device. Which board answered is still
/// decided by serial, topology and protocol identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbModeIdentity {
    pub mode: DeviceMode,
    pub vendor_id: u16,
    pub product_id: u16,
    /// The evidence entry this identity was measured under. A VID/PID pair
    /// nobody measured is a guess, and a guess that names a mode is how a
    /// wrong device gets adopted.
    pub evidence_ref: OpaqueId,
}

impl CanonicalCbor for UsbModeIdentity {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("mode", self.mode.to_cbor()),
            ("vendorId", CborValue::Unsigned(self.vendor_id as u64)),
            ("productId", CborValue::Unsigned(self.product_id as u64)),
            ("evidenceRef", self.evidence_ref.to_cbor()),
        ])
    }
}

/// A mode and the aliases that mean the same mode.
///
/// Aliases are declared, not inferred: on DAYU200 the "normal" mode answers to
/// more than one name during re-enumeration, and a preflight that did not know
/// that rejected a healthy device (#1067).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeDeclaration {
    pub id: DeviceMode,
    pub aliases: Vec<DeviceMode>,
}

impl ModeDeclaration {
    pub fn matches(&self, observed: &DeviceMode) -> bool {
        self.id == *observed || self.aliases.contains(observed)
    }
}

impl CanonicalCbor for ModeDeclaration {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("id", self.id.to_cbor()),
            (
                "aliases",
                CborValue::array(self.aliases.iter().map(|alias| alias.to_cbor()).collect()),
            ),
        ])
    }
}

/// How much transient noise a rebind may show before it is a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RebindTolerance {
    pub require_disconnect: bool,
    /// How long transitional observations are still evidence rather than
    /// failure (architecture.md 11.3).
    pub tolerance_window_ms: u64,
    /// Whether a malformed descriptor mid-re-enumeration is tolerated inside
    /// the window (#1068).
    pub tolerate_transient_malformed: bool,
}

impl CanonicalCbor for RebindTolerance {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            (
                "requireDisconnect",
                CborValue::Bool(self.require_disconnect),
            ),
            (
                "toleranceWindowMs",
                CborValue::Unsigned(self.tolerance_window_ms),
            ),
            (
                "tolerateTransientMalformed",
                CborValue::Bool(self.tolerate_transient_malformed),
            ),
        ])
    }
}

/// Whether an identity field survives a transition.
///
/// Measured, not assumed. On DAYU200 both the USB serial and the port path
/// change between HDC-normal and Loader — the loader personality enumerates
/// behind a different hub — so a rebind that required either to match would
/// reject a healthy board (measured 2026-08-14, AD-009).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityFieldPolicy {
    MustMatch,
    MayChange,
}

impl IdentityFieldPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            IdentityFieldPolicy::MustMatch => "must-match",
            IdentityFieldPolicy::MayChange => "may-change",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "must-match" => Some(IdentityFieldPolicy::MustMatch),
            "may-change" => Some(IdentityFieldPolicy::MayChange),
            _ => None,
        }
    }
}

impl CanonicalCbor for IdentityFieldPolicy {
    fn to_cbor(&self) -> CborValue {
        CborValue::text(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeTransition {
    pub from: DeviceMode,
    pub to: DeviceMode,
    /// The managed control action that effects the transition. ArkForge names
    /// the action; the authority's adapter maps it to a real control port
    /// (architecture.md 9.2).
    pub action: OpaqueId,
    pub rebind: RebindTolerance,
    /// Whether the USB serial survives this transition.
    pub serial_policy: IdentityFieldPolicy,
    /// Whether the port path survives this transition.
    pub topology_policy: IdentityFieldPolicy,
}

impl CanonicalCbor for ModeTransition {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("from", self.from.to_cbor()),
            ("to", self.to.to_cbor()),
            ("action", self.action.to_cbor()),
            ("rebind", self.rebind.to_cbor()),
            ("serialPolicy", self.serial_policy.to_cbor()),
            ("topologyPolicy", self.topology_policy.to_cbor()),
        ])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageDeclaration {
    pub kind: OpaqueId,
    /// `None` when the medium's block size has not been measured. An
    /// executable plan needs it; a research profile may honestly lack it.
    pub logical_block_size: Option<u32>,
}

impl CanonicalCbor for StorageDeclaration {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("kind", self.kind.to_cbor()),
            (
                "logicalBlockSize",
                match self.logical_block_size {
                    Some(size) => CborValue::Unsigned(size as u64),
                    None => CborValue::Null,
                },
            ),
        ])
    }
}

/// How far each face of the medium reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadDomainPolicy {
    /// The write face. `full-disk` on DAYU200: a short read window does not
    /// imply a short write window (AD-006).
    pub write: WriteDomainDeclaration,
    pub read: ReadDomainDeclaration,
    /// The byte an erased or unreachable region reads as. On DAYU200, `0xCC`.
    /// `None` when nobody has measured it, which is different from `Some(0x00)`.
    pub erased_medium_filler: Option<u8>,
}

impl CanonicalCbor for ReadDomainPolicy {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("write", CborValue::text(self.write.as_str())),
            ("read", self.read.to_cbor()),
            (
                "erasedMediumFiller",
                match self.erased_medium_filler {
                    Some(byte) => CborValue::Unsigned(byte as u64),
                    None => CborValue::Null,
                },
            ),
        ])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteDomainDeclaration {
    FullDisk,
    CharacterizeAtRuntime,
}

impl WriteDomainDeclaration {
    pub fn as_str(self) -> &'static str {
        match self {
            WriteDomainDeclaration::FullDisk => "full-disk",
            WriteDomainDeclaration::CharacterizeAtRuntime => "characterize-at-runtime",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "full-disk" => Some(WriteDomainDeclaration::FullDisk),
            "characterize-at-runtime" => Some(WriteDomainDeclaration::CharacterizeAtRuntime),
            _ => None,
        }
    }
}

/// A target the profile permits writing, and how it is verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowedTarget {
    pub partition: PartitionId,
    /// Position in the write order, starting at 1.
    pub write_order: u32,
    /// Sector offset the device's own table must agree with.
    pub offset_sectors: u64,
    /// The artifact member that supplies the content, when the format has one.
    pub source_member: Option<String>,
    pub verification: TargetVerificationDeclaration,
}

impl CanonicalCbor for AllowedTarget {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("partition", self.partition.to_cbor()),
            ("writeOrder", CborValue::Unsigned(self.write_order as u64)),
            ("offsetSectors", CborValue::Unsigned(self.offset_sectors)),
            (
                "sourceMember",
                match &self.source_member {
                    Some(member) => CborValue::text(member.clone()),
                    None => CborValue::Null,
                },
            ),
            ("verification", self.verification.to_cbor()),
        ])
    }
}

/// A published recovery coverage declaration (architecture.md 14.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryDeclaration {
    pub supports_complete_overwrite: bool,
    pub version: Version,
    /// Effects the recovery recipe is proven to cover. A possible effect
    /// outside this set makes recovery ineligible rather than best-effort.
    pub covered_effects: Vec<OpaqueId>,
    pub unsupported_states: Vec<String>,
}

impl RecoveryDeclaration {
    pub fn unsupported() -> Self {
        RecoveryDeclaration {
            supports_complete_overwrite: false,
            version: Version::new(0, 0, 0),
            covered_effects: Vec::new(),
            unsupported_states: vec!["no published recovery coverage".into()],
        }
    }
}

impl CanonicalCbor for RecoveryDeclaration {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            (
                "supportsCompleteOverwrite",
                CborValue::Bool(self.supports_complete_overwrite),
            ),
            ("version", self.version.to_cbor()),
            (
                "coveredEffects",
                CborValue::array(self.covered_effects.iter().map(|e| e.to_cbor()).collect()),
            ),
            (
                "unsupportedStates",
                CborValue::array(
                    self.unsupported_states
                        .iter()
                        .map(|state| CborValue::text(state.clone()))
                        .collect(),
                ),
            ),
        ])
    }
}

/// A complete device profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceProfile {
    pub schema_version: String,
    pub id: OpaqueId,
    pub version: Version,
    pub product_models: Vec<String>,
    pub soc: SocIdentity,
    pub hardware_revisions: HardwareRevisionPolicy,
    pub providers: Vec<ProviderCombination>,
    pub artifact_formats: Vec<OpaqueId>,
    /// Members a compatible artifact carries that the container format cannot
    /// classify and that are execution-irrelevant.
    ///
    /// The format parser reports such a member as unclassified, which is an
    /// execution-relevant unknown — it will not guess. Discharging that unknown
    /// is a device fact, so it lives here, where it is reviewed and enters the
    /// profile digest (architecture.md 10.4).
    pub known_metadata_members: Vec<String>,
    pub modes: Vec<ModeDeclaration>,
    /// Measured USB identities that indicate a mode. Empty means no identity
    /// has been measured, and a USB transport can then recognize nothing.
    pub usb_identities: Vec<UsbModeIdentity>,
    pub mode_transitions: Vec<ModeTransition>,
    pub storage: StorageDeclaration,
    pub read_domain: ReadDomainPolicy,
    pub allowed_targets: Vec<AllowedTarget>,
    pub protected_targets: Vec<PartitionId>,
    pub data_impact: DataImpact,
    pub recovery: RecoveryDeclaration,
    pub evidence_refs: Vec<OpaqueId>,
}

impl CanonicalCbor for DeviceProfile {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            (
                "schemaVersion",
                CborValue::text(self.schema_version.clone()),
            ),
            ("id", self.id.to_cbor()),
            ("version", self.version.to_cbor()),
            (
                "productModels",
                CborValue::array(
                    self.product_models
                        .iter()
                        .map(|model| CborValue::text(model.clone()))
                        .collect(),
                ),
            ),
            ("soc", self.soc.to_cbor()),
            ("hardwareRevisions", self.hardware_revisions.to_cbor()),
            (
                "providers",
                CborValue::array(self.providers.iter().map(|p| p.to_cbor()).collect()),
            ),
            (
                "artifactFormats",
                CborValue::array(self.artifact_formats.iter().map(|f| f.to_cbor()).collect()),
            ),
            (
                "knownMetadataMembers",
                CborValue::array(
                    self.known_metadata_members
                        .iter()
                        .map(|member| CborValue::text(member.clone()))
                        .collect(),
                ),
            ),
            (
                "modes",
                CborValue::array(self.modes.iter().map(|m| m.to_cbor()).collect()),
            ),
            (
                "usbIdentities",
                CborValue::array(self.usb_identities.iter().map(|i| i.to_cbor()).collect()),
            ),
            (
                "modeTransitions",
                CborValue::array(self.mode_transitions.iter().map(|t| t.to_cbor()).collect()),
            ),
            ("storage", self.storage.to_cbor()),
            ("readDomain", self.read_domain.to_cbor()),
            (
                "allowedTargets",
                CborValue::array(self.allowed_targets.iter().map(|t| t.to_cbor()).collect()),
            ),
            (
                "protectedTargets",
                CborValue::array(self.protected_targets.iter().map(|t| t.to_cbor()).collect()),
            ),
            ("dataImpact", self.data_impact.to_cbor()),
            ("recovery", self.recovery.to_cbor()),
            (
                "evidenceRefs",
                CborValue::array(self.evidence_refs.iter().map(|e| e.to_cbor()).collect()),
            ),
        ])
    }
}

impl DeviceProfile {
    pub fn digest(&self) -> Result<Sha256Digest, CborError> {
        digest_canonical(Domain::DeviceProfile, self)
    }

    pub fn identity(&self) -> Result<DeviceProfileIdentity, CborError> {
        Ok(DeviceProfileIdentity {
            id: self.id.clone(),
            version: self.version,
            digest: self.digest()?,
        })
    }

    pub fn allowed_target(&self, partition: &PartitionId) -> Option<&AllowedTarget> {
        self.allowed_targets
            .iter()
            .find(|target| &target.partition == partition)
    }

    pub fn mode(&self, mode: &DeviceMode) -> Option<&ModeDeclaration> {
        self.modes.iter().find(|declared| declared.matches(mode))
    }

    /// The mode a measured USB identity indicates, if the Profile declares one.
    pub fn mode_for_usb_identity(&self, vendor_id: u16, product_id: u16) -> Option<&DeviceMode> {
        self.usb_identities
            .iter()
            .find(|identity| identity.vendor_id == vendor_id && identity.product_id == product_id)
            .map(|identity| &identity.mode)
    }

    pub fn transition(&self, from: &DeviceMode, to: &DeviceMode) -> Option<&ModeTransition> {
        self.mode_transitions
            .iter()
            .find(|transition| &transition.from == from && &transition.to == to)
    }

    /// Why this profile cannot back an executable plan, if it cannot.
    ///
    /// An empty list does not mean a plan *will* be executable — provider
    /// maturity, artifact unknowns and the authority all still have a say. It
    /// means the Profile itself is not the reason.
    pub fn execution_blockers(&self) -> Vec<ProfileExecutionBlocker> {
        let mut blockers = Vec::new();

        let unknown_axes = self.data_impact.unknown_axes();
        if !unknown_axes.is_empty() {
            blockers.push(ProfileExecutionBlocker::UnknownDataImpact(
                unknown_axes.iter().map(|axis| axis.to_string()).collect(),
            ));
        }
        if self.storage.logical_block_size.is_none() {
            blockers.push(ProfileExecutionBlocker::UnknownBlockSize);
        }
        if self.read_domain.erased_medium_filler.is_none() {
            blockers.push(ProfileExecutionBlocker::UnknownErasedMediumFiller);
        }
        if let HardwareRevisionPolicy::Allow(revisions) = &self.hardware_revisions
            && revisions.is_empty()
        {
            blockers.push(ProfileExecutionBlocker::NoHardwareRevisionMeasured);
        }
        if self.allowed_targets.is_empty() {
            blockers.push(ProfileExecutionBlocker::NoWritableTargets);
        }
        if self.mode_transitions.is_empty() {
            blockers.push(ProfileExecutionBlocker::NoModeTransitions);
        }
        blockers
    }

    /// Whether the Profile itself permits an executable plan.
    pub fn permits_executable_plan(&self) -> bool {
        self.execution_blockers().is_empty()
    }

    /// Enforces the invariants of architecture.md 18.3 that are about the
    /// profile being *well formed*.
    ///
    /// Missing device facts are not schema errors. A DAYU600 profile whose data
    /// impact, block size and hardware revisions are all unknown is an accurate
    /// profile of a device nobody has measured, and it must be loadable so it
    /// can be displayed and reasoned about. What such a profile may not do is
    /// back an executable plan — that is [`Self::execution_blockers`], and the
    /// Provider consults it before materializing anything (architecture.md
    /// 5.2, 18.3).
    pub fn validate(&self) -> Result<(), ProfileError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ProfileError::UnknownSchemaVersion(
                self.schema_version.clone(),
            ));
        }
        if self.storage.logical_block_size == Some(0) {
            // Zero is not "unknown"; it is a wrong answer.
            return Err(ProfileError::ZeroBlockSize);
        }

        // A wildcard revision is a *claim* about untested hardware, so it stays
        // a hard error. An empty list claims nothing and is merely unexecutable.
        if let HardwareRevisionPolicy::Allow(revisions) = &self.hardware_revisions
            && revisions.iter().any(|revision| revision == "*")
        {
            return Err(ProfileError::WildcardHardwareRevision);
        }

        // Allowed and protected must not intersect: a protected target that a
        // later edit also lists as allowed would be silently writable.
        let protected: BTreeSet<&str> = self
            .protected_targets
            .iter()
            .map(|target| target.as_str())
            .collect();
        for target in &self.allowed_targets {
            if protected.contains(target.partition.as_str()) {
                return Err(ProfileError::TargetBothAllowedAndProtected(
                    target.partition.to_string(),
                ));
            }
        }

        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for target in &self.allowed_targets {
            if !seen.insert(target.partition.as_str()) {
                return Err(ProfileError::DuplicateAllowedTarget(
                    target.partition.to_string(),
                ));
            }
        }

        // Write order is contiguous from 1 and ascends by offset, matching the
        // ArkDeck profile invariant it replaces.
        let mut ordered: Vec<&AllowedTarget> = self.allowed_targets.iter().collect();
        ordered.sort_by_key(|target| target.write_order);
        for (index, target) in ordered.iter().enumerate() {
            if target.write_order as usize != index + 1 {
                return Err(ProfileError::WriteOrderNotContiguous(
                    target.partition.to_string(),
                ));
            }
            if index > 0 && ordered[index - 1].offset_sectors >= target.offset_sectors {
                return Err(ProfileError::WriteOrderNotAscendingByOffset(
                    target.partition.to_string(),
                ));
            }
        }

        // A verification claim may not exceed what the read face can reach.
        for target in &self.allowed_targets {
            let fallback = target.verification.fallback;
            let has_fallback = fallback.write_completion_semantics || fallback.build_postflight;
            if self.read_domain.read == ReadDomainDeclaration::CharacterizeAtRuntime
                && !has_fallback
            {
                return Err(ProfileError::VerificationWithoutFallback(
                    target.partition.to_string(),
                ));
            }
        }

        // Every declared transition names modes the profile declares.
        for transition in &self.mode_transitions {
            for mode in [&transition.from, &transition.to] {
                if self.mode(mode).is_none() {
                    return Err(ProfileError::UndeclaredMode(mode.to_string()));
                }
            }
            if transition.rebind.tolerance_window_ms == 0 {
                return Err(ProfileError::ZeroRebindTolerance(format!(
                    "{} -> {}",
                    transition.from, transition.to
                )));
            }
        }

        // A USB identity must indicate a mode the profile declares, and no two
        // identities may indicate different modes for the same VID/PID — an
        // ambiguous identity is worse than none.
        for identity in &self.usb_identities {
            if self.mode(&identity.mode).is_none() {
                return Err(ProfileError::UndeclaredMode(identity.mode.to_string()));
            }
        }
        for (index, identity) in self.usb_identities.iter().enumerate() {
            for other in self.usb_identities.iter().skip(index + 1) {
                if identity.vendor_id == other.vendor_id
                    && identity.product_id == other.product_id
                    && identity.mode != other.mode
                {
                    return Err(ProfileError::AmbiguousUsbIdentity {
                        vendor_id: identity.vendor_id,
                        product_id: identity.product_id,
                        first: identity.mode.to_string(),
                        second: other.mode.to_string(),
                    });
                }
            }
        }

        // One alias may not belong to two modes.
        let mut alias_owner: Vec<(&str, &str)> = Vec::new();
        for declaration in &self.modes {
            for name in std::iter::once(&declaration.id).chain(declaration.aliases.iter()) {
                if let Some((_, owner)) = alias_owner
                    .iter()
                    .find(|(existing, _)| *existing == name.as_str())
                {
                    return Err(ProfileError::AmbiguousModeAlias {
                        alias: name.to_string(),
                        first: owner.to_string(),
                        second: declaration.id.to_string(),
                    });
                }
                alias_owner.push((name.as_str(), declaration.id.as_str()));
            }
        }

        if self.recovery.supports_complete_overwrite && self.recovery.covered_effects.is_empty() {
            return Err(ProfileError::RecoveryWithoutCoverage);
        }
        Ok(())
    }
}

/// A device fact the Profile does not carry, which an executable plan needs.
///
/// Each variant is a fact somebody has to *measure*, not a setting somebody has
/// to change — which is why they read as evidence requirements rather than as
/// configuration errors (architecture.md 5.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileExecutionBlocker {
    UnknownDataImpact(Vec<String>),
    UnknownBlockSize,
    UnknownErasedMediumFiller,
    NoHardwareRevisionMeasured,
    NoWritableTargets,
    NoModeTransitions,
}

impl ProfileExecutionBlocker {
    /// A stable identifier, so an assessment can name the blocker the same way
    /// every time and an evidence item can close it by name.
    pub fn id(&self) -> &'static str {
        match self {
            ProfileExecutionBlocker::UnknownDataImpact(_) => "PROF-B01",
            ProfileExecutionBlocker::UnknownBlockSize => "PROF-B02",
            ProfileExecutionBlocker::UnknownErasedMediumFiller => "PROF-B03",
            ProfileExecutionBlocker::NoHardwareRevisionMeasured => "PROF-B04",
            ProfileExecutionBlocker::NoWritableTargets => "PROF-B05",
            ProfileExecutionBlocker::NoModeTransitions => "PROF-B06",
        }
    }
}

impl fmt::Display for ProfileExecutionBlocker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProfileExecutionBlocker::UnknownDataImpact(axes) => write!(
                f,
                "data impact is unknown for {}; no plan may be executed until each axis is measured",
                axes.join(", ")
            ),
            ProfileExecutionBlocker::UnknownBlockSize => {
                f.write_str("the medium's logical block size has not been measured")
            }
            ProfileExecutionBlocker::UnknownErasedMediumFiller => f.write_str(
                "the erased-medium filler byte has not been measured, so a readback cannot be \
                 classified",
            ),
            ProfileExecutionBlocker::NoHardwareRevisionMeasured => f.write_str(
                "no hardware revision has been measured; a production profile names the \
                 revisions it was tested on",
            ),
            ProfileExecutionBlocker::NoWritableTargets => {
                f.write_str("the profile declares no writable target")
            }
            ProfileExecutionBlocker::NoModeTransitions => f.write_str(
                "the profile declares no mode transition, so no plan can reach a writable mode",
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileError {
    UnknownSchemaVersion(String),
    ZeroBlockSize,
    NoHardwareRevisions,
    WildcardHardwareRevision,
    UnknownDataImpact(Vec<String>),
    TargetBothAllowedAndProtected(String),
    DuplicateAllowedTarget(String),
    WriteOrderNotContiguous(String),
    WriteOrderNotAscendingByOffset(String),
    VerificationWithoutFallback(String),
    UndeclaredMode(String),
    ZeroRebindTolerance(String),
    AmbiguousModeAlias {
        alias: String,
        first: String,
        second: String,
    },
    AmbiguousUsbIdentity {
        vendor_id: u16,
        product_id: u16,
        first: String,
        second: String,
    },
    RecoveryWithoutCoverage,
    Yaml(yaml::YamlError),
    MissingField(String),
    BadField {
        field: String,
        detail: String,
    },
    DigestMismatch {
        expected: Sha256Digest,
        computed: Sha256Digest,
    },
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProfileError::UnknownSchemaVersion(version) => write!(
                f,
                "unknown profile schema version {version:?}; this build reads {SCHEMA_VERSION}"
            ),
            ProfileError::ZeroBlockSize => f.write_str("logical block size must be non-zero"),
            ProfileError::NoHardwareRevisions => {
                f.write_str("profile permits no hardware revision at all")
            }
            ProfileError::WildcardHardwareRevision => f.write_str(
                "wildcard hardware revision requires an explicit revision-independence evidence reference",
            ),
            ProfileError::UnknownDataImpact(axes) => write!(
                f,
                "profile leaves data impact unknown for {}",
                axes.join(", ")
            ),
            ProfileError::TargetBothAllowedAndProtected(target) => {
                write!(f, "target {target} is both allowed and protected")
            }
            ProfileError::DuplicateAllowedTarget(target) => {
                write!(f, "target {target} is listed twice")
            }
            ProfileError::WriteOrderNotContiguous(target) => write!(
                f,
                "write order is not contiguous from 1 at target {target}"
            ),
            ProfileError::WriteOrderNotAscendingByOffset(target) => write!(
                f,
                "target {target} writes out of offset order"
            ),
            ProfileError::VerificationWithoutFallback(target) => write!(
                f,
                "target {target} has a runtime-characterized read domain but declares no fallback evidence"
            ),
            ProfileError::UndeclaredMode(mode) => {
                write!(f, "mode transition references undeclared mode {mode}")
            }
            ProfileError::ZeroRebindTolerance(transition) => write!(
                f,
                "transition {transition} declares a zero rebind tolerance window"
            ),
            ProfileError::AmbiguousModeAlias {
                alias,
                first,
                second,
            } => write!(
                f,
                "mode alias {alias} is claimed by both {first} and {second}"
            ),
            ProfileError::AmbiguousUsbIdentity {
                vendor_id,
                product_id,
                first,
                second,
            } => write!(
                f,
                "USB identity {vendor_id:#06x}:{product_id:#06x} is claimed by both {first} and {second}"
            ),
            ProfileError::RecoveryWithoutCoverage => f.write_str(
                "recovery claims complete-overwrite support but declares no covered effects",
            ),
            ProfileError::Yaml(error) => write!(f, "{error}"),
            ProfileError::MissingField(field) => write!(f, "profile is missing {field}"),
            ProfileError::BadField { field, detail } => write!(f, "profile field {field}: {detail}"),
            ProfileError::DigestMismatch { expected, computed } => write!(
                f,
                "profile pins digest {expected} but hashes to {computed}"
            ),
        }
    }
}

impl std::error::Error for ProfileError {}

fn missing(field: &str) -> ProfileError {
    ProfileError::MissingField(field.to_string())
}

fn bad(field: &str, detail: impl Into<String>) -> ProfileError {
    ProfileError::BadField {
        field: field.to_string(),
        detail: detail.into(),
    }
}

fn scalar<'a>(value: &'a YamlValue, path: &str) -> Result<&'a str, ProfileError> {
    value
        .get(path.rsplit('.').next().unwrap_or(path))
        .and_then(YamlValue::as_scalar)
        .ok_or_else(|| missing(path))
}

fn identifier(value: &YamlValue, key: &str, path: &str) -> Result<OpaqueId, ProfileError> {
    let text = value
        .get(key)
        .and_then(YamlValue::as_scalar)
        .ok_or_else(|| missing(path))?;
    OpaqueId::new(text).map_err(|error| bad(path, error.to_string()))
}

fn boolean(value: &YamlValue, key: &str, path: &str) -> Result<bool, ProfileError> {
    match value.get(key).and_then(YamlValue::as_scalar) {
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        Some(other) => Err(bad(
            path,
            format!("expected true or false, found {other:?}"),
        )),
        None => Err(missing(path)),
    }
}

/// Reads a number that may honestly be `unknown`.
///
/// `unknown` is spelled out rather than left absent: an omitted field reads as
/// an oversight, a written `unknown` reads as a measurement nobody has taken.
fn optional_unsigned(
    value: &YamlValue,
    key: &str,
    path: &str,
) -> Result<Option<u64>, ProfileError> {
    let text = value
        .get(key)
        .and_then(YamlValue::as_scalar)
        .ok_or_else(|| missing(path))?;
    if text == "unknown" {
        return Ok(None);
    }
    parse_number(text).map(Some).ok_or_else(|| {
        bad(
            path,
            format!("expected a number or `unknown`, found {text:?}"),
        )
    })
}

fn unsigned(value: &YamlValue, key: &str, path: &str) -> Result<u64, ProfileError> {
    let text = value
        .get(key)
        .and_then(YamlValue::as_scalar)
        .ok_or_else(|| missing(path))?;
    parse_number(text).ok_or_else(|| bad(path, format!("expected a number, found {text:?}")))
}

/// Accepts decimal, `0x` hex and `_` digit separators.
fn parse_number(text: &str) -> Option<u64> {
    let cleaned: String = text.chars().filter(|c| *c != '_').collect();
    if let Some(hex) = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16).ok()
    } else {
        cleaned.parse().ok()
    }
}

fn data_impact_state(text: &str) -> Option<DataImpactState> {
    match text {
        "preserved" => Some(DataImpactState::Preserved),
        "overwritten" => Some(DataImpactState::Overwritten),
        "unknown" => Some(DataImpactState::Unknown),
        _ => None,
    }
}

/// Loads and validates a profile document.
///
/// The document may pin `expectedDigest`; when it does, the computed digest
/// must match, so a profile cannot be edited without its pin being updated in
/// the same change.
pub fn load(source: &str) -> Result<DeviceProfile, ProfileError> {
    let document = yaml::parse(source).map_err(ProfileError::Yaml)?;

    let schema_version = document
        .get("schemaVersion")
        .and_then(YamlValue::as_scalar)
        .ok_or_else(|| missing("schemaVersion"))?
        .to_string();

    let profile_block = document.get("profile").ok_or_else(|| missing("profile"))?;
    let id = identifier(profile_block, "id", "profile.id")?;
    let version = Version::parse(scalar(profile_block, "profile.version")?)
        .ok_or_else(|| bad("profile.version", "expected major.minor.patch"))?;
    let expected_digest = match profile_block
        .get("expectedDigest")
        .and_then(YamlValue::as_scalar)
    {
        Some(text) => Some(
            Sha256Digest::parse_hex(text.strip_prefix("sha256:").unwrap_or(text))
                .map_err(|error| bad("profile.expectedDigest", error.to_string()))?,
        ),
        None => None,
    };

    let identity_block = document
        .get("identity")
        .ok_or_else(|| missing("identity"))?;
    let product_models = identity_block
        .get("productModels")
        .and_then(YamlValue::as_sequence)
        .ok_or_else(|| missing("identity.productModels"))?
        .iter()
        .filter_map(|value| value.as_scalar().map(str::to_string))
        .collect::<Vec<_>>();
    if product_models.is_empty() {
        return Err(bad(
            "identity.productModels",
            "must list at least one model",
        ));
    }
    let soc_block = identity_block
        .get("soc")
        .ok_or_else(|| missing("identity.soc"))?;
    let soc = SocIdentity {
        vendor: identifier(soc_block, "vendor", "identity.soc.vendor")?,
        family: identifier(soc_block, "family", "identity.soc.family")?,
    };

    let revisions_block = identity_block
        .get("hardwareRevisions")
        .ok_or_else(|| missing("identity.hardwareRevisions"))?;
    let hardware_revisions = if let Some(evidence) = revisions_block
        .get("anyRevisionEvidence")
        .and_then(YamlValue::as_scalar)
    {
        HardwareRevisionPolicy::AnyRevision {
            evidence_ref: OpaqueId::new(evidence).map_err(|error| {
                bad(
                    "identity.hardwareRevisions.anyRevisionEvidence",
                    error.to_string(),
                )
            })?,
        }
    } else {
        let allow = revisions_block
            .get("allow")
            .and_then(YamlValue::as_sequence)
            .ok_or_else(|| missing("identity.hardwareRevisions.allow"))?
            .iter()
            .filter_map(|value| value.as_scalar().map(str::to_string))
            .collect();
        HardwareRevisionPolicy::Allow(allow)
    };

    let mut providers = Vec::new();
    for entry in document
        .get("providers")
        .and_then(YamlValue::as_sequence)
        .ok_or_else(|| missing("providers"))?
    {
        let minimum = Version::parse(scalar(entry, "providers[].minimumVersion")?)
            .ok_or_else(|| bad("providers[].minimumVersion", "expected major.minor.patch"))?;
        let maximum = entry
            .get("maximumVersionExclusive")
            .and_then(YamlValue::as_scalar)
            .and_then(Version::parse)
            .ok_or_else(|| missing("providers[].maximumVersionExclusive"))?;
        providers.push(ProviderCombination {
            provider_id: identifier(entry, "id", "providers[].id")?,
            backend: identifier(entry, "backend", "providers[].backend")?,
            minimum_version: minimum,
            maximum_version_exclusive: maximum,
        });
    }

    let artifact_formats = document
        .get("artifactCompatibility")
        .and_then(|block| block.get("formats"))
        .and_then(YamlValue::as_sequence)
        .ok_or_else(|| missing("artifactCompatibility.formats"))?
        .iter()
        .filter_map(|value| value.as_scalar())
        .map(|text| {
            OpaqueId::new(text)
                .map_err(|error| bad("artifactCompatibility.formats", error.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let known_metadata_members = document
        .get("artifactCompatibility")
        .and_then(|block| block.get("knownMetadataMembers"))
        .and_then(YamlValue::as_sequence)
        .unwrap_or(&[])
        .iter()
        .filter_map(|value| value.as_scalar().map(str::to_string))
        .collect::<Vec<_>>();

    let mut modes = Vec::new();
    for entry in document
        .get("modes")
        .and_then(YamlValue::as_sequence)
        .ok_or_else(|| missing("modes"))?
    {
        let id = DeviceMode::new(scalar(entry, "modes[].id")?)
            .map_err(|error| bad("modes[].id", error.to_string()))?;
        let aliases = match entry.get("aliases").and_then(YamlValue::as_sequence) {
            Some(items) => items
                .iter()
                .filter_map(|value| value.as_scalar())
                .map(|text| {
                    DeviceMode::new(text).map_err(|error| bad("modes[].aliases", error.to_string()))
                })
                .collect::<Result<Vec<_>, _>>()?,
            None => Vec::new(),
        };
        modes.push(ModeDeclaration { id, aliases });
    }

    let mut usb_identities = Vec::new();
    for entry in document
        .get("usbIdentities")
        .and_then(YamlValue::as_sequence)
        .unwrap_or(&[])
    {
        let mode = DeviceMode::new(scalar(entry, "usbIdentities[].mode")?)
            .map_err(|error| bad("usbIdentities[].mode", error.to_string()))?;
        usb_identities.push(UsbModeIdentity {
            mode,
            vendor_id: unsigned(entry, "vendorId", "usbIdentities[].vendorId")? as u16,
            product_id: unsigned(entry, "productId", "usbIdentities[].productId")? as u16,
            evidence_ref: identifier(entry, "evidenceRef", "usbIdentities[].evidenceRef")?,
        });
    }

    let mut mode_transitions = Vec::new();
    for entry in document
        .get("modeTransitions")
        .and_then(YamlValue::as_sequence)
        .unwrap_or(&[])
    {
        let rebind_block = entry
            .get("rebind")
            .ok_or_else(|| missing("modeTransitions[].rebind"))?;
        mode_transitions.push(ModeTransition {
            from: DeviceMode::new(scalar(entry, "modeTransitions[].from")?)
                .map_err(|error| bad("modeTransitions[].from", error.to_string()))?,
            to: DeviceMode::new(scalar(entry, "modeTransitions[].to")?)
                .map_err(|error| bad("modeTransitions[].to", error.to_string()))?,
            action: identifier(entry, "action", "modeTransitions[].action")?,
            serial_policy: IdentityFieldPolicy::parse(scalar(
                entry,
                "modeTransitions[].serialPolicy",
            )?)
            .ok_or_else(|| {
                bad(
                    "modeTransitions[].serialPolicy",
                    "expected must-match or may-change",
                )
            })?,
            topology_policy: IdentityFieldPolicy::parse(scalar(
                entry,
                "modeTransitions[].topologyPolicy",
            )?)
            .ok_or_else(|| {
                bad(
                    "modeTransitions[].topologyPolicy",
                    "expected must-match or may-change",
                )
            })?,
            rebind: RebindTolerance {
                require_disconnect: boolean(
                    rebind_block,
                    "requireDisconnect",
                    "modeTransitions[].rebind.requireDisconnect",
                )?,
                tolerance_window_ms: unsigned(
                    rebind_block,
                    "toleranceWindowMs",
                    "modeTransitions[].rebind.toleranceWindowMs",
                )?,
                tolerate_transient_malformed: boolean(
                    rebind_block,
                    "tolerateTransientMalformed",
                    "modeTransitions[].rebind.tolerateTransientMalformed",
                )?,
            },
        });
    }

    let storage_block = document.get("storage").ok_or_else(|| missing("storage"))?;
    let storage = StorageDeclaration {
        kind: identifier(storage_block, "kind", "storage.kind")?,
        logical_block_size: optional_unsigned(
            storage_block,
            "logicalBlockSize",
            "storage.logicalBlockSize",
        )?
        .map(|size| size as u32),
    };

    let read_block = document
        .get("readDomain")
        .ok_or_else(|| missing("readDomain"))?;
    let read_domain = ReadDomainPolicy {
        write: WriteDomainDeclaration::parse(scalar(read_block, "readDomain.write")?).ok_or_else(
            || {
                bad(
                    "readDomain.write",
                    "expected full-disk or characterize-at-runtime",
                )
            },
        )?,
        read: ReadDomainDeclaration::parse(scalar(read_block, "readDomain.read")?).ok_or_else(
            || {
                bad(
                    "readDomain.read",
                    "expected full or characterize-at-runtime",
                )
            },
        )?,
        erased_medium_filler: optional_unsigned(
            read_block,
            "erasedMediumFiller",
            "readDomain.erasedMediumFiller",
        )?
        .map(|byte| byte as u8),
    };

    let mut allowed_targets = Vec::new();
    for entry in document
        .get("allowedTargets")
        .and_then(YamlValue::as_sequence)
        .unwrap_or(&[])
    {
        let verification_block = entry
            .get("verification")
            .ok_or_else(|| missing("allowedTargets[].verification"))?;
        let fallback_block = verification_block
            .get("fallback")
            .ok_or_else(|| missing("allowedTargets[].verification.fallback"))?;
        allowed_targets.push(AllowedTarget {
            partition: PartitionId::new(scalar(entry, "allowedTargets[].partition")?)
                .map_err(|error| bad("allowedTargets[].partition", error.to_string()))?,
            write_order: unsigned(entry, "writeOrder", "allowedTargets[].writeOrder")? as u32,
            offset_sectors: unsigned(entry, "offsetSectors", "allowedTargets[].offsetSectors")?,
            source_member: entry
                .get("sourceMember")
                .and_then(YamlValue::as_scalar)
                .map(str::to_string),
            verification: TargetVerificationDeclaration {
                max_strength_when_readable: VerificationStrength::parse(scalar(
                    verification_block,
                    "allowedTargets[].verification.maxStrengthWhenReadable",
                )?)
                .ok_or_else(|| {
                    bad(
                        "allowedTargets[].verification.maxStrengthWhenReadable",
                        "unknown verification strength",
                    )
                })?,
                fallback: VerificationFallback {
                    write_completion_semantics: boolean(
                        fallback_block,
                        "writeCompletionSemantics",
                        "allowedTargets[].verification.fallback.writeCompletionSemantics",
                    )?,
                    build_postflight: boolean(
                        fallback_block,
                        "buildPostflight",
                        "allowedTargets[].verification.fallback.buildPostflight",
                    )?,
                },
            },
        });
    }

    let protected_targets = document
        .get("protectedTargets")
        .and_then(YamlValue::as_sequence)
        .unwrap_or(&[])
        .iter()
        .filter_map(|value| value.as_scalar())
        .map(|text| {
            PartitionId::new(text).map_err(|error| bad("protectedTargets", error.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let impact_block = document
        .get("dataImpact")
        .ok_or_else(|| missing("dataImpact"))?;
    let axis = |key: &str| -> Result<DataImpactState, ProfileError> {
        let text = impact_block
            .get(key)
            .and_then(YamlValue::as_scalar)
            .ok_or_else(|| missing(&format!("dataImpact.{key}")))?;
        data_impact_state(text).ok_or_else(|| {
            bad(
                &format!("dataImpact.{key}"),
                format!("unknown state {text:?}"),
            )
        })
    };
    let data_impact = DataImpact {
        userdata: axis("userdata")?,
        calibration: axis("calibration")?,
        non_volatile_config: axis("nonVolatileConfig")?,
        secure_storage: axis("secureStorage")?,
    };

    let recovery = match document.get("recovery") {
        None => RecoveryDeclaration::unsupported(),
        Some(block) if block.is_null() => RecoveryDeclaration::unsupported(),
        Some(block) => RecoveryDeclaration {
            supports_complete_overwrite: boolean(
                block,
                "supportsCompleteOverwrite",
                "recovery.supportsCompleteOverwrite",
            )?,
            version: block
                .get("version")
                .and_then(YamlValue::as_scalar)
                .and_then(Version::parse)
                .ok_or_else(|| missing("recovery.version"))?,
            covered_effects: block
                .get("coveredEffects")
                .and_then(YamlValue::as_sequence)
                .unwrap_or(&[])
                .iter()
                .filter_map(|value| value.as_scalar())
                .map(|text| {
                    OpaqueId::new(text)
                        .map_err(|error| bad("recovery.coveredEffects", error.to_string()))
                })
                .collect::<Result<Vec<_>, _>>()?,
            unsupported_states: block
                .get("unsupportedStates")
                .and_then(YamlValue::as_sequence)
                .unwrap_or(&[])
                .iter()
                .filter_map(|value| value.as_scalar().map(str::to_string))
                .collect(),
        },
    };

    let evidence_refs = document
        .get("evidenceRefs")
        .and_then(YamlValue::as_sequence)
        .unwrap_or(&[])
        .iter()
        .filter_map(|value| value.as_scalar())
        .map(|text| OpaqueId::new(text).map_err(|error| bad("evidenceRefs", error.to_string())))
        .collect::<Result<Vec<_>, _>>()?;

    let profile = DeviceProfile {
        schema_version,
        id,
        version,
        product_models,
        soc,
        hardware_revisions,
        providers,
        artifact_formats,
        known_metadata_members,
        modes,
        usb_identities,
        mode_transitions,
        storage,
        read_domain,
        allowed_targets,
        protected_targets,
        data_impact,
        recovery,
        evidence_refs,
    };
    profile.validate()?;

    if let Some(expected) = expected_digest {
        let computed = profile
            .digest()
            .map_err(|error| bad("digest", error.to_string()))?;
        if computed != expected {
            return Err(ProfileError::DigestMismatch { expected, computed });
        }
    }
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
schemaVersion: arkforge.device-profile/v1
profile:
  id: test.profile
  version: 1.0.0
identity:
  productModels: [TESTBOARD]
  soc:
    vendor: testvendor
    family: testfamily
  hardwareRevisions:
    allow: [rev-a]
providers:
  - id: arkforge.test
    backend: test-fixed
    minimumVersion: 1.0.0
    maximumVersionExclusive: 2.0.0
artifactCompatibility:
  formats: [test-format]
modes:
  - id: normal
    aliases: [normal-alias]
  - id: loader
modeTransitions:
  - from: normal
    to: loader
    action: enter-updater
    serialPolicy: may-change
    topologyPolicy: must-match
    rebind:
      requireDisconnect: true
      toleranceWindowMs: 20000
      tolerateTransientMalformed: true
storage:
  kind: emmc
  logicalBlockSize: 512
readDomain:
  write: full-disk
  read: characterize-at-runtime
  erasedMediumFiller: 0xCC
allowedTargets:
  - partition: uboot
    writeOrder: 1
    offsetSectors: 8192
    sourceMember: uboot.img
    verification:
      maxStrengthWhenReadable: fullHash
      fallback:
        writeCompletionSemantics: true
        buildPostflight: true
protectedTargets: [misc]
dataImpact:
  userdata: overwritten
  calibration: preserved
  nonVolatileConfig: preserved
  secureStorage: preserved
"#;

    #[test]
    fn loads_and_validates_a_minimal_profile() {
        let profile = load(MINIMAL).unwrap();
        assert_eq!(profile.id.as_str(), "test.profile");
        assert_eq!(profile.version, Version::new(1, 0, 0));
        assert_eq!(profile.read_domain.erased_medium_filler, Some(0xCC));
        assert_eq!(profile.read_domain.write, WriteDomainDeclaration::FullDisk);
        assert_eq!(
            profile.read_domain.read,
            ReadDomainDeclaration::CharacterizeAtRuntime
        );
        assert_eq!(profile.allowed_targets.len(), 1);
        assert_eq!(profile.protected_targets.len(), 1);
    }

    #[test]
    fn a_mode_alias_resolves_to_its_mode() {
        let profile = load(MINIMAL).unwrap();
        let alias = DeviceMode::new("normal-alias").unwrap();
        assert_eq!(
            profile.mode(&alias).map(|mode| mode.id.as_str()),
            Some("normal")
        );
    }

    #[test]
    fn the_profile_digest_is_stable_and_content_sensitive() {
        let profile = load(MINIMAL).unwrap();
        let baseline = profile.digest().unwrap();
        assert_eq!(baseline, load(MINIMAL).unwrap().digest().unwrap());

        let edited = MINIMAL.replace("offsetSectors: 8192", "offsetSectors: 8193");
        assert_ne!(baseline, load(&edited).unwrap().digest().unwrap());
    }

    #[test]
    fn a_pinned_digest_that_no_longer_matches_is_refused() {
        let profile = load(MINIMAL).unwrap();
        let pinned = MINIMAL.replace(
            "  version: 1.0.0",
            &format!(
                "  version: 1.0.0\n  expectedDigest: sha256:{}",
                profile.digest().unwrap()
            ),
        );
        load(&pinned).expect("a correct pin loads");

        let tampered = pinned.replace("offsetSectors: 8192", "offsetSectors: 8193");
        assert!(matches!(
            load(&tampered),
            Err(ProfileError::DigestMismatch { .. })
        ));
    }

    #[test]
    fn a_target_cannot_be_both_allowed_and_protected() {
        let document = MINIMAL.replace("protectedTargets: [misc]", "protectedTargets: [uboot]");
        assert!(matches!(
            load(&document),
            Err(ProfileError::TargetBothAllowedAndProtected(_))
        ));
    }

    #[test]
    fn unknown_userdata_impact_loads_but_blocks_execution() {
        // A profile of a device nobody has measured is an accurate profile, and
        // it has to be loadable so it can be shown. What it may not do is back
        // an executable plan.
        let document = MINIMAL.replace("userdata: overwritten", "userdata: unknown");
        let profile = load(&document).expect("an honest profile still loads");
        assert!(!profile.permits_executable_plan());
        assert!(profile
            .execution_blockers()
            .iter()
            .any(|blocker| matches!(
                blocker,
                ProfileExecutionBlocker::UnknownDataImpact(axes) if axes.contains(&"userdata".to_string())
            )));
    }

    #[test]
    fn a_fully_measured_profile_has_no_execution_blockers() {
        let profile = load(MINIMAL).unwrap();
        assert!(
            profile.permits_executable_plan(),
            "{:?}",
            profile.execution_blockers()
        );
    }

    #[test]
    fn unknown_block_size_and_filler_load_and_block() {
        let document = MINIMAL
            .replace("logicalBlockSize: 512", "logicalBlockSize: unknown")
            .replace("erasedMediumFiller: 0xCC", "erasedMediumFiller: unknown");
        let profile = load(&document).unwrap();
        assert_eq!(profile.storage.logical_block_size, None);
        assert_eq!(profile.read_domain.erased_medium_filler, None);
        let ids: Vec<&str> = profile
            .execution_blockers()
            .iter()
            .map(|blocker| blocker.id())
            .collect();
        assert!(ids.contains(&"PROF-B02"), "{ids:?}");
        assert!(ids.contains(&"PROF-B03"), "{ids:?}");
    }

    #[test]
    fn a_zero_block_size_is_still_a_hard_error_because_it_is_a_wrong_answer() {
        let document = MINIMAL.replace("logicalBlockSize: 512", "logicalBlockSize: 0");
        assert!(matches!(load(&document), Err(ProfileError::ZeroBlockSize)));
    }

    #[test]
    fn an_empty_revision_list_loads_but_blocks_while_a_wildcard_never_loads() {
        let empty = MINIMAL.replace("allow: [rev-a]", "allow: []");
        let profile = load(&empty).expect("claiming nothing is honest");
        assert!(
            profile
                .execution_blockers()
                .iter()
                .any(|blocker| blocker.id() == "PROF-B04")
        );

        // A wildcard is a claim about hardware nobody tested, so it stays a
        // hard error.
        let wildcard = MINIMAL.replace("allow: [rev-a]", "allow: [\"*\"]");
        assert!(matches!(
            load(&wildcard),
            Err(ProfileError::WildcardHardwareRevision)
        ));
    }

    #[test]
    fn a_wildcard_hardware_revision_is_refused() {
        let document = MINIMAL.replace("allow: [rev-a]", "allow: [\"*\"]");
        assert!(matches!(
            load(&document),
            Err(ProfileError::WildcardHardwareRevision)
        ));
    }

    #[test]
    fn a_runtime_read_domain_requires_fallback_evidence() {
        let document = MINIMAL
            .replace(
                "        writeCompletionSemantics: true",
                "        writeCompletionSemantics: false",
            )
            .replace(
                "        buildPostflight: true",
                "        buildPostflight: false",
            );
        assert!(matches!(
            load(&document),
            Err(ProfileError::VerificationWithoutFallback(_))
        ));
    }

    #[test]
    fn a_transition_to_an_undeclared_mode_is_refused() {
        let document = MINIMAL.replace("    to: loader", "    to: maskrom");
        assert!(matches!(
            load(&document),
            Err(ProfileError::UndeclaredMode(_))
        ));
    }

    #[test]
    fn an_alias_claimed_by_two_modes_is_refused() {
        let document = MINIMAL.replace(
            "  - id: loader",
            "  - id: loader\n    aliases: [normal-alias]",
        );
        assert!(matches!(
            load(&document),
            Err(ProfileError::AmbiguousModeAlias { .. })
        ));
    }

    #[test]
    fn an_unknown_schema_version_fails_closed() {
        let document = MINIMAL.replace("arkforge.device-profile/v1", "arkforge.device-profile/v2");
        assert!(matches!(
            load(&document),
            Err(ProfileError::UnknownSchemaVersion(_))
        ));
    }

    #[test]
    fn a_zero_rebind_tolerance_window_is_refused() {
        let document = MINIMAL.replace("toleranceWindowMs: 20000", "toleranceWindowMs: 0");
        assert!(matches!(
            load(&document),
            Err(ProfileError::ZeroRebindTolerance(_))
        ));
    }
}
