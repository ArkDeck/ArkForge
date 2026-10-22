
fn handle_materialize_for(
    runtime_dir: &Path,
    public: &mut PublicClient,
    arguments: &[String],
    kind: MaterializationKind<'_>,
    hdc_digest: Option<Sha256Digest>,
    hardware_campaign: Option<&str>,
) -> Result<MaterializePlanResponse, StandaloneError> {
    let (artifact, profile, device, intent, source_job) = match arguments {
        [artifact, profile, device] => (artifact, profile, device, "fullRestore", None),
        [artifact, profile, device, intent] => (artifact, profile, device, intent.as_str(), None),
        [artifact, profile, device, intent, source] if intent == "restoreRamdisk" => (
            artifact,
            profile,
            device,
            intent.as_str(),
            Some(source.as_str()),
        ),
        _ => {
            return Err(StandaloneError::new(
                "SUPERVISOR_REQUEST_INVALID",
                "materialize-plan requires artifact, profile, and exact device observation.",
                2,
                false,
            ));
        }
    };
    if !matches!(intent, "fullRestore" | "restoreRamdisk")
        || (intent == "restoreRamdisk"
            && (hardware_campaign.is_none()
                || profile != "org.openharmony.dayu200@1.0.0"
                || kind.execution_purpose != "primaryFlash"))
    {
        return Err(StandaloneError::invalid(
            "Ramdisk restoration requires a primary DAYU200 hardware campaign.",
        ));
    }
    let observations = public.device_list()?;
    let observation = observations
        .iter()
        .find(|candidate| candidate.observation_id == *device)
        .ok_or_else(|| {
            StandaloneError::new(
                "OBSERVATION_NOT_FOUND",
                format!("No current observation exactly matches {device}."),
                5,
                true,
            )
        })?;
    let probe = public.device_probe(device, profile)?;
    if let Some(source_job) = source_job {
        restoration::require_source(
            runtime_dir,
            public,
            source_job,
            observation,
            &probe,
            hardware_campaign.unwrap_or_default(),
        )?;
    }
    let stable_identity = stable_identity_digest(observation, &probe);
    let binding_id = format!("CLI-BIND-{}", &stable_identity.to_hex()[..24]);
    if !kind.assessment_only {
        persist_binding(
            runtime_dir,
            &binding_id,
            device,
            profile,
            &stable_identity.to_hex(),
        )?;
    }
    let toolchain_id = public.runtime_info().toolchain_id.clone();
    let mut controller = ControllerClient::connect(runtime_dir)?;
    let pending_support_key = sha256(b"arkforge.cli-authority-support-pending");
    let first = controller.materialize_plan(&MaterializeInput {
        intent,
        artifact_id: artifact,
        profile_id: profile,
        device_id: device,
        toolchain_id: &toolchain_id,
        authority_namespace: "arkforge.cli",
        binding_id: &binding_id,
        binding_revision: 1,
        stable_identity_sha256: stable_identity.as_bytes(),
        execution_purpose: kind.execution_purpose,
        authority_support_key_sha256: pending_support_key.as_bytes(),
        authority_support_state: "hardwareGated",
        authority_support_detail: "the exact mechanics maturity key has not been returned yet",
    })?;
    let mechanics_key = match &first {
        MaterializePlanResponse::Assessment(assessment) => {
            parse_mechanics_key(&assessment.mechanics_maturity_key_sha256)?
        }
        MaterializePlanResponse::Plan(_) => {
            return Err(StandaloneError::new(
                "AUTHORITY_GATE_BYPASSED",
                "The daemon produced an executable plan for a hardware-gated authority binding.",
                10,
                false,
            ));
        }
    };
    let (support_key, support_state) =
        current_authority_support(mechanics_key, hdc_digest, hardware_campaign)?;
    let MaterializePlanResponse::Assessment(mut first_assessment) = first else {
        unreachable!("the pending support gate above requires an assessment")
    };
    first_assessment.authority_support_key_sha256 = support_key.to_hex();
    first_assessment.authority_support_state = support_state.as_str().to_string();
    if !support_state.permits_execution() {
        first_assessment.unavailable_reason = format!(
            "authority support is {} for exact key {}: {} Mechanics state {} does not bypass this independent gate.",
            support_state.as_str(),
            support_key,
            support_state
                .blocker()
                .unwrap_or("no reviewed support record"),
            first_assessment.mechanics_maturity_state,
        );
        return Ok(MaterializePlanResponse::Assessment(first_assessment));
    }
    if kind.assessment_only {
        close_resolved_authority_blocker(&mut first_assessment);
        return Ok(MaterializePlanResponse::Assessment(first_assessment));
    }
    let support_detail = support_state
        .campaign()
        .or_else(|| support_state.blocker())
        .unwrap_or_default();
    let response = controller.materialize_plan(&MaterializeInput {
        intent,
        artifact_id: artifact,
        profile_id: profile,
        device_id: device,
        toolchain_id: &toolchain_id,
        authority_namespace: "arkforge.cli",
        binding_id: &binding_id,
        binding_revision: 1,
        stable_identity_sha256: stable_identity.as_bytes(),
        execution_purpose: kind.execution_purpose,
        authority_support_key_sha256: support_key.as_bytes(),
        authority_support_state: support_state.as_str(),
        authority_support_detail: support_detail,
    })?;
    if let MaterializePlanResponse::Plan(plan) = &response {
        require_authority_support(plan, support_key, &support_state)?;
        persist_authority_plan(
            runtime_dir,
            &AuthorityPlanRecord {
                plan: plan.clone(),
                binding_id,
                stable_identity_sha256: stable_identity.as_bytes().to_vec(),
                device_id: device.clone(),
                profile_id: profile.clone(),
                supersedes_job_id: kind.supersedes_job_id.to_string(),
                topology_sha256: observation.topology_sha256.clone(),
                toolchain_id,
            },
        )?;
    }
    Ok(response)
}

fn close_resolved_authority_blocker(assessment: &mut arkforge_ipc::messages::Assessment) {
    assessment
        .unknowns
        .retain(|unknown| unknown.key != "RK-A01");
    assessment
        .evidence_requirements
        .retain(|requirement| requirement.key != "EVR-RK-A01");
    if assessment.unknowns.is_empty()
        && matches!(
            assessment.mechanics_maturity_state.as_str(),
            "productionVerified" | "hardwareCampaign"
        )
    {
        assessment.availability = "available".into();
        assessment.unavailable_reason.clear();
    }
}

fn parse_mechanics_key(value: &str) -> Result<Sha256Digest, StandaloneError> {
    Sha256Digest::parse_hex(value).map_err(|error| {
        StandaloneError::new(
            "MECHANICS_MATURITY_KEY_INVALID",
            format!("The materialization carries no usable mechanics maturity key: {error}"),
            10,
            false,
        )
    })
}

fn current_authority_support(
    mechanics: Sha256Digest,
    hdc_digest: Option<Sha256Digest>,
    hardware_campaign: Option<&str>,
) -> Result<(Sha256Digest, AuthoritySupportState), StandaloneError> {
    let executable =
        std::env::current_exe().map_err(|error| internal("identify arkforge", error))?;
    let implementation = arkforged::dispatch::executable_digest(&executable)
        .map_err(|error| internal("digest the CLI authority build", error))?;
    let key = AuthoritySupportKey::for_running_build(
        implementation,
        mechanics,
        hdc_digest.unwrap_or_else(|| sha256(b"arkforge.cli-hdc-unbound")),
    );
    let key_digest = key
        .digest()
        .map_err(|error| StandaloneError::new("AUTHORITY_SUPPORT_KEY_INVALID", error, 10, false))?;
    let state = match hdc_digest {
        Some(_) => authority_support::classify(&key, hardware_campaign),
        None => AuthoritySupportState::HardwareGated {
            blocker: "No exact HDC executable and digest are bound to this authority runtime."
                .into(),
        },
    };
    Ok((key_digest, state))
}

fn require_authority_support(
    plan: &ExecutablePlan,
    expected_key: Sha256Digest,
    expected_state: &AuthoritySupportState,
) -> Result<(), StandaloneError> {
    let actual_key =
        Sha256Digest::parse_hex(&plan.authority_support_key_sha256).map_err(|error| {
            StandaloneError::new(
                "AUTHORITY_SUPPORT_KEY_INVALID",
                format!("The executable plan carries no usable authority support key: {error}"),
                10,
                false,
            )
        })?;
    let expected_campaign = expected_state.campaign().unwrap_or_default();
    if actual_key != expected_key
        || plan.authority_support_state != expected_state.as_str()
        || plan.authority_support_campaign != expected_campaign
    {
        return Err(StandaloneError::new(
            "AUTHORITY_SUPPORT_SEAL_MISMATCH",
            "The daemon did not seal the exact authority support key and state supplied by this supervisor.",
            10,
            false,
        ));
    }
    Ok(())
}

fn require_current_authority_support(
    plan: &ExecutablePlan,
    hdc_digest: Option<Sha256Digest>,
    hardware_campaign: Option<&str>,
) -> Result<(), StandaloneError> {
    let mechanics_key = parse_mechanics_key(&plan.mechanics_maturity_key_sha256)?;
    let (support_key, support_state) =
        current_authority_support(mechanics_key, hdc_digest, hardware_campaign)?;
    if !support_state.permits_execution() {
        return Err(StandaloneError::new(
            "AUTHORITY_SUPPORT_UNAVAILABLE",
            support_state
                .blocker()
                .unwrap_or("The current CLI authority combination is not executable."),
            3,
            false,
        ));
    }
    require_authority_support(plan, support_key, &support_state)
}

fn handle_apply(
    runtime_dir: &Path,
    public: &mut PublicClient,
    epoch: u64,
    hdc_digest: Option<Sha256Digest>,
    hardware_campaign: Option<&str>,
    arguments: &[String],
) -> Result<Vec<u8>, StandaloneError> {
    if arguments.len() < 3 {
        return Err(StandaloneError::new(
            "SUPERVISOR_REQUEST_INVALID",
            "apply-plan requires plan id, expected digest, detach disposition, and acknowledgements.",
            2,
            false,
        ));
    }
    let plan_id = &arguments[0];
    PlanId::new(plan_id).map_err(|error| {
        StandaloneError::new(
            "SUPERVISOR_REQUEST_INVALID",
            format!("apply-plan requires a canonical plan id: {error}"),
            2,
            false,
        )
    })?;
    let expected_digest = &arguments[1];
    let detach = match arguments[2].as_str() {
        "true" => true,
        "false" => false,
        _ => {
            return Err(StandaloneError::new(
                "SUPERVISOR_REQUEST_INVALID",
                "apply-plan detach disposition must be true or false.",
                2,
                false,
            ));
        }
    };
    let record = load_authority_plan(runtime_dir, plan_id)?;
    if record.plan.plan_sha256 != *expected_digest {
        return Err(StandaloneError::new(
            "PLAN_DIGEST_MISMATCH",
            format!(
                "Plan {} is sealed as {}, not caller expectation {}.",
                record.plan.plan_id, record.plan.plan_sha256, expected_digest
            ),
            4,
            false,
        ));
    }
    let supplied: std::collections::BTreeSet<String> = arguments[3..].iter().cloned().collect();
    if supplied.len() != arguments[3..].len() {
        return Err(StandaloneError::new(
            "UNEXPECTED_ACKNOWLEDGEMENT",
            "Each acknowledgement token must be supplied exactly once.",
            4,
            false,
        ));
    }
    let required: std::collections::BTreeSet<String> =
        record_acknowledgements(&record).into_iter().collect();
    if supplied != required {
        let missing = required.difference(&supplied).cloned().collect::<Vec<_>>();
        let unexpected = supplied.difference(&required).cloned().collect::<Vec<_>>();
        return Err(StandaloneError::new(
            if !missing.is_empty() {
                "ACKNOWLEDGEMENT_REQUIRED"
            } else {
                "UNEXPECTED_ACKNOWLEDGEMENT"
            },
            format!(
                "Acknowledgement set is not exact; missing=[{}], unexpected=[{}].",
                missing.join(","),
                unexpected.join(",")
            ),
            4,
            !missing.is_empty(),
        )
        .with_required_acknowledgements(missing));
    }
    require_current_authority_support(&record.plan, hdc_digest, hardware_campaign)?;
    if public.runtime_info().toolchain_id != record.toolchain_id {
        return Err(StandaloneError::new(
            "MECHANICS_RUNTIME_CHANGED",
            "The running mechanics toolchain differs from the one sealed by this authority plan; materialize a new plan.",
            3,
            false,
        ));
    }
    let now = arkforged::rescue::now_epoch_ms()?;
    if now >= record.plan.expires_at_epoch_ms {
        return Err(StandaloneError::new(
            "PLAN_EXPIRED",
            format!("Plan {plan_id} expired before apply."),
            3,
            false,
        ));
    }
    let mut controller = ControllerClient::connect(runtime_dir)?;
    recovery::before_start(runtime_dir, public, &mut controller, &record, now)?;
    let session = format!("CLI-SESSION-{epoch}");
    let job_id = controller.start_execution(
        plan_id,
        expected_digest,
        &record.plan.execution_purpose,
        &session,
    )?;
    persist_job_lineage(
        runtime_dir,
        &job_id,
        &ActiveTargetLineage {
            plan_id: record.plan.plan_id.clone(),
            current_device_id: record.device_id.clone(),
            current_stable_identity_sha256: record.stable_identity_sha256.clone(),
            topology_sha256: record.topology_sha256.clone(),
            revision: 1,
        },
    )?;
    recovery::correlate(runtime_dir, &record, &job_id)?;
    let mut out = Vec::new();
    wire::write_string(&mut out, 1, &job_id);
    wire::write_bool(&mut out, 2, detach);
    Ok(out)
}

/// Stable acknowledgement tokens required before an application may submit
/// this plan. Presentation layers render the tokens but never derive them.
pub fn required_acknowledgements(plan: &ExecutablePlan) -> Vec<String> {
    if plan
        .persistent_effects
        .iter()
        .any(|effect| effect.target == "userdata")
    {
        return vec!["data-loss:userdata".into()];
    }
    plan.persistent_effects
        .iter()
        .map(|effect| format!("overwrite:partition={}", effect.target))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn record_acknowledgements(record: &AuthorityPlanRecord) -> Vec<String> {
    let mut tokens = required_acknowledgements(&record.plan);
    if !record.supersedes_job_id.is_empty() {
        tokens.push(format!(
            "recovery:supersedes-job={}",
            record.supersedes_job_id
        ));
    }
    tokens.sort();
    tokens
}

fn authority_plan_path(runtime_dir: &Path, plan_id: &str) -> PathBuf {
    runtime_dir
        .join("authority")
        .join("plans")
        .join(format!("{}.plan", sha256(plan_id.as_bytes())))
}
