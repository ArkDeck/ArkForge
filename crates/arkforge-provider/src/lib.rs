//! # arkforge-provider
//!
//! The Provider SPI and the DAYU200 vertical's read-only provider.
//!
//! architecture.md 12. A Provider owns lowering: addresses, tool actions,
//! packets, FDL parameters. It does not own authority — it cannot call the
//! ExecutionAuthority, cannot interpret a permit, and cannot decide that a
//! target is writable. Those answers come from the DeviceProfile and the
//! authority, and the Provider's job is to produce a plan whose every private
//! action is covered by a digest that crosses the authority boundary.

#![forbid(unsafe_code)]

pub mod rockchip;
pub mod unisoc;

use arkforge_artifact::manifest::ArtifactManifest;
use arkforge_core::identity::{
    ArtifactFormat, DeviceProfileIdentity, HostPlatform, MaturityKey, MaturityState,
    ProviderIdentity, ToolchainIdentity,
};
use arkforge_core::ids::OpaqueId;
use arkforge_core::plan::PlanMaterialization;
use arkforge_core::profile::DeviceProfile;
use arkforge_core::{AuthorityBindingRef, PlanId, Sha256Digest};
use arkforge_transport::{DeviceObservation, DeviceTransport};
use core::fmt;

/// Static description of a provider implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderDescriptor {
    pub identity: ProviderIdentity,
    /// Artifact formats this provider can lower.
    pub artifact_formats: Vec<ArtifactFormat>,
    /// Backends this provider can dispatch through.
    pub backends: Vec<OpaqueId>,
}

/// What a probe learned about the attached device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderProbe {
    pub observation: DeviceObservation,
    /// Facts the provider read that are specific to its protocol.
    pub protocol_facts: Vec<(OpaqueId, String)>,
    /// The profile the provider believes applies, if it can tell.
    pub profile_candidate: Option<DeviceProfileIdentity>,
    /// Digest over everything the probe observed, for the admission snapshot.
    pub facts_digest: Sha256Digest,
}

/// Context for a probe. Read-only by construction: the provider gets a
/// transport and an observation, and nothing that could mutate.
#[derive(Debug)]
pub struct ProbeContext<'a> {
    pub transport: &'a dyn DeviceTransport,
    pub observation: &'a DeviceObservation,
    pub profile: &'a DeviceProfile,
}

/// One reason a combination is not ready to execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationViolation {
    pub id: OpaqueId,
    pub detail: String,
}

/// The result of checking artifact, profile and device against each other.
///
/// architecture.md 16.3: the Profile allowlist, the observed partition table
/// and the artifact manifest must agree. Two out of three is a rejection, not a
/// majority vote.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ValidationReport {
    pub violations: Vec<ValidationViolation>,
}

impl ValidationReport {
    pub fn is_clean(&self) -> bool {
        self.violations.is_empty()
    }

    pub fn violation(&mut self, id: &str, detail: impl Into<String>) {
        self.violations.push(ValidationViolation {
            id: OpaqueId::new(id).expect("violation ids are literals"),
            detail: detail.into(),
        });
    }
}

/// What the caller wants done, in semantic terms only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashIntent {
    /// Write every target the profile allows, from the leased artifact.
    FullRestore,
}

impl FlashIntent {
    pub fn as_str(self) -> &'static str {
        match self {
            FlashIntent::FullRestore => "fullRestore",
        }
    }
}

/// Everything materialization needs.
#[derive(Debug)]
pub struct MaterializeRequest<'a> {
    pub plan_id: PlanId,
    pub intent: FlashIntent,
    pub artifact: &'a ArtifactManifest,
    pub artifact_id: OpaqueId,
    pub profile: &'a DeviceProfile,
    pub probe: &'a ProviderProbe,
    pub authority_binding: AuthorityBindingRef,
    pub toolchain: ToolchainIdentity,
    pub host_platform: HostPlatform,
    pub driver_facts_digest: Sha256Digest,
    pub evidence_set_digest: Sha256Digest,
    pub created_at_epoch_ms: u64,
    pub plan_lifetime_ms: u64,
}

/// Published maturity for exact combinations (architecture.md 12.3).
///
/// A lookup miss is `Unavailable`, not a default: an unpublished combination is
/// one nobody reviewed.
#[derive(Debug, Clone, Default)]
pub struct MaturityRegistry {
    entries: Vec<(Sha256Digest, MaturityState)>,
}

impl MaturityRegistry {
    pub fn new() -> Self {
        MaturityRegistry {
            entries: Vec::new(),
        }
    }

    pub fn publish(&mut self, key: &MaturityKey, state: MaturityState) {
        let digest = key.digest().expect("maturity keys are canonical");
        self.entries.retain(|(existing, _)| *existing != digest);
        self.entries.push((digest, state));
    }

    pub fn lookup(&self, key: &MaturityKey) -> MaturityState {
        let Ok(digest) = key.digest() else {
            return MaturityState::Unavailable {
                reason: "maturity key is not canonicalizable".into(),
            };
        };
        self.entries
            .iter()
            .find(|(existing, _)| *existing == digest)
            .map(|(_, state)| state.clone())
            .unwrap_or(MaturityState::Unavailable {
                reason: "this provider/profile/artifact/toolchain/platform combination is not published"
                    .into(),
            })
    }
}

/// The Provider SPI.
///
/// The execute-side methods are declared so the shape is the one AF-V2 will
/// fill in, and they default to refusing. A read-only vertical implements the
/// read-only half and inherits a refusal for the rest, rather than inheriting a
/// stub that looks like it works.
pub trait FlashProvider: fmt::Debug + Send + Sync {
    fn descriptor(&self) -> ProviderDescriptor;

    fn probe(&self, ctx: &ProbeContext<'_>) -> Result<ProviderProbe, ProviderError>;

    fn validate(
        &self,
        artifact: &ArtifactManifest,
        profile: &DeviceProfile,
        probe: &ProviderProbe,
    ) -> Result<ValidationReport, ProviderError>;

    fn materialize(
        &self,
        request: &MaterializeRequest<'_>,
        maturity: &MaturityRegistry,
    ) -> Result<PlanMaterialization, ProviderError>;

    /// AF-V2. Executes one stored private action under a verified permit.
    fn execute_stored_action(&self) -> Result<(), ProviderError> {
        Err(ProviderError::ExecutionUnavailable(
            "execution is an AF-V2 capability; this build has no durable engine".into(),
        ))
    }

    /// AF-V2. Read-only reconcile after an unknown outcome.
    fn reconcile_read_only(&self) -> Result<(), ProviderError> {
        Err(ProviderError::ExecutionUnavailable(
            "reconcile is an AF-V2 capability".into(),
        ))
    }

    /// AF-V2. Materializes a distinct superseding recovery plan.
    fn materialize_superseding_recovery(&self) -> Result<(), ProviderError> {
        Err(ProviderError::ExecutionUnavailable(
            "superseding recovery is an AF-V2 capability".into(),
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    /// The provider cannot drive this device/artifact/profile combination.
    Unsupported(String),
    /// The provider could produce a plan, but the facts do not permit it.
    FactsInsufficient(String),
    ExecutionUnavailable(String),
    Core(String),
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderError::Unsupported(detail) => write!(f, "unsupported: {detail}"),
            ProviderError::FactsInsufficient(detail) => {
                write!(f, "facts insufficient: {detail}")
            }
            ProviderError::ExecutionUnavailable(detail) => {
                write!(f, "execution unavailable: {detail}")
            }
            ProviderError::Core(detail) => write!(f, "{detail}"),
        }
    }
}

impl std::error::Error for ProviderError {}

#[cfg(test)]
mod tests {
    use super::*;
    use arkforge_core::digest::sha256;
    use arkforge_core::identity::Version;

    fn key(platform: &str) -> MaturityKey {
        MaturityKey {
            provider: ProviderIdentity {
                id: OpaqueId::new("arkforge.rockchip").unwrap(),
                version: Version::new(1, 0, 0),
                implementation_digest: sha256(b"impl"),
            },
            profile: DeviceProfileIdentity {
                id: OpaqueId::new("org.openharmony.dayu200").unwrap(),
                version: Version::new(1, 0, 0),
                digest: sha256(b"profile"),
            },
            artifact_format: ArtifactFormat {
                id: OpaqueId::new("rockchip-images-targz").unwrap(),
                version: Version::new(1, 0, 0),
            },
            toolchain: ToolchainIdentity {
                id: OpaqueId::new("rkdeveloptool-fixed").unwrap(),
                kind: arkforge_core::identity::ToolchainKind::FixedTool,
                version: Version::new(1, 32, 0),
                backend_digest: sha256(b"tool"),
                upstream_ref: None,
            },
            host_platform: HostPlatform::new(platform, "aarch64").unwrap(),
            driver_facts_digest: sha256(b"driver"),
            evidence_set_digest: sha256(b"evidence"),
        }
    }

    #[test]
    fn an_unpublished_combination_is_unavailable_not_defaulted() {
        let registry = MaturityRegistry::new();
        let state = registry.lookup(&key("macos"));
        assert!(matches!(state, MaturityState::Unavailable { .. }));
        assert!(!state.permits_executable_plan());
    }

    #[test]
    fn maturity_is_scoped_to_the_exact_combination() {
        let mut registry = MaturityRegistry::new();
        registry.publish(&key("macos"), MaturityState::ProductionVerified);
        assert!(registry.lookup(&key("macos")).permits_executable_plan());
        // The same provider on another platform was not measured there
        // (architecture.md 24.1: support is declared only where it was tested).
        assert!(!registry.lookup(&key("linux")).permits_executable_plan());
    }

    #[test]
    fn republishing_replaces_rather_than_shadows() {
        let mut registry = MaturityRegistry::new();
        registry.publish(&key("macos"), MaturityState::ProductionVerified);
        registry.publish(
            &key("macos"),
            MaturityState::Unavailable {
                reason: "withdrawn".into(),
            },
        );
        assert!(!registry.lookup(&key("macos")).permits_executable_plan());
    }
}
