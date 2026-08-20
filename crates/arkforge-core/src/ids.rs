//! Typed identifiers.
//!
//! architecture.md 15.4 forbids "non-conforming IDs" from entering the digest
//! model. That is enforced here rather than by convention: an `OpaqueId` cannot
//! be constructed from a string that would be ambiguous to canonicalize (case
//! folding, whitespace, path separators, non-ASCII confusables).

use crate::digest::{CanonicalCbor, CborValue};
use core::fmt;

/// The maximum length of any ArkForge identifier.
pub const MAX_ID_LEN: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdError {
    Empty,
    TooLong(usize),
    InvalidCharacter(char),
}

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdError::Empty => f.write_str("identifier must not be empty"),
            IdError::TooLong(len) => {
                write!(
                    f,
                    "identifier must be at most {MAX_ID_LEN} bytes, found {len}"
                )
            }
            IdError::InvalidCharacter(found) => write!(
                f,
                "identifier must be ASCII [A-Za-z0-9._:-], found {found:?}"
            ),
        }
    }
}

impl std::error::Error for IdError {}

/// An identifier ArkForge stores and hashes but does not interpret.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OpaqueId(String);

impl OpaqueId {
    pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
        let value = value.into();
        if value.is_empty() {
            return Err(IdError::Empty);
        }
        if value.len() > MAX_ID_LEN {
            return Err(IdError::TooLong(value.len()));
        }
        for character in value.chars() {
            let allowed =
                character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | ':' | '-');
            if !allowed {
                return Err(IdError::InvalidCharacter(character));
            }
        }
        Ok(OpaqueId(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for OpaqueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for OpaqueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl CanonicalCbor for OpaqueId {
    fn to_cbor(&self) -> CborValue {
        CborValue::Text(self.0.clone())
    }
}

macro_rules! typed_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(OpaqueId);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
                Ok($name(OpaqueId::new(value)?))
            }

            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }

            pub fn opaque(&self) -> &OpaqueId {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "({:?})"), self.0.as_str())
            }
        }

        impl CanonicalCbor for $name {
            fn to_cbor(&self) -> CborValue {
                self.0.to_cbor()
            }
        }
    };
}

typed_id!(
    /// Identifies an immutable materialized plan.
    PlanId
);
typed_id!(
    /// Identifies a public step inside a plan.
    StepId
);
typed_id!(
    /// Identifies a private provider action inside a stored execution plan.
    ActionId
);
typed_id!(
    /// Identifies a durable execution job.
    JobId
);
typed_id!(
    /// Distinguishes attempts of one step. Attempts never re-dispatch a
    /// completed external effect (architecture.md 14.1).
    AttemptId
);
typed_id!(
    /// Identifies a single-use StepPermit.
    PermitId
);
typed_id!(RequestId);
typed_id!(
    /// Identifies the controller session that owns destructive intent.
    ControllerSessionId
);
typed_id!(ObservationId);
typed_id!(
    /// A semantic partition name, never an address.
    PartitionId
);
typed_id!(
    /// A named raw region for providers whose medium has no partition table.
    RegionId
);
typed_id!(ArtifactId);
typed_id!(EvidenceId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_conforming_identifiers() {
        for value in [
            "PLAN-1",
            "step.enter-updater",
            "arkforge.example:example-tool-fixed",
            "ECAMP-96EFFF150CEEECBFCC7AEB52",
        ] {
            assert!(OpaqueId::new(value).is_ok(), "{value} should be accepted");
        }
    }

    #[test]
    fn rejects_shapes_that_would_be_ambiguous_in_a_digest() {
        assert_eq!(OpaqueId::new(""), Err(IdError::Empty));
        assert_eq!(
            OpaqueId::new("has space"),
            Err(IdError::InvalidCharacter(' '))
        );
        assert_eq!(
            OpaqueId::new("/etc/passwd"),
            Err(IdError::InvalidCharacter('/'))
        );
        assert_eq!(OpaqueId::new("naïve"), Err(IdError::InvalidCharacter('ï')));
        assert_eq!(OpaqueId::new("a\nb"), Err(IdError::InvalidCharacter('\n')));
        assert!(matches!(
            OpaqueId::new("x".repeat(MAX_ID_LEN + 1)),
            Err(IdError::TooLong(_))
        ));
    }

    #[test]
    fn typed_ids_do_not_interconvert() {
        let plan = PlanId::new("PLAN-1").unwrap();
        let step = StepId::new("PLAN-1").unwrap();
        // Same text, different types: the compiler is the guard. Their CBOR is
        // identical by construction, which is why the digest domain — not the
        // ID type — separates them on the wire.
        assert_eq!(plan.to_cbor(), step.to_cbor());
    }
}
