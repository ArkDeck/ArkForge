//! The device profiles compiled into this build.
//!
//! One registry, in one place. The runtime that loads profiles and the frontend
//! that reasons about which profiles a device could be must never be able to
//! disagree about what this build knows, and two `include_str!` lists in two
//! crates is exactly how that disagreement starts.
//!
//! It lives here rather than in `arkforge-core` because naming a device belongs
//! on the device-aware side of architecture.md 4.3; the neutral crates stay
//! neutral.

use arkforge_core::profile::{self, DeviceProfile, ProfileError};

/// The profile documents shipped inside this build, as `(name, source)`.
pub const SHIPPED_PROFILE_SOURCES: &[(&str, &str)] = &[
    (
        "shipped dayu200",
        include_str!("../../../profiles/dayu200.yaml"),
    ),
    (
        "shipped dayu600",
        include_str!("../../../profiles/dayu600.yaml"),
    ),
];

/// Loads every shipped profile, reporting the first that fails to validate.
///
/// A shipped profile that does not validate is a build defect, not a caller
/// error, so it is surfaced rather than skipped.
pub fn shipped() -> Result<Vec<DeviceProfile>, (&'static str, ProfileError)> {
    SHIPPED_PROFILE_SOURCES
        .iter()
        .map(|(name, source)| profile::load(source).map_err(|error| (*name, error)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shipped_profile_validates_and_is_distinct() {
        let profiles = shipped().expect("shipped profiles must validate");
        assert_eq!(profiles.len(), SHIPPED_PROFILE_SOURCES.len());
        let mut references = profiles
            .iter()
            .map(|profile| format!("{}@{}", profile.id, profile.version))
            .collect::<Vec<_>>();
        references.sort();
        references.dedup();
        assert_eq!(references.len(), profiles.len());
    }
}
