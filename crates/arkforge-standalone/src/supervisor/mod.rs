//! Persistent local CLI authority supervisor.
//!
//! The supervisor, not a short-lived command, owns the pairing secret. It
//! passes the secret to `arkforged` over an anonymous stdin pipe and exposes an
//! owner-only local control socket containing no secret-bearing operation.

mod recovery;
mod restoration;
mod verification;
pub use verification::FlashVerificationResult;

include!("parts/startup.rs");
include!("parts/02_start_with_launcher.rs");
include!("parts/03_serve.rs");
include!("parts/04_handle_materialize_for.rs");
include!("parts/05_job_lineage_path.rs");
include!("parts/06_submit_control_refusal.rs");
include!("parts/07_append_stable_observation.rs");
include!("parts/08_spawn_daemon.rs");
include!("parts/09_archive_requests_keep_the_materialization_budget.rs");
