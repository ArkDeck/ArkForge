//! Deterministic, I/O-free application service for ArkFlash development.

use super::dto::*;
use super::service::ApplicationService;
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ApplicationMethod {
    RuntimeStatus,
    StartRuntime,
    StopRuntime,
    DiscoverDevices,
    ImportArtifact,
    InspectArtifact,
    PrepareFlash,
    StartFlash,
    Job,
    JobUpdates,
    CancelJob,
    ReconcileJob,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplicationCall {
    RuntimeStatus,
    StartRuntime(RuntimeStartRequest),
    StopRuntime,
    DiscoverDevices,
    ImportArtifact {
        file: PathBuf,
        expected_sha256: Option<String>,
    },
    InspectArtifact(String),
    PrepareFlash(PrepareFlashRequest),
    StartFlash(StartFlashRequest),
    Job(String),
    JobUpdates {
        job_id: String,
        after_sequence: u64,
    },
    CancelJob {
        job_id: String,
        expected_sequence: u64,
    },
    ReconcileJob(String),
}

#[derive(Debug)]
struct MockState {
    runtime: RuntimeStatus,
    devices: Vec<DeviceSummary>,
    next_import: Option<ImportedArtifact>,
    artifacts: BTreeMap<String, ArtifactSummary>,
    preparation: Option<PrepareFlashResult>,
    next_started_job: Option<FlashJob>,
    jobs: BTreeMap<String, FlashJob>,
    events: BTreeMap<String, Vec<JobEvent>>,
    reconciliations: BTreeMap<String, Reconciliation>,
    failures: BTreeMap<ApplicationMethod, VecDeque<ApplicationError>>,
    calls: Vec<ApplicationCall>,
}

impl Default for MockState {
    fn default() -> Self {
        Self {
            runtime: RuntimeStatus::stopped(),
            devices: Vec::new(),
            next_import: None,
            artifacts: BTreeMap::new(),
            preparation: None,
            next_started_job: None,
            jobs: BTreeMap::new(),
            events: BTreeMap::new(),
            reconciliations: BTreeMap::new(),
            failures: BTreeMap::new(),
            calls: Vec::new(),
        }
    }
}

/// Scriptable fake with call recording. It performs no filesystem, process,
/// IPC, USB or HDC operation and is safe for preview builds and UI tests.
#[derive(Debug, Default)]
pub struct MockApplicationService {
    state: Mutex<MockState>,
}

impl MockApplicationService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_runtime(&self, runtime: RuntimeStatus) {
        self.lock().runtime = runtime;
    }

    pub fn set_devices(&self, devices: Vec<DeviceSummary>) {
        self.lock().devices = devices;
    }

    pub fn set_next_import(&self, artifact: ImportedArtifact) {
        self.lock().next_import = Some(artifact);
    }

    pub fn insert_artifact(&self, artifact: ArtifactSummary) {
        self.lock().artifacts.insert(artifact.id.clone(), artifact);
    }

    pub fn set_preparation(&self, preparation: PrepareFlashResult) {
        self.lock().preparation = Some(preparation);
    }

    pub fn set_next_started_job(&self, job: FlashJob) {
        self.lock().next_started_job = Some(job);
    }

    pub fn upsert_job(&self, job: FlashJob) {
        self.lock().jobs.insert(job.id.clone(), job);
    }

    pub fn set_job_events(&self, job_id: impl Into<String>, events: Vec<JobEvent>) {
        self.lock().events.insert(job_id.into(), events);
    }

    pub fn set_reconciliation(&self, reconciliation: Reconciliation) {
        self.lock()
            .reconciliations
            .insert(reconciliation.job_id.clone(), reconciliation);
    }

    pub fn fail_next(&self, method: ApplicationMethod, error: ApplicationError) {
        self.lock()
            .failures
            .entry(method)
            .or_default()
            .push_back(error);
    }

    pub fn take_calls(&self) -> Vec<ApplicationCall> {
        std::mem::take(&mut self.lock().calls)
    }

    fn lock(&self) -> MutexGuard<'_, MockState> {
        self.state
            .lock()
            .expect("mock application state must not be poisoned")
    }

    fn begin(
        &self,
        method: ApplicationMethod,
        call: ApplicationCall,
    ) -> Result<MutexGuard<'_, MockState>, ApplicationError> {
        let mut state = self.lock();
        state.calls.push(call);
        if let Some(error) = state
            .failures
            .get_mut(&method)
            .and_then(VecDeque::pop_front)
        {
            return Err(error);
        }
        Ok(state)
    }

    fn missing(fixture: &str) -> ApplicationError {
        ApplicationError::new(
            "MOCK_FIXTURE_MISSING",
            ErrorCategory::Internal,
            format!("The mock application service has no {fixture} fixture."),
            false,
        )
    }
}

impl ApplicationService for MockApplicationService {
    fn runtime_status(&self) -> Result<RuntimeStatus, ApplicationError> {
        let state = self.begin(
            ApplicationMethod::RuntimeStatus,
            ApplicationCall::RuntimeStatus,
        )?;
        Ok(state.runtime.clone())
    }

    fn start_runtime(
        &self,
        request: RuntimeStartRequest,
    ) -> Result<RuntimeStatus, ApplicationError> {
        let mut state = self.begin(
            ApplicationMethod::StartRuntime,
            ApplicationCall::StartRuntime(request),
        )?;
        state.runtime.state = RuntimeState::Ready;
        Ok(state.runtime.clone())
    }

    fn stop_runtime(&self) -> Result<RuntimeStatus, ApplicationError> {
        let mut state = self.begin(ApplicationMethod::StopRuntime, ApplicationCall::StopRuntime)?;
        state.runtime = RuntimeStatus::stopped();
        Ok(state.runtime.clone())
    }

    fn discover_devices(&self) -> Result<Vec<DeviceSummary>, ApplicationError> {
        let state = self.begin(
            ApplicationMethod::DiscoverDevices,
            ApplicationCall::DiscoverDevices,
        )?;
        Ok(state.devices.clone())
    }

    fn import_artifact(
        &self,
        file: &Path,
        expected_sha256: Option<&str>,
    ) -> Result<ImportedArtifact, ApplicationError> {
        let mut state = self.begin(
            ApplicationMethod::ImportArtifact,
            ApplicationCall::ImportArtifact {
                file: file.to_path_buf(),
                expected_sha256: expected_sha256.map(str::to_string),
            },
        )?;
        state
            .next_import
            .take()
            .ok_or_else(|| Self::missing("artifact import"))
    }

    fn inspect_artifact(&self, artifact_id: &str) -> Result<ArtifactSummary, ApplicationError> {
        let state = self.begin(
            ApplicationMethod::InspectArtifact,
            ApplicationCall::InspectArtifact(artifact_id.to_string()),
        )?;
        state
            .artifacts
            .get(artifact_id)
            .cloned()
            .ok_or_else(|| Self::missing("artifact inspection"))
    }

    fn prepare_flash(
        &self,
        request: PrepareFlashRequest,
    ) -> Result<PrepareFlashResult, ApplicationError> {
        let state = self.begin(
            ApplicationMethod::PrepareFlash,
            ApplicationCall::PrepareFlash(request),
        )?;
        state
            .preparation
            .clone()
            .ok_or_else(|| Self::missing("flash preparation"))
    }

    fn start_flash(&self, request: StartFlashRequest) -> Result<FlashJob, ApplicationError> {
        let mut state = self.begin(
            ApplicationMethod::StartFlash,
            ApplicationCall::StartFlash(request),
        )?;
        let job = state
            .next_started_job
            .take()
            .ok_or_else(|| Self::missing("started job"))?;
        state.jobs.insert(job.id.clone(), job.clone());
        Ok(job)
    }

    fn job(&self, job_id: &str) -> Result<FlashJob, ApplicationError> {
        let state = self.begin(
            ApplicationMethod::Job,
            ApplicationCall::Job(job_id.to_string()),
        )?;
        state
            .jobs
            .get(job_id)
            .cloned()
            .ok_or_else(|| Self::missing("job"))
    }

    fn job_updates(
        &self,
        job_id: &str,
        after_sequence: u64,
    ) -> Result<JobUpdates, ApplicationError> {
        let state = self.begin(
            ApplicationMethod::JobUpdates,
            ApplicationCall::JobUpdates {
                job_id: job_id.to_string(),
                after_sequence,
            },
        )?;
        let current = state
            .jobs
            .get(job_id)
            .cloned()
            .ok_or_else(|| Self::missing("job"))?;
        let events = state
            .events
            .get(job_id)
            .into_iter()
            .flatten()
            .filter(|event| event.sequence > after_sequence)
            .cloned()
            .collect();
        Ok(JobUpdates { events, current })
    }

    fn cancel_job(
        &self,
        job_id: &str,
        expected_sequence: u64,
    ) -> Result<FlashJob, ApplicationError> {
        let mut state = self.begin(
            ApplicationMethod::CancelJob,
            ApplicationCall::CancelJob {
                job_id: job_id.to_string(),
                expected_sequence,
            },
        )?;
        let job = state
            .jobs
            .get_mut(job_id)
            .ok_or_else(|| Self::missing("job"))?;
        job.status = JobStatus::Cancelled;
        job.terminal = true;
        job.progress.phase = ProgressPhase::AttentionRequired;
        job.progress.can_cancel = false;
        Ok(job.clone())
    }

    fn reconcile_job(&self, job_id: &str) -> Result<Reconciliation, ApplicationError> {
        let state = self.begin(
            ApplicationMethod::ReconcileJob,
            ApplicationCall::ReconcileJob(job_id.to_string()),
        )?;
        state
            .reconciliations
            .get(job_id)
            .cloned()
            .ok_or_else(|| Self::missing("reconciliation"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_calls_and_consumes_scripted_failures_once() {
        let service = MockApplicationService::new();
        service.set_devices(vec![]);
        service.fail_next(
            ApplicationMethod::DiscoverDevices,
            ApplicationError::new("DEVICE_BUSY", ErrorCategory::Device, "busy", true),
        );
        assert_eq!(service.discover_devices().unwrap_err().code, "DEVICE_BUSY");
        assert!(service.discover_devices().unwrap().is_empty());
        assert_eq!(
            service.take_calls(),
            vec![
                ApplicationCall::DiscoverDevices,
                ApplicationCall::DiscoverDevices
            ]
        );
    }

    #[test]
    fn mock_never_touches_io_to_run_a_flash_scenario() {
        let service = MockApplicationService::new();
        let job = FlashJob {
            id: "JOB-MOCK".into(),
            status: JobStatus::Running,
            terminal: false,
            stopped_reason: None,
            last_event_sequence: 0,
            progress: ProgressSnapshot {
                phase: ProgressPhase::Executing,
                completed_units: 0,
                total_units: 4,
                current_unit_id: Some("STEP-001".into()),
                indeterminate: false,
                can_cancel: true,
                needs_reconciliation: false,
            },
        };
        service.set_next_started_job(job.clone());
        let started = service
            .start_flash(StartFlashRequest {
                review_id: "review-mock".into(),
                accepted_acknowledgements: vec![],
                detach: true,
            })
            .unwrap();
        assert_eq!(started, job);
        assert_eq!(service.job("JOB-MOCK").unwrap(), job);
    }
}
