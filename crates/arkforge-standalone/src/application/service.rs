use super::dto::*;
use crate::{StandaloneError, supervisor};
use arkforge_artifact::{CasError, CasQuota, ContentAddressedStore};
use arkforge_client::{ClientError, DeviceObservationView, PublicClient};
use arkforge_core::Sha256Digest;
use arkforge_ipc::messages::{
    Assessment, Effect, ExecutablePlan, InspectArtifactResponse, JobEventKind as IpcJobEventKind,
    JobSummary, KeyValue, MaterializePlanResponse, PublicStep,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// The complete presentation-facing ArkForge use-case boundary.
///
/// Implementations are synchronous by design: desktop adapters decide which
/// executor, async runtime or actor model to use. The contract itself has no
/// UI framework or async-runtime dependency.
pub trait ApplicationService: Send + Sync {
    fn runtime_status(&self) -> Result<RuntimeStatus, ApplicationError>;
    fn start_runtime(
        &self,
        request: RuntimeStartRequest,
    ) -> Result<RuntimeStatus, ApplicationError>;
    fn stop_runtime(&self) -> Result<RuntimeStatus, ApplicationError>;
    fn discover_devices(&self) -> Result<Vec<DeviceSummary>, ApplicationError>;
    fn import_artifact(
        &self,
        file: &Path,
        expected_sha256: Option<&str>,
    ) -> Result<ImportedArtifact, ApplicationError>;
    fn inspect_artifact(&self, artifact_id: &str) -> Result<ArtifactSummary, ApplicationError>;
    fn prepare_flash(
        &self,
        request: PrepareFlashRequest,
    ) -> Result<PrepareFlashResult, ApplicationError>;
    fn start_flash(&self, request: StartFlashRequest) -> Result<FlashJob, ApplicationError>;
    fn job(&self, job_id: &str) -> Result<FlashJob, ApplicationError>;
    fn job_updates(
        &self,
        job_id: &str,
        after_sequence: u64,
    ) -> Result<JobUpdates, ApplicationError>;
    fn cancel_job(
        &self,
        job_id: &str,
        expected_sequence: u64,
    ) -> Result<FlashJob, ApplicationError>;
    fn reconcile_job(&self, job_id: &str) -> Result<Reconciliation, ApplicationError>;
}

#[derive(Debug, Clone)]
struct ReviewRecord {
    plan_id: String,
    plan_sha256: String,
    expires_at_epoch_ms: u64,
    required_acknowledgements: Vec<String>,
}

/// Production implementation backed by one standalone ArkForge runtime.
pub struct StandaloneApplicationService {
    runtime_dir: PathBuf,
    reviews: Mutex<BTreeMap<String, ReviewRecord>>,
    next_review: AtomicU64,
}

impl StandaloneApplicationService {
    pub fn new(runtime_dir: impl Into<PathBuf>) -> Self {
        Self {
            runtime_dir: runtime_dir.into(),
            reviews: Mutex::new(BTreeMap::new()),
            next_review: AtomicU64::new(1),
        }
    }

    pub fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
    }

    fn public_client(&self) -> Result<PublicClient, ApplicationError> {
        PublicClient::connect(&self.runtime_dir).map_err(ApplicationError::from)
    }

    fn reviews(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, BTreeMap<String, ReviewRecord>>, ApplicationError> {
        self.reviews.lock().map_err(|_| {
            ApplicationError::new(
                "APPLICATION_STATE_POISONED",
                ErrorCategory::Internal,
                "The in-process flash review state is unavailable.",
                false,
            )
            .with_actions([SuggestedAction::OpenRuntimeLog])
        })
    }

    fn new_review_id(&self) -> String {
        let sequence = self.next_review.fetch_add(1, Ordering::Relaxed);
        format!("review-{}-{sequence}", std::process::id())
    }
}

impl ApplicationService for StandaloneApplicationService {
    fn runtime_status(&self) -> Result<RuntimeStatus, ApplicationError> {
        match supervisor::status(&self.runtime_dir) {
            Ok(status) => Ok(runtime_status(status)),
            Err(error) if error.code == "DAEMON_UNAVAILABLE" => Ok(RuntimeStatus::stopped()),
            Err(error) => Err(error.into()),
        }
    }

    fn start_runtime(
        &self,
        request: RuntimeStartRequest,
    ) -> Result<RuntimeStatus, ApplicationError> {
        let mut arguments = Vec::new();
        for profile in request.profile_files {
            arguments.push("--profile-file".to_string());
            arguments.push(profile);
        }
        if let Some(hdc) = request.hdc_path {
            arguments.push("--hdc".to_string());
            arguments.push(hdc);
        }
        if let Some(digest) = request.expected_hdc_sha256 {
            arguments.push("--expect-hdc-sha256".to_string());
            arguments.push(digest);
        }
        if request.require_release_signing {
            arguments.push("--require-release-signing".to_string());
        }
        if let Some(campaign) = request.hardware_campaign {
            arguments.push("--hardware-campaign".to_string());
            arguments.push(campaign);
        }
        let options = supervisor::DaemonOptions::parse(&arguments)?;
        let status = match request.authority_executable {
            Some(path) => supervisor::start_with_launcher(
                self.runtime_dir.clone(),
                options,
                PathBuf::from(path),
            ),
            None => supervisor::start(self.runtime_dir.clone(), options),
        }?;
        Ok(runtime_status(status))
    }

    fn stop_runtime(&self) -> Result<RuntimeStatus, ApplicationError> {
        supervisor::stop(&self.runtime_dir)
            .map(runtime_status)
            .map_err(ApplicationError::from)
    }

    fn discover_devices(&self) -> Result<Vec<DeviceSummary>, ApplicationError> {
        let mut client = self.public_client()?;
        client
            .device_list()?
            .into_iter()
            .map(device_summary)
            .collect()
    }

    fn import_artifact(
        &self,
        file: &Path,
        expected_sha256: Option<&str>,
    ) -> Result<ImportedArtifact, ApplicationError> {
        let metadata = std::fs::metadata(file).map_err(|error| {
            ApplicationError::new(
                "ARTIFACT_FILE_NOT_FOUND",
                ErrorCategory::Artifact,
                format!("Cannot read artifact input {}: {error}", file.display()),
                false,
            )
            .with_actions([SuggestedAction::SelectArtifact])
        })?;
        if !metadata.is_file() {
            return Err(ApplicationError::new(
                "INVALID_ARGUMENT",
                ErrorCategory::InvalidInput,
                format!(
                    "Artifact input must be one regular file: {}",
                    file.display()
                ),
                false,
            )
            .with_actions([SuggestedAction::SelectArtifact]));
        }
        let expected = expected_sha256
            .map(Sha256Digest::parse_hex)
            .transpose()
            .map_err(|error| {
                ApplicationError::new(
                    "INVALID_ARGUMENT",
                    ErrorCategory::InvalidInput,
                    format!("Invalid expected artifact SHA-256: {error}"),
                    false,
                )
            })?;
        supervisor::prepare_storage(&self.runtime_dir)?;
        let store = ContentAddressedStore::open(
            self.runtime_dir.join("store"),
            CasQuota::dayu200_default(),
        )?;
        let input = File::open(file).map_err(|error| {
            ApplicationError::new(
                "ARTIFACT_FILE_NOT_FOUND",
                ErrorCategory::Artifact,
                format!("Cannot open artifact input {}: {error}", file.display()),
                false,
            )
            .with_actions([SuggestedAction::SelectArtifact])
        })?;
        let imported = store.import(input, metadata.len(), expected)?;
        Ok(ImportedArtifact {
            id: imported.digest.to_hex(),
            size_bytes: imported.size_bytes,
            deduplicated: imported.deduplicated,
        })
    }

    fn inspect_artifact(&self, artifact_id: &str) -> Result<ArtifactSummary, ApplicationError> {
        let mut client = self.public_client()?;
        client
            .artifact_show(artifact_id)
            .map(|manifest| artifact_summary(artifact_id, manifest))
            .map_err(ApplicationError::from)
    }

    fn prepare_flash(
        &self,
        request: PrepareFlashRequest,
    ) -> Result<PrepareFlashResult, ApplicationError> {
        let response = supervisor::materialize_plan(
            &self.runtime_dir,
            &request.artifact_id,
            &request.profile_id,
            &request.device_id,
        )?;
        match response {
            MaterializePlanResponse::Assessment(assessment) => Ok(PrepareFlashResult::Unavailable(
                unavailable_flash(assessment),
            )),
            MaterializePlanResponse::Plan(plan) => {
                let review_id = self.new_review_id();
                let required = supervisor::required_acknowledgements(&plan);
                let prepared = prepared_flash(&review_id, &request, &plan, &required);
                self.reviews()?.insert(
                    review_id,
                    ReviewRecord {
                        plan_id: plan.plan_id,
                        plan_sha256: plan.plan_sha256,
                        expires_at_epoch_ms: plan.expires_at_epoch_ms,
                        required_acknowledgements: required,
                    },
                );
                Ok(PrepareFlashResult::Ready(prepared))
            }
        }
    }

    fn start_flash(&self, request: StartFlashRequest) -> Result<FlashJob, ApplicationError> {
        let record = self
            .reviews()?
            .get(&request.review_id)
            .cloned()
            .ok_or_else(|| {
                ApplicationError::new(
                    "FLASH_REVIEW_NOT_FOUND",
                    ErrorCategory::InvalidInput,
                    "The flash review is missing or was already applied. Prepare the operation again.",
                    false,
                )
            })?;
        if now_epoch_ms() >= record.expires_at_epoch_ms {
            self.reviews()?.remove(&request.review_id);
            return Err(ApplicationError::new(
                "PLAN_EXPIRED",
                ErrorCategory::Authority,
                "The reviewed operation expired. Inspect the current device and prepare it again.",
                false,
            )
            .with_actions([SuggestedAction::Retry]));
        }
        validate_acknowledgements(
            &record.required_acknowledgements,
            &request.accepted_acknowledgements,
        )?;
        let job_id = supervisor::apply_plan(
            &self.runtime_dir,
            &record.plan_id,
            &record.plan_sha256,
            &request.accepted_acknowledgements,
            request.detach,
        )?;
        self.reviews()?.remove(&request.review_id);
        self.job(&job_id)
    }

    fn job(&self, job_id: &str) -> Result<FlashJob, ApplicationError> {
        let mut client = self.public_client()?;
        client
            .job_show(job_id)
            .map(flash_job)
            .map_err(ApplicationError::from)
    }

    fn job_updates(
        &self,
        job_id: &str,
        after_sequence: u64,
    ) -> Result<JobUpdates, ApplicationError> {
        let mut client = self.public_client()?;
        let events = client.job_events(job_id, after_sequence)?;
        let summary = client.job_show(job_id)?;
        Ok(JobUpdates {
            events: events
                .into_iter()
                .map(|event| JobEvent {
                    sequence: event.sequence,
                    kind: progress_event_kind(event.kind),
                    at_epoch_ms: event.at_epoch_ms,
                    state: job_status(&event.job_state),
                    facts: named_values(event.facts),
                })
                .collect(),
            current: flash_job(summary),
        })
    }

    fn cancel_job(
        &self,
        job_id: &str,
        expected_sequence: u64,
    ) -> Result<FlashJob, ApplicationError> {
        supervisor::cancel_job(&self.runtime_dir, job_id, expected_sequence)?;
        self.job(job_id)
    }

    fn reconcile_job(&self, job_id: &str) -> Result<Reconciliation, ApplicationError> {
        let status = supervisor::reconcile_job(&self.runtime_dir, job_id)?;
        Ok(Reconciliation {
            job_id: status.job_id,
            verdict: status.verdict,
            detail: status.detail,
            completeness: status.completeness,
            possible_effects: status.possible_effects,
            original_status: job_status(&status.original_state),
        })
    }
}

fn runtime_status(status: supervisor::DaemonStatus) -> RuntimeStatus {
    RuntimeStatus {
        state: if status.blockers.is_empty()
            && status.mechanics_ready
            && status.authority_support_available
        {
            RuntimeState::Ready
        } else {
            RuntimeState::Blocked
        },
        daemon_version: status.daemon_version,
        protocol_major: status.protocol_major,
        protocol_minor: status.protocol_minor,
        active_jobs: status.active_jobs,
        blockers: status.blockers,
    }
}

fn device_summary(device: DeviceObservationView) -> Result<DeviceSummary, ApplicationError> {
    if device.observation_id.is_empty() {
        return Err(ApplicationError::new(
            "DEVICE_OBSERVATION_INVALID",
            ErrorCategory::Compatibility,
            "The runtime returned a device observation without an id.",
            false,
        )
        .with_actions([SuggestedAction::CheckVersion]));
    }
    Ok(DeviceSummary {
        id: device.observation_id,
        mode: device.mode,
        identity_strength: device.identity_strength,
        observed_at_epoch_ms: device.observed_at_epoch_ms,
        malformed_descriptor: device.malformed_descriptor,
        facts: named_values(device.protocol_identity),
    })
}

fn artifact_summary(id: &str, manifest: InspectArtifactResponse) -> ArtifactSummary {
    ArtifactSummary {
        id: id.to_string(),
        format: manifest.format_id,
        size_bytes: manifest.size_bytes,
        confidence: manifest.confidence,
        partitions: manifest
            .partitions
            .into_iter()
            .map(|partition| partition.name)
            .collect(),
        unknowns: named_values(manifest.execution_relevant_unknowns),
    }
}

fn prepared_flash(
    review_id: &str,
    request: &PrepareFlashRequest,
    plan: &ExecutablePlan,
    required: &[String],
) -> PreparedFlash {
    PreparedFlash {
        review_id: review_id.to_string(),
        expires_at_epoch_ms: plan.expires_at_epoch_ms,
        artifact_id: request.artifact_id.clone(),
        profile_id: request.profile_id.clone(),
        device_id: request.device_id.clone(),
        steps: plan.public_steps.iter().map(operation_step).collect(),
        persistent_effects: plan
            .persistent_effects
            .iter()
            .map(operation_effect)
            .collect(),
        data_impact: named_values(plan.data_impact.clone()),
        required_acknowledgements: required
            .iter()
            .map(|token| Acknowledgement {
                token: token.clone(),
                destructive: token.starts_with("data-loss:") || token.starts_with("overwrite:"),
            })
            .collect(),
    }
}

fn unavailable_flash(assessment: Assessment) -> UnavailableFlash {
    UnavailableFlash {
        reason: if assessment.unavailable_reason.is_empty() {
            assessment.availability
        } else {
            assessment.unavailable_reason
        },
        would_be_steps: assessment
            .would_be_steps
            .iter()
            .map(operation_step)
            .collect(),
        known_persistent_effects: assessment
            .known_persistent_effects
            .iter()
            .map(operation_effect)
            .collect(),
        data_impact: named_values(assessment.data_impact),
        unknowns: named_values(assessment.unknowns),
        evidence_requirements: named_values(assessment.evidence_requirements),
    }
}

fn operation_step(step: &PublicStep) -> OperationStep {
    OperationStep {
        id: step.step_id.clone(),
        kind: step.kind.clone(),
        effect: step.effect.clone(),
        target: (!step.semantic_target.is_empty()).then(|| step.semantic_target.clone()),
        cancellable: step.cancellation != "nonCancellable",
    }
}

fn operation_effect(effect: &Effect) -> OperationEffect {
    OperationEffect {
        kind: effect.kind.clone(),
        target: effect.target.clone(),
        range_start: effect.range_start,
        range_length: effect.range_length,
    }
}

fn named_values(values: Vec<KeyValue>) -> Vec<NamedValue> {
    values
        .into_iter()
        .map(|value| NamedValue {
            name: value.key,
            value: value.value,
        })
        .collect()
}

pub(crate) fn flash_job(summary: JobSummary) -> FlashJob {
    let state = summary.state.as_str();
    let status = job_status(state);
    FlashJob {
        id: summary.job_id,
        status,
        terminal: summary.terminal,
        stopped_reason: (!summary.stopped_reason.is_empty()).then_some(summary.stopped_reason),
        last_event_sequence: summary.last_sequence,
        progress: ProgressSnapshot {
            phase: progress_phase(state),
            completed_units: summary.completed_steps,
            total_units: summary.total_steps,
            current_unit_id: (!summary.current_step_id.is_empty())
                .then_some(summary.current_step_id),
            indeterminate: summary.total_steps == 0,
            can_cancel: !summary.terminal
                && !matches!(
                    status,
                    JobStatus::OutcomeUnknown
                        | JobStatus::Reconciling
                        | JobStatus::RecoveryAvailable
                ),
            needs_reconciliation: status == JobStatus::OutcomeUnknown,
        },
    }
}

pub(crate) fn job_status(state: &str) -> JobStatus {
    match state {
        "planned" | "awaitingStart" => JobStatus::Queued,
        "awaitingPermit" => JobStatus::WaitingForAuthorization,
        "rebindWait" => JobStatus::WaitingForDevice,
        "reconciling" => JobStatus::Reconciling,
        "succeeded" => JobStatus::Succeeded,
        "confirmedFailed" => JobStatus::Failed,
        "cancelledSafe" => JobStatus::Cancelled,
        "outcomeUnknown" => JobStatus::OutcomeUnknown,
        "recoveryAssessable" => JobStatus::RecoveryAvailable,
        _ => JobStatus::Running,
    }
}

pub(crate) fn progress_phase(state: &str) -> ProgressPhase {
    match state {
        "planned" | "awaitingStart" => ProgressPhase::Queued,
        "preflight" => ProgressPhase::Preflight,
        "awaitingPermit" => ProgressPhase::WaitingForAuthorization,
        "rebindWait" => ProgressPhase::WaitingForDevice,
        "postflight" => ProgressPhase::Postflight,
        "reconciling" => ProgressPhase::Reconciling,
        "succeeded" => ProgressPhase::Completed,
        "confirmedFailed" | "cancelledSafe" | "outcomeUnknown" | "recoveryAssessable" => {
            ProgressPhase::AttentionRequired
        }
        _ => ProgressPhase::Executing,
    }
}

fn progress_event_kind(kind: IpcJobEventKind) -> ProgressEventKind {
    match kind {
        IpcJobEventKind::StateChanged => ProgressEventKind::StateChanged,
        IpcJobEventKind::StepAdmissionRequested => ProgressEventKind::AuthorizationRequested,
        IpcJobEventKind::ManagedControlRequested => ProgressEventKind::DeviceControlRequested,
        IpcJobEventKind::ActionReceipt => ProgressEventKind::ActionCompleted,
        IpcJobEventKind::StepCheckpointed => ProgressEventKind::ProgressCheckpointed,
        IpcJobEventKind::PostflightRecorded => ProgressEventKind::PostflightRecorded,
        IpcJobEventKind::OutcomeClassified => ProgressEventKind::OutcomeClassified,
        IpcJobEventKind::PossibleEffectSet => ProgressEventKind::PossibleEffectsUpdated,
        IpcJobEventKind::RecoveryAssessment => ProgressEventKind::RecoveryAssessed,
    }
}

fn validate_acknowledgements(
    required: &[String],
    accepted: &[String],
) -> Result<(), ApplicationError> {
    let required = required.iter().cloned().collect::<BTreeSet<_>>();
    let accepted = accepted.iter().cloned().collect::<BTreeSet<_>>();
    let missing = required.difference(&accepted).cloned().collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(ApplicationError::new(
            "ACKNOWLEDGEMENT_REQUIRED",
            ErrorCategory::Authority,
            "The destructive effects have not all been acknowledged.",
            true,
        )
        .with_actions([SuggestedAction::ReviewAcknowledgements])
        .with_required_acknowledgements(missing));
    }
    if let Some(unexpected) = accepted.difference(&required).next() {
        return Err(ApplicationError::new(
            "UNEXPECTED_ACKNOWLEDGEMENT",
            ErrorCategory::InvalidInput,
            format!("Acknowledgement {unexpected:?} is not required by this operation."),
            false,
        )
        .with_actions([SuggestedAction::ReviewAcknowledgements]));
    }
    Ok(())
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

impl From<StandaloneError> for ApplicationError {
    fn from(error: StandaloneError) -> Self {
        classify_service_error(
            error.code,
            error.message,
            error.retryable,
            error.required_acknowledgements,
        )
    }
}

impl From<ClientError> for ApplicationError {
    fn from(error: ClientError) -> Self {
        classify_service_error(error.code, error.message, error.retryable, Vec::new())
    }
}

impl From<CasError> for ApplicationError {
    fn from(error: CasError) -> Self {
        match error {
            CasError::DigestMismatch { .. } => ApplicationError::new(
                "ARTIFACT_IMPORT_REFUSED",
                ErrorCategory::Artifact,
                error.to_string(),
                false,
            )
            .with_actions([SuggestedAction::ReimportArtifact]),
            CasError::QuotaExceeded(_) | CasError::ArtifactTooLarge { .. } => {
                ApplicationError::new(
                    "ARTIFACT_IMPORT_REFUSED",
                    ErrorCategory::Artifact,
                    error.to_string(),
                    false,
                )
                .with_actions([SuggestedAction::SelectArtifact])
            }
            CasError::NotFound(_) => ApplicationError::new(
                "ARTIFACT_NOT_FOUND",
                ErrorCategory::Artifact,
                error.to_string(),
                false,
            )
            .with_actions([SuggestedAction::ReimportArtifact]),
            _ => ApplicationError::new(
                "ARTIFACT_STORE_FAILED",
                ErrorCategory::Artifact,
                error.to_string(),
                true,
            )
            .with_actions([SuggestedAction::Retry, SuggestedAction::OpenRuntimeLog]),
        }
    }
}

fn classify_service_error(
    code: String,
    message: String,
    retryable: bool,
    acknowledgements: Vec<String>,
) -> ApplicationError {
    let (category, actions): (ErrorCategory, &[SuggestedAction]) = match code.as_str() {
        "INVALID_ARGUMENT" | "UNEXPECTED_ACKNOWLEDGEMENT" | "FLASH_REVIEW_NOT_FOUND" => {
            (ErrorCategory::InvalidInput, &[])
        }
        "DAEMON_UNAVAILABLE" | "IPC_IO_FAILED" | "RUNTIME_ALREADY_RUNNING" => (
            ErrorCategory::RuntimeUnavailable,
            &[SuggestedAction::StartRuntime, SuggestedAction::Retry],
        ),
        "PROTOCOL_REFUSED"
        | "IPC_RESPONSE_INVALID"
        | "IPC_RESPONSE_MISMATCH"
        | "SUPERVISOR_RESPONSE_INVALID" => (
            ErrorCategory::Compatibility,
            &[SuggestedAction::CheckVersion],
        ),
        code if code.starts_with("ARTIFACT_") => (
            ErrorCategory::Artifact,
            &[SuggestedAction::ReimportArtifact],
        ),
        code if code.starts_with("DEVICE_") || code.starts_with("OBSERVATION_") => (
            ErrorCategory::Device,
            &[SuggestedAction::ReconnectDevice, SuggestedAction::Retry],
        ),
        code if code.contains("HDC") || code.contains("USB") || code.contains("TRANSPORT") => (
            ErrorCategory::Transport,
            &[SuggestedAction::CheckToolBinding],
        ),
        "OUTCOME_UNKNOWN" => (
            ErrorCategory::OutcomeUnknown,
            &[SuggestedAction::ReconcileJob],
        ),
        code if code.starts_with("RECOVERY_") => {
            (ErrorCategory::Recovery, &[SuggestedAction::ReconcileJob])
        }
        code if code.contains("AUTHORITY")
            || code.contains("ACKNOWLEDGEMENT")
            || code.starts_with("PLAN_") =>
        {
            (
                ErrorCategory::Authority,
                &[SuggestedAction::ReviewAcknowledgements],
            )
        }
        code if code.starts_with("JOB_") || code == "UNKNOWN_JOB" => {
            (ErrorCategory::Execution, &[SuggestedAction::Retry])
        }
        _ => (ErrorCategory::Internal, &[SuggestedAction::OpenRuntimeLog]),
    };
    ApplicationError::new(code, category, message, retryable)
        .with_actions(actions.iter().copied())
        .with_required_acknowledgements(acknowledgements)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_is_structured_without_provider_step_names() {
        let job = flash_job(JobSummary {
            job_id: "JOB-1".into(),
            state: "dispatching".into(),
            current_step_id: "STEP-007".into(),
            completed_steps: 6,
            total_steps: 12,
            ..JobSummary::default()
        });
        assert_eq!(job.progress.phase, ProgressPhase::Executing);
        assert_eq!(job.progress.completed_units, 6);
        assert_eq!(job.progress.total_units, 12);
        assert_eq!(job.progress.current_unit_id.as_deref(), Some("STEP-007"));
    }

    #[test]
    fn outcome_unknown_is_never_rendered_as_generic_failure() {
        let job = flash_job(JobSummary {
            job_id: "JOB-1".into(),
            state: "outcomeUnknown".into(),
            ..JobSummary::default()
        });
        assert_eq!(job.status, JobStatus::OutcomeUnknown);
        assert!(job.progress.needs_reconciliation);
        assert!(!job.progress.can_cancel);
    }

    #[test]
    fn acknowledgement_validation_is_exact() {
        let required = vec!["data-loss:userdata".to_string()];
        let error = validate_acknowledgements(&required, &[]).unwrap_err();
        assert_eq!(error.code, "ACKNOWLEDGEMENT_REQUIRED");
        assert_eq!(error.category, ErrorCategory::Authority);
        assert_eq!(error.required_acknowledgements, required);

        let error = validate_acknowledgements(&[], &["invented".into()]).unwrap_err();
        assert_eq!(error.code, "UNEXPECTED_ACKNOWLEDGEMENT");
    }

    #[test]
    fn compatibility_errors_have_a_stable_category_and_action() {
        let error = ApplicationError::from(ClientError {
            code: "PROTOCOL_REFUSED".into(),
            message: "new major".into(),
            exit_code: 3,
            retryable: false,
        });
        assert_eq!(error.category, ErrorCategory::Compatibility);
        assert_eq!(error.suggested_actions, vec![SuggestedAction::CheckVersion]);
    }

    #[test]
    fn prepared_flash_does_not_expose_plan_authority_fields() {
        let request = PrepareFlashRequest {
            artifact_id: "artifact".into(),
            profile_id: "profile".into(),
            device_id: "device".into(),
        };
        let plan = ExecutablePlan {
            plan_id: "SECRET-PLAN-ID".into(),
            plan_sha256: "secret-digest".into(),
            expires_at_epoch_ms: u64::MAX,
            ..ExecutablePlan::default()
        };
        let prepared = prepared_flash("review-1", &request, &plan, &[]);
        assert_eq!(prepared.review_id, "review-1");
        let debug = format!("{prepared:?}");
        assert!(!debug.contains("SECRET-PLAN-ID"));
        assert!(!debug.contains("secret-digest"));
    }
}
