//! How an action ended.
//!
//! Core vocabulary, because a Provider produces a disposition and an authority
//! consumes one, and neither crate may depend on the other (architecture.md
//! 4.3). Two copies of a four-variant enum on either side of that boundary is
//! how the two spellings drift.

use crate::digest::{CanonicalCbor, CborValue};

/// How an action ended.
///
/// `exit 0` is not a variant, because a zero exit is not a disposition
/// (architecture.md 12.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ActionDisposition {
    /// The action's own semantic success marker was observed.
    SemanticSuccess,
    /// The action provably did not take effect.
    ConfirmedNoEffect,
    /// The action provably failed after taking effect.
    ConfirmedPartialEffect,
    /// Whether the effect happened is unknown. Never replay
    /// (architecture.md 14.1).
    OutcomeUnknown,
}

impl ActionDisposition {
    pub fn as_str(self) -> &'static str {
        match self {
            ActionDisposition::SemanticSuccess => "semanticSuccess",
            ActionDisposition::ConfirmedNoEffect => "confirmedNoEffect",
            ActionDisposition::ConfirmedPartialEffect => "confirmedPartialEffect",
            ActionDisposition::OutcomeUnknown => "outcomeUnknown",
        }
    }

    /// Whether this outcome permits any further dispatch of the same action.
    pub fn permits_redispatch(self) -> bool {
        // Never, for any variant. The method exists so the answer is stated
        // once, in a place a caller can reach, rather than assumed.
        false
    }
}

impl CanonicalCbor for ActionDisposition {
    fn to_cbor(&self) -> CborValue {
        CborValue::text(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_disposition_permits_a_redispatch() {
        for disposition in [
            ActionDisposition::SemanticSuccess,
            ActionDisposition::ConfirmedNoEffect,
            ActionDisposition::ConfirmedPartialEffect,
            ActionDisposition::OutcomeUnknown,
        ] {
            assert!(!disposition.permits_redispatch(), "{disposition:?}");
        }
    }
}
