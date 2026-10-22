use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn readback_partition(
    record: &PrivateActionRecord,
    session: &mut ExecutionSession,
    port: &dyn RockUsbPort,
    scratch: &Path,
    partition: &str,
    begin_sector: u64,
    max_strength: VerificationStrength,
    filler: Option<u8>,
) -> Result<ActionOutcome, ExecutionError> {
    let range = record
        .declared_range
        .ok_or(ExecutionError::VerificationRangeMissing)?;
    let expected = record
        .content_digest
        .ok_or(ExecutionError::VerificationRangeMissing)?;
    let domain = session
        .read_domain()
        .cloned()
        .ok_or(ExecutionError::ReadDomainNotCharacterized)?;

    let sectors = ceil_div(range.length, ROCKUSB_SECTOR_BYTES);
    // On a windowed read face, probe the end of the declared range before
    // transferring the whole image. If that endpoint is the measured filler,
    // the range cannot support a full-hash claim; reading gigabytes of the same
    // uninformative byte only to reach the same TypedSkip wastes minutes and
    // can turn a range that crosses the window edge into a false mismatch.
    if matches!(domain, MeasuredReadDomain::Windowed { .. }) {
        let last_sector = begin_sector
            .checked_add(sectors.saturating_sub(1))
            .ok_or(ExecutionError::VerificationRangeMissing)?;
        let tail = read_sectors(
            port,
            scratch,
            last_sector,
            1,
            &format!("{partition}-range-end-probe"),
        )?;
        let is_declared_filler = uniform_byte(&tail)
            .is_some_and(|byte| filler.map(|declared| declared == byte).unwrap_or(true));
        if is_declared_filler {
            let verification = VerificationOutcome::TypedSkip {
                range,
                reason: TypedSkipReason::OutsideReadDomain,
                detail: format!(
                    "range endpoint sector {last_sector} returned the measured filler through a \
                     windowed read face; the full range was not transferred"
                ),
            };
            return Ok(outcome(
                record,
                ActionDisposition::SemanticSuccess,
                vec![
                    fact("partition", partition),
                    fact("addressableMedium", domain.summary()),
                    fact("outcome", verification.as_str()),
                    fact("readbackStrategy", "range-end-probe"),
                    fact("probedSector", last_sector.to_string()),
                    fact("readbackBytes", tail.len().to_string()),
                ],
                sha256(&tail),
                Some(verification),
            ));
        }
    }
    let bytes = read_sectors(port, scratch, begin_sector, sectors, partition)?;
    let read = &bytes[..(range.length as usize).min(bytes.len())];

    let verification = classify_readback(
        read,
        range,
        expected,
        max_strength,
        filler,
        &domain,
        session.demonstrated_readable.contains(&begin_sector),
    );

    // A verification action is read-only: it never had an effect, so its
    // disposition is about the observation, not the medium. A failed
    // verification is a real, confirmed observation.
    let outcome_facts = vec![
        fact("partition", partition),
        fact("addressableMedium", domain.summary()),
        fact("outcome", verification.as_str()),
        fact("readbackStrategy", "full-range"),
        fact("readbackBytes", read.len().to_string()),
    ];
    Ok(outcome(
        record,
        ActionDisposition::SemanticSuccess,
        outcome_facts,
        sha256(read),
        Some(verification),
    ))
}

/// The three-state verdict of architecture.md 16.4.
///
/// The ordering matters and is the whole lesson of AD-006: uniform filler is
/// classified *before* any hash comparison, and outside a windowed read face it
/// is a skip rather than a failure. Every "the write did not land" verdict of
/// 2026-08-04 came from hashing filler that the read face had invented.
pub(super) fn classify_readback(
    read: &[u8],
    range: ByteRange,
    expected: Sha256Digest,
    max_strength: VerificationStrength,
    filler: Option<u8>,
    domain: &MeasuredReadDomain,
    start_demonstrated_readable: bool,
) -> VerificationOutcome {
    if let Some(byte) = uniform_byte(read) {
        let is_filler = filler.map(|declared| declared == byte).unwrap_or(true);
        if is_filler {
            return match domain {
                MeasuredReadDomain::Windowed { detail } => VerificationOutcome::TypedSkip {
                    range,
                    reason: TypedSkipReason::OutsideReadDomain,
                    detail: format!(
                        "read 0x{byte:02X} throughout; the read face is windowed ({detail}), so \
                         filler here carries no information about the medium"
                    ),
                },
                MeasuredReadDomain::Full if start_demonstrated_readable => {
                    VerificationOutcome::Failed {
                        range,
                        classification: FailureClassification::ErasedMediumFiller,
                    }
                }
                MeasuredReadDomain::Full => VerificationOutcome::Failed {
                    range,
                    classification: FailureClassification::ErasedMediumFiller,
                },
            };
        }
    }

    if read.len() as u64 != range.length {
        return VerificationOutcome::TypedSkip {
            range,
            reason: TypedSkipReason::OutsideReadDomain,
            detail: format!(
                "the read face returned {} of {} declared bytes",
                read.len(),
                range.length
            ),
        };
    }

    if sha256(read) == expected {
        VerificationOutcome::Verified {
            strength: max_strength,
            range,
        }
    } else {
        VerificationOutcome::Failed {
            range,
            classification: FailureClassification::ContentMismatch,
        }
    }
}

/// The single byte a buffer consists of, if it consists of one.
pub(super) fn uniform_byte(bytes: &[u8]) -> Option<u8> {
    let first = *bytes.first()?;
    bytes.iter().all(|byte| *byte == first).then_some(first)
}

pub(super) fn read_sectors(
    port: &dyn RockUsbPort,
    scratch: &Path,
    begin_sector: u64,
    sectors: u64,
    label: &str,
) -> Result<Vec<u8>, ExecutionError> {
    let operation_scratch = scratch.join(format!("read-{label}"));
    std::fs::create_dir_all(&operation_scratch).map_err(|error| {
        ExecutionError::ScratchUnusable(format!("{}: {error}", operation_scratch.display()))
    })?;
    port.read_sectors(begin_sector, sectors, &operation_scratch)
        .map(|receipt| receipt.value)
        .map_err(|error| port_error("readSectors", error))
}

pub(super) fn port_error(operation: &str, error: RockUsbPortFailure) -> ExecutionError {
    match error {
        RockUsbPortFailure::BeforeIo(message) => ExecutionError::PortRefused {
            operation: operation.into(),
            message,
        },
        RockUsbPortFailure::AfterIo(message) => ExecutionError::ExternalIo {
            operation: operation.into(),
            message,
        },
    }
}

pub(super) fn outcome(
    record: &PrivateActionRecord,
    disposition: ActionDisposition,
    facts: Vec<(OpaqueId, String)>,
    evidence_digest: Sha256Digest,
    verification: Option<VerificationOutcome>,
) -> ActionOutcome {
    ActionOutcome {
        action_id: record.action_id.clone(),
        step_id: record.step_id.clone(),
        disposition,
        facts,
        evidence_digest,
        verification,
    }
}

pub(super) fn fact(key: &str, value: impl Into<String>) -> (OpaqueId, String) {
    (OpaqueId::new(key).expect("literal fact key"), value.into())
}

pub(super) fn check_conformance(
    observed: &PartitionTableFact,
    profile: &DeviceProfile,
) -> Result<(), ExecutionError> {
    for target in &profile.allowed_targets {
        let entry = observed
            .entries
            .iter()
            .find(|entry| entry.name == target.partition.as_str())
            .ok_or_else(|| ExecutionError::PartitionNotOnDevice(target.partition.to_string()))?;
        if entry.offset_sectors != target.offset_sectors {
            return Err(ExecutionError::TargetOffsetDisagrees {
                partition: target.partition.to_string(),
                profile: target.offset_sectors,
                plan: entry.offset_sectors,
            });
        }
    }

    let mut unknown: Vec<String> = Vec::new();
    for entry in &observed.entries {
        let known = profile
            .allowed_targets
            .iter()
            .any(|target| target.partition.as_str() == entry.name)
            || profile
                .protected_targets
                .iter()
                .any(|protected| protected.as_str() == entry.name);
        if !known {
            unknown.push(entry.name.clone());
        }
    }
    if !unknown.is_empty() {
        return Err(ExecutionError::DeviceDeclaresUnknownPartitions(unknown));
    }
    Ok(())
}

/// Applies the same profile/device layout gate used by normal execution.
///
/// Native rescue must not grow a second, weaker interpretation of the device's
/// GPT. It can plan a write only when every allowed target and every protected
/// target is understood by the shipped profile.
pub fn validate_partition_table_for_profile(
    observed: &PartitionTableFact,
    profile: &DeviceProfile,
) -> Result<(), ExecutionError> {
    check_conformance(observed, profile)
}

/// The Profile's own declared layout, hashed the way the Provider hashed it
/// when it built the plan. Recomputed here so a plan that drifted from the
/// Profile it names is caught before the device is touched.
pub(super) fn profile_layout_digest(profile: &DeviceProfile) -> Sha256Digest {
    use arkforge_core::digest::{Domain, digest_in_domain};
    let mut ordered: Vec<_> = profile.allowed_targets.iter().collect();
    ordered.sort_by_key(|target| target.write_order);
    let value = CborValue::array(
        ordered
            .iter()
            .map(|target| {
                CborValue::map(vec![
                    ("partition", CborValue::text(target.partition.as_str())),
                    ("offsetSectors", CborValue::Unsigned(target.offset_sectors)),
                ])
            })
            .collect(),
    );
    let bytes = value
        .to_canonical_bytes()
        .expect("layout values are canonical");
    digest_in_domain(Domain::DeviceProfile, &bytes)
}

/// The observed layout reduced to name + first sector, in table order.
///
/// Deliberately not the archive's grammar branch or attributes: the device's
/// table does not carry them, and a digest over fields only one side has would
/// never match.
pub(super) fn layout_digest_of(table: &PartitionTableFact) -> Sha256Digest {
    let mut hasher = Sha256::new();
    for entry in &table.entries {
        hasher.update(entry.name.as_bytes());
        hasher.update(b"@");
        hasher.update(entry.offset_sectors.to_string().as_bytes());
        hasher.update(b"\n");
    }
    hasher.finalize()
}

/// Stable evidence digest for the exact name/offset table observed on-device.
pub fn observed_layout_digest(table: &PartitionTableFact) -> Sha256Digest {
    layout_digest_of(table)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionError {
    /// The action belongs to the authority's control port.
    RequiresAuthority {
        control_action: String,
    },
    ActionUndecodable(String),
    PortRefused {
        operation: String,
        message: String,
    },
    ExternalIo {
        operation: String,
        message: String,
    },
    LayoutMismatch {
        expected: Sha256Digest,
        observed: Sha256Digest,
    },
    PartitionTableUnreadable(String),
    DeviceDeclaresUnknownPartitions(Vec<String>),
    TableNotObservedYet,
    ReadDomainNotCharacterized,
    NoTableAtLba1,
    TargetNotAllowed(String),
    PartitionNotOnDevice(String),
    TargetOffsetDisagrees {
        partition: String,
        profile: u64,
        plan: u64,
    },
    ImageNotStaged(String),
    ImageOverrunsPartition {
        partition: String,
        image_sectors: u64,
        partition_sectors: u64,
    },
    StagingChanged(String),
    VerificationRangeMissing,
    ReadFailed {
        begin_sector: u64,
        sectors: u64,
        output: String,
    },
    ScratchUnusable(String),
}
