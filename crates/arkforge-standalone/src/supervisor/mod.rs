//! Persistent local CLI authority supervisor.
//!
//! The supervisor, not a short-lived command, owns the pairing secret. It
//! passes the secret to `arkforged` over an anonymous stdin pipe and exposes an
//! owner-only local control socket containing no secret-bearing operation.

use crate::StandaloneError;
use crate::authority_support::{self, AuthoritySupportKey};
use crate::hdc_control::{ControlContext, HdcControlPort};
use arkforge_authority_api::authority_side::mint_integrity_tag;
use arkforge_authority_api::{
    ControllerPairingSecret, PairingEpoch, PermitIntegrityTag, StepPermit,
};
use arkforge_client::{ControllerClient, MaterializeInput};
use arkforge_client::{DeviceObservationView, DeviceProbeView, PublicClient, PublicRuntimeInfo};
use arkforge_core::Sha256Digest;
use arkforge_core::authority::{AuthorityBindingRef, AuthorityNamespace, AuthoritySupportState};
use arkforge_core::digest::sha256;
use arkforge_core::ids::{
    AttemptId, ControllerSessionId, JobId, OpaqueId, PermitId, PlanId, StepId,
};
use arkforge_ipc::framing::{read_frame, write_frame};
use arkforge_ipc::messages::{
    ExecutablePlan, JobEventKind, MaterializePlanResponse, StepAdmissionSnapshot,
    SubmitManagedControlReceiptRequest, SubmitStepPermitRequest,
};
use arkforge_ipc::wire;
use arkforge_platform::{
    LocalChannel, LocalEndpoint, LocalListener, LocalStream, detach_standard_handles, fill_random,
    protect_path, replace_file, sync_directory, unix_socket_path,
};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

mod recovery;
mod restoration;
mod verification;
pub use verification::FlashVerificationResult;

#[path = "parts/07_append_stable_observation.rs"]
mod append_stable_observation;
#[path = "parts/04_handle_materialize_for.rs"]
mod handle_materialize_for;
#[path = "parts/05_job_lineage_path.rs"]
mod job_lineage_path;
#[path = "parts/03_serve.rs"]
mod serve;
#[path = "parts/08_spawn_daemon.rs"]
mod spawn_daemon;
#[path = "parts/02_start_with_launcher.rs"]
mod start_with_launcher;
#[path = "parts/startup.rs"]
mod startup;
#[path = "parts/06_submit_control_refusal.rs"]
mod submit_control_refusal;
#[cfg(test)]
#[path = "parts/09_archive_requests_keep_the_materialization_budget.rs"]
mod tests;

pub use append_stable_observation::*;
pub use handle_materialize_for::*;
use job_lineage_path::*;
use serve::*;
use spawn_daemon::*;
pub use start_with_launcher::*;
pub use startup::*;
use submit_control_refusal::*;
