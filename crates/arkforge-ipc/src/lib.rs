//! # arkforge-ipc
//!
//! Versioned local IPC: framing, session kinds and the message codec for
//! `proto/arkforge.proto`.
//!
//! architecture.md 15.2. Two properties matter more than the encoding:
//!
//! - the public socket cannot carry `startExecution`, and that is enforced in
//!   [`SessionKind::may_call`] rather than in a handler's first `if`;
//! - an unknown enum value fails closed (see `wire`).
//!
//! Windows named pipes are a design reservation and out of AF-V1/AF-V2
//! acceptance (architecture.md 15.2); the framing here is transport-agnostic
//! and the daemon binds a Unix domain socket.

#![forbid(unsafe_code)]

pub mod framing;
pub mod messages;
pub mod wire;

use core::fmt;

/// Protocol version. Major must match; a peer with a higher minor is accepted
/// and its unknown fields are skipped (IPC-001).
pub const PROTOCOL_MAJOR: u32 = 1;
pub const PROTOCOL_MINOR: u32 = 0;

/// Which socket a peer connected on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SessionKind {
    /// Read-only. Never carries an execute-side call.
    Public,
    /// Established with the handle/secret the authority passed at daemon start.
    Controller,
}

impl SessionKind {
    pub fn wire_value(self) -> i32 {
        match self {
            SessionKind::Public => 1,
            SessionKind::Controller => 2,
        }
    }

    pub fn from_wire(value: i32) -> Option<Self> {
        match value {
            1 => Some(SessionKind::Public),
            2 => Some(SessionKind::Controller),
            _ => None,
        }
    }

    /// Whether this session may call `api` at all.
    ///
    /// This is the socket split from architecture.md 15.2/15.3, stated once.
    /// It is about *who is asking*; whether the daemon implements the call is a
    /// separate question, answered by the execution gate.
    pub fn may_call(self, api: Api) -> bool {
        match api {
            Api::InspectArtifact | Api::DiscoverDevices | Api::ProbeDevice => true,
            // A public caller may ask for an assessment; the daemon returns an
            // assessment to it regardless of what the provider could build.
            Api::MaterializePlan => true,
            Api::WatchJob | Api::GetRecoveryGuide | Api::GetJob | Api::ListJobs => true,
            // Answering an admission is minting authority. A public caller
            // that could submit a permit would be an authority nobody paired.
            Api::ImportArtifact
            | Api::StartExecution
            | Api::CancelJob
            | Api::ReconcileJob
            | Api::PlanSupersedingRecovery
            | Api::SubmitStepPermit
            | Api::SubmitManagedControlReceipt => self == SessionKind::Controller,
        }
    }
}

/// The API surface (architecture.md 15.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Api {
    ImportArtifact,
    InspectArtifact,
    DiscoverDevices,
    ProbeDevice,
    MaterializePlan,
    StartExecution,
    WatchJob,
    CancelJob,
    ReconcileJob,
    PlanSupersedingRecovery,
    GetRecoveryGuide,
    /// The authority answers an admission the daemon asked for on the
    /// `watchJob` stream.
    SubmitStepPermit,
    /// The authority reports what its own device control channel observed.
    SubmitManagedControlReceipt,
    /// Read-only durable point-in-time job status.
    GetJob,
    /// Read-only durable status for every known job.
    ListJobs,
}

impl Api {
    pub const ALL: [Api; 15] = [
        Api::ImportArtifact,
        Api::InspectArtifact,
        Api::DiscoverDevices,
        Api::ProbeDevice,
        Api::MaterializePlan,
        Api::StartExecution,
        Api::WatchJob,
        Api::CancelJob,
        Api::ReconcileJob,
        Api::PlanSupersedingRecovery,
        Api::GetRecoveryGuide,
        Api::SubmitStepPermit,
        Api::SubmitManagedControlReceipt,
        Api::GetJob,
        Api::ListJobs,
    ];

    pub fn wire_value(self) -> i32 {
        match self {
            Api::ImportArtifact => 1,
            Api::InspectArtifact => 2,
            Api::DiscoverDevices => 3,
            Api::ProbeDevice => 4,
            Api::MaterializePlan => 5,
            Api::StartExecution => 6,
            Api::WatchJob => 7,
            Api::CancelJob => 8,
            Api::ReconcileJob => 9,
            Api::PlanSupersedingRecovery => 10,
            Api::GetRecoveryGuide => 11,
            Api::SubmitStepPermit => 12,
            Api::SubmitManagedControlReceipt => 13,
            Api::GetJob => 14,
            Api::ListJobs => 15,
        }
    }

    pub fn from_wire(value: i32) -> Option<Self> {
        Api::ALL.into_iter().find(|api| api.wire_value() == value)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Api::ImportArtifact => "importArtifact",
            Api::InspectArtifact => "inspectArtifact",
            Api::DiscoverDevices => "discoverDevices",
            Api::ProbeDevice => "probeDevice",
            Api::MaterializePlan => "materializePlan",
            Api::StartExecution => "startExecution",
            Api::WatchJob => "watchJob",
            Api::CancelJob => "cancelJob",
            Api::ReconcileJob => "reconcileJob",
            Api::PlanSupersedingRecovery => "planSupersedingRecovery",
            Api::GetRecoveryGuide => "getRecoveryGuide",
            Api::SubmitStepPermit => "submitStepPermit",
            Api::SubmitManagedControlReceipt => "submitManagedControlReceipt",
            Api::GetJob => "getJob",
            Api::ListJobs => "listJobs",
        }
    }
}

impl fmt::Display for Api {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Response status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Status {
    Ok,
    Refused,
    /// The capability does not exist in this build.
    Unavailable,
    InvalidArgument,
    NotFound,
    Internal,
}

impl Status {
    pub fn wire_value(self) -> i32 {
        match self {
            Status::Ok => 1,
            Status::Refused => 2,
            Status::Unavailable => 3,
            Status::InvalidArgument => 4,
            Status::NotFound => 5,
            Status::Internal => 6,
        }
    }

    pub fn from_wire(value: i32) -> Option<Self> {
        match value {
            1 => Some(Status::Ok),
            2 => Some(Status::Refused),
            3 => Some(Status::Unavailable),
            4 => Some(Status::InvalidArgument),
            5 => Some(Status::NotFound),
            6 => Some(Status::Internal),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Refused => "refused",
            Status::Unavailable => "unavailable",
            Status::InvalidArgument => "invalidArgument",
            Status::NotFound => "notFound",
            Status::Internal => "internal",
        }
    }
}

/// Whether a peer's protocol version is acceptable.
pub fn negotiate(peer_major: u32, _peer_minor: u32) -> Result<(), String> {
    if peer_major != PROTOCOL_MAJOR {
        return Err(format!(
            "protocol major {peer_major} is not compatible with {PROTOCOL_MAJOR}"
        ));
    }
    // Minor differences are additive in both directions: a newer peer's extra
    // fields are skipped, an older peer simply omits them.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_public_socket_cannot_start_execution() {
        assert!(!SessionKind::Public.may_call(Api::StartExecution));
        assert!(SessionKind::Controller.may_call(Api::StartExecution));
    }

    #[test]
    fn the_public_socket_cannot_import_or_cancel_or_reconcile() {
        for api in [
            Api::ImportArtifact,
            Api::CancelJob,
            Api::ReconcileJob,
            Api::PlanSupersedingRecovery,
        ] {
            assert!(!SessionKind::Public.may_call(api), "{api}");
            assert!(SessionKind::Controller.may_call(api), "{api}");
        }
    }

    #[test]
    fn read_only_calls_are_open_to_both_sockets() {
        for api in [
            Api::InspectArtifact,
            Api::DiscoverDevices,
            Api::ProbeDevice,
            Api::MaterializePlan,
            Api::WatchJob,
            Api::GetRecoveryGuide,
        ] {
            assert!(SessionKind::Public.may_call(api), "{api}");
            assert!(SessionKind::Controller.may_call(api), "{api}");
        }
    }

    #[test]
    fn every_api_round_trips_through_its_wire_value() {
        for api in Api::ALL {
            assert_eq!(Api::from_wire(api.wire_value()), Some(api));
        }
        assert_eq!(Api::from_wire(0), None);
        assert_eq!(Api::from_wire(99), None);
    }

    #[test]
    fn version_negotiation_pins_the_major() {
        assert!(negotiate(PROTOCOL_MAJOR, PROTOCOL_MINOR).is_ok());
        assert!(negotiate(PROTOCOL_MAJOR, PROTOCOL_MINOR + 5).is_ok());
        assert!(negotiate(PROTOCOL_MAJOR + 1, 0).is_err());
    }
}
