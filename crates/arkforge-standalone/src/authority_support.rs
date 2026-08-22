//! Independent release gate for the reusable standalone authority implementation.
//!
//! Mechanics maturity and authority maturity are deliberately different keys.
//! An ArkDeck record, another CLI build, another control map, another permit
//! codec, or another mechanics combination cannot authorize this process.

use arkforge_core::authority::{AuthorityNamespace, AuthoritySupportState};
use arkforge_core::digest::{
    CanonicalCbor, CborValue, Domain, Sha256Digest, digest_in_domain, sha256,
};
use arkforge_core::identity::{HostPlatform, Version};

pub const AUTHORITY_NAMESPACE: &str = "arkforge.cli";
pub const IMPLEMENTATION_VERSION: Version = Version::new(0, 1, 0);

const CONTROL_MAPPING: &str = "arkforge.cli-hdc-control/v1\nenterUpdater=accepted+exactDetach+uniqueLoaderRebind\nrebootToNormal=exactBoundNormalRebind\nreadProductFacts=const.product.model\nreadBuildFacts=const.ohos.fullname\n";
const PERMIT_CODEC: &str =
    "arkforge.step-permit/rfc8949-v1+hmac-sha256;exact-stored-bytes;same-epoch-retransmit";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthoritySupportKey {
    pub authority_namespace: AuthorityNamespace,
    pub authority_implementation_version: Version,
    pub authority_implementation_digest: Sha256Digest,
    pub managed_control_mapping_digest: Sha256Digest,
    pub managed_control_tool_digest: Sha256Digest,
    pub permit_codec_digest: Sha256Digest,
    pub mechanics_maturity_key_digest: Sha256Digest,
    pub host_platform: HostPlatform,
}

impl AuthoritySupportKey {
    pub fn for_running_build(
        implementation_digest: Sha256Digest,
        mechanics_maturity_key_digest: Sha256Digest,
        managed_control_tool_digest: Sha256Digest,
    ) -> Self {
        Self {
            authority_namespace: AuthorityNamespace::new(AUTHORITY_NAMESPACE)
                .expect("literal namespace"),
            authority_implementation_version: IMPLEMENTATION_VERSION,
            authority_implementation_digest: implementation_digest,
            managed_control_mapping_digest: sha256(CONTROL_MAPPING.as_bytes()),
            managed_control_tool_digest,
            permit_codec_digest: sha256(PERMIT_CODEC.as_bytes()),
            mechanics_maturity_key_digest,
            host_platform: HostPlatform::current(),
        }
    }

    pub fn digest(&self) -> Result<Sha256Digest, String> {
        let bytes = self
            .to_canonical_bytes()
            .map_err(|error| error.to_string())?;
        Ok(digest_in_domain(Domain::AuthoritySupportKey, &bytes))
    }
}

impl CanonicalCbor for AuthoritySupportKey {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("authorityNamespace", self.authority_namespace.to_cbor()),
            (
                "authorityImplementationVersion",
                self.authority_implementation_version.to_cbor(),
            ),
            (
                "authorityImplementationDigest",
                self.authority_implementation_digest.to_cbor(),
            ),
            (
                "managedControlMappingDigest",
                self.managed_control_mapping_digest.to_cbor(),
            ),
            (
                "managedControlToolDigest",
                self.managed_control_tool_digest.to_cbor(),
            ),
            ("permitCodecDigest", self.permit_codec_digest.to_cbor()),
            (
                "mechanicsMaturityKeyDigest",
                self.mechanics_maturity_key_digest.to_cbor(),
            ),
            ("hostPlatform", self.host_platform.to_cbor()),
        ])
    }
}

/// Production support records are intentionally empty until the exact
/// CLI-authority hardware and crash campaign is reviewed. Mechanics maturity
/// cannot populate this registry and rescue evidence is not consulted.
pub fn lookup(_key: &AuthoritySupportKey) -> AuthoritySupportState {
    AuthoritySupportState::HardwareGated {
        blocker: "The exact arkforge.cli authority build/control-map/permit-codec/mechanics/platform combination has no reviewed hardware support record.".into(),
    }
}

/// Classifies the exact authority key for a named acceptance campaign or the
/// reviewed production registry. Campaign state is deliberately explicit and
/// remains distinct from production support in every sealed plan and receipt.
pub fn classify(
    key: &AuthoritySupportKey,
    hardware_campaign: Option<&str>,
) -> AuthoritySupportState {
    match hardware_campaign {
        Some(campaign) => AuthoritySupportState::HardwareCampaign {
            campaign: campaign.to_string(),
        },
        None => lookup(key),
    }
}

/// Whether this build contains any maintainer-reviewed support record. Exact
/// execution still requires [`lookup`] for the full key; this coarse fact is
/// only for `status` readiness reporting.
pub const fn has_reviewed_support_records() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use arkforge_core::authority::AuthoritySupportState;

    #[test]
    fn every_authority_axis_changes_the_support_key() {
        let first = AuthoritySupportKey::for_running_build(
            sha256(b"cli-a"),
            sha256(b"mechanics"),
            sha256(b"hdc"),
        );
        let first_digest = first.digest().unwrap();
        let assert_changed = |candidate: &AuthoritySupportKey| {
            assert_ne!(first_digest, candidate.digest().unwrap());
        };

        let mut second = first.clone();
        second.authority_namespace = AuthorityNamespace::new("arkforge.other").unwrap();
        assert_changed(&second);
        second = first.clone();
        second.authority_implementation_version = Version::new(0, 1, 1);
        assert_changed(&second);
        second = first.clone();
        second.authority_implementation_digest = sha256(b"cli-b");
        assert_changed(&second);
        second = first.clone();
        second.managed_control_mapping_digest = sha256(b"other mapping");
        assert_changed(&second);
        second = first.clone();
        second.managed_control_tool_digest = sha256(b"other hdc");
        assert_changed(&second);
        second = first.clone();
        second.permit_codec_digest = sha256(b"other codec");
        assert_changed(&second);
        second = first.clone();
        second.mechanics_maturity_key_digest = sha256(b"other mechanics");
        assert_changed(&second);
        second = first.clone();
        second.host_platform = HostPlatform::new("other-os", "other-arch").unwrap();
        assert_changed(&second);
    }

    #[test]
    fn unpublished_support_is_hardware_gated_not_inherited() {
        let key = AuthoritySupportKey::for_running_build(
            sha256(b"cli"),
            sha256(b"mechanics"),
            sha256(b"hdc"),
        );
        let state = lookup(&key);
        assert!(!state.permits_execution());
        assert_eq!(state.as_str(), "hardwareGated");
    }

    #[test]
    fn campaign_and_production_are_the_only_executable_states() {
        assert!(AuthoritySupportState::ProductionVerified.permits_execution());
        assert!(
            AuthoritySupportState::HardwareCampaign {
                campaign: "CLI-AC".into()
            }
            .permits_execution()
        );
        let key = AuthoritySupportKey::for_running_build(
            sha256(b"cli"),
            sha256(b"mechanics"),
            sha256(b"hdc"),
        );
        assert!(matches!(
            classify(&key, Some("CLI-AC-28")),
            AuthoritySupportState::HardwareCampaign { campaign }
                if campaign == "CLI-AC-28"
        ));
    }
}
