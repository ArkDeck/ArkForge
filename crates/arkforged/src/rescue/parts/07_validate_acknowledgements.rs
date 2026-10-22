
fn validate_acknowledgements(
    plan: &RescuePlan,
    acknowledgements: &[String],
) -> Result<(), RescueError> {
    let required: BTreeSet<String> = plan.required_acknowledgements().into_iter().collect();
    let supplied: BTreeSet<String> = acknowledgements.iter().cloned().collect();
    if supplied != required || supplied.len() != acknowledgements.len() {
        let missing: Vec<String> = required.difference(&supplied).cloned().collect();
        let unexpected: Vec<String> = supplied.difference(&required).cloned().collect();
        let duplicates = acknowledgements.len().saturating_sub(supplied.len());
        return Err(RescueError::new(
            "ACKNOWLEDGEMENT_REQUIRED",
            format!(
                "Supply exactly the sealed acknowledgement set. Missing: [{}]. Unexpected: [{}]. Duplicate count: {duplicates}.",
                missing.join(", "),
                unexpected.join(", ")
            ),
            4,
        )
        .retryable());
    }
    Ok(())
}

fn mutation_precondition<T>(
    result: Result<RockUsbObservation<T>, RockUsbPortFailure>,
    operation: &str,
) -> Result<RockUsbObservation<T>, (RescueDisposition, String, Sha256Digest)> {
    match result {
        Ok(value) => Ok(value),
        Err(RockUsbPortFailure::BeforeIo(message)) => Err(confirmed_no_effect(format!(
            "Unable to {operation}: {message}"
        ))),
        // The operation is read-only and happened before the mutation, so even
        // a transport failure here confirms the rescue mutation did not begin.
        Err(RockUsbPortFailure::AfterIo(message)) => Err(confirmed_no_effect(format!(
            "Unable to {operation}: {message}"
        ))),
    }
}

fn confirmed_no_effect(message: String) -> (RescueDisposition, String, Sha256Digest) {
    (
        RescueDisposition::ConfirmedNoEffect,
        message.clone(),
        sha256(message.as_bytes()),
    )
}

fn outcome_unknown(message: String) -> (RescueDisposition, String, Sha256Digest) {
    (
        RescueDisposition::OutcomeUnknown,
        message.clone(),
        sha256(message.as_bytes()),
    )
}

fn receipt_from_mutation(
    plan_id: &str,
    plan_digest: Sha256Digest,
    device: &RescueDevice,
    operation: &str,
    receipt: RockUsbMutationReceipt,
    completed_at_epoch_ms: u64,
) -> RescueReceipt {
    let disposition = if receipt.semantic_success {
        RescueDisposition::SemanticSuccess
    } else {
        RescueDisposition::OutcomeUnknown
    };
    let (payload_bytes, payload_digest) = receipt
        .progress
        .as_ref()
        .map(|progress| (Some(progress.payload_bytes), Some(progress.payload_digest)))
        .unwrap_or((None, None));
    RescueReceipt {
        plan_id: plan_id.to_string(),
        plan_digest,
        device_id: device.device_id.clone(),
        operation: operation.to_string(),
        disposition,
        evidence_digest: receipt.evidence_digest,
        completed_at_epoch_ms,
        detail: receipt.detail,
        payload_bytes,
        payload_digest,
    }
}

pub fn now_epoch_ms() -> Result<u64, RescueError> {
    Ok(SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|error| RescueError::new("CLOCK_INVALID", error.to_string(), 10))?
        .as_millis() as u64)
}

fn create_private_dir(path: &Path) -> Result<(), RescueError> {
    fs::create_dir_all(path)
        .map_err(|error| RescueError::io(&format!("create {}", path.display()), error))?;
    protect_path(path, true)
        .map_err(|error| RescueError::io(&format!("protect {}", path.display()), error))?;
    Ok(())
}

fn create_private_file_new(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn write_content_addressed(path: &Path, bytes: &[u8]) -> Result<(), RescueError> {
    match create_private_file_new(path) {
        Ok(mut file) => {
            file.write_all(bytes)
                .and_then(|_| file.sync_all())
                .map_err(|error| RescueError::io(&format!("write {}", path.display()), error))?;
            sync_parent(path)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = fs::read(path).map_err(|read_error| {
                RescueError::io(&format!("read {}", path.display()), read_error)
            })?;
            if existing == bytes {
                Ok(())
            } else {
                Err(RescueError::new(
                    "RESCUE_STORE_COLLISION",
                    format!("{} already contains different bytes.", path.display()),
                    10,
                ))
            }
        }
        Err(error) => Err(RescueError::io(
            &format!("create {}", path.display()),
            error,
        )),
    }
}

fn sync_parent(path: &Path) -> Result<(), RescueError> {
    let parent = path
        .parent()
        .ok_or_else(|| RescueError::io("sync parent", "path has no parent"))?;
    sync_directory(parent)
        .map_err(|error| RescueError::io(&format!("sync {}", parent.display()), error))
}

fn value_map<'a>(
    value: &'a CborValue,
    name: &str,
) -> Result<&'a [(CborValue, CborValue)], RescueError> {
    match value {
        CborValue::Map(entries) => Ok(entries),
        _ => Err(RescueError::refused(
            "PLAN_INVALID",
            format!("{name} is not a map."),
        )),
    }
}

fn field<'a>(
    entries: &'a [(CborValue, CborValue)],
    name: &str,
) -> Result<&'a CborValue, RescueError> {
    entries
        .iter()
        .find_map(|(key, value)| match key {
            CborValue::Text(key) if key == name => Some(value),
            _ => None,
        })
        .ok_or_else(|| {
            RescueError::refused("PLAN_INVALID", format!("Plan field {name:?} is missing."))
        })
}

fn text_field<'a>(
    entries: &'a [(CborValue, CborValue)],
    name: &str,
) -> Result<&'a str, RescueError> {
    match field(entries, name)? {
        CborValue::Text(value) => Ok(value),
        _ => Err(RescueError::refused(
            "PLAN_INVALID",
            format!("Plan field {name:?} is not text."),
        )),
    }
}

fn unsigned_field(entries: &[(CborValue, CborValue)], name: &str) -> Result<u64, RescueError> {
    match field(entries, name)? {
        CborValue::Unsigned(value) => Ok(*value),
        _ => Err(RescueError::refused(
            "PLAN_INVALID",
            format!("Plan field {name:?} is not an unsigned integer."),
        )),
    }
}

fn digest_field(
    entries: &[(CborValue, CborValue)],
    name: &str,
) -> Result<Sha256Digest, RescueError> {
    digest_value(field(entries, name)?, name)
}

fn digest_value(value: &CborValue, name: &str) -> Result<Sha256Digest, RescueError> {
    match value {
        CborValue::Bytes(bytes) if bytes.len() == 32 => {
            let mut digest = [0u8; 32];
            digest.copy_from_slice(bytes);
            Ok(Sha256Digest::from_bytes(digest))
        }
        _ => Err(RescueError::refused(
            "PLAN_INVALID",
            format!("Plan field {name:?} is not a 32-byte digest."),
        )),
    }
}

fn ensure_fields(
    entries: &[(CborValue, CborValue)],
    expected: &[&str],
    name: &str,
) -> Result<(), RescueError> {
    let actual: BTreeSet<&str> = entries
        .iter()
        .map(|(key, _)| match key {
            CborValue::Text(key) => Ok(key.as_str()),
            _ => Err(RescueError::refused(
                "PLAN_INVALID",
                format!("{name} contains a non-text field name."),
            )),
        })
        .collect::<Result<_, _>>()?;
    let expected: BTreeSet<&str> = expected.iter().copied().collect();
    if actual != expected {
        return Err(RescueError::refused(
            "PLAN_INVALID",
            format!("{name} contains missing or unknown fields."),
        ));
    }
    Ok(())
}
