
fn serve(
    mut listener: LocalListener,
    runtime_dir: &Path,
    daemon: &mut Child,
    mut session: SupervisorSession<'_>,
) -> Result<(), StandaloneError> {
    let mut job_cursors = BTreeMap::new();
    let mut target_lineages = BTreeMap::new();
    let mut last_drive = Instant::now() - Duration::from_secs(1);
    loop {
        if let Some(exit) = daemon
            .try_wait()
            .map_err(|error| internal("observe arkforged", error))?
        {
            return Err(StandaloneError::new(
                "MECHANICS_DAEMON_EXITED",
                format!("arkforged exited unexpectedly with {exit}."),
                10,
                true,
            ));
        }
        if last_drive.elapsed() >= Duration::from_millis(100) {
            if let Err(error) = drive_active_jobs(
                runtime_dir,
                session.epoch,
                session.pairing,
                session.hdc.as_deref_mut(),
                &mut job_cursors,
                &mut target_lineages,
            ) {
                eprintln!("arkforge supervisor: {}: {}", error.code, error.message);
            }
            last_drive = Instant::now();
        }
        match listener.accept() {
            Ok(mut stream) => {
                // A caller may time out, terminate, or abandon a response.
                // Its connection does not own the durable runtime or its jobs.
                // Handle this request once; never replay it after a failed reply.
                let handled = (|| -> Result<bool, StandaloneError> {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(10)))
                        .map_err(|error| internal("bound the supervisor request wait", error))?;
                    let Some(request) = read_frame(&mut stream)
                        .map_err(|error| internal("read supervisor request", error))?
                    else {
                        return Ok(false);
                    };
                    let (command, arguments) = decode_request(&request)?;
                    let mut public = PublicClient::connect(runtime_dir)?;
                    let runtime_info = public.runtime_info().clone();
                    let active_jobs = public
                        .job_list()?
                        .into_iter()
                        .filter(|job| !job.terminal)
                        .count();
                    let mut blockers = runtime_info.execution_blockers.clone();
                    if session.hdc_digest.is_none() {
                        blockers.push("AUTHORITY_HDC_UNBOUND".into());
                    }
                    if session.hardware_campaign.is_none()
                        && !authority_support::has_reviewed_support_records()
                    {
                        blockers.push("AUTHORITY_SUPPORT_UNPUBLISHED".into());
                    }
                    let authority_support_available = session.hdc_digest.is_some()
                        && (session.hardware_campaign.is_some()
                            || authority_support::has_reviewed_support_records());
                    let status = DaemonStatus {
                        supervisor_pid: std::process::id(),
                        daemon_pid: daemon.id(),
                        epoch: session.epoch,
                        protocol_major: runtime_info.protocol_major,
                        protocol_minor: runtime_info.protocol_minor,
                        daemon_version: runtime_info.daemon_version.clone(),
                        mechanics_ready: runtime_info.execution_ready,
                        authority_support_available,
                        hdc_bound: session.hdc_digest.is_some(),
                        hdc_sha256: session
                            .hdc_digest
                            .map_or_else(String::new, |digest| digest.to_hex()),
                        hardware_campaign: session
                            .hardware_campaign
                            .unwrap_or_default()
                            .to_string(),
                        active_jobs,
                        blockers,
                    };
                    match command.as_str() {
                        "verify-flash" => {
                            let result = verification::handle(
                                runtime_dir,
                                &mut public,
                                &mut session,
                                active_jobs,
                                &arguments,
                            )
                            .map(|result| result.encode());
                            write_reply(&mut stream, result)?;
                        }
                        "status" => write_reply(&mut stream, Ok(encode_status(&status)))?,
                        "stop" if active_jobs == 0 => {
                            write_reply(&mut stream, Ok(encode_status(&status)))?;
                            return Ok(true);
                        }
                        "stop" => {
                            write_reply(
                                &mut stream,
                                Err(StandaloneError::new(
                                    "ACTIVE_JOBS",
                                    format!(
                                        "The runtime has {active_jobs} active job(s); request cancellation and wait for a terminal state before stopping."
                                    ),
                                    6,
                                    true,
                                )),
                            )?;
                        }
                        "materialize-plan" => {
                            let result = handle_materialize(
                                runtime_dir,
                                &mut public,
                                &arguments,
                                session.hdc_digest,
                                session.hardware_campaign,
                            )
                            .map(|response| response.encode());
                            write_reply(&mut stream, result)?;
                        }
                        "assess-plan" => {
                            let result = handle_assess(
                                runtime_dir,
                                &mut public,
                                &arguments,
                                session.hdc_digest,
                                session.hardware_campaign,
                            )
                            .map(|response| response.encode());
                            write_reply(&mut stream, result)?;
                        }
                        "apply-plan" => {
                            let result = handle_apply(
                                runtime_dir,
                                &mut public,
                                session.epoch,
                                session.hdc_digest,
                                session.hardware_campaign,
                                &arguments,
                            );
                            write_reply(&mut stream, result)?;
                        }
                        "materialize-recovery-plan" => {
                            let result = handle_recovery_plan(
                                runtime_dir,
                                &mut public,
                                &arguments,
                                session.hdc_digest,
                                session.hardware_campaign,
                            )
                            .map(|response| response.encode());
                            write_reply(&mut stream, result)?;
                        }
                        "cancel-job" => {
                            let result = handle_cancel(runtime_dir, &arguments);
                            write_reply(&mut stream, result)?;
                        }
                        "reconcile-job" => {
                            let result = handle_reconcile(runtime_dir, &arguments);
                            write_reply(&mut stream, result)?;
                        }
                        _ => {
                            write_reply(
                                &mut stream,
                                Err(StandaloneError::new(
                                    "SUPERVISOR_REQUEST_INVALID",
                                    "The authority supervisor does not recognize this request.",
                                    2,
                                    false,
                                )),
                            )?;
                        }
                    }
                    Ok(false)
                })();
                match handled {
                    Ok(true) => return Ok(()),
                    Ok(false) => {}
                    Err(error) => {
                        eprintln!(
                            "arkforge supervisor client: {}: {}",
                            error.code, error.message
                        );
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(internal("accept a supervisor client", error)),
        }
    }
}

fn handle_cancel(runtime_dir: &Path, arguments: &[String]) -> Result<Vec<u8>, StandaloneError> {
    let [job_id, expected] = arguments else {
        return Err(StandaloneError::new(
            "SUPERVISOR_REQUEST_INVALID",
            "cancel-job requires a job id and expected journal sequence.",
            2,
            false,
        ));
    };
    JobId::new(job_id).map_err(|error| {
        StandaloneError::new(
            "SUPERVISOR_REQUEST_INVALID",
            format!("cancel-job requires a canonical job id: {error}"),
            2,
            false,
        )
    })?;
    let expected = expected.parse::<u64>().map_err(|_| {
        StandaloneError::new(
            "SUPERVISOR_REQUEST_INVALID",
            "cancel-job expected sequence is not an unsigned integer.",
            2,
            false,
        )
    })?;
    let mut controller = ControllerClient::connect(runtime_dir)?;
    let state = controller.cancel(job_id, expected)?;
    let mut out = Vec::new();
    wire::write_string(&mut out, 1, &state);
    Ok(out)
}

fn handle_reconcile(runtime_dir: &Path, arguments: &[String]) -> Result<Vec<u8>, StandaloneError> {
    let [job_id] = arguments else {
        return Err(StandaloneError::new(
            "SUPERVISOR_REQUEST_INVALID",
            "reconcile-job requires exactly one job id.",
            2,
            false,
        ));
    };
    JobId::new(job_id).map_err(|error| {
        StandaloneError::new(
            "SUPERVISOR_REQUEST_INVALID",
            format!("reconcile-job requires a canonical job id: {error}"),
            2,
            false,
        )
    })?;
    let mut controller = ControllerClient::connect(runtime_dir)?;
    Ok(controller.reconcile(job_id)?)
}

fn decode_single_string(
    input: &[u8],
    wanted: u32,
    context: &str,
) -> Result<String, StandaloneError> {
    let mut reader = wire::Reader::new(input);
    while let Some((field, value)) = reader
        .next_field()
        .map_err(|error| internal(&format!("decode {context}"), error))?
    {
        if field == wanted {
            return Ok(value.as_str(wanted).map_err(status_decode)?.to_string());
        }
    }
    Err(StandaloneError::new(
        "SUPERVISOR_RESPONSE_INVALID",
        format!("The supervisor returned no {context}."),
        10,
        false,
    ))
}

fn decode_reconcile(input: &[u8]) -> Result<ReconcileStatus, StandaloneError> {
    let mut status = ReconcileStatus {
        job_id: String::new(),
        verdict: String::new(),
        detail: String::new(),
        completeness: String::new(),
        possible_effects: Vec::new(),
        original_state: String::new(),
    };
    let mut reader = wire::Reader::new(input);
    while let Some((field, value)) = reader
        .next_field()
        .map_err(|error| internal("decode reconciliation", error))?
    {
        match field {
            1 => status.job_id = value.as_str(1).map_err(status_decode)?.to_string(),
            2 => status.verdict = value.as_str(2).map_err(status_decode)?.to_string(),
            3 => status.detail = value.as_str(3).map_err(status_decode)?.to_string(),
            4 => status.completeness = value.as_str(4).map_err(status_decode)?.to_string(),
            5 => status
                .possible_effects
                .push(value.as_str(5).map_err(status_decode)?.to_string()),
            6 => status.original_state = value.as_str(6).map_err(status_decode)?.to_string(),
            _ => {}
        }
    }
    if status.job_id.is_empty() || status.verdict.is_empty() {
        return Err(StandaloneError::new(
            "SUPERVISOR_RESPONSE_INVALID",
            "The supervisor returned an incomplete reconciliation.",
            10,
            false,
        ));
    }
    Ok(status)
}

fn handle_materialize(
    runtime_dir: &Path,
    public: &mut PublicClient,
    arguments: &[String],
    hdc_digest: Option<Sha256Digest>,
    hardware_campaign: Option<&str>,
) -> Result<MaterializePlanResponse, StandaloneError> {
    handle_materialize_for(
        runtime_dir,
        public,
        arguments,
        MaterializationKind {
            execution_purpose: "primaryFlash",
            supersedes_job_id: "",
            assessment_only: false,
        },
        hdc_digest,
        hardware_campaign,
    )
}

fn handle_assess(
    runtime_dir: &Path,
    public: &mut PublicClient,
    arguments: &[String],
    hdc_digest: Option<Sha256Digest>,
    hardware_campaign: Option<&str>,
) -> Result<MaterializePlanResponse, StandaloneError> {
    handle_materialize_for(
        runtime_dir,
        public,
        arguments,
        MaterializationKind {
            execution_purpose: "primaryFlash",
            supersedes_job_id: "",
            assessment_only: true,
        },
        hdc_digest,
        hardware_campaign,
    )
}

fn handle_recovery_plan(
    runtime_dir: &Path,
    public: &mut PublicClient,
    arguments: &[String],
    hdc_digest: Option<Sha256Digest>,
    hardware_campaign: Option<&str>,
) -> Result<MaterializePlanResponse, StandaloneError> {
    let [job_id, artifact, profile, device] = arguments else {
        return Err(StandaloneError::new(
            "SUPERVISOR_REQUEST_INVALID",
            "materialize-recovery-plan requires job, artifact, profile, and device.",
            2,
            false,
        ));
    };
    JobId::new(job_id).map_err(|error| {
        StandaloneError::new(
            "SUPERVISOR_REQUEST_INVALID",
            format!("materialize-recovery-plan requires a canonical job id: {error}"),
            2,
            false,
        )
    })?;
    let mut controller = ControllerClient::connect(runtime_dir)?;
    recovery::require_source(
        runtime_dir,
        public,
        &mut controller,
        job_id,
        profile,
        device,
    )?;
    let response = handle_materialize_for(
        runtime_dir,
        public,
        &[artifact.clone(), profile.clone(), device.clone()],
        MaterializationKind {
            execution_purpose: "supersedingRecovery",
            supersedes_job_id: job_id,
            assessment_only: false,
        },
        hdc_digest,
        hardware_campaign,
    )?;
    if let MaterializePlanResponse::Plan(plan) = &response {
        recovery::authorize(runtime_dir, public, &mut controller, plan)?;
    }
    Ok(response)
}

struct MaterializationKind<'a> {
    execution_purpose: &'a str,
    supersedes_job_id: &'a str,
    assessment_only: bool,
}
