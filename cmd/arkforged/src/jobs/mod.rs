//! The controller execution/admission surface.
//!
//! architecture.md 8, 13, 15.3. This is the half of execution that faces the
//! authority: a job is created, it asks for a permit, the authority answers,
//! the intent becomes durable, and — for the steps the authority itself
//! performs — it reports back what its own control channel observed.
//!
//! # The shape, and why it is this shape
//!
//! The daemon never calls out. It publishes what it needs on the `watchJob`
//! stream and waits for the authority to call back in. Every message stays
//! client-initiated, which means the authority is free to answer, to refuse, or
//! to stop — and those are three different things to a job, not one.
//!
//! Nothing here blocks. A job advances only when a request arrives, so the
//! service lock is never held across anything slow. That matters because the
//! daemon serves every connection under one mutex: a handler that waited for a
//! device would stop the event stream that was supposed to report on it.
//!
//! # Where dispatch is
//!
//! Next door, in [`crate::dispatch`], and deliberately not here. A step whose
//! private action this daemon runs itself becomes a [`PendingDispatch`] that a
//! dispatcher **takes** — the work leaves the service lock before it runs,
//! comes back through [`JobRegistry::complete_dispatch`], and the lock is held
//! only for the two short journal writes at either end.
//!
//! Between taking the work and reporting on it, the job holds it `in_flight`
//! and nothing else may hand it out. Whether the device changed in that window
//! is unknown, which is exactly what the durable intent already records.

use arkforge_authority_api::{
    ControllerPairingSecret, CurrentFacts, DispatchIntent, FreshnessVerdict, PairingEpoch,
    PermitIntegrityTag, PermitVerificationError, StepAdmissionSnapshot as AuthoritySnapshot,
    StepPermit, evaluate_freshness,
};
use arkforge_core::PersistentEffect;
use arkforge_core::Sha256Digest;
use arkforge_core::digest::{CanonicalCbor, sha256};
use arkforge_core::ids::{AttemptId, ControllerSessionId, JobId, OpaqueId, PlanId, StepId};
use arkforge_core::outcome::ActionDisposition;
use arkforge_core::plan::FlashPlanEnvelope;
use arkforge_core::profile::DeviceProfile;
use arkforge_core::projection::{PrivateActionRecord, PrivateActionRole, StoredProviderPlan};
use arkforge_core::verification::VerificationOutcome;
use arkforge_engine::JobState;
use arkforge_engine::durable::{DurableJournal, DurableJournalError};
use arkforge_engine::journal::{JournalRecord, JournalRecordKind};
use arkforge_engine::recovery::{CrashDisposition, fact};
use arkforge_engine::step::{
    DispatchInFlight, StepError, admit_step, begin_dispatch, begin_dispatch_with_facts, checkpoint,
    record_receipt_with_facts, record_transport_evidence,
};
use arkforge_engine::superseding::{EffectObservation, ReconcileVerdict, reconcile};
use arkforge_ipc::messages::{
    ActionReceiptSummary, JobEvent, JobEventKind, KeyValue, ManagedControlAction,
    ManagedControlRequest, StepAdmissionSnapshot, SubmitManagedControlReceiptRequest,
};
use arkforge_provider::rockchip_execute::StoredAction;
use arkforge_transport::{DeviceObservation, SerialEvidence};
use core::fmt;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[path = "parts/helpers_02_digest_from.rs"]
mod digest_from;
#[path = "parts/helpers_04_code.rs"]
mod error_code;
#[path = "parts/helpers_01_finish_queued_cancellation.rs"]
mod finish_queued_cancellation;
#[path = "parts/preamble.rs"]
mod preamble;
#[path = "parts/helpers_03_recover_job.rs"]
mod recover_job;
#[path = "parts/registry_admission.rs"]
mod registry_admission;
#[path = "parts/registry_dispatch.rs"]
mod registry_dispatch;
#[path = "parts/registry_open.rs"]
mod registry_open;
#[path = "parts/registry_reconcile.rs"]
mod registry_reconcile;
#[cfg(test)]
#[path = "parts/tests.rs"]
mod tests;

use digest_from::*;
pub use error_code::*;
pub use finish_queued_cancellation::*;
pub use preamble::*;
pub use recover_job::*;
