//! Neutral authority reference.
//!
//! architecture.md 5.1. ArkForge stores and hashes the authority's binding but
//! never interprets its business meaning: no `arkdeck_*` type reaches Core
//! (architecture.md 0.2, 1.3).

use crate::digest::{CanonicalCbor, CborValue, Sha256Digest};
use crate::ids::{IdError, OpaqueId};
use core::fmt;

/// Names the authority that issued a binding, e.g. `arkdeck`.
///
/// ArkForge compares this for equality and refuses to mix namespaces inside one
/// plan; it attaches no other meaning to the value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuthorityNamespace(OpaqueId);

impl AuthorityNamespace {
    pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
        Ok(AuthorityNamespace(OpaqueId::new(value)?))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for AuthorityNamespace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl CanonicalCbor for AuthorityNamespace {
    fn to_cbor(&self) -> CborValue {
        self.0.to_cbor()
    }
}

/// A reference to the authority's target binding.
///
/// `stable_identity_digest` is what lets ArkForge detect that "the same
/// binding" now points at a different device without ArkForge having to know
/// how the authority computes identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityBindingRef {
    pub authority_namespace: AuthorityNamespace,
    pub binding_id: OpaqueId,
    pub binding_revision: u64,
    pub stable_identity_digest: Sha256Digest,
}

impl CanonicalCbor for AuthorityBindingRef {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("authorityNamespace", self.authority_namespace.to_cbor()),
            ("bindingId", self.binding_id.to_cbor()),
            ("bindingRevision", CborValue::Unsigned(self.binding_revision)),
            (
                "stableIdentityDigest",
                self.stable_identity_digest.to_cbor(),
            ),
        ])
    }
}

impl AuthorityBindingRef {
    /// Two references describe the same binding at the same revision *and* the
    /// same underlying identity. A revision bump or an identity change makes
    /// this false, which is what forces re-materialization rather than reuse.
    pub fn is_same_binding_state(&self, other: &AuthorityBindingRef) -> bool {
        self == other
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::sha256;

    fn binding(revision: u64, identity: &[u8]) -> AuthorityBindingRef {
        AuthorityBindingRef {
            authority_namespace: AuthorityNamespace::new("test-authority").unwrap(),
            binding_id: OpaqueId::new("TGT-958780b2ffb7").unwrap(),
            binding_revision: revision,
            stable_identity_digest: sha256(identity),
        }
    }

    #[test]
    fn revision_bump_is_a_different_binding_state() {
        assert!(!binding(2, b"device").is_same_binding_state(&binding(3, b"device")));
    }

    #[test]
    fn identity_change_is_a_different_binding_state() {
        assert!(!binding(2, b"device").is_same_binding_state(&binding(2, b"other-device")));
    }

    #[test]
    fn binding_digest_is_stable_across_constructions() {
        let left = binding(2, b"device").to_canonical_bytes().unwrap();
        let right = binding(2, b"device").to_canonical_bytes().unwrap();
        assert_eq!(left, right);
    }
}
