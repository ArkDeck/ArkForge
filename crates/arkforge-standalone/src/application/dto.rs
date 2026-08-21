//! Versioned, presentation-neutral application data transfer objects.
//!
//! These types deliberately contain no IPC messages, plan digests, USB
//! handles, filesystem handles or vendor commands. A desktop frontend can
//! therefore change toolkit without changing the ArkForge authority boundary.

/// Increment when a DTO is removed or its meaning changes incompatibly.
pub const APPLICATION_CONTRACT_VERSION: &str = "arkforge.application/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ErrorCategory {
    InvalidInput,
    RuntimeUnavailable,
    Compatibility,
    Artifact,
    Device,
    Authority,
    Transport,
    Execution,
    OutcomeUnknown,
    Recovery,
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SuggestedAction {
    Retry,
    StartRuntime,
    CheckVersion,
    SelectArtifact,
    ReimportArtifact,
    ReconnectDevice,
    ReviewAcknowledgements,
    CheckToolBinding,
    ReconcileJob,
    OpenRuntimeLog,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationError {
    pub code: String,
    pub category: ErrorCategory,
    pub message: String,
    pub retryable: bool,
    pub suggested_actions: Vec<SuggestedAction>,
    pub required_acknowledgements: Vec<String>,
}

impl ApplicationError {
    pub fn new(
        code: impl Into<String>,
        category: ErrorCategory,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            code: code.into(),
            category,
            message: message.into(),
            retryable,
            suggested_actions: Vec::new(),
            required_acknowledgements: Vec::new(),
        }
    }

    pub fn with_actions(mut self, actions: impl IntoIterator<Item = SuggestedAction>) -> Self {
        self.suggested_actions = actions.into_iter().collect();
        self
    }

    pub fn with_required_acknowledgements(mut self, tokens: Vec<String>) -> Self {
        self.required_acknowledgements = tokens;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedValue {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeState {
    Stopped,
    Starting,
    Ready,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeStatus {
    pub state: RuntimeState,
    pub daemon_version: String,
    pub protocol_major: u32,
    pub protocol_minor: u32,
    pub active_jobs: usize,
    pub blockers: Vec<String>,
}

impl RuntimeStatus {
    pub fn stopped() -> Self {
        Self {
            state: RuntimeState::Stopped,
            daemon_version: String::new(),
            protocol_major: 0,
            protocol_minor: 0,
            active_jobs: 0,
            blockers: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeStartRequest {
    /// Optional packaged `arkforge` helper. CLI callers leave this unset;
    /// desktop bundles set it so lifecycle never depends on the UI executable.
    pub authority_executable: Option<String>,
    pub profile_files: Vec<String>,
    pub hdc_path: Option<String>,
    pub expected_hdc_sha256: Option<String>,
    pub hardware_campaign: Option<String>,
    pub require_release_signing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceSummary {
    pub id: String,
    pub mode: String,
    pub identity_strength: String,
    pub observed_at_epoch_ms: u64,
    pub malformed_descriptor: bool,
    pub facts: Vec<NamedValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactSummary {
    pub id: String,
    pub format: String,
    pub size_bytes: u64,
    pub confidence: String,
    pub partitions: Vec<String>,
    pub unknowns: Vec<NamedValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedArtifact {
    pub id: String,
    pub size_bytes: u64,
    pub deduplicated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrepareFlashRequest {
    pub artifact_id: String,
    pub profile_id: String,
    pub device_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationStep {
    pub id: String,
    pub kind: String,
    pub effect: String,
    pub target: Option<String>,
    pub cancellable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationEffect {
    pub kind: String,
    pub target: String,
    pub range_start: u64,
    pub range_length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Acknowledgement {
    pub token: String,
    pub destructive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedFlash {
    /// Opaque, process-local reference. It is not a plan id or plan digest.
    pub review_id: String,
    pub expires_at_epoch_ms: u64,
    pub artifact_id: String,
    pub profile_id: String,
    pub device_id: String,
    pub steps: Vec<OperationStep>,
    pub persistent_effects: Vec<OperationEffect>,
    pub data_impact: Vec<NamedValue>,
    pub required_acknowledgements: Vec<Acknowledgement>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnavailableFlash {
    pub reason: String,
    pub would_be_steps: Vec<OperationStep>,
    pub known_persistent_effects: Vec<OperationEffect>,
    pub data_impact: Vec<NamedValue>,
    pub unknowns: Vec<NamedValue>,
    pub evidence_requirements: Vec<NamedValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrepareFlashResult {
    Ready(PreparedFlash),
    Unavailable(UnavailableFlash),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartFlashRequest {
    pub review_id: String,
    pub accepted_acknowledgements: Vec<String>,
    pub detach: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    Queued,
    Running,
    WaitingForAuthorization,
    WaitingForDevice,
    Reconciling,
    Succeeded,
    Failed,
    Cancelled,
    OutcomeUnknown,
    RecoveryAvailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressPhase {
    Queued,
    Preflight,
    WaitingForAuthorization,
    Executing,
    WaitingForDevice,
    Postflight,
    Reconciling,
    Completed,
    AttentionRequired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressSnapshot {
    pub phase: ProgressPhase,
    pub completed_units: u64,
    pub total_units: u64,
    pub current_unit_id: Option<String>,
    pub indeterminate: bool,
    pub can_cancel: bool,
    pub needs_reconciliation: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlashJob {
    pub id: String,
    pub status: JobStatus,
    pub terminal: bool,
    pub stopped_reason: Option<String>,
    pub last_event_sequence: u64,
    pub progress: ProgressSnapshot,
}

/// Presentation-stable event vocabulary. Provider/private action names stay
/// below the application boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressEventKind {
    StateChanged,
    AuthorizationRequested,
    DeviceControlRequested,
    ActionCompleted,
    ProgressCheckpointed,
    PostflightRecorded,
    OutcomeClassified,
    PossibleEffectsUpdated,
    RecoveryAssessed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobEvent {
    pub sequence: u64,
    pub kind: ProgressEventKind,
    pub at_epoch_ms: u64,
    pub state: JobStatus,
    pub facts: Vec<NamedValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobUpdates {
    pub events: Vec<JobEvent>,
    pub current: FlashJob,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciliation {
    pub job_id: String,
    pub verdict: String,
    pub detail: String,
    pub completeness: String,
    pub possible_effects: Vec<String>,
    pub original_status: JobStatus,
}
