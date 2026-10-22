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

include!("parts/preamble.rs");
include!("parts/registry_open.rs");
include!("parts/registry_admission.rs");
include!("parts/registry_reconcile.rs");
include!("parts/registry_dispatch.rs");
include!("parts/helpers_01_finish_queued_cancellation.rs");
include!("parts/helpers_02_digest_from.rs");
include!("parts/helpers_03_recover_job.rs");
include!("parts/helpers_04_code.rs");
include!("parts/tests.rs");
