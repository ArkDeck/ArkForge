
fn job_lineage_path(runtime_dir: &Path, job_id: &str) -> PathBuf {
    runtime_dir
        .join("authority")
        .join("jobs")
        .join(format!("{}.binding", sha256(job_id.as_bytes())))
}

fn encode_job_lineage(lineage: &ActiveTargetLineage) -> Vec<u8> {
    let mut out = Vec::new();
    wire::write_string(&mut out, 1, &lineage.plan_id);
    wire::write_string(&mut out, 2, &lineage.current_device_id);
    wire::write_bytes(&mut out, 3, &lineage.current_stable_identity_sha256);
    wire::write_string(&mut out, 4, &lineage.topology_sha256);
    wire::write_uint64(&mut out, 5, lineage.revision);
    out
}

fn persist_job_lineage(
    runtime_dir: &Path,
    job_id: &str,
    lineage: &ActiveTargetLineage,
) -> Result<(), StandaloneError> {
    let path = job_lineage_path(runtime_dir, job_id);
    let root = path.parent().expect("job lineage path has parent");
    std::fs::create_dir_all(root)
        .map_err(|error| internal("create the authority job journal", error))?;
    protect_path(root, true)
        .map_err(|error| internal("protect the authority job journal", error))?;
    let encoded = encode_job_lineage(lineage);
    if path.exists() {
        let existing = load_job_lineage(runtime_dir, job_id)?;
        if existing.revision > lineage.revision {
            return Err(StandaloneError::new(
                "TARGET_LINEAGE_STALE",
                "A stale target-lineage revision cannot replace newer durable evidence.",
                6,
                false,
            ));
        }
        if existing.revision == lineage.revision {
            if existing == *lineage {
                return Ok(());
            }
            return Err(StandaloneError::new(
                "TARGET_LINEAGE_CONFLICT",
                "The same target-lineage revision already names different facts.",
                6,
                false,
            ));
        }
    }
    let temporary = root.join(format!(
        ".{}.{}.{}.tmp",
        sha256(job_id.as_bytes()),
        lineage.revision,
        std::process::id(),
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| internal("create a target-lineage transaction", error))?;
    protect_path(&temporary, false)
        .map_err(|error| internal("protect a target-lineage transaction", error))?;
    file.write_all(&encoded)
        .and_then(|_| file.sync_all())
        .map_err(|error| internal("commit target-lineage bytes", error))?;
    replace_file(&temporary, &path)
        .map_err(|error| internal("publish target-lineage revision", error))
}

fn load_job_lineage(
    runtime_dir: &Path,
    job_id: &str,
) -> Result<ActiveTargetLineage, StandaloneError> {
    let encoded = std::fs::read(job_lineage_path(runtime_dir, job_id))
        .map_err(|error| internal("read the authority job lineage", error))?;
    let mut lineage = ActiveTargetLineage {
        plan_id: String::new(),
        current_device_id: String::new(),
        current_stable_identity_sha256: Vec::new(),
        topology_sha256: String::new(),
        revision: 0,
    };
    let mut reader = wire::Reader::new(&encoded);
    while let Some((field, value)) = reader
        .next_field()
        .map_err(|error| internal("decode the authority job lineage", error))?
    {
        match field {
            1 => lineage.plan_id = value.as_str(1).map_err(status_decode)?.to_string(),
            2 => lineage.current_device_id = value.as_str(2).map_err(status_decode)?.to_string(),
            3 => {
                lineage.current_stable_identity_sha256 =
                    value.as_bytes().map_err(status_decode)?.to_vec()
            }
            4 => lineage.topology_sha256 = value.as_str(4).map_err(status_decode)?.to_string(),
            5 => lineage.revision = value.as_u64().map_err(status_decode)?,
            _ => {}
        }
    }
    if lineage.plan_id.is_empty()
        || lineage.current_device_id.is_empty()
        || lineage.current_stable_identity_sha256.len() != 32
        || lineage.topology_sha256.is_empty()
        || lineage.revision == 0
    {
        return Err(StandaloneError::new(
            "TARGET_LINEAGE_INVALID",
            "The durable target lineage is incomplete.",
            10,
            false,
        ));
    }
    Ok(lineage)
}

fn encode_authority_plan(record: &AuthorityPlanRecord) -> Vec<u8> {
    let mut out = Vec::new();
    wire::write_message(&mut out, 1, &record.plan.encode());
    wire::write_string(&mut out, 2, &record.binding_id);
    wire::write_bytes(&mut out, 3, &record.stable_identity_sha256);
    wire::write_string(&mut out, 4, &record.device_id);
    wire::write_string(&mut out, 5, &record.profile_id);
    wire::write_string(&mut out, 6, &record.supersedes_job_id);
    wire::write_string(&mut out, 7, &record.topology_sha256);
    wire::write_string(&mut out, 8, &record.toolchain_id);
    out
}

fn persist_authority_plan(
    runtime_dir: &Path,
    record: &AuthorityPlanRecord,
) -> Result<(), StandaloneError> {
    let path = authority_plan_path(runtime_dir, &record.plan.plan_id);
    let root = path.parent().expect("plan path has parent");
    std::fs::create_dir_all(root)
        .map_err(|error| internal("create the authority plan journal", error))?;
    protect_path(root, true)
        .map_err(|error| internal("protect the authority plan journal", error))?;
    let encoded = encode_authority_plan(record);
    if path.exists() {
        let existing = std::fs::read(&path)
            .map_err(|error| internal("read the authority plan journal", error))?;
        if existing != encoded {
            return Err(StandaloneError::new(
                "PLAN_STATE_CONFLICT",
                format!(
                    "Authority plan {} already exists with different bytes.",
                    record.plan.plan_id
                ),
                6,
                false,
            ));
        }
        return Ok(());
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| internal("create the authority plan journal", error))?;
    protect_path(&path, false)
        .map_err(|error| internal("protect the authority plan journal", error))?;
    file.write_all(&encoded)
        .and_then(|_| file.sync_all())
        .map_err(|error| internal("commit the authority plan journal", error))
}

fn load_authority_plan(
    runtime_dir: &Path,
    plan_id: &str,
) -> Result<AuthorityPlanRecord, StandaloneError> {
    let path = authority_plan_path(runtime_dir, plan_id);
    let encoded = std::fs::read(&path).map_err(|error| {
        StandaloneError::new(
            "PLAN_NOT_FOUND",
            format!(
                "Cannot read authority plan {plan_id} at {}: {error}",
                path.display()
            ),
            5,
            false,
        )
    })?;
    let mut plan = None;
    let mut binding_id = String::new();
    let mut stable_identity_sha256 = Vec::new();
    let mut device_id = String::new();
    let mut profile_id = String::new();
    let mut supersedes_job_id = String::new();
    let mut topology_sha256 = String::new();
    let mut toolchain_id = String::new();
    let mut reader = wire::Reader::new(&encoded);
    while let Some((field, value)) = reader
        .next_field()
        .map_err(|error| internal("decode the authority plan journal", error))?
    {
        match field {
            1 => {
                plan = Some(
                    ExecutablePlan::decode(value.as_bytes().map_err(status_decode)?)
                        .map_err(status_decode)?,
                )
            }
            2 => binding_id = value.as_str(2).map_err(status_decode)?.to_string(),
            3 => stable_identity_sha256 = value.as_bytes().map_err(status_decode)?.to_vec(),
            4 => device_id = value.as_str(4).map_err(status_decode)?.to_string(),
            5 => profile_id = value.as_str(5).map_err(status_decode)?.to_string(),
            6 => supersedes_job_id = value.as_str(6).map_err(status_decode)?.to_string(),
            7 => topology_sha256 = value.as_str(7).map_err(status_decode)?.to_string(),
            8 => toolchain_id = value.as_str(8).map_err(status_decode)?.to_string(),
            _ => {}
        }
    }
    let record = AuthorityPlanRecord {
        plan: plan.ok_or_else(|| internal("decode the authority plan journal", "missing plan"))?,
        binding_id,
        stable_identity_sha256,
        device_id,
        profile_id,
        supersedes_job_id,
        topology_sha256,
        toolchain_id,
    };
    if record.plan.plan_id != plan_id
        || record.binding_id.is_empty()
        || record.stable_identity_sha256.len() != 32
        || record.topology_sha256.is_empty()
        || record.toolchain_id.is_empty()
    {
        return Err(StandaloneError::new(
            "PLAN_STATE_INVALID",
            format!("Authority plan journal for {plan_id} is incomplete or mismatched."),
            10,
            false,
        ));
    }
    Ok(record)
}

fn drive_active_jobs(
    runtime_dir: &Path,
    epoch: u64,
    pairing: &ControllerPairingSecret,
    mut hdc: Option<&mut HdcControlPort>,
    cursors: &mut BTreeMap<String, u64>,
    target_lineages: &mut BTreeMap<String, ActiveTargetLineage>,
) -> Result<(), StandaloneError> {
    let mut public = PublicClient::connect(runtime_dir)?;
    recovery::finalize(runtime_dir, &mut public)?;
    let active = public
        .job_list()?
        .into_iter()
        .filter(|job| !job.terminal)
        .collect::<Vec<_>>();
    if active.is_empty() {
        return Ok(());
    }
    let mut controller = ControllerClient::connect(runtime_dir)?;
    for job in active {
        let mut cursor = *cursors.get(&job.job_id).unwrap_or(&0);
        let events = controller.job_events(&job.job_id, cursor)?;
        for event in events {
            match event.kind {
                JobEventKind::StepAdmissionRequested => {
                    let snapshot = event.admission.as_ref().ok_or_else(|| {
                        StandaloneError::new(
                            "ADMISSION_EVENT_INVALID",
                            "A stepAdmissionRequested event contains no admission snapshot.",
                            10,
                            false,
                        )
                    })?;
                    answer_admission(
                        runtime_dir,
                        epoch,
                        pairing,
                        &mut public,
                        &mut controller,
                        snapshot,
                        target_lineages,
                    )?;
                }
                JobEventKind::ManagedControlRequested => {
                    let request = event.control_request.as_ref().ok_or_else(|| {
                        StandaloneError::new(
                            "CONTROL_EVENT_INVALID",
                            "A managedControlRequested event contains no typed request.",
                            10,
                            false,
                        )
                    })?;
                    if let Some(port) = hdc.as_deref_mut() {
                        let record = load_authority_plan(runtime_dir, &job.plan_id)?;
                        if let Err(error) = recovery::before_effect(
                            runtime_dir,
                            &mut public,
                            &mut controller,
                            &record,
                            &job.job_id,
                        ) {
                            submit_control_refusal(&mut controller, request, &error.message)?;
                            cursor = event.sequence;
                            cursors.insert(job.job_id.clone(), cursor);
                            continue;
                        }
                        let lineage = match target_lineages.get(&job.job_id) {
                            Some(lineage) => lineage.clone(),
                            None => {
                                let loaded = load_job_lineage(runtime_dir, &job.job_id)?;
                                target_lineages.insert(job.job_id.clone(), loaded.clone());
                                loaded
                            }
                        };
                        if lineage.plan_id != record.plan.plan_id
                            || lineage.topology_sha256 != record.topology_sha256
                        {
                            return Err(StandaloneError::new(
                                "TARGET_LINEAGE_CONFLICT",
                                "The active target lineage does not belong to the job's sealed plan.",
                                6,
                                false,
                            ));
                        }
                        let result = port.perform(
                            &mut public,
                            request,
                            &ControlContext {
                                current_device_id: lineage.current_device_id.clone(),
                                profile_id: record.profile_id.clone(),
                                authority_stable_identity_sha256: digest_bytes(
                                    &record.stable_identity_sha256,
                                    "authority stable identity",
                                )?
                                .to_hex(),
                                topology_sha256: lineage.topology_sha256.clone(),
                            },
                        );
                        if result.receipt.accepted
                            && let Some(rebound) = result.rebound_observation
                        {
                            let current_stable = current_stable_identity(
                                &mut public,
                                &rebound.observation_id,
                                &record.profile_id,
                            )?;
                            let advanced = ActiveTargetLineage {
                                plan_id: lineage.plan_id,
                                current_device_id: rebound.observation_id,
                                current_stable_identity_sha256: current_stable.as_bytes().to_vec(),
                                topology_sha256: lineage.topology_sha256,
                                revision: lineage.revision.saturating_add(1),
                            };
                            persist_job_lineage(runtime_dir, &job.job_id, &advanced)?;
                            target_lineages.insert(job.job_id.clone(), advanced);
                        }
                        submit_control_receipt(&mut controller, &result.receipt)?;
                    } else {
                        submit_control_refusal(
                            &mut controller,
                            request,
                            "No exact HDC executable is bound to this authority runtime.",
                        )?;
                    }
                }
                _ => {}
            }
            cursor = event.sequence;
            cursors.insert(job.job_id.clone(), cursor);
        }
    }
    Ok(())
}

fn submit_control_receipt(
    controller: &mut ControllerClient,
    receipt: &SubmitManagedControlReceiptRequest,
) -> Result<(), StandaloneError> {
    let outcome = controller.submit_control_receipt(receipt)?;
    if !outcome.accepted {
        return Err(StandaloneError::new(
            outcome.rejection_code,
            outcome.rejection_message,
            3,
            false,
        ));
    }
    Ok(())
}
