//! Closed, typed HDC authority port for normal-flash managed control.
//!
//! The process boundary is intentionally tiny: one exact executable selected
//! at runtime startup and four semantic operations lowered to fixed argument
//! arrays. Raw paths, connect keys and argv never enter facts, receipts,
//! journals or errors.

use crate::CliError;
use arkforge_core::Sha256Digest;
use arkforge_core::digest::{Domain, digest_in_domain};
use arkforge_ipc::messages::{
    KeyValue, ManagedControlAction, ManagedControlRequest, SubmitManagedControlReceiptRequest,
};
use arkforged::jobs::canonical_facts_digest;
use arkforged::public_client::{DeviceObservationView, PublicClient};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const OUTPUT_LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub(super) struct ControlContext {
    pub current_device_id: String,
    pub profile_id: String,
    pub authority_stable_identity_sha256: String,
    pub topology_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ControlResult {
    pub receipt: SubmitManagedControlReceiptRequest,
    pub rebound_observation: Option<DeviceObservationView>,
}

trait ObservationPort {
    fn list(&mut self) -> Result<Vec<DeviceObservationView>, CliError>;
    fn probe(&mut self, device: &str, profile: &str) -> Result<(), CliError>;
}

impl ObservationPort for PublicClient {
    fn list(&mut self) -> Result<Vec<DeviceObservationView>, CliError> {
        self.device_list().map_err(Into::into)
    }

    fn probe(&mut self, device: &str, profile: &str) -> Result<(), CliError> {
        self.device_probe(device, profile)
            .map(|_| ())
            .map_err(Into::into)
    }
}

pub(super) trait CommandPort {
    fn run(&mut self, arguments: &[&str], deadline: Instant) -> Result<Vec<u8>, ControlFailure>;
}

#[derive(Debug)]
pub(super) struct ProcessPort {
    executable: PathBuf,
    working_directory: PathBuf,
    expected_digest: Sha256Digest,
}

impl CommandPort for ProcessPort {
    fn run(&mut self, arguments: &[&str], deadline: Instant) -> Result<Vec<u8>, ControlFailure> {
        let current_digest = arkforged::dispatch::executable_digest(&self.executable)
            .map_err(|_| ControlFailure::CommandUnavailable)?;
        if current_digest != self.expected_digest {
            return Err(ControlFailure::CommandChanged);
        }
        let mut child = Command::new(&self.executable)
            .args(arguments)
            .env_clear()
            .current_dir(&self.working_directory)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| ControlFailure::CommandUnavailable)?;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => {
                    let output = child
                        .wait_with_output()
                        .map_err(|_| ControlFailure::CommandFailed)?;
                    if !output.status.success() {
                        return Err(ControlFailure::CommandFailed);
                    }
                    if output.stdout.len() > OUTPUT_LIMIT {
                        return Err(ControlFailure::OutputTooLarge);
                    }
                    return Ok(output.stdout);
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ControlFailure::DeadlineExceeded);
                }
                Err(_) => return Err(ControlFailure::CommandFailed),
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ControlFailure {
    CommandUnavailable,
    CommandChanged,
    CommandFailed,
    DeadlineExceeded,
    OutputTooLarge,
    InvalidUtf8,
    MalformedTargetList,
    NoExactHdcTarget,
    AmbiguousHdcTarget,
    CurrentObservationMissing,
    WrongStartingMode,
    NoUniqueLoaderRebind,
    NoUniqueNormalRebind,
    PropertyEmpty,
    PropertyMismatch,
}

impl ControlFailure {
    fn public_reason(&self) -> &'static str {
        match self {
            Self::CommandUnavailable => "HDC_COMMAND_UNAVAILABLE",
            Self::CommandChanged => "HDC_EXECUTABLE_CHANGED",
            Self::CommandFailed => "HDC_COMMAND_FAILED",
            Self::DeadlineExceeded => "HDC_CONTROL_DEADLINE_EXCEEDED",
            Self::OutputTooLarge => "HDC_OUTPUT_LIMIT_EXCEEDED",
            Self::InvalidUtf8 => "HDC_OUTPUT_INVALID_UTF8",
            Self::MalformedTargetList => "HDC_TARGET_LIST_MALFORMED",
            Self::NoExactHdcTarget => "EXACT_HDC_TARGET_NOT_FOUND",
            Self::AmbiguousHdcTarget => "EXACT_HDC_TARGET_AMBIGUOUS",
            Self::CurrentObservationMissing => "BOUND_OBSERVATION_NOT_FOUND",
            Self::WrongStartingMode => "BOUND_OBSERVATION_MODE_MISMATCH",
            Self::NoUniqueLoaderRebind => "UNIQUE_LOADER_REBIND_NOT_OBSERVED",
            Self::NoUniqueNormalRebind => "UNIQUE_NORMAL_REBIND_NOT_OBSERVED",
            Self::PropertyEmpty => "HDC_PROPERTY_EMPTY",
            Self::PropertyMismatch => "HDC_PROPERTY_MISMATCH",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TargetRow {
    connect_key: String,
    transport: String,
    state: String,
}

fn parse_target_list(stdout: &[u8]) -> Result<Vec<TargetRow>, ControlFailure> {
    if stdout.len() > OUTPUT_LIMIT {
        return Err(ControlFailure::OutputTooLarge);
    }
    let text = std::str::from_utf8(stdout).map_err(|_| ControlFailure::InvalidUtf8)?;
    let lines = text
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if lines == ["[Empty]"] {
        return Ok(Vec::new());
    }
    if lines.is_empty() {
        return Err(ControlFailure::MalformedTargetList);
    }
    let mut rows = Vec::new();
    for line in lines {
        let columns = line.split('\t').collect::<Vec<_>>();
        if columns.len() != 5
            || !columns[1].is_empty()
            || columns[4] != "localhost"
            || columns[0].is_empty()
            || columns[0].len() > 128
            || !columns[0]
                .bytes()
                .all(|byte| byte.is_ascii() && !byte.is_ascii_whitespace())
            || !matches!(columns[2], "USB" | "TCP" | "UART")
            || !matches!(columns[3], "Connected" | "Unauthorized" | "Offline")
        {
            return Err(ControlFailure::MalformedTargetList);
        }
        rows.push(TargetRow {
            connect_key: columns[0].to_string(),
            transport: columns[2].to_string(),
            state: columns[3].to_string(),
        });
    }
    Ok(rows)
}

#[derive(Debug)]
pub(super) struct HdcControlPort<R = ProcessPort> {
    runner: R,
    // Raw connect keys live only in this supervisor process.
    sessions: BTreeMap<String, String>,
}

impl HdcControlPort<ProcessPort> {
    pub fn new(
        executable: PathBuf,
        working_directory: PathBuf,
        expected_digest: Sha256Digest,
    ) -> Self {
        Self {
            runner: ProcessPort {
                executable,
                working_directory,
                expected_digest,
            },
            sessions: BTreeMap::new(),
        }
    }
}

impl<R: CommandPort> HdcControlPort<R> {
    pub fn perform(
        &mut self,
        observations: &mut PublicClient,
        request: &ManagedControlRequest,
        context: &ControlContext,
    ) -> ControlResult {
        self.perform_with(observations, request, context)
    }

    fn perform_with<O: ObservationPort>(
        &mut self,
        observations: &mut O,
        request: &ManagedControlRequest,
        context: &ControlContext,
    ) -> ControlResult {
        let deadline = deadline(request.deadline_epoch_ms);
        let result = match request.action {
            ManagedControlAction::EnterUpdater => {
                self.enter_updater(observations, request, context, deadline)
            }
            ManagedControlAction::RebootToNormal => {
                self.reboot_to_normal(observations, request, context, deadline)
            }
            ManagedControlAction::ReadProductFacts => self.read_property(
                observations,
                request,
                context,
                deadline,
                "const.product.model",
            ),
            ManagedControlAction::ReadBuildFacts => self.read_property(
                observations,
                request,
                context,
                deadline,
                "const.ohos.fullname",
            ),
        };
        match result {
            Ok((facts, rebound)) => ControlResult {
                receipt: accepted_receipt(request, facts),
                rebound_observation: rebound,
            },
            Err(failure) => ControlResult {
                receipt: refused_receipt(request, failure.public_reason()),
                rebound_observation: None,
            },
        }
    }

    fn enter_updater<O: ObservationPort>(
        &mut self,
        observations: &mut O,
        request: &ManagedControlRequest,
        context: &ControlContext,
        deadline: Instant,
    ) -> Result<(Vec<KeyValue>, Option<DeviceObservationView>), ControlFailure> {
        let before = exact_observation(observations, &context.current_device_id)?;
        if normalized_mode(&before.mode) == "loader" {
            return Ok((lineage_facts("Loader", &before, context), Some(before)));
        }
        if normalized_mode(&before.mode) != "hdc-normal" {
            return Err(ControlFailure::WrongStartingMode);
        }
        let connect_key = self.exact_target(&before.serial_sha256, deadline)?;
        self.runner.run(
            &["-t", connect_key.as_str(), "target", "boot", "loader"],
            deadline,
        )?;

        let rebound = loop {
            if Instant::now() >= deadline {
                return Err(ControlFailure::NoUniqueLoaderRebind);
            }
            let current = observations
                .list()
                .map_err(|_| ControlFailure::CurrentObservationMissing)?;
            let detached = !current
                .iter()
                .any(|item| item.observation_id == before.observation_id);
            let hdc_detached = !self
                .list_targets(deadline)?
                .iter()
                .any(|row| row.connect_key == connect_key && row.state == "Connected");
            if detached
                && hdc_detached
                && let Some(rebound) = unique_mode_on_topology(
                    observations,
                    &current,
                    &context.profile_id,
                    &context.topology_sha256,
                    "loader",
                )?
            {
                break rebound;
            }
            std::thread::sleep(POLL_INTERVAL);
        };
        self.sessions.insert(request.job_id.clone(), connect_key);
        Ok((lineage_facts("Loader", &rebound, context), Some(rebound)))
    }

    fn reboot_to_normal<O: ObservationPort>(
        &mut self,
        observations: &mut O,
        request: &ManagedControlRequest,
        context: &ControlContext,
        deadline: Instant,
    ) -> Result<(Vec<KeyValue>, Option<DeviceObservationView>), ControlFailure> {
        let rebound = loop {
            if Instant::now() >= deadline {
                return Err(ControlFailure::NoUniqueNormalRebind);
            }
            let current = observations
                .list()
                .map_err(|_| ControlFailure::CurrentObservationMissing)?;
            if let Some(rebound) = unique_mode_on_topology(
                observations,
                &current,
                &context.profile_id,
                &context.topology_sha256,
                "hdc-normal",
            )? {
                break rebound;
            }
            std::thread::sleep(POLL_INTERVAL);
        };
        let connect_key = self.exact_target(&rebound.serial_sha256, deadline)?;
        self.sessions.insert(request.job_id.clone(), connect_key);
        Ok((lineage_facts("Normal", &rebound, context), Some(rebound)))
    }

    fn read_property<O: ObservationPort>(
        &mut self,
        observations: &mut O,
        request: &ManagedControlRequest,
        context: &ControlContext,
        deadline: Instant,
        property: &str,
    ) -> Result<(Vec<KeyValue>, Option<DeviceObservationView>), ControlFailure> {
        let current = exact_observation(observations, &context.current_device_id)?;
        if normalized_mode(&current.mode) != "hdc-normal" {
            return Err(ControlFailure::WrongStartingMode);
        }
        let connect_key = self.exact_target(&current.serial_sha256, deadline)?;
        self.sessions
            .insert(request.job_id.clone(), connect_key.clone());
        let stdout = self.runner.run(
            &[
                "-t",
                connect_key.as_str(),
                "shell",
                "param",
                "get",
                property,
            ],
            deadline,
        )?;
        let value = parse_property(&stdout, property)?;
        if let Some(expected) = request
            .expected_facts
            .iter()
            .find(|fact| fact.key == property)
            .map(|fact| fact.value.as_str())
            && value != expected
        {
            return Err(ControlFailure::PropertyMismatch);
        }
        Ok((
            vec![KeyValue {
                key: property.to_string(),
                value,
            }],
            None,
        ))
    }

    fn exact_target(
        &mut self,
        serial_sha256: &str,
        deadline: Instant,
    ) -> Result<String, ControlFailure> {
        if serial_sha256.is_empty() {
            return Err(ControlFailure::NoExactHdcTarget);
        }
        let matches = self
            .list_targets(deadline)?
            .into_iter()
            .filter(|row| row.transport == "USB" && row.state == "Connected")
            .filter(|row| {
                digest_in_domain(Domain::DeviceFacts, row.connect_key.as_bytes()).to_hex()
                    == serial_sha256
            })
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [one] => Ok(one.connect_key.clone()),
            [] => Err(ControlFailure::NoExactHdcTarget),
            _ => Err(ControlFailure::AmbiguousHdcTarget),
        }
    }

    fn list_targets(&mut self, deadline: Instant) -> Result<Vec<TargetRow>, ControlFailure> {
        parse_target_list(&self.runner.run(&["list", "targets", "-v"], deadline)?)
    }
}

fn exact_observation<O: ObservationPort>(
    observations: &mut O,
    device_id: &str,
) -> Result<DeviceObservationView, ControlFailure> {
    observations
        .list()
        .map_err(|_| ControlFailure::CurrentObservationMissing)?
        .into_iter()
        .find(|item| item.observation_id == device_id)
        .ok_or(ControlFailure::CurrentObservationMissing)
}

fn unique_mode_on_topology<O: ObservationPort>(
    observations: &mut O,
    current: &[DeviceObservationView],
    profile: &str,
    topology_sha256: &str,
    mode: &str,
) -> Result<Option<DeviceObservationView>, ControlFailure> {
    let mut matches = Vec::new();
    for candidate in current.iter().filter(|candidate| {
        candidate.topology_sha256 == topology_sha256 && normalized_mode(&candidate.mode) == mode
    }) {
        if observations
            .probe(&candidate.observation_id, profile)
            .is_ok()
        {
            matches.push(candidate.clone());
        }
    }
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.pop()),
        _ if mode == "loader" => Err(ControlFailure::NoUniqueLoaderRebind),
        _ => Err(ControlFailure::NoUniqueNormalRebind),
    }
}

fn normalized_mode(mode: &str) -> &str {
    match mode {
        "Loader" | "loader" => "loader",
        "HDCNormal" | "hdc-normal" | "normal" => "hdc-normal",
        other => other,
    }
}

fn lineage_facts(
    mode: &str,
    observation: &DeviceObservationView,
    context: &ControlContext,
) -> Vec<KeyValue> {
    vec![
        KeyValue {
            key: "mode".into(),
            value: mode.into(),
        },
        KeyValue {
            key: "stableIdentitySHA256".into(),
            value: if observation.serial_sha256.is_empty() {
                context.authority_stable_identity_sha256.clone()
            } else {
                observation.serial_sha256.clone()
            },
        },
        KeyValue {
            key: "usbTopology".into(),
            value: context.topology_sha256.clone(),
        },
    ]
}

fn parse_property(stdout: &[u8], requested: &str) -> Result<String, ControlFailure> {
    if stdout.len() > OUTPUT_LIMIT {
        return Err(ControlFailure::OutputTooLarge);
    }
    let text = std::str::from_utf8(stdout).map_err(|_| ControlFailure::InvalidUtf8)?;
    let trimmed = text.trim();
    let value = if let Some(remainder) = trimmed.strip_prefix(requested) {
        let remainder = remainder.trim_start_matches([' ', '\t']);
        remainder
            .strip_prefix('=')
            .map(str::trim)
            .unwrap_or(trimmed)
    } else {
        trimmed
    };
    if value.is_empty() {
        Err(ControlFailure::PropertyEmpty)
    } else {
        Ok(value.to_string())
    }
}

fn accepted_receipt(
    request: &ManagedControlRequest,
    mut facts: Vec<KeyValue>,
) -> SubmitManagedControlReceiptRequest {
    facts.sort_by(|left, right| left.key.cmp(&right.key));
    let evidence = canonical_facts_digest(&facts);
    SubmitManagedControlReceiptRequest {
        job_id: request.job_id.clone(),
        request_id: request.request_id.clone(),
        action: request.action,
        accepted: true,
        facts,
        evidence_sha256: evidence.as_bytes().to_vec(),
        failure_reason: String::new(),
    }
}

fn refused_receipt(
    request: &ManagedControlRequest,
    reason: &str,
) -> SubmitManagedControlReceiptRequest {
    SubmitManagedControlReceiptRequest {
        job_id: request.job_id.clone(),
        request_id: request.request_id.clone(),
        action: request.action,
        accepted: false,
        facts: Vec::new(),
        evidence_sha256: Vec::new(),
        failure_reason: reason.to_string(),
    }
}

fn deadline(epoch_ms: u64) -> Instant {
    let now_epoch_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    Instant::now() + Duration::from_millis(epoch_ms.saturating_sub(now_epoch_ms))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[derive(Debug)]
    struct ScriptedRunner {
        calls: Vec<Vec<String>>,
        replies: VecDeque<Result<Vec<u8>, ControlFailure>>,
    }

    impl CommandPort for ScriptedRunner {
        fn run(
            &mut self,
            arguments: &[&str],
            _deadline: Instant,
        ) -> Result<Vec<u8>, ControlFailure> {
            self.calls
                .push(arguments.iter().map(|value| value.to_string()).collect());
            self.replies.pop_front().expect("scripted HDC reply")
        }
    }

    #[derive(Debug)]
    struct ScriptedObservations {
        lists: VecDeque<Vec<DeviceObservationView>>,
    }

    impl ObservationPort for ScriptedObservations {
        fn list(&mut self) -> Result<Vec<DeviceObservationView>, CliError> {
            Ok(self.lists.pop_front().expect("scripted observation list"))
        }

        fn probe(&mut self, _device: &str, _profile: &str) -> Result<(), CliError> {
            Ok(())
        }
    }

    fn observation(id: &str, mode: &str, topology: &str, serial: &str) -> DeviceObservationView {
        DeviceObservationView {
            observation_id: id.into(),
            mode: mode.into(),
            topology_sha256: topology.into(),
            serial_sha256: digest_in_domain(Domain::DeviceFacts, serial.as_bytes()).to_hex(),
            serial_evidence_kind: "descriptor".into(),
            ..DeviceObservationView::default()
        }
    }

    fn request(action: ManagedControlAction) -> ManagedControlRequest {
        ManagedControlRequest {
            job_id: "JOB-1".into(),
            step_id: "STEP-1".into(),
            request_id: "REQ-1".into(),
            action,
            permit_id: "PERMIT-1".into(),
            expected_facts: Vec::new(),
            deadline_epoch_ms: u64::MAX,
        }
    }

    fn context() -> ControlContext {
        ControlContext {
            current_device_id: "NORMAL-1".into(),
            profile_id: "org.openharmony.dayu200@1.0.0".into(),
            authority_stable_identity_sha256: "stable".into(),
            topology_sha256: "topology".into(),
        }
    }

    #[test]
    fn strict_target_parser_accepts_only_registered_rows() {
        let rows = parse_target_list(b"serial\t\tUSB\tConnected\tlocalhost\n").unwrap();
        assert_eq!(rows.len(), 1);
        assert!(parse_target_list(b"serial USB Connected\n").is_err());
        assert_eq!(parse_target_list(b"[Empty]\n").unwrap(), Vec::new());
    }

    #[test]
    fn process_port_rehashes_the_exact_executable_before_every_call() {
        let executable = PathBuf::from("/bin/echo");
        let actual = arkforged::dispatch::executable_digest(&executable).unwrap();
        let mut exact = ProcessPort {
            executable: executable.clone(),
            working_directory: std::env::temp_dir(),
            expected_digest: actual,
        };
        let output = exact
            .run(&["ok"], Instant::now() + Duration::from_secs(1))
            .unwrap();
        assert_eq!(output, b"ok\n");

        let mut changed = ProcessPort {
            executable,
            working_directory: std::env::temp_dir(),
            expected_digest: arkforge_core::digest::sha256(b"replaced executable"),
        };
        assert_eq!(
            changed.run(&["must-not-run"], Instant::now() + Duration::from_secs(1)),
            Err(ControlFailure::CommandChanged)
        );
    }

    #[test]
    fn enter_updater_requires_command_detach_and_unique_loader_rebind() {
        let normal = observation("NORMAL-1", "hdc-normal", "topology", "serial");
        let loader = observation("LOADER-1", "loader", "topology", "loader-serial");
        let runner = ScriptedRunner {
            calls: Vec::new(),
            replies: VecDeque::from([
                Ok(b"serial\t\tUSB\tConnected\tlocalhost\n".to_vec()),
                Ok(Vec::new()),
                Ok(b"[Empty]\n".to_vec()),
            ]),
        };
        let mut port = HdcControlPort {
            runner,
            sessions: BTreeMap::new(),
        };
        let mut observations = ScriptedObservations {
            lists: VecDeque::from([vec![normal], vec![loader.clone()]]),
        };
        let result = port.perform_with(
            &mut observations,
            &request(ManagedControlAction::EnterUpdater),
            &context(),
        );
        assert!(result.receipt.accepted);
        assert_eq!(
            result.rebound_observation.unwrap().observation_id,
            "LOADER-1"
        );
        assert_eq!(
            port.runner.calls[1],
            ["-t", "serial", "target", "boot", "loader"]
        );
        let rendered = format!("{:?}", result.receipt);
        assert!(!rendered.contains("serial\""));
        assert!(!rendered.contains("argv"));
    }

    #[test]
    fn property_read_is_closed_exact_and_secret_free() {
        let normal = observation("NORMAL-1", "hdc-normal", "topology", "serial");
        let runner = ScriptedRunner {
            calls: Vec::new(),
            replies: VecDeque::from([
                Ok(b"serial\t\tUSB\tConnected\tlocalhost\n".to_vec()),
                Ok(b"const.product.model = ohos\n".to_vec()),
            ]),
        };
        let mut port = HdcControlPort {
            runner,
            sessions: BTreeMap::new(),
        };
        let mut observations = ScriptedObservations {
            lists: VecDeque::from([vec![normal]]),
        };
        let mut request = request(ManagedControlAction::ReadProductFacts);
        request.expected_facts.push(KeyValue {
            key: "const.product.model".into(),
            value: "ohos".into(),
        });
        let result = port.perform_with(&mut observations, &request, &context());
        assert!(result.receipt.accepted);
        assert_eq!(result.receipt.facts[0].value, "ohos");
        assert_eq!(
            port.runner.calls[1],
            [
                "-t",
                "serial",
                "shell",
                "param",
                "get",
                "const.product.model"
            ]
        );
        assert!(!format!("{:?}", result.receipt).contains("serial\""));
    }

    #[test]
    fn extra_or_replacement_targets_never_become_the_selected_target() {
        let serial_digest = digest_in_domain(Domain::DeviceFacts, b"serial").to_hex();
        let runner = ScriptedRunner {
            calls: Vec::new(),
            replies: VecDeque::from([Ok(b"other\t\tUSB\tConnected\tlocalhost\n".to_vec())]),
        };
        let mut port = HdcControlPort {
            runner,
            sessions: BTreeMap::new(),
        };
        assert_eq!(
            port.exact_target(&serial_digest, Instant::now() + Duration::from_secs(1)),
            Err(ControlFailure::NoExactHdcTarget)
        );
        assert_eq!(port.runner.calls.len(), 1);
    }
}
