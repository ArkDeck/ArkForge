//! Digest primitives and the domain separation table.
//!
//! Every digest in ArkForge is `SHA-256(domain || payload)` (architecture.md
//! 6.2). The domain string is what stops a byte sequence that is a valid
//! private action from also being a valid public step, or a journal record from
//! colliding with the plan it describes.

pub mod cbor;
pub mod sha256;

pub use cbor::{CanonicalCbor, CborError, CborValue, decode_canonical};
pub use sha256::{DigestParseError, Sha256, Sha256Digest, sha256};

/// A digest domain separator.
///
/// Each variant's byte string is part of the wire contract: changing one
/// invalidates every stored digest of that kind, which is a schema-version
/// event, not a refactor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Domain {
    PrivateAction,
    ProviderExecutionPlan,
    PublicProjection,
    PublicStep,
    EffectSet,
    Plan,
    DeviceFacts,
    TransportSession,
    ProviderFacts,
    ToolchainFacts,
    ArtifactFacts,
    ArtifactManifest,
    DeviceProfile,
    AdmissionSnapshot,
    ActionReceipt,
    PossibleEffectSet,
    JournalRecord,
    Transcript,
    RecoveryCoverage,
}

impl Domain {
    /// The literal prefix bytes. The trailing NUL keeps one domain from being a
    /// prefix of another once a longer name is added.
    pub const fn as_bytes(self) -> &'static [u8] {
        match self {
            Domain::PrivateAction => b"arkforge/v1/private-action\0",
            Domain::ProviderExecutionPlan => b"arkforge/v1/provider-execution-plan\0",
            Domain::PublicProjection => b"arkforge/v1/public-projection\0",
            Domain::PublicStep => b"arkforge/v1/public-step\0",
            Domain::EffectSet => b"arkforge/v1/effect-set\0",
            Domain::Plan => b"arkforge/v1/plan\0",
            Domain::DeviceFacts => b"arkforge/v1/device-facts\0",
            Domain::TransportSession => b"arkforge/v1/transport-session\0",
            Domain::ProviderFacts => b"arkforge/v1/provider-facts\0",
            Domain::ToolchainFacts => b"arkforge/v1/toolchain-facts\0",
            Domain::ArtifactFacts => b"arkforge/v1/artifact-facts\0",
            Domain::ArtifactManifest => b"arkforge/v1/artifact-manifest\0",
            Domain::DeviceProfile => b"arkforge/v1/device-profile\0",
            Domain::AdmissionSnapshot => b"arkforge/v1/admission-snapshot\0",
            Domain::ActionReceipt => b"arkforge/v1/action-receipt\0",
            Domain::PossibleEffectSet => b"arkforge/v1/possible-effect-set\0",
            Domain::JournalRecord => b"arkforge/v1/journal-record\0",
            Domain::Transcript => b"arkforge/v1/transcript\0",
            Domain::RecoveryCoverage => b"arkforge/v1/recovery-coverage\0",
        }
    }
}

/// `SHA-256(domain || payload)`.
pub fn digest_in_domain(domain: Domain, payload: &[u8]) -> Sha256Digest {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update(payload);
    hasher.finalize()
}

/// `SHA-256(domain || deterministic_cbor(value))`.
pub fn digest_canonical<T: CanonicalCbor>(
    domain: Domain,
    value: &T,
) -> Result<Sha256Digest, CborError> {
    let bytes = value.to_canonical_bytes()?;
    Ok(digest_in_domain(domain, &bytes))
}

/// `SHA-256(domain || d0 || d1 || …)` over an ordered digest list.
///
/// Used where the architecture specifies a digest *of ordered digests*
/// (providerExecutionPlanDigest, architecture.md 6.2) rather than of a
/// structure.
pub fn digest_ordered(domain: Domain, digests: &[Sha256Digest]) -> Sha256Digest {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    for digest in digests {
        hasher.update(digest.as_bytes());
    }
    hasher.finalize()
}

impl CanonicalCbor for Sha256Digest {
    fn to_cbor(&self) -> CborValue {
        CborValue::Bytes(self.as_bytes().to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains_are_distinct_and_nul_terminated() {
        let all = [
            Domain::PrivateAction,
            Domain::ProviderExecutionPlan,
            Domain::PublicProjection,
            Domain::PublicStep,
            Domain::EffectSet,
            Domain::Plan,
            Domain::DeviceFacts,
            Domain::TransportSession,
            Domain::ProviderFacts,
            Domain::ToolchainFacts,
            Domain::ArtifactFacts,
            Domain::ArtifactManifest,
            Domain::DeviceProfile,
            Domain::AdmissionSnapshot,
            Domain::ActionReceipt,
            Domain::PossibleEffectSet,
            Domain::JournalRecord,
            Domain::Transcript,
            Domain::RecoveryCoverage,
        ];
        let mut seen = std::collections::BTreeSet::new();
        for domain in all {
            let bytes = domain.as_bytes();
            assert_eq!(
                *bytes.last().unwrap(),
                0,
                "{domain:?} must be NUL-terminated"
            );
            assert!(seen.insert(bytes), "duplicate domain string for {domain:?}");
        }
    }

    #[test]
    fn same_payload_in_two_domains_yields_two_digests() {
        let payload = b"identical bytes";
        assert_ne!(
            digest_in_domain(Domain::PrivateAction, payload),
            digest_in_domain(Domain::PublicStep, payload)
        );
    }

    #[test]
    fn ordered_digest_is_order_sensitive() {
        let a = sha256(b"a");
        let b = sha256(b"b");
        assert_ne!(
            digest_ordered(Domain::ProviderExecutionPlan, &[a, b]),
            digest_ordered(Domain::ProviderExecutionPlan, &[b, a])
        );
    }
}

/// HMAC-SHA-256 (RFC 2104).
///
/// Used for the StepPermit integrity tag (architecture.md 8.6). It lives in
/// Core because both sides of the authority boundary must compute it the same
/// way; *who may mint* versus *who may only verify* is enforced one layer up,
/// in `arkforge-authority-api`.
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> Sha256Digest {
    const BLOCK: usize = 64;
    let mut padded = [0u8; BLOCK];
    if key.len() > BLOCK {
        padded[..32].copy_from_slice(sha256(key).as_bytes());
    } else {
        padded[..key.len()].copy_from_slice(key);
    }

    let mut inner_key = [0u8; BLOCK];
    let mut outer_key = [0u8; BLOCK];
    for index in 0..BLOCK {
        inner_key[index] = padded[index] ^ 0x36;
        outer_key[index] = padded[index] ^ 0x5c;
    }

    let mut inner = Sha256::new();
    inner.update(&inner_key);
    inner.update(message);
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(&outer_key);
    outer.update(inner_digest.as_bytes());
    outer.finalize()
}

/// Constant-time equality for secret-derived values.
///
/// A tag comparison that short-circuits on the first differing byte leaks how
/// much of a forgery was right.
pub fn constant_time_eq(left: &Sha256Digest, right: &Sha256Digest) -> bool {
    let mut difference = 0u8;
    for (a, b) in left.as_bytes().iter().zip(right.as_bytes().iter()) {
        difference |= a ^ b;
    }
    difference == 0
}

#[cfg(test)]
mod hmac_tests {
    use super::*;

    /// RFC 4231 test vectors for HMAC-SHA-256.
    #[test]
    fn rfc4231_vectors() {
        let key = vec![0x0bu8; 20];
        assert_eq!(
            hmac_sha256(&key, b"Hi There").to_hex(),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );

        assert_eq!(
            hmac_sha256(b"Jefe", b"what do ya want for nothing?").to_hex(),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );

        // Case 6: a key longer than the block size is hashed first.
        let long_key = vec![0xaau8; 131];
        assert_eq!(
            hmac_sha256(
                &long_key,
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )
            .to_hex(),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn a_different_key_produces_a_different_tag() {
        assert_ne!(hmac_sha256(b"k1", b"m"), hmac_sha256(b"k2", b"m"));
    }

    #[test]
    fn constant_time_eq_agrees_with_equality() {
        let a = sha256(b"a");
        let b = sha256(b"b");
        assert!(constant_time_eq(&a, &a));
        assert!(!constant_time_eq(&a, &b));
    }
}
