
/// Executes one decoded action.
///
/// `profile` is consulted for the target allowlist; the Provider does not hold
/// its own (architecture.md 10.4).
pub fn execute_action(
    action: &StoredAction,
    record: &PrivateActionRecord,
    session: &mut ExecutionSession,
    profile: &DeviceProfile,
    port: &dyn RockUsbPort,
    scratch: &Path,
) -> Result<ActionOutcome, ExecutionError> {
    match action {
        StoredAction::ManagedControl { control_action, .. } => {
            Err(ExecutionError::RequiresAuthority {
                control_action: control_action.clone(),
            })
        }
        StoredAction::ProbeLoader => {
            let receipt = port
                .discover()
                .map_err(|error| port_error("discoverDevices", error))?;
            let summary = receipt
                .value
                .iter()
                .map(RockUsbDevice::summary)
                .collect::<Vec<_>>()
                .join("; ");
            Ok(outcome(
                record,
                ActionDisposition::SemanticSuccess,
                vec![fact("rockusbDevices", summary)],
                receipt.evidence_digest,
                None,
            ))
        }
        StoredAction::ValidatePartitionTable {
            expected_layout_digest,
        } => {
            // Two different checks, against two different things. The digest
            // says the plan still agrees with the Profile it was built from;
            // conformance says the device agrees with the Profile. An earlier
            // revision compared the digest against the *device's* table, which
            // could never match: the Profile names nine writable targets and
            // the device's table declares fifteen partitions with no sizes at
            // all (AD-018).
            let profile_digest = profile_layout_digest(profile);
            if profile_digest != *expected_layout_digest {
                return Err(ExecutionError::LayoutMismatch {
                    expected: *expected_layout_digest,
                    observed: profile_digest,
                });
            }

            let receipt = port
                .read_partition_table()
                .map_err(|error| port_error("readPartitionTable", error))?;
            let observed = receipt.value;
            check_conformance(&observed, profile)?;
            let observed_digest = layout_digest_of(&observed);
            session.set_observed_table(observed);
            Ok(outcome(
                record,
                ActionDisposition::SemanticSuccess,
                vec![
                    fact("observedLayoutDigest", observed_digest.to_string()),
                    fact("planLayoutDigest", profile_digest.to_string()),
                ],
                receipt.evidence_digest,
                None,
            ))
        }
        StoredAction::CharacterizeReadDomain => {
            characterize_read_domain(record, session, port, scratch)
        }
        StoredAction::WritePartition {
            partition,
            member,
            begin_sector,
        } => write_partition(
            record,
            session,
            profile,
            port,
            partition,
            member,
            *begin_sector,
        ),
        StoredAction::ReadbackPartition {
            partition,
            begin_sector,
            max_strength_when_readable,
            erased_medium_filler,
        } => readback_partition(
            record,
            session,
            port,
            scratch,
            partition,
            *begin_sector,
            *max_strength_when_readable,
            *erased_medium_filler,
        ),
        StoredAction::ResetDevice => {
            let receipt = port
                .reset_device()
                .map_err(|error| port_error("resetDevice", error))?;
            let disposition = if receipt.semantic_success {
                ActionDisposition::SemanticSuccess
            } else {
                // A reset whose marker never appeared may still have reset the
                // device. Unknown, not failed.
                ActionDisposition::OutcomeUnknown
            };
            Ok(outcome(
                record,
                disposition,
                vec![fact("operation", "DEVICE_RESET")],
                receipt.evidence_digest,
                None,
            ))
        }
    }
}

/// Probes the near and far ends of the medium.
///
/// The primary table sits at LBA 1 and must read as real data — without it the
/// name-addressed write has nothing to resolve against. The backup table sits
/// at the far end; if that reads as uniform filler while the primary did not,
/// the read face is windowed and every filler read past it is uninformative.
fn characterize_read_domain(
    record: &PrivateActionRecord,
    session: &mut ExecutionSession,
    port: &dyn RockUsbPort,
    scratch: &Path,
) -> Result<ActionOutcome, ExecutionError> {
    let primary = read_sectors(port, scratch, 1, 1, "primary-table")?;
    if uniform_byte(&primary).is_some() {
        return Err(ExecutionError::NoTableAtLba1);
    }
    session.note_readable_sector(1);

    let table = session
        .observed_table()
        .ok_or(ExecutionError::TableNotObservedYet)?;
    // The far end the device's own table declares. `None` size is the grow
    // marker: the last declared partition runs to the end of the medium, so
    // its start is the furthest address the table names.
    let far_sector = table
        .entries
        .iter()
        .map(|entry| match entry.size_sectors {
            Some(size) => entry.offset_sectors + size.saturating_sub(1),
            None => entry.offset_sectors,
        })
        .max()
        .ok_or(ExecutionError::TableNotObservedYet)?;

    let far = read_sectors(port, scratch, far_sector, 1, "far-end")?;
    let domain = match uniform_byte(&far) {
        Some(byte) => MeasuredReadDomain::Windowed {
            detail: format!(
                "sector 1 read real data; sector {far_sector} read uniform 0x{byte:02X}"
            ),
        },
        None => {
            session.note_readable_sector(far_sector);
            MeasuredReadDomain::Full
        }
    };
    let summary = domain.summary();
    let detail = match &domain {
        MeasuredReadDomain::Windowed { detail } => detail.clone(),
        MeasuredReadDomain::Full => format!("sectors 1 and {far_sector} both read real data"),
    };
    session.set_read_domain(domain);

    Ok(outcome(
        record,
        ActionDisposition::SemanticSuccess,
        vec![
            fact("addressableMedium", summary),
            fact("readDomainDetail", detail.clone()),
        ],
        sha256(detail.as_bytes()),
        None,
    ))
}

#[allow(clippy::too_many_arguments)]
fn write_partition(
    record: &PrivateActionRecord,
    session: &mut ExecutionSession,
    profile: &DeviceProfile,
    port: &dyn RockUsbPort,
    partition: &str,
    member: &str,
    begin_sector: u64,
) -> Result<ActionOutcome, ExecutionError> {
    // 1. The Profile's allowlist, not the Provider's opinion.
    let target = profile
        .allowed_targets
        .iter()
        .find(|target| target.partition.as_str() == partition)
        .ok_or_else(|| ExecutionError::TargetNotAllowed(partition.to_string()))?;
    if target.offset_sectors != begin_sector {
        return Err(ExecutionError::TargetOffsetDisagrees {
            partition: partition.to_string(),
            profile: target.offset_sectors,
            plan: begin_sector,
        });
    }

    // 2. The device's own table has to have been read, and has to agree.
    let table = session
        .observed_table()
        .ok_or(ExecutionError::TableNotObservedYet)?;
    let entry = table
        .entries
        .iter()
        .find(|entry| entry.name == partition)
        .ok_or_else(|| ExecutionError::PartitionNotOnDevice(partition.to_string()))?;
    if entry.offset_sectors != begin_sector {
        return Err(ExecutionError::TargetOffsetDisagrees {
            partition: partition.to_string(),
            profile: begin_sector,
            plan: entry.offset_sectors,
        });
    }

    let device_offset_sectors = entry.offset_sectors;
    let partition_size_sectors = entry.size_sectors;

    // 3. The staged image is still the image that was staged.
    let staged = session
        .staged
        .get(member)
        .ok_or_else(|| ExecutionError::ImageNotStaged(member.to_string()))?
        .clone();

    // 4. It has to fit inside the span the device's table gives it. This is a
    //    refusal, not an addressing step: a write that would cross into the
    //    next partition is refused before external I/O begins.
    let image_sectors = ceil_div(staged.size_bytes, ROCKUSB_SECTOR_BYTES);
    if let Some(size_sectors) = partition_size_sectors
        && image_sectors > size_sectors
    {
        return Err(ExecutionError::ImageOverrunsPartition {
            partition: partition.to_string(),
            image_sectors,
            partition_sectors: size_sectors,
        });
    }

    // 5. Only now, the write. Address the native transfer from the observed
    //    entry itself — the equality checks above prove the plan and Profile
    //    agree, but neither is allowed to replace the device fact. From the
    //    first external I/O the device may have changed, so nothing after this
    //    point may report "no effect".
    let (mut validated, validation_wait_ms, parallel_validation) =
        session.take_validated_image(member)?;
    let validation_duration_ms = validated.validation_duration_ms();
    let validation_backend = validated.validation_backend();
    let image = validated.staged().clone();
    let receipt = port
        .write_partition(partition, device_offset_sectors, &mut validated)
        .map_err(|error| port_error("writePartition", error))?;
    if let Some(progress) = &receipt.progress
        && (progress.payload_bytes != image.size_bytes || progress.payload_digest != image.sha256)
    {
        return Err(ExecutionError::ExternalIo {
            operation: "writePartition".into(),
            message: format!(
                "native WRITE_LBA sent {} bytes hashing to {}; staged image is {} bytes hashing to {}",
                progress.payload_bytes, progress.payload_digest, image.size_bytes, image.sha256
            ),
        });
    }
    let disposition = if receipt.semantic_success {
        ActionDisposition::SemanticSuccess
    } else {
        // External I/O began. Whether it wrote anything is not knowable from
        // an unconfirmed receipt (architecture.md 14.1).
        ActionDisposition::OutcomeUnknown
    };

    let mut facts = vec![
        fact("partition", partition),
        fact("member", member),
        fact("imageSha256", image.sha256.to_string()),
        fact("imageBytes", image.size_bytes.to_string()),
        fact(
            "imageValidationDurationMs",
            validation_duration_ms.to_string(),
        ),
        fact("imageValidationWaitMs", validation_wait_ms.to_string()),
        fact("imageValidationBackend", validation_backend),
        fact(
            "imageValidationMode",
            if parallel_validation {
                "parallel-open-file"
            } else {
                "inline-open-file"
            },
        ),
        fact("beginSector", begin_sector.to_string()),
        fact("operationDurationMs", receipt.duration_ms.to_string()),
    ];
    if let Some(progress) = &receipt.progress {
        facts.push(fact(
            "writePayloadBytes",
            progress.payload_bytes.to_string(),
        ));
        facts.push(fact("writeWireSectors", progress.wire_sectors.to_string()));
        facts.push(fact("writeChunks", progress.chunks.to_string()));
        facts.push(fact(
            "writeChunkSectors",
            progress.chunk_sectors.to_string(),
        ));
        facts.push(fact(
            "writePayloadSha256",
            progress.payload_digest.to_string(),
        ));
    }
    if disposition != ActionDisposition::SemanticSuccess {
        // The receipt text itself is digested, not stored, so an unexplained
        // write used to leave nothing behind but "unknown" — the one fact that
        // names the cause was the one fact thrown away. The tail of the
        // executor's own detail string is where it says why it stopped
        // (AD-032's lesson, restated for the native receipt).
        let tail: String = receipt
            .detail
            .chars()
            .rev()
            .take(400)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        facts.push(fact("operationDetailTail", tail));
    }
    Ok(outcome(
        record,
        disposition,
        facts,
        receipt.evidence_digest,
        None,
    ))
}
