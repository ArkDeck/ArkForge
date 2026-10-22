use super::*;

pub(super) fn append_stable_observation(
    stable: &mut Vec<u8>,
    source: &str,
    observation: &DeviceObservationView,
) {
    append_stable_field(stable, "observation.source", source.as_bytes());
    append_stable_field(
        stable,
        "observation.id",
        observation.observation_id.as_bytes(),
    );
    append_stable_field(stable, "observation.mode", observation.mode.as_bytes());
    append_stable_field(
        stable,
        "observation.topology",
        observation.topology_sha256.as_bytes(),
    );
    append_stable_field(
        stable,
        "observation.descriptor",
        observation.descriptor_sha256.as_bytes(),
    );
    append_stable_field(
        stable,
        "observation.serial",
        observation.serial_sha256.as_bytes(),
    );
    append_stable_field(
        stable,
        "observation.serial-evidence-kind",
        observation.serial_evidence_kind.as_bytes(),
    );
    append_stable_field(
        stable,
        "observation.identity-strength",
        observation.identity_strength.as_bytes(),
    );
    append_stable_field(
        stable,
        "observation.malformed-descriptor",
        &[u8::from(observation.malformed_descriptor)],
    );
    for fact in &observation.protocol_identity {
        append_stable_field(stable, "observation.fact.key", fact.key.as_bytes());
        append_stable_field(stable, "observation.fact.value", fact.value.as_bytes());
    }
}

pub(super) fn append_stable_field(stable: &mut Vec<u8>, label: &str, value: &[u8]) {
    stable.extend_from_slice(&(label.len() as u64).to_be_bytes());
    stable.extend_from_slice(label.as_bytes());
    stable.extend_from_slice(&(value.len() as u64).to_be_bytes());
    stable.extend_from_slice(value);
}

pub(super) fn recompute_admitted_device_digest(
    snapshot: &StepAdmissionSnapshot,
) -> Result<Sha256Digest, StandaloneError> {
    use arkforge_transport::{
        DeviceObservation, IdentityEvidenceStrength, ProtocolIdentityFact, SerialEvidence,
    };
    let serial_evidence = match snapshot.serial_evidence_kind.as_str() {
        "absent" if snapshot.serial_sha256.is_empty() => SerialEvidence::Absent,
        "descriptor" => SerialEvidence::Descriptor {
            digest: digest_bytes(&snapshot.serial_sha256, "serial digest")?,
        },
        "protocolUnique" => SerialEvidence::ProtocolUnique {
            digest: digest_bytes(&snapshot.serial_sha256, "serial digest")?,
        },
        _ => return Err(permit_invalid("admission serial evidence is malformed")),
    };
    let observation = DeviceObservation {
        observation_id: arkforge_core::ids::ObservationId::new("CLI-ADMISSION-SNAPSHOT")
            .map_err(permit_invalid)?,
        observed_at_epoch_ms: snapshot.observed_at_epoch_ms,
        mode: arkforge_core::DeviceMode::new(&snapshot.observed_mode).map_err(permit_invalid)?,
        topology_digest: digest_bytes(&snapshot.topology_sha256, "topology digest")?,
        descriptor_digest: digest_bytes(&snapshot.descriptor_sha256, "descriptor digest")?,
        serial_evidence,
        protocol_identity: snapshot
            .protocol_identity
            .iter()
            .map(|fact| {
                Ok(ProtocolIdentityFact {
                    key: OpaqueId::new(&fact.key).map_err(permit_invalid)?,
                    value: fact.value.clone(),
                })
            })
            .collect::<Result<Vec<_>, StandaloneError>>()?,
        provider_candidates: Vec::new(),
        identity_strength: IdentityEvidenceStrength::parse(&snapshot.identity_strength)
            .ok_or_else(|| permit_invalid("admission identity strength is unknown"))?,
        malformed_descriptor: snapshot.malformed_descriptor,
    };
    observation.admission_facts_digest().map_err(permit_invalid)
}

pub(super) fn digest_bytes(bytes: &[u8], name: &str) -> Result<Sha256Digest, StandaloneError> {
    if bytes.len() != 32 {
        return Err(permit_invalid(format!("{name} is not 32 bytes")));
    }
    let mut digest = [0u8; 32];
    digest.copy_from_slice(bytes);
    Ok(Sha256Digest::from_bytes(digest))
}

pub(super) fn permit_invalid(error: impl std::fmt::Display) -> StandaloneError {
    StandaloneError::new(
        "PERMIT_INPUT_INVALID",
        format!("Cannot construct an exact step permit: {error}"),
        10,
        false,
    )
}

pub(super) fn persist_binding(
    runtime_dir: &Path,
    binding_id: &str,
    device: &str,
    profile: &str,
    stable_identity: &str,
) -> Result<(), StandaloneError> {
    let root = runtime_dir.join("authority").join("bindings");
    std::fs::create_dir_all(&root)
        .map_err(|error| internal("create the authority binding journal", error))?;
    protect_path(&root, true)
        .map_err(|error| internal("protect the authority binding journal", error))?;
    let path = root.join(format!("{binding_id}.binding"));
    let record = format!(
        "schema=arkforge.cli-target-binding/v1\nbinding_id={binding_id}\nrevision=1\ndevice={device}\nprofile={profile}\nstable_identity_sha256={stable_identity}\n"
    );
    if path.exists() {
        let existing = std::fs::read_to_string(&path)
            .map_err(|error| internal("read the durable target binding", error))?;
        if existing != record {
            return Err(StandaloneError::new(
                "TARGET_BINDING_CONFLICT",
                format!("Durable binding {binding_id} disagrees with the current target facts."),
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
        .map_err(|error| internal("create the durable target binding", error))?;
    protect_path(&path, false)
        .map_err(|error| internal("protect the durable target binding", error))?;
    file.write_all(record.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|error| internal("commit the durable target binding", error))
}

pub(super) fn encode_status(status: &DaemonStatus) -> Vec<u8> {
    let mut out = Vec::new();
    wire::write_uint32(&mut out, 1, status.supervisor_pid);
    wire::write_uint32(&mut out, 2, status.daemon_pid);
    wire::write_uint64(&mut out, 3, status.epoch);
    wire::write_uint32(&mut out, 4, status.protocol_major);
    wire::write_uint32(&mut out, 5, status.protocol_minor);
    wire::write_string(&mut out, 6, &status.daemon_version);
    wire::write_bool(&mut out, 7, status.mechanics_ready);
    wire::write_uint64(&mut out, 8, status.active_jobs as u64);
    for blocker in &status.blockers {
        wire::write_string(&mut out, 9, blocker);
    }
    wire::write_bool(&mut out, 10, status.authority_support_available);
    wire::write_bool(&mut out, 11, status.hdc_bound);
    wire::write_string(&mut out, 12, &status.hdc_sha256);
    wire::write_string(&mut out, 13, &status.hardware_campaign);
    out
}

pub(super) fn decode_status(input: &[u8]) -> Result<DaemonStatus, StandaloneError> {
    let mut status = DaemonStatus {
        supervisor_pid: 0,
        daemon_pid: 0,
        epoch: 0,
        protocol_major: 0,
        protocol_minor: 0,
        daemon_version: String::new(),
        mechanics_ready: false,
        authority_support_available: false,
        hdc_bound: false,
        hdc_sha256: String::new(),
        hardware_campaign: String::new(),
        active_jobs: 0,
        blockers: Vec::new(),
    };
    let mut reader = wire::Reader::new(input);
    while let Some((field, value)) = reader
        .next_field()
        .map_err(|error| internal("decode supervisor status", error))?
    {
        match field {
            1 => status.supervisor_pid = value.as_u64().map_err(status_decode)? as u32,
            2 => status.daemon_pid = value.as_u64().map_err(status_decode)? as u32,
            3 => status.epoch = value.as_u64().map_err(status_decode)?,
            4 => status.protocol_major = value.as_u64().map_err(status_decode)? as u32,
            5 => status.protocol_minor = value.as_u64().map_err(status_decode)? as u32,
            6 => status.daemon_version = value.as_str(6).map_err(status_decode)?.to_string(),
            7 => status.mechanics_ready = value.as_bool().map_err(status_decode)?,
            8 => status.active_jobs = value.as_u64().map_err(status_decode)? as usize,
            9 => status
                .blockers
                .push(value.as_str(9).map_err(status_decode)?.to_string()),
            10 => status.authority_support_available = value.as_bool().map_err(status_decode)?,
            11 => status.hdc_bound = value.as_bool().map_err(status_decode)?,
            12 => status.hdc_sha256 = value.as_str(12).map_err(status_decode)?.to_string(),
            13 => status.hardware_campaign = value.as_str(13).map_err(status_decode)?.to_string(),
            _ => {}
        }
    }
    if status.supervisor_pid == 0
        || status.daemon_pid == 0
        || status.epoch == 0
        || status.protocol_major == 0
        || status.daemon_version.is_empty()
    {
        return Err(StandaloneError::new(
            "SUPERVISOR_RESPONSE_INVALID",
            "The authority supervisor returned an incomplete status response.",
            10,
            false,
        ));
    }
    Ok(status)
}

pub(super) fn decode_request(input: &[u8]) -> Result<(String, Vec<String>), StandaloneError> {
    let mut command = String::new();
    let mut arguments = Vec::new();
    let mut reader = wire::Reader::new(input);
    while let Some((field, value)) = reader
        .next_field()
        .map_err(|error| internal("decode supervisor request", error))?
    {
        match field {
            1 => command = value.as_str(1).map_err(status_decode)?.to_string(),
            2 => arguments.push(value.as_str(2).map_err(status_decode)?.to_string()),
            _ => {}
        }
    }
    if command.is_empty() {
        return Err(StandaloneError::new(
            "SUPERVISOR_REQUEST_INVALID",
            "The supervisor request contains no command.",
            2,
            false,
        ));
    }
    Ok((command, arguments))
}

pub(super) fn write_reply(
    stream: &mut LocalStream,
    result: Result<Vec<u8>, StandaloneError>,
) -> Result<(), StandaloneError> {
    let mut out = Vec::new();
    match result {
        Ok(payload) => {
            wire::write_uint32(&mut out, 1, 1);
            wire::write_bytes(&mut out, 4, &payload);
        }
        Err(error) => {
            wire::write_uint32(&mut out, 1, 2);
            wire::write_string(&mut out, 2, &error.code);
            wire::write_string(&mut out, 3, &error.message);
            wire::write_uint32(&mut out, 5, error.exit_code as u32);
            wire::write_bool(&mut out, 6, error.retryable);
            for token in &error.required_acknowledgements {
                wire::write_string(&mut out, 7, token);
            }
        }
    }
    write_frame(stream, &out).map_err(|error| internal("write supervisor response", error))
}

pub(super) fn decode_reply(input: &[u8]) -> Result<Vec<u8>, StandaloneError> {
    let mut disposition = 0;
    let mut code = String::new();
    let mut message = String::new();
    let mut payload = Vec::new();
    let mut exit_code = 10;
    let mut retryable = false;
    let mut required_acknowledgements = Vec::new();
    let mut reader = wire::Reader::new(input);
    while let Some((field, value)) = reader
        .next_field()
        .map_err(|error| internal("decode supervisor response", error))?
    {
        match field {
            1 => disposition = value.as_u64().map_err(status_decode)?,
            2 => code = value.as_str(2).map_err(status_decode)?.to_string(),
            3 => message = value.as_str(3).map_err(status_decode)?.to_string(),
            4 => payload = value.as_bytes().map_err(status_decode)?.to_vec(),
            5 => exit_code = value.as_u64().map_err(status_decode)? as i32,
            6 => retryable = value.as_bool().map_err(status_decode)?,
            7 => {
                required_acknowledgements.push(value.as_str(7).map_err(status_decode)?.to_string())
            }
            _ => {}
        }
    }
    match disposition {
        1 => Ok(payload),
        2 if !code.is_empty() => Err(StandaloneError::new(code, message, exit_code, retryable)
            .with_required_acknowledgements(required_acknowledgements)),
        _ => Err(StandaloneError::new(
            "SUPERVISOR_RESPONSE_INVALID",
            "The authority supervisor returned an incomplete response envelope.",
            10,
            false,
        )),
    }
}

pub(super) fn status_decode(error: impl std::fmt::Display) -> StandaloneError {
    StandaloneError::new(
        "SUPERVISOR_RESPONSE_INVALID",
        format!("The authority supervisor response is invalid: {error}"),
        10,
        false,
    )
}

/// Creates the per-user runtime root and applies the host owner-only boundary.
/// Offline artifact import uses this before the daemon exists so Windows CAS
/// children inherit the same protected DACL.
pub fn prepare_storage(runtime_dir: &Path) -> Result<(), StandaloneError> {
    std::fs::create_dir_all(runtime_dir)
        .map_err(|error| internal("create the runtime directory", error))?;
    protect_path(runtime_dir, true)
        .map_err(|error| internal("protect the runtime directory", error))
}

pub(super) fn prepare_runtime(runtime_dir: &Path) -> Result<(), StandaloneError> {
    prepare_storage(runtime_dir)
}

pub(super) fn fresh_pairing(runtime_dir: &Path) -> Result<(u64, Vec<u8>), StandaloneError> {
    let mut secret = [0u8; 32];
    fill_random(&mut secret)
        .map_err(|error| internal("read host randomness for pairing", error))?;
    let authority = runtime_dir.join("authority");
    std::fs::create_dir_all(&authority)
        .map_err(|error| internal("create pairing epoch journal", error))?;
    protect_path(&authority, true)
        .map_err(|error| internal("protect pairing epoch journal", error))?;
    let path = authority.join("pairing-epoch");
    let previous = if path.exists() {
        std::fs::read_to_string(&path)
            .map_err(|error| internal("read pairing epoch journal", error))?
            .trim()
            .parse::<u64>()
            .map_err(|_| {
                internal(
                    "decode pairing epoch journal",
                    "epoch is not an unsigned integer",
                )
            })?
    } else {
        0
    };
    let epoch = previous.checked_add(1).ok_or_else(|| {
        StandaloneError::new(
            "PAIRING_EPOCH_EXHAUSTED",
            "The durable pairing epoch cannot advance; this runtime must not execute.",
            10,
            false,
        )
    })?;
    let transaction_id = &sha256(&secret).to_hex()[..16];
    let temporary = authority.join(format!(
        ".pairing-epoch.{}.{}.tmp",
        std::process::id(),
        transaction_id
    ));
    let mut journal = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| internal("create pairing epoch transaction", error))?;
    protect_path(&temporary, false)
        .map_err(|error| internal("protect pairing epoch transaction", error))?;
    journal
        .write_all(epoch.to_string().as_bytes())
        .and_then(|_| journal.sync_all())
        .map_err(|error| internal("commit pairing epoch bytes", error))?;
    replace_file(&temporary, &path).map_err(|error| internal("publish pairing epoch", error))?;
    sync_directory(&authority).map_err(|error| internal("sync pairing epoch directory", error))?;
    Ok((epoch, secret.to_vec()))
}
