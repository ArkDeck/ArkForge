//! Read-domain-aware verification.
//!
//! architecture.md 16.4 and evidence AD-006. The hard fact this module encodes:
//! on the DAYU200 loader the read face and the write face are not the same
//! size. Reads past the window return uniform filler regardless of what is on
//! the medium, so a readback there can neither confirm nor refute a write.
//!
//! The three-state outcome exists so "we could not look" never renders as
//! "verified", and never renders as "the write failed" either — that
//! conflation is what produced a day of false "fake write" diagnoses
//! (AD-006, PR #1066–#1070).

use crate::digest::{CanonicalCbor, CborValue, Sha256Digest};
use crate::effect::ByteRange;
use core::fmt;

/// How strong a verification claim is. Never inflate: `PrefixHash` must not be
/// reported as full verification (architecture.md 16.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VerificationStrength {
    /// The write completed with the protocol's own success semantics, and
    /// nothing was read back.
    SemanticOnly,
    /// A prefix of the declared range was hashed.
    PrefixHash,
    /// Sampled ranges were hashed.
    SampledRanges,
    /// The whole declared range was hashed.
    FullHash,
}

impl VerificationStrength {
    pub fn as_str(self) -> &'static str {
        match self {
            VerificationStrength::SemanticOnly => "semanticOnly",
            VerificationStrength::PrefixHash => "prefixHash",
            VerificationStrength::SampledRanges => "sampledRanges",
            VerificationStrength::FullHash => "fullHash",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "semanticOnly" => Some(VerificationStrength::SemanticOnly),
            "prefixHash" => Some(VerificationStrength::PrefixHash),
            "sampledRanges" => Some(VerificationStrength::SampledRanges),
            "fullHash" => Some(VerificationStrength::FullHash),
            _ => None,
        }
    }
}

impl CanonicalCbor for VerificationStrength {
    fn to_cbor(&self) -> CborValue {
        CborValue::text(self.as_str())
    }
}

/// How much of the medium the read path can actually see.
///
/// `CharacterizeAtRuntime` is what a Profile declares: the window size is a
/// measured fact of the session, never a Profile constant (architecture.md 16.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadDomainDeclaration {
    /// The read face is known to reach the whole medium.
    Full,
    /// The read face must be characterized at the start of every execution.
    CharacterizeAtRuntime,
}

impl ReadDomainDeclaration {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReadDomainDeclaration::Full => "full",
            ReadDomainDeclaration::CharacterizeAtRuntime => "characterize-at-runtime",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "full" => Some(ReadDomainDeclaration::Full),
            "characterize-at-runtime" => Some(ReadDomainDeclaration::CharacterizeAtRuntime),
            _ => None,
        }
    }
}

impl CanonicalCbor for ReadDomainDeclaration {
    fn to_cbor(&self) -> CborValue {
        CborValue::text(self.as_str())
    }
}

/// A measured read domain for one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeasuredReadDomain {
    /// The read face reached the end of the medium (the backup table read back
    /// and self-identified).
    Full,
    /// The read face stops before the medium does. `detail` is recorded into
    /// every receipt that skips because of it.
    Windowed { detail: String },
}

impl MeasuredReadDomain {
    /// Whether a readback of `range` can be trusted either way.
    ///
    /// A windowed domain answers `false` for every range: this implementation
    /// deliberately does not guess where the window ends. The observed
    /// behaviour (AD-006) is that reads past the boundary succeed and return
    /// filler, so a computed boundary would be a guess that reads like a fact.
    pub fn covers(&self, _range: &ByteRange) -> bool {
        matches!(self, MeasuredReadDomain::Full)
    }

    pub fn detail(&self) -> Option<&str> {
        match self {
            MeasuredReadDomain::Full => None,
            MeasuredReadDomain::Windowed { detail } => Some(detail),
        }
    }
}

impl CanonicalCbor for MeasuredReadDomain {
    fn to_cbor(&self) -> CborValue {
        match self {
            MeasuredReadDomain::Full => CborValue::map(vec![
                ("domain", CborValue::text("full")),
                ("detail", CborValue::Null),
            ]),
            MeasuredReadDomain::Windowed { detail } => CborValue::map(vec![
                ("domain", CborValue::text("windowed")),
                ("detail", CborValue::text(detail.clone())),
            ]),
        }
    }
}

/// What a readback actually saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadbackObservation {
    /// Content hashed to the declared digest.
    ContentMatched,
    /// The range read back as a single repeated byte. On this medium that is
    /// the erased-medium filler, which means "nothing was written here" — a
    /// different fact from "the wrong bytes are here", and reported as such.
    UniformFiller { byte: u8 },
    /// Content read back and hashed to something else.
    ContentMismatched { observed: Sha256Digest },
}

/// The verdict for one verification step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationOutcome {
    Verified {
        strength: VerificationStrength,
        range: ByteRange,
    },
    /// The read domain does not cover the range. Not a failure, and not any
    /// grade of verified.
    TypedSkip {
        range: ByteRange,
        reason: TypedSkipReason,
        detail: String,
    },
    Failed {
        range: ByteRange,
        classification: FailureClassification,
    },
}

impl VerificationOutcome {
    /// TypedSkip never counts as verified at any strength (architecture.md 25.23).
    pub fn verified_strength(&self) -> Option<VerificationStrength> {
        match self {
            VerificationOutcome::Verified { strength, .. } => Some(*strength),
            _ => None,
        }
    }

    pub fn is_failure(&self) -> bool {
        matches!(self, VerificationOutcome::Failed { .. })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            VerificationOutcome::Verified { .. } => "verified",
            VerificationOutcome::TypedSkip { .. } => "typedSkip",
            VerificationOutcome::Failed { .. } => "failed",
        }
    }
}

impl CanonicalCbor for VerificationOutcome {
    fn to_cbor(&self) -> CborValue {
        match self {
            VerificationOutcome::Verified { strength, range } => CborValue::map(vec![
                ("outcome", CborValue::text("verified")),
                ("strength", strength.to_cbor()),
                ("range", range.to_cbor()),
            ]),
            VerificationOutcome::TypedSkip {
                range,
                reason,
                detail,
            } => CborValue::map(vec![
                ("outcome", CborValue::text("typedSkip")),
                ("range", range.to_cbor()),
                ("reason", CborValue::text(reason.as_str())),
                ("detail", CborValue::text(detail.clone())),
            ]),
            VerificationOutcome::Failed {
                range,
                classification,
            } => CborValue::map(vec![
                ("outcome", CborValue::text("failed")),
                ("range", range.to_cbor()),
                ("classification", CborValue::text(classification.as_str())),
            ]),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TypedSkipReason {
    /// The declared range lies outside the measured read window.
    OutsideReadDomain,
    /// The Profile declares this target unreachable by readback at any strength.
    ProfileDeclaresUnreachable,
}

impl TypedSkipReason {
    pub fn as_str(self) -> &'static str {
        match self {
            TypedSkipReason::OutsideReadDomain => "skipped-lba-read-window",
            TypedSkipReason::ProfileDeclaresUnreachable => "profile-declares-unreachable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FailureClassification {
    /// Content read back and did not match. An honest mismatch.
    ContentMismatch,
    /// The range read back as erased-medium filler inside a read domain that
    /// covers it: the write did not land. Reported under its own name so it is
    /// never confused with a content mismatch, and never re-derived from a
    /// read outside the window.
    ErasedMediumFiller,
}

impl FailureClassification {
    pub fn as_str(self) -> &'static str {
        match self {
            FailureClassification::ContentMismatch => "content-mismatch",
            FailureClassification::ErasedMediumFiller => "erased-medium-filler",
        }
    }
}

/// What a Profile declares about verifying one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetVerificationDeclaration {
    /// The strongest claim this target can support when the read domain covers
    /// it. A Profile may not declare readback strength for a target the read
    /// domain cannot reach (architecture.md 18.3).
    pub max_strength_when_readable: VerificationStrength,
    /// What stands in when the read domain does not cover the target.
    pub fallback: VerificationFallback,
}

/// The evidence that carries a target when readback cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VerificationFallback {
    /// The write protocol's own completion semantics were observed.
    pub write_completion_semantics: bool,
    /// A booted-device build/model postflight covers this target.
    pub build_postflight: bool,
}

impl CanonicalCbor for VerificationFallback {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            (
                "writeCompletionSemantics",
                CborValue::Bool(self.write_completion_semantics),
            ),
            ("buildPostflight", CborValue::Bool(self.build_postflight)),
        ])
    }
}

impl CanonicalCbor for TargetVerificationDeclaration {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            (
                "maxStrengthWhenReadable",
                self.max_strength_when_readable.to_cbor(),
            ),
            ("fallback", self.fallback.to_cbor()),
        ])
    }
}

/// Classifies one verification attempt.
///
/// The read domain is consulted *first*: outside it, the observation is not
/// evidence at all and is not even examined.
pub fn classify_verification(
    range: ByteRange,
    read_domain: &MeasuredReadDomain,
    declaration: &TargetVerificationDeclaration,
    observation: Option<ReadbackObservation>,
) -> VerificationOutcome {
    if !read_domain.covers(&range) {
        return VerificationOutcome::TypedSkip {
            range,
            reason: TypedSkipReason::OutsideReadDomain,
            detail: read_domain
                .detail()
                .unwrap_or("read domain does not cover this range")
                .to_string(),
        };
    }
    match observation {
        None => VerificationOutcome::TypedSkip {
            range,
            reason: TypedSkipReason::ProfileDeclaresUnreachable,
            detail: "no readback was performed for this target".to_string(),
        },
        Some(ReadbackObservation::ContentMatched) => VerificationOutcome::Verified {
            strength: declaration.max_strength_when_readable,
            range,
        },
        Some(ReadbackObservation::UniformFiller { .. }) => VerificationOutcome::Failed {
            range,
            classification: FailureClassification::ErasedMediumFiller,
        },
        Some(ReadbackObservation::ContentMismatched { .. }) => VerificationOutcome::Failed {
            range,
            classification: FailureClassification::ContentMismatch,
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationError {
    /// A Profile claimed a readback strength for a target its declared read
    /// domain cannot reach.
    StrengthExceedsReadDomain {
        target: String,
        declared: VerificationStrength,
    },
    /// A target that cannot be read back and has no fallback evidence has no
    /// verification story at all.
    NoEvidencePath { target: String },
}

impl fmt::Display for VerificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VerificationError::StrengthExceedsReadDomain { target, declared } => write!(
                f,
                "target {target} declares readback strength {} but its read domain cannot reach it",
                declared.as_str()
            ),
            VerificationError::NoEvidencePath { target } => write!(
                f,
                "target {target} has neither a reachable readback nor a declared fallback"
            ),
        }
    }
}

impl std::error::Error for VerificationError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::sha256;

    fn declaration() -> TargetVerificationDeclaration {
        TargetVerificationDeclaration {
            max_strength_when_readable: VerificationStrength::FullHash,
            fallback: VerificationFallback {
                write_completion_semantics: true,
                build_postflight: true,
            },
        }
    }

    fn range() -> ByteRange {
        // system starts at LBA 245760, far past the 2026-08-04 read window.
        ByteRange::new(245_760 * 512, 2_147_483_648).unwrap()
    }

    #[test]
    fn outside_the_read_domain_a_match_is_still_a_typed_skip() {
        // The observation is ignored on purpose: past the window the read path
        // answers with filler, so *any* answer there is not evidence.
        let outcome = classify_verification(
            range(),
            &MeasuredReadDomain::Windowed {
                detail: "backup GPT header did not read back".into(),
            },
            &declaration(),
            Some(ReadbackObservation::ContentMatched),
        );
        assert_eq!(outcome.verified_strength(), None);
        assert!(!outcome.is_failure());
        assert!(matches!(
            outcome,
            VerificationOutcome::TypedSkip {
                reason: TypedSkipReason::OutsideReadDomain,
                ..
            }
        ));
    }

    #[test]
    fn uniform_filler_outside_the_window_is_never_a_failure() {
        // This is the AD-006 regression: nine partitions were declared "fake
        // writes" because a windowed read answered 0xCC.
        let outcome = classify_verification(
            range(),
            &MeasuredReadDomain::Windowed {
                detail: "read window ends before the medium does".into(),
            },
            &declaration(),
            Some(ReadbackObservation::UniformFiller { byte: 0xCC }),
        );
        assert!(!outcome.is_failure(), "got {outcome:?}");
    }

    #[test]
    fn uniform_filler_inside_the_window_fails_under_its_own_name() {
        let outcome = classify_verification(
            range(),
            &MeasuredReadDomain::Full,
            &declaration(),
            Some(ReadbackObservation::UniformFiller { byte: 0xCC }),
        );
        assert_eq!(
            outcome,
            VerificationOutcome::Failed {
                range: range(),
                classification: FailureClassification::ErasedMediumFiller,
            }
        );
    }

    #[test]
    fn a_real_mismatch_inside_the_window_is_a_content_mismatch() {
        let outcome = classify_verification(
            range(),
            &MeasuredReadDomain::Full,
            &declaration(),
            Some(ReadbackObservation::ContentMismatched {
                observed: sha256(b"other"),
            }),
        );
        assert_eq!(
            outcome,
            VerificationOutcome::Failed {
                range: range(),
                classification: FailureClassification::ContentMismatch,
            }
        );
    }

    #[test]
    fn a_match_inside_the_window_verifies_at_the_declared_strength() {
        let mut declaration = declaration();
        declaration.max_strength_when_readable = VerificationStrength::PrefixHash;
        let outcome = classify_verification(
            range(),
            &MeasuredReadDomain::Full,
            &declaration,
            Some(ReadbackObservation::ContentMatched),
        );
        // A prefix hash reports as a prefix hash, not as full verification.
        assert_eq!(
            outcome.verified_strength(),
            Some(VerificationStrength::PrefixHash)
        );
    }

    #[test]
    fn strength_ordering_does_not_let_prefix_pass_for_full() {
        assert!(VerificationStrength::PrefixHash < VerificationStrength::FullHash);
        assert!(VerificationStrength::SemanticOnly < VerificationStrength::PrefixHash);
    }
}
