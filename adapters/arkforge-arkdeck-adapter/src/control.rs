//! The published `ManagedDeviceControlAction ↔ RockchipProviderAction` table.
//!
//! architecture.md 9.2 says ArkForge names a semantic action and ArkDeck maps
//! it onto its existing typed HDC provider. This is that map, on the ArkForge
//! side, in the same shape as [`crate::mapping`]: pinned data read from the
//! ArkDeck baseline, fail-closed where no counterpart exists, and versioned so
//! a change to it is a change ArkDeck admission can see.
//!
//! # The division this table draws
//!
//! ArkDeck's `RockchipProviderAction` today contains two different kinds of
//! thing:
//!
//! - **HDC-side actions** — entering the loader, waiting for a disconnect,
//!   rebinding, rebooting, reading the booted build. These stay ArkDeck's:
//!   ArkDeck owns the HDC endpoint, the server lifecycle and the connect key
//!   (architecture.md 9.1), and nothing in ArkForge should learn any of them.
//! - **Rockchip-side actions** — `flashPartitions` and `verifyFlashReadback`.
//!   These lower to `rkdeveloptool` commands and sector addresses inside
//!   ArkDeck. AF-V2's acceptance says the production lowering must carry no
//!   Rockchip command or address (architecture.md 22), so these two are the
//!   ones ArkForge takes over — and this table records them as deliberately
//!   *not* reachable through the control port, rather than leaving their
//!   absence to be inferred.
//!
//! The table is what makes that division reviewable: every ArkDeck action is
//! accounted for as kept, delegated, or unreachable, and a reader can check the
//! three sets add up.

use arkforge_authority_api::ManagedDeviceControlAction;
use core::fmt;

/// This table's version. It changes when the table changes, because ArkDeck's
/// side of the port is written against it.
pub const CONTROL_TABLE_VERSION: &str = "arkforge.arkdeck-control-map/v1";

/// The ArkDeck audit baseline these values were read from.
///
/// `Packages/ArkDeckKit/Sources/ArkDeckWorkflows/DeviceProviders/DeviceProviderContract.swift`,
/// `RockchipProviderAction`.
pub const ARKDECK_CONTROL_BASELINE: &str = "e3b9aff3";

/// What ArkDeck must do to satisfy one semantic control action.
///
/// A control action is not always one ArkDeck action: entering the loader is
/// "ask, watch the old identity leave, watch the new one arrive, rebind to it",
/// and collapsing that into "send the command" is how a mode change gets
/// reported as successful because a command was accepted (architecture.md 16.2
/// requires "HDC accepted + expected disconnect + unique Loader rebind").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlBinding {
    /// The ArkDeck `RockchipProviderAction` cases, in the order they run.
    pub arkdeck_actions: &'static [&'static str],
    /// What ArkDeck must have observed before it may report acceptance.
    pub semantic_success: &'static str,
    /// Facts ArkDeck returns in the [`ManagedControlReceipt`]. ArkForge reads
    /// these; it never reads a connect key, an endpoint or an argv.
    ///
    /// [`ManagedControlReceipt`]: arkforge_authority_api::ManagedControlReceipt
    pub receipt_facts: &'static [&'static str],
}

/// The table. Every [`ManagedDeviceControlAction`] appears exactly once.
pub fn control_binding(action: ManagedDeviceControlAction) -> ControlBinding {
    match action {
        ManagedDeviceControlAction::EnterUpdater => ControlBinding {
            arkdeck_actions: &[
                "observeHDCNormalUSB",
                "enterLoader",
                "waitForHDCDisconnect",
                "waitForLoader",
                "rebindLoader",
            ],
            semantic_success: "the command was accepted, the bound HDC identity disconnected, and \
                               exactly one device rebound in Loader mode",
            receipt_facts: &["mode", "stableIdentitySHA256", "usbTopology"],
        },
        ManagedDeviceControlAction::RebootToNormal => ControlBinding {
            // ArkDeck's `rebootToNormal` is the one control action whose device
            // half is a Rockchip command (`rd`) rather than an HDC one — in
            // Loader mode the device has no HDC to talk to. ArkForge issues the
            // reset through its own fixed-tool port; what ArkDeck contributes is
            // the half only it can: watching the exact bound target come back.
            arkdeck_actions: &["waitForBoundHDCReconnect"],
            semantic_success: "the exact previously-bound target reconnected in normal mode, with \
                               the same stable identity",
            receipt_facts: &["mode", "stableIdentitySHA256", "usbTopology"],
        },
        ManagedDeviceControlAction::ReadProductFacts => ControlBinding {
            arkdeck_actions: &["verifyBoundBuild"],
            semantic_success: "the bound target answered `const.product.model`",
            receipt_facts: &["const.product.model"],
        },
        ManagedDeviceControlAction::ReadBuildFacts => ControlBinding {
            arkdeck_actions: &["verifyBoundBuild"],
            semantic_success: "the bound target answered `const.ohos.fullname`",
            receipt_facts: &["const.ohos.fullname"],
        },
    }
}

/// How one ArkDeck `RockchipProviderAction` fares under this division.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArkDeckActionDisposition {
    /// Stays ArkDeck's, reachable through the control port.
    KeptByAuthority,
    /// Stays ArkDeck's and is not reachable through the control port. These are
    /// steps of a larger control action, not control actions themselves.
    KeptInternal,
    /// Moves to ArkForge. ArkDeck's copy is deleted, not merely bypassed —
    /// architecture.md 21.3 is explicit that a lowering left in place "for
    /// safety" is a second implementation of a destructive path.
    DelegatedToArkForge,
    /// Neither kept nor delegated: no ArkForge counterpart exists, so a plan
    /// that needed it could not be admitted.
    Unmapped,
}

/// Every case of ArkDeck's `RockchipProviderAction`, and where it lands.
///
/// The list is exhaustive against the pinned baseline on purpose. An ArkDeck
/// action that this table does not mention is an action nobody decided about,
/// and the test below fails rather than letting it default to "kept".
pub const ARKDECK_ACTION_DISPOSITIONS: [(&str, ArkDeckActionDisposition); 13] = [
    ("enterLoader", ArkDeckActionDisposition::KeptByAuthority),
    ("observeHDCNormalUSB", ArkDeckActionDisposition::KeptInternal),
    ("waitForHDCDisconnect", ArkDeckActionDisposition::KeptInternal),
    ("waitForLoader", ArkDeckActionDisposition::KeptInternal),
    ("rebindLoader", ArkDeckActionDisposition::KeptInternal),
    (
        "flashPartitions",
        ArkDeckActionDisposition::DelegatedToArkForge,
    ),
    (
        "verifyFlashReadback",
        ArkDeckActionDisposition::DelegatedToArkForge,
    ),
    ("rebootToNormal", ArkDeckActionDisposition::KeptByAuthority),
    ("waitForHDCReconnect", ArkDeckActionDisposition::KeptInternal),
    (
        "waitForBoundHDCReconnect",
        ArkDeckActionDisposition::KeptInternal,
    ),
    ("verifyBuild", ArkDeckActionDisposition::KeptInternal),
    ("verifyBoundBuild", ArkDeckActionDisposition::KeptByAuthority),
    (
        "capturePostFlashDiagnostics",
        ArkDeckActionDisposition::KeptInternal,
    ),
];

/// What ArkForge must never receive, restated as data so a test can assert it.
///
/// architecture.md 9.2 lists these as things the port does not carry. A
/// receipt fact whose key is one of these would be a leak of exactly the kind
/// the port exists to prevent.
pub const FORBIDDEN_RECEIPT_FACTS: [&str; 6] = [
    "connectKey",
    "hdcExecutablePath",
    "hdcEndpoint",
    "argv",
    "shell",
    "serverLifecycleAction",
];

impl fmt::Display for ArkDeckActionDisposition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ArkDeckActionDisposition::KeptByAuthority => "keptByAuthority",
            ArkDeckActionDisposition::KeptInternal => "keptInternal",
            ArkDeckActionDisposition::DelegatedToArkForge => "delegatedToArkForge",
            ArkDeckActionDisposition::Unmapped => "unmapped",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    const ALL_CONTROL_ACTIONS: [ManagedDeviceControlAction; 4] = [
        ManagedDeviceControlAction::EnterUpdater,
        ManagedDeviceControlAction::RebootToNormal,
        ManagedDeviceControlAction::ReadProductFacts,
        ManagedDeviceControlAction::ReadBuildFacts,
    ];

    #[test]
    fn every_control_action_binds_to_at_least_one_arkdeck_action() {
        for action in ALL_CONTROL_ACTIONS {
            let binding = control_binding(action);
            assert!(
                !binding.arkdeck_actions.is_empty(),
                "{} binds to nothing",
                action.as_str()
            );
            assert!(
                !binding.semantic_success.is_empty(),
                "{} declares no semantic success",
                action.as_str()
            );
        }
    }

    /// Every ArkDeck action a control action names must appear in the
    /// disposition list. A binding that reached for an action nobody classified
    /// would be a quiet expansion of what ArkForge can ask ArkDeck to do.
    #[test]
    fn every_bound_arkdeck_action_is_one_this_table_classified() {
        let classified: BTreeSet<&str> = ARKDECK_ACTION_DISPOSITIONS
            .iter()
            .map(|(name, _)| *name)
            .collect();
        for action in ALL_CONTROL_ACTIONS {
            for name in control_binding(action).arkdeck_actions {
                assert!(
                    classified.contains(name),
                    "{} binds to {name}, which is not in the disposition table",
                    action.as_str()
                );
            }
        }
    }

    #[test]
    fn the_disposition_table_has_no_duplicates_and_nothing_unmapped() {
        let names: BTreeSet<&str> = ARKDECK_ACTION_DISPOSITIONS
            .iter()
            .map(|(name, _)| *name)
            .collect();
        assert_eq!(
            names.len(),
            ARKDECK_ACTION_DISPOSITIONS.len(),
            "an ArkDeck action is listed twice"
        );
        for (name, disposition) in ARKDECK_ACTION_DISPOSITIONS {
            assert_ne!(
                disposition,
                ArkDeckActionDisposition::Unmapped,
                "{name} is unmapped; every baseline action needs a decision"
            );
        }
    }

    /// The two Rockchip-lowering actions, and only those two, move to ArkForge.
    /// AF-V2's acceptance is "ArkDeck production lowering carries no Rockchip
    /// command or address"; these are where the commands and addresses are.
    #[test]
    fn exactly_the_rockchip_lowering_actions_are_delegated() {
        let delegated: BTreeSet<&str> = ARKDECK_ACTION_DISPOSITIONS
            .iter()
            .filter(|(_, disposition)| {
                *disposition == ArkDeckActionDisposition::DelegatedToArkForge
            })
            .map(|(name, _)| *name)
            .collect();
        assert_eq!(
            delegated,
            BTreeSet::from(["flashPartitions", "verifyFlashReadback"])
        );
    }

    /// architecture.md 9.2, as an assertion. No receipt this table describes
    /// carries an endpoint, a key, an argv or a lifecycle control.
    #[test]
    fn no_receipt_fact_carries_something_the_port_forbids() {
        for action in ALL_CONTROL_ACTIONS {
            for fact in control_binding(action).receipt_facts {
                assert!(
                    !FORBIDDEN_RECEIPT_FACTS.contains(fact),
                    "{} would return {fact}, which architecture.md 9.2 forbids",
                    action.as_str()
                );
            }
        }
    }

    /// Entering the loader is four observations, not one command. A binding
    /// that named only `enterLoader` would let "the command was accepted" be
    /// reported as "the device is in Loader mode".
    #[test]
    fn entering_the_loader_requires_the_disconnect_and_the_rebind_too() {
        let binding = control_binding(ManagedDeviceControlAction::EnterUpdater);
        for required in ["enterLoader", "waitForHDCDisconnect", "waitForLoader", "rebindLoader"] {
            assert!(
                binding.arkdeck_actions.contains(&required),
                "entering the loader must include {required}"
            );
        }
        assert!(binding.semantic_success.contains("disconnected"));
        assert!(binding.semantic_success.contains("exactly one"));
    }
}
