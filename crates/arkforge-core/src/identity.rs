//! Provider / profile / artifact / toolchain identity and combination-scoped
//! maturity.
//!
//! architecture.md 5.3 and 12.3. Maturity is not a provider-global field: the
//! same Unisoc provider can be ProductionVerified for one SoC and PlanOnly for
//! DAYU600, so the state is keyed by the whole combination.

use crate::digest::{digest_canonical, CanonicalCbor, CborError, CborValue, Domain, Sha256Digest};
use crate::ids::{IdError, OpaqueId};
use core::fmt;

/// A semantic version triple. Strings are avoided so ordering is total and
/// comparison never depends on locale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Version {
            major,
            minor,
            patch,
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        let mut parts = value.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        Some(Version::new(major, minor, patch))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl CanonicalCbor for Version {
    fn to_cbor(&self) -> CborValue {
        CborValue::array(vec![
            CborValue::Unsigned(self.major as u64),
            CborValue::Unsigned(self.minor as u64),
            CborValue::Unsigned(self.patch as u64),
        ])
    }
}

/// Identifies a provider implementation exactly.
///
/// `implementation_digest` is what makes a rebuilt provider a *different*
/// provider for maturity and plan-binding purposes (architecture.md 16.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderIdentity {
    pub id: OpaqueId,
    pub version: Version,
    pub implementation_digest: Sha256Digest,
}

impl CanonicalCbor for ProviderIdentity {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("id", self.id.to_cbor()),
            ("version", self.version.to_cbor()),
            (
                "implementationDigest",
                self.implementation_digest.to_cbor(),
            ),
        ])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceProfileIdentity {
    pub id: OpaqueId,
    pub version: Version,
    pub digest: Sha256Digest,
}

impl CanonicalCbor for DeviceProfileIdentity {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("id", self.id.to_cbor()),
            ("version", self.version.to_cbor()),
            ("digest", self.digest.to_cbor()),
        ])
    }
}

/// The firmware container format an artifact is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactFormat {
    pub id: OpaqueId,
    pub version: Version,
}

impl CanonicalCbor for ArtifactFormat {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("id", self.id.to_cbor()),
            ("version", self.version.to_cbor()),
        ])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactIdentity {
    pub artifact_id: OpaqueId,
    pub format: ArtifactFormat,
    /// SHA-256 of the imported bytes as a whole.
    pub content_digest: Sha256Digest,
    pub size_bytes: u64,
    /// Digest of the parsed manifest, so a re-parse that yields different facts
    /// invalidates the plan even when the bytes are identical.
    pub manifest_digest: Sha256Digest,
}

impl CanonicalCbor for ArtifactIdentity {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("artifactId", self.artifact_id.to_cbor()),
            ("format", self.format.to_cbor()),
            ("contentDigest", self.content_digest.to_cbor()),
            ("sizeBytes", CborValue::Unsigned(self.size_bytes)),
            ("manifestDigest", self.manifest_digest.to_cbor()),
        ])
    }
}

/// The backend a provider dispatches through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ToolchainKind {
    /// A pinned vendor executable invoked with a closed argument vocabulary.
    FixedTool,
    /// A protocol implemented inside ArkForge.
    NativeProtocol,
    /// A recorded transcript replayed instead of touching hardware. Never
    /// ProductionVerified — it exists so contract tests can run without a
    /// device, and its presence in a plan marks that plan non-executable.
    Replay,
}

impl ToolchainKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ToolchainKind::FixedTool => "fixedTool",
            ToolchainKind::NativeProtocol => "nativeProtocol",
            ToolchainKind::Replay => "replay",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "fixedTool" => Some(ToolchainKind::FixedTool),
            "nativeProtocol" => Some(ToolchainKind::NativeProtocol),
            "replay" => Some(ToolchainKind::Replay),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolchainIdentity {
    pub id: OpaqueId,
    pub kind: ToolchainKind,
    pub version: Version,
    /// SHA-256 of the executable or of the native backend build.
    ///
    /// This is the discriminator: two builds of the same source are two
    /// toolchains, and the maturity key treats them as such.
    pub backend_digest: Sha256Digest,
    /// Where the source came from, when it is known — an upstream commit, a
    /// tag, a package revision.
    ///
    /// Not a substitute for the digest and never compared instead of it. It
    /// exists because a receipt that says only `038a8a0e…` cannot tell a reader
    /// *which* build that was, and on a host that carries several builds of one
    /// upstream commit that is the question a reader actually has (AD-010).
    pub upstream_ref: Option<String>,
}

impl CanonicalCbor for ToolchainIdentity {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("id", self.id.to_cbor()),
            ("kind", CborValue::text(self.kind.as_str())),
            ("version", self.version.to_cbor()),
            ("backendDigest", self.backend_digest.to_cbor()),
            (
                "upstreamRef",
                match &self.upstream_ref {
                    Some(reference) => CborValue::text(reference.clone()),
                    None => CborValue::Null,
                },
            ),
        ])
    }
}

/// The host platform a maturity claim was measured on.
///
/// architecture.md 21 and 24.1: a hosted CI build is not a hardware pass, and
/// support is declared only where it was measured.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HostPlatform {
    pub os: OpaqueId,
    pub arch: OpaqueId,
}

impl HostPlatform {
    pub fn new(os: &str, arch: &str) -> Result<Self, IdError> {
        Ok(HostPlatform {
            os: OpaqueId::new(os)?,
            arch: OpaqueId::new(arch)?,
        })
    }

    /// The platform this build is running on.
    pub fn current() -> Self {
        HostPlatform {
            os: OpaqueId::new(std::env::consts::OS).unwrap_or_else(|_| {
                OpaqueId::new("unknown").expect("literal is a conforming identifier")
            }),
            arch: OpaqueId::new(std::env::consts::ARCH).unwrap_or_else(|_| {
                OpaqueId::new("unknown").expect("literal is a conforming identifier")
            }),
        }
    }
}

impl CanonicalCbor for HostPlatform {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![("os", self.os.to_cbor()), ("arch", self.arch.to_cbor())])
    }
}

/// The exact combination a maturity state is scoped to (architecture.md 12.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaturityKey {
    pub provider: ProviderIdentity,
    pub profile: DeviceProfileIdentity,
    pub artifact_format: ArtifactFormat,
    pub toolchain: ToolchainIdentity,
    pub host_platform: HostPlatform,
    pub driver_facts_digest: Sha256Digest,
    pub evidence_set_digest: Sha256Digest,
}

impl MaturityKey {
    pub fn digest(&self) -> Result<Sha256Digest, CborError> {
        digest_canonical(Domain::ProviderFacts, self)
    }
}

impl CanonicalCbor for MaturityKey {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("provider", self.provider.to_cbor()),
            ("profile", self.profile.to_cbor()),
            ("artifactFormat", self.artifact_format.to_cbor()),
            ("toolchain", self.toolchain.to_cbor()),
            ("hostPlatform", self.host_platform.to_cbor()),
            ("driverFactsDigest", self.driver_facts_digest.to_cbor()),
            ("evidenceSetDigest", self.evidence_set_digest.to_cbor()),
        ])
    }
}

/// The published state of one combination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaturityState {
    /// Real hardware acceptance passed for this exact combination.
    ProductionVerified,
    /// Implementation complete, awaiting the hardware campaign.
    HardwareGated { blocker: String },
    /// Can materialize an assessment only; no executable plan.
    PlanOnly { blocker: String },
    /// Parsing and inspection only.
    ResearchOnly { blocker: String },
    Unavailable { reason: String },
}

impl MaturityState {
    /// Only ProductionVerified may back an executable plan (architecture.md 5.5).
    pub fn permits_executable_plan(&self) -> bool {
        matches!(self, MaturityState::ProductionVerified)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            MaturityState::ProductionVerified => "productionVerified",
            MaturityState::HardwareGated { .. } => "hardwareGated",
            MaturityState::PlanOnly { .. } => "planOnly",
            MaturityState::ResearchOnly { .. } => "researchOnly",
            MaturityState::Unavailable { .. } => "unavailable",
        }
    }

    pub fn blocker(&self) -> Option<&str> {
        match self {
            MaturityState::ProductionVerified => None,
            MaturityState::HardwareGated { blocker }
            | MaturityState::PlanOnly { blocker }
            | MaturityState::ResearchOnly { blocker } => Some(blocker),
            MaturityState::Unavailable { reason } => Some(reason),
        }
    }
}

impl CanonicalCbor for MaturityState {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("state", CborValue::text(self.as_str())),
            (
                "blocker",
                match self.blocker() {
                    Some(text) => CborValue::text(text),
                    None => CborValue::Null,
                },
            ),
        ])
    }
}

/// What the provider and transport agreed they can do for this plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegotiatedCapabilities {
    pub entries: Vec<(OpaqueId, bool)>,
}

impl NegotiatedCapabilities {
    pub fn empty() -> Self {
        NegotiatedCapabilities {
            entries: Vec::new(),
        }
    }

    pub fn is_enabled(&self, capability: &str) -> bool {
        self.entries
            .iter()
            .any(|(id, enabled)| id.as_str() == capability && *enabled)
    }
}

impl CanonicalCbor for NegotiatedCapabilities {
    fn to_cbor(&self) -> CborValue {
        CborValue::Map(
            self.entries
                .iter()
                .map(|(id, enabled)| (id.to_cbor(), CborValue::Bool(*enabled)))
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::sha256;

    fn key(toolchain_kind: ToolchainKind) -> MaturityKey {
        MaturityKey {
            provider: ProviderIdentity {
                id: OpaqueId::new("arkforge.example").unwrap(),
                version: Version::new(1, 0, 0),
                implementation_digest: sha256(b"impl"),
            },
            profile: DeviceProfileIdentity {
                id: OpaqueId::new("org.example.testboard").unwrap(),
                version: Version::new(1, 0, 0),
                digest: sha256(b"profile"),
            },
            artifact_format: ArtifactFormat {
                id: OpaqueId::new("example-images-targz").unwrap(),
                version: Version::new(1, 0, 0),
            },
            toolchain: ToolchainIdentity {
                id: OpaqueId::new("example-tool-fixed").unwrap(),
                kind: toolchain_kind,
                version: Version::new(1, 32, 0),
                backend_digest: sha256(b"tool"),
                upstream_ref: None,
            },
            host_platform: HostPlatform::new("macos", "aarch64").unwrap(),
            driver_facts_digest: sha256(b"driver"),
            evidence_set_digest: sha256(b"evidence"),
        }
    }

    #[test]
    fn a_changed_toolchain_is_a_different_maturity_key() {
        assert_ne!(
            key(ToolchainKind::FixedTool).digest().unwrap(),
            key(ToolchainKind::NativeProtocol).digest().unwrap()
        );
    }

    #[test]
    fn only_production_verified_permits_an_executable_plan() {
        assert!(MaturityState::ProductionVerified.permits_executable_plan());
        for state in [
            MaturityState::HardwareGated {
                blocker: "no device".into(),
            },
            MaturityState::PlanOnly {
                blocker: "UNI-U01 missing".into(),
            },
            MaturityState::ResearchOnly {
                blocker: "format unknown".into(),
            },
            MaturityState::Unavailable {
                reason: "withdrawn".into(),
            },
        ] {
            assert!(!state.permits_executable_plan(), "{state:?}");
        }
    }

    #[test]
    fn version_parse_is_strict() {
        assert_eq!(Version::parse("1.32.0"), Some(Version::new(1, 32, 0)));
        assert_eq!(Version::parse("1.32"), None);
        assert_eq!(Version::parse("1.32.0.1"), None);
        assert_eq!(Version::parse("v1.32.0"), None);
    }
}
