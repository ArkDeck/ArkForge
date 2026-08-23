//! Permit consumption: the one path from an authority's permit to an effect.
//!
//! architecture.md 8.5 says a permit is single-use and 13.3 says "single" has
//! to hold across a crash. Both are enforced here, and enforced by types rather
//! than by discipline:
//!
//! ```text
//! admit_step   -> AdmittedStep     (permit verified, intent durable)
//! begin_dispatch(AdmittedStep)  -> DispatchInFlight
//! record_receipt(DispatchInFlight) -> CompletedStep
//! checkpoint(CompletedStep)     -> ()
//! ```
//!
//! Each token is consumed by value, so a caller cannot dispatch twice from one
//! admission, cannot record a receipt for a dispatch that never started, and
//! cannot checkpoint a step with no receipt. None of these types can be built
//! any other way — there is no public constructor — so the ordering is not
//! something a provider can be trusted to get right, it is something it cannot
//! get wrong.
//!
//! What this module deliberately does not do is decide *whether* a step should
//! run. That is the authority's, expressed as the permit it signs.

use crate::durable::{DurableJournal, DurableJournalError};
use crate::journal::JournalRecordKind;
use crate::recovery::{PermitDisposition, PermitLedger, fact};
use arkforge_authority_api::ControllerPairingSecret;
use arkforge_authority_api::{
    DispatchIntent, PermitVerificationError, StepPermit, VerifiedStepPermit, verify_permit,
};
use arkforge_core::Sha256Digest;
use arkforge_core::ids::OpaqueId;
use core::fmt;
use std::collections::BTreeSet;

/// A step whose permit verified and whose intent is on stable storage.
///
/// Holding one is the proof that architecture.md 13.3's "permit received,
/// StepIntent not durable → dispatch forbidden" window has been left.
#[derive(Debug)]
pub struct AdmittedStep {
    permit: VerifiedStepPermit,
    intent_digest: Sha256Digest,
}

impl AdmittedStep {
    pub fn permit(&self) -> &VerifiedStepPermit {
        &self.permit
    }

    /// The journal record that makes this step's intent durable. A provider
    /// includes it in the evidence it returns, so a receipt can be tied to the
    /// intent it answers rather than to a step id that may repeat.
    pub fn intent_digest(&self) -> Sha256Digest {
        self.intent_digest
    }

    pub fn private_action_digest(&self) -> Sha256Digest {
        self.permit.private_action_digest()
    }
}

/// A dispatch that has started and whose outcome is not yet recorded.
///
/// While one of these exists, the honest answer to "did the device change?" is
/// "unknown". Dropping it without recording a receipt leaves exactly that in
/// the journal, which is the correct record of a process that died mid-step.
#[derive(Debug)]
pub struct DispatchInFlight {
    context: StepContext,
    intent_digest: Sha256Digest,
    dispatch_digest: Sha256Digest,
}

impl DispatchInFlight {
    pub fn job_id(&self) -> &str {
        &self.context.job_id
    }

    pub fn plan_id(&self) -> &str {
        &self.context.plan_id
    }

    pub fn step_id(&self) -> &str {
        &self.context.step_id
    }

    pub fn attempt_id(&self) -> &str {
        &self.context.attempt_id
    }

    pub fn permit_id(&self) -> &str {
        &self.context.permit_id
    }

    pub fn intent_digest(&self) -> Sha256Digest {
        self.intent_digest
    }

    /// The `externalDispatchStarted` record written immediately before this
    /// token was issued.
    pub fn dispatch_digest(&self) -> Sha256Digest {
        self.dispatch_digest
    }
}

/// A step with a durable semantic receipt, not yet checkpointed.
#[derive(Debug)]
pub struct CompletedStep {
    context: StepContext,
    receipt_digest: Sha256Digest,
    receipt_record_digest: Sha256Digest,
}

impl CompletedStep {
    pub fn receipt_digest(&self) -> Sha256Digest {
        self.receipt_digest
    }

    pub fn permit_id(&self) -> &str {
        &self.context.permit_id
    }

    /// Digest of the durable `semanticReceiptRecorded` journal record.
    pub fn receipt_record_digest(&self) -> Sha256Digest {
        self.receipt_record_digest
    }
}

#[derive(Debug)]
struct StepContext {
    job_id: String,
    plan_id: String,
    step_id: String,
    attempt_id: String,
    permit_id: String,
}

impl StepContext {
    fn from_permit(permit: &VerifiedStepPermit) -> Self {
        let permit = permit.permit();
        Self {
            job_id: permit.job_id.as_str().to_string(),
            plan_id: permit.plan_id.as_str().to_string(),
            step_id: permit.step_id.as_str().to_string(),
            attempt_id: permit.attempt_id.as_str().to_string(),
            permit_id: permit.permit_id.as_str().to_string(),
        }
    }

    fn facts(&self) -> Result<Vec<(OpaqueId, String)>, StepError> {
        Ok(vec![
            (key(fact::JOB_ID)?, self.job_id.clone()),
            (key(fact::PLAN_ID)?, self.plan_id.clone()),
            (key(fact::STEP_ID)?, self.step_id.clone()),
            (key(fact::ATTEMPT_ID)?, self.attempt_id.clone()),
            (key(fact::PERMIT_ID)?, self.permit_id.clone()),
        ])
    }

    fn facts_with_receipt(
        &self,
        receipt_digest: Sha256Digest,
    ) -> Result<Vec<(OpaqueId, String)>, StepError> {
        let mut facts = self.facts()?;
        facts.push((key(fact::RECEIPT_DIGEST)?, receipt_digest.to_string()));
        Ok(facts)
    }
}

/// Verifies a permit against the durable ledger and makes its intent durable.
///
/// The `already_consumed` argument that [`verify_permit`] takes is answered
/// from the journal, not from memory: a daemon that restarted has no memory,
/// and a permit that was consumed before the restart must still be refused.
pub fn admit_step(
    journal: &mut DurableJournal,
    permit: &StepPermit,
    secret: &ControllerPairingSecret,
    intent: &DispatchIntent,
) -> Result<AdmittedStep, StepError> {
    let permit_id = permit.permit_id.as_str().to_string();
    let ledger = PermitLedger::from_journal(journal.journal());

    match ledger.disposition(&permit_id) {
        PermitDisposition::Unseen => {}
        PermitDisposition::Consumed { receipt_digest } => {
            return Err(StepError::AlreadyConsumed {
                permit_id,
                receipt_digest,
            });
        }
        PermitDisposition::ConsumingOutcomeUnknown => {
            return Err(StepError::OutcomeUnknown { permit_id });
        }
        PermitDisposition::IntentDurable | PermitDisposition::AcceptedIntentNotDurable => {
            // architecture.md 13.3: an intent already exists for this permit
            // id. Re-admitting would create a second intent for one permit,
            // which is the thing the row forbids by name.
            return Err(StepError::IntentAlreadyRecorded { permit_id });
        }
    }

    let verified = verify_permit(permit, secret, intent, false).map_err(StepError::Verification)?;

    let context = StepContext::from_permit(&verified);

    journal.append(
        JournalRecordKind::StepPermitAccepted,
        intent.now_epoch_ms,
        1,
        subject(&context.step_id)?,
        context.facts()?,
    )?;

    // After this call returns, the intent is on stable storage. Nothing before
    // this line may touch the device; everything after it must assume the
    // device may have been touched.
    let intent_digest = journal.append(
        JournalRecordKind::StepIntentRecorded,
        intent.now_epoch_ms,
        1,
        subject(&context.step_id)?,
        context.facts()?,
    )?;

    Ok(AdmittedStep {
        permit: verified,
        intent_digest,
    })
}

/// Marks the permit as being consumed and the dispatch as started.
///
/// Takes the admission by value: one admission, one dispatch.
pub fn begin_dispatch(
    journal: &mut DurableJournal,
    step: AdmittedStep,
    now_epoch_ms: u64,
) -> Result<DispatchInFlight, StepError> {
    begin_dispatch_with_facts(journal, step, now_epoch_ms, Vec::new())
}

/// Marks dispatch as started and attaches operation-specific, non-authority
/// facts to both durable boundary records.
///
/// The engine always supplies the identity facts itself. Callers may add facts
/// such as a managed-control request id, but cannot omit the job/plan/step/
/// attempt/permit correlation used during recovery.
pub fn begin_dispatch_with_facts(
    journal: &mut DurableJournal,
    step: AdmittedStep,
    now_epoch_ms: u64,
    extra_facts: Vec<(String, String)>,
) -> Result<DispatchInFlight, StepError> {
    let context = StepContext::from_permit(&step.permit);
    let mut facts = context.facts()?;
    extend_unique_facts(&mut facts, extra_facts)?;

    journal.append(
        JournalRecordKind::PermitConsuming,
        now_epoch_ms,
        1,
        subject(&context.step_id)?,
        facts.clone(),
    )?;
    let dispatch_digest = journal.append(
        JournalRecordKind::ExternalDispatchStarted,
        now_epoch_ms,
        1,
        subject(&context.step_id)?,
        facts,
    )?;

    Ok(DispatchInFlight {
        context,
        intent_digest: step.intent_digest,
        dispatch_digest,
    })
}

/// Records transport evidence for a dispatch that is still in flight.
///
/// Buffered, and deliberately so: transport evidence is detail for an operator
/// reading the record afterwards. No recovery decision reads it, so losing it
/// in a crash costs nothing that matters.
pub fn record_transport_evidence(
    journal: &mut DurableJournal,
    in_flight: &DispatchInFlight,
    now_epoch_ms: u64,
    facts: Vec<(String, String)>,
) -> Result<(), StepError> {
    let mut entries = in_flight.context.facts()?;
    extend_unique_facts(&mut entries, facts)?;
    journal.append(
        JournalRecordKind::TransportEvidenceRecorded,
        now_epoch_ms,
        1,
        subject(&in_flight.context.step_id)?,
        entries,
    )?;
    Ok(())
}

/// Records the semantic receipt and consumes the permit.
pub fn record_receipt(
    journal: &mut DurableJournal,
    in_flight: DispatchInFlight,
    receipt_digest: Sha256Digest,
    now_epoch_ms: u64,
) -> Result<CompletedStep, StepError> {
    record_receipt_with_facts(journal, in_flight, receipt_digest, Vec::new(), now_epoch_ms)
}

/// Records a semantic receipt with durable reconstruction metadata.
///
/// Extra facts belong only to the semantic receipt record. The following
/// consumption and checkpoint markers remain compact and refer to it by its
/// canonical digest. Engine-owned correlation keys cannot be shadowed.
pub fn record_receipt_with_facts(
    journal: &mut DurableJournal,
    in_flight: DispatchInFlight,
    receipt_digest: Sha256Digest,
    extra_facts: Vec<(String, String)>,
    now_epoch_ms: u64,
) -> Result<CompletedStep, StepError> {
    let mut semantic_facts = in_flight.context.facts_with_receipt(receipt_digest)?;
    extend_unique_facts(&mut semantic_facts, extra_facts)?;
    let receipt_record_digest = journal.append(
        JournalRecordKind::SemanticReceiptRecorded,
        now_epoch_ms,
        1,
        subject(&in_flight.context.step_id)?,
        semantic_facts,
    )?;
    journal.append(
        JournalRecordKind::PermitConsumed,
        now_epoch_ms,
        1,
        subject(&in_flight.context.step_id)?,
        in_flight.context.facts_with_receipt(receipt_digest)?,
    )?;

    Ok(CompletedStep {
        context: in_flight.context,
        receipt_digest,
        receipt_record_digest,
    })
}

/// Checkpoints a completed step.
pub fn checkpoint(
    journal: &mut DurableJournal,
    completed: CompletedStep,
    now_epoch_ms: u64,
) -> Result<Sha256Digest, StepError> {
    let digest = journal.append(
        JournalRecordKind::StepCheckpointed,
        now_epoch_ms,
        1,
        subject(&completed.context.step_id)?,
        completed
            .context
            .facts_with_receipt(completed.receipt_digest)?,
    )?;
    Ok(digest)
}

fn subject(value: &str) -> Result<OpaqueId, StepError> {
    OpaqueId::new(value).map_err(|_| StepError::UnusableIdentifier(value.to_string()))
}

fn key(value: &str) -> Result<OpaqueId, StepError> {
    OpaqueId::new(value).map_err(|_| StepError::UnusableIdentifier(value.to_string()))
}

fn extend_unique_facts(
    facts: &mut Vec<(OpaqueId, String)>,
    extra: Vec<(String, String)>,
) -> Result<(), StepError> {
    let mut seen = facts
        .iter()
        .map(|(name, _)| name.as_str().to_string())
        .collect::<BTreeSet<_>>();
    for (name, value) in extra {
        let name = key(&name)?;
        if !seen.insert(name.as_str().to_string()) {
            return Err(StepError::DuplicateFactKey(name.to_string()));
        }
        facts.push((name, value));
    }
    Ok(())
}

#[derive(Debug)]
pub enum StepError {
    /// The permit was consumed. architecture.md 8.5: return the original
    /// receipt, do not dispatch again.
    AlreadyConsumed {
        permit_id: String,
        receipt_digest: String,
    },
    /// Consumption started and never finished. Reconcile; never replay.
    OutcomeUnknown {
        permit_id: String,
    },
    /// An intent already exists for this permit id.
    IntentAlreadyRecorded {
        permit_id: String,
    },
    Verification(PermitVerificationError),
    Journal(DurableJournalError),
    UnusableIdentifier(String),
    DuplicateFactKey(String),
}

impl fmt::Display for StepError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StepError::AlreadyConsumed {
                permit_id,
                receipt_digest,
            } => write!(
                f,
                "permit {permit_id} was already consumed; return receipt {receipt_digest} rather \
                 than dispatching again"
            ),
            StepError::OutcomeUnknown { permit_id } => write!(
                f,
                "permit {permit_id} was being consumed when the process stopped; the outcome is \
                 unknown and must be reconciled, never replayed"
            ),
            StepError::IntentAlreadyRecorded { permit_id } => write!(
                f,
                "permit {permit_id} already has a durable step intent; a second intent must not \
                 be created for one permit"
            ),
            StepError::Verification(error) => write!(f, "{error}"),
            StepError::Journal(error) => write!(f, "{error}"),
            StepError::UnusableIdentifier(value) => {
                write!(f, "{value:?} is not usable as a journal identifier")
            }
            StepError::DuplicateFactKey(value) => {
                write!(
                    f,
                    "journal fact key {value:?} duplicates a reserved or prior key"
                )
            }
        }
    }
}

impl std::error::Error for StepError {}

impl From<DurableJournalError> for StepError {
    fn from(error: DurableJournalError) -> Self {
        StepError::Journal(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arkforge_authority_api::authority_side::mint_integrity_tag;
    use arkforge_authority_api::{PairingEpoch, PermitIntegrityTag};
    use arkforge_core::digest::sha256;
    use arkforge_core::ids::{AttemptId, ControllerSessionId, JobId, PermitId, PlanId, StepId};
    use arkforge_core::{AuthorityBindingRef, AuthorityNamespace};

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("arkforge-step-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }

        fn journal(&self) -> DurableJournal {
            DurableJournal::open(self.0.join("journal.cbor")).unwrap().0
        }

        fn reopen(&self) -> DurableJournal {
            DurableJournal::open(self.0.join("journal.cbor")).unwrap().0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn secret() -> ControllerPairingSecret {
        ControllerPairingSecret::new(PairingEpoch(7), b"pairing-secret".to_vec())
    }

    fn permit(secret: &ControllerPairingSecret) -> StepPermit {
        let mut permit = StepPermit {
            permit_id: PermitId::new("PERMIT-1").unwrap(),
            authority_namespace: AuthorityNamespace::new("test-authority").unwrap(),
            controller_session_id: ControllerSessionId::new("SESSION-1").unwrap(),
            job_id: JobId::new("JOB-1").unwrap(),
            plan_id: PlanId::new("PLAN-1").unwrap(),
            plan_digest: sha256(b"plan"),
            step_id: StepId::new("STEP-1").unwrap(),
            attempt_id: AttemptId::new("ATTEMPT-1").unwrap(),
            public_step_digest: sha256(b"public"),
            private_action_digest: sha256(b"action"),
            effect_set_digest: sha256(b"effects"),
            authority_binding: AuthorityBindingRef {
                authority_namespace: AuthorityNamespace::new("test-authority").unwrap(),
                binding_id: OpaqueId::new("BINDING-1").unwrap(),
                binding_revision: 1,
                stable_identity_digest: sha256(b"identity"),
            },
            admitted_device_facts_digest: sha256(b"facts"),
            issued_at_epoch_ms: 1_000,
            expires_at_epoch_ms: 100_000,
            single_use: true,
            integrity_tag: PermitIntegrityTag {
                epoch: PairingEpoch(7),
                tag: sha256(b""),
            },
        };
        permit.integrity_tag = mint_integrity_tag(&permit, secret).unwrap();
        permit
    }

    fn intent() -> DispatchIntent {
        let permit = permit(&secret());
        DispatchIntent {
            controller_session_id: permit.controller_session_id,
            job_id: permit.job_id,
            plan_id: permit.plan_id,
            plan_digest: sha256(b"plan"),
            step_id: permit.step_id,
            attempt_id: permit.attempt_id,
            public_step_digest: permit.public_step_digest,
            private_action_digest: sha256(b"action"),
            effect_set_digest: permit.effect_set_digest,
            authority_binding: permit.authority_binding,
            admitted_device_facts_digest: permit.admitted_device_facts_digest,
            now_epoch_ms: 2_000,
        }
    }

    #[test]
    fn a_full_step_walks_the_journal_from_intent_to_checkpoint() {
        let dir = TempDir::new("full");
        let mut journal = dir.journal();
        let secret = secret();
        let permit = permit(&secret);

        let admitted = admit_step(&mut journal, &permit, &secret, &intent()).unwrap();
        let in_flight = begin_dispatch(&mut journal, admitted, 3_000).unwrap();
        record_transport_evidence(
            &mut journal,
            &in_flight,
            3_500,
            vec![("bytesWritten".into(), "4194304".into())],
        )
        .unwrap();
        let completed = record_receipt(&mut journal, in_flight, sha256(b"receipt"), 4_000).unwrap();
        checkpoint(&mut journal, completed, 5_000).unwrap();
        drop(journal);

        let reopened = dir.reopen();
        reopened.journal().verify().unwrap();
        let kinds: Vec<_> = reopened
            .journal()
            .records()
            .iter()
            .map(|record| record.kind)
            .collect();
        assert_eq!(
            kinds,
            vec![
                JournalRecordKind::StepPermitAccepted,
                JournalRecordKind::StepIntentRecorded,
                JournalRecordKind::PermitConsuming,
                JournalRecordKind::ExternalDispatchStarted,
                JournalRecordKind::TransportEvidenceRecorded,
                JournalRecordKind::SemanticReceiptRecorded,
                JournalRecordKind::PermitConsumed,
                JournalRecordKind::StepCheckpointed,
            ]
        );
        for record in reopened.journal().records() {
            for (name, expected) in [
                (fact::JOB_ID, "JOB-1"),
                (fact::PLAN_ID, "PLAN-1"),
                (fact::STEP_ID, "STEP-1"),
                (fact::ATTEMPT_ID, "ATTEMPT-1"),
                (fact::PERMIT_ID, "PERMIT-1"),
            ] {
                assert!(
                    record
                        .facts
                        .iter()
                        .any(|(key, value)| key.as_str() == name && value == expected),
                    "{} omitted {name}",
                    record.kind.as_str()
                );
            }
        }
    }

    /// The property AF-V2 exists for: a restarted daemon that is handed the
    /// same permit again refuses, from the journal alone.
    #[test]
    fn a_consumed_permit_is_refused_after_a_restart() {
        let dir = TempDir::new("restart");
        let secret = secret();
        let permit = permit(&secret);

        let mut journal = dir.journal();
        let admitted = admit_step(&mut journal, &permit, &secret, &intent()).unwrap();
        let in_flight = begin_dispatch(&mut journal, admitted, 3_000).unwrap();
        let completed = record_receipt(&mut journal, in_flight, sha256(b"receipt"), 4_000).unwrap();
        checkpoint(&mut journal, completed, 5_000).unwrap();
        drop(journal);

        let mut restarted = dir.reopen();
        match admit_step(&mut restarted, &permit, &secret, &intent()) {
            Err(StepError::AlreadyConsumed { permit_id, .. }) => {
                assert_eq!(permit_id, "PERMIT-1")
            }
            other => panic!("a consumed permit was not refused: {other:?}"),
        }
    }

    #[test]
    fn a_permit_caught_mid_dispatch_is_refused_as_unknown_rather_than_retried() {
        let dir = TempDir::new("midflight");
        let secret = secret();
        let permit = permit(&secret);

        let mut journal = dir.journal();
        let admitted = admit_step(&mut journal, &permit, &secret, &intent()).unwrap();
        // Dropping the in-flight token without a receipt is exactly what a
        // process death mid-dispatch leaves behind.
        let _in_flight = begin_dispatch(&mut journal, admitted, 3_000).unwrap();
        drop(_in_flight);
        drop(journal);

        let mut restarted = dir.reopen();
        match admit_step(&mut restarted, &permit, &secret, &intent()) {
            Err(StepError::OutcomeUnknown { permit_id }) => assert_eq!(permit_id, "PERMIT-1"),
            other => panic!("a mid-dispatch permit was not refused: {other:?}"),
        }
    }

    #[test]
    fn one_permit_cannot_produce_two_intents() {
        let dir = TempDir::new("twice");
        let secret = secret();
        let permit = permit(&secret);

        let mut journal = dir.journal();
        let _admitted = admit_step(&mut journal, &permit, &secret, &intent()).unwrap();
        match admit_step(&mut journal, &permit, &secret, &intent()) {
            Err(StepError::IntentAlreadyRecorded { permit_id }) => {
                assert_eq!(permit_id, "PERMIT-1")
            }
            other => panic!("a second intent was allowed: {other:?}"),
        }
    }

    #[test]
    fn receipt_reconstruction_facts_cannot_shadow_engine_identity() {
        let dir = TempDir::new("receipt-fact-shadow");
        let secret = secret();
        let permit = permit(&secret);
        let mut journal = dir.journal();
        let admitted = admit_step(&mut journal, &permit, &secret, &intent()).unwrap();
        let in_flight = begin_dispatch(&mut journal, admitted, 3_000).unwrap();
        let records_before = journal.len();

        let error = record_receipt_with_facts(
            &mut journal,
            in_flight,
            sha256(b"receipt"),
            vec![(fact::RECEIPT_DIGEST.into(), "forged".into())],
            4_000,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            StepError::DuplicateFactKey(key) if key == fact::RECEIPT_DIGEST
        ));
        assert_eq!(journal.len(), records_before);
    }

    #[test]
    fn a_permit_that_does_not_verify_leaves_nothing_in_the_journal() {
        let dir = TempDir::new("badtag");
        let mut journal = dir.journal();
        let secret = secret();
        let mut permit = permit(&secret);
        permit.integrity_tag.tag = sha256(b"not the tag");

        let error = admit_step(&mut journal, &permit, &secret, &intent()).unwrap_err();
        assert!(matches!(
            error,
            StepError::Verification(PermitVerificationError::IntegrityTagInvalid)
        ));
        // Zero dispatch, and zero record: an authority-boundary failure is not
        // a step that started (architecture.md 8.6).
        assert!(journal.is_empty());
    }
}
