impl<B: RescueBackend> RescueManager<B> {

    pub fn apply(
        &self,
        plan_id: &str,
        expected_plan_digest: Sha256Digest,
        acknowledgements: &[String],
        now_epoch_ms: u64,
    ) -> Result<RescueApplyResult, RescueError> {
        let (plan, plan_digest, plan_bytes) = self.load_plan(plan_id)?;
        if plan_digest != expected_plan_digest {
            return Err(RescueError::new(
                "PLAN_DIGEST_MISMATCH",
                format!(
                    "The stored rescue plan hashes to {plan_digest}; --expect-plan-sha256 supplied {expected_plan_digest}."
                ),
                4,
            ));
        }
        if plan.expires_at_epoch_ms <= now_epoch_ms {
            return Err(RescueError::refused(
                "PLAN_EXPIRED",
                "The rescue plan expired. Inspect the current device and create a new plan.",
            ));
        }
        if plan.created_at_epoch_ms > now_epoch_ms {
            return Err(RescueError::refused(
                "PLAN_NOT_YET_VALID",
                "The rescue plan was created in the future according to this host clock. Correct the clock and create a new plan.",
            ));
        }
        if plan.native_build_digest != self.native_build_digest {
            return Err(RescueError::refused(
                "NATIVE_BUILD_CHANGED",
                format!(
                    "The plan binds native build {}; this command is {}. Create a new rescue plan with this build.",
                    plan.native_build_digest, self.native_build_digest
                ),
            ));
        }
        if plan.profile_digest != self.profile_digest
            || plan.profile_id != self.profile.id.as_str()
            || plan.profile_version != self.profile.version.to_string()
        {
            return Err(RescueError::refused(
                "PROFILE_CHANGED",
                "The shipped profile no longer matches this rescue plan. Create a new plan.",
            ));
        }
        validate_acknowledgements(&plan, acknowledgements)?;

        let planned_write_layout = match &plan.operation {
            RescueOperation::WritePartition { .. } => {
                Some(plan.observed_layout_digest.ok_or_else(|| {
                    RescueError::refused(
                        "PLAN_INVALID",
                        "Write plan carries no observed layout digest.",
                    )
                })?)
            }
            RescueOperation::ResetDevice => None,
        };

        let (device, port) = self.backend.open_device(&plan.device_id)?;
        if device.facts_digest != plan.device_facts_digest {
            return Err(RescueError::refused(
                "DEVICE_CHANGED",
                "The selected Loader observation changed after planning. Create a new rescue plan.",
            ));
        }

        self.record_intent(plan_id, plan_digest, &plan_bytes, now_epoch_ms)?;
        let receipt = match &plan.operation {
            RescueOperation::WritePartition {
                partition,
                begin_sector,
                partition_sectors,
                image_digest,
                image_size_bytes,
            } => self.apply_write(
                plan_id,
                plan_digest,
                &device,
                port.as_ref(),
                partition,
                *begin_sector,
                *partition_sectors,
                *image_digest,
                *image_size_bytes,
                planned_write_layout.ok_or_else(|| {
                    RescueError::refused("PLAN_INVALID", "Write plan layout binding is missing.")
                })?,
                now_epoch_ms,
            ),
            RescueOperation::ResetDevice => {
                self.apply_reset(plan_id, plan_digest, &device, port.as_ref(), now_epoch_ms)
            }
        };
        let receipt = match receipt {
            Ok(receipt) => receipt,
            Err((disposition, detail, evidence_digest)) => RescueReceipt {
                plan_id: plan_id.to_string(),
                plan_digest,
                device_id: device.device_id,
                operation: plan.operation.as_str().to_string(),
                disposition,
                evidence_digest,
                completed_at_epoch_ms: now_epoch_ms,
                detail,
                payload_bytes: None,
                payload_digest: None,
            },
        };
        self.store_receipt(&receipt)?;
        Ok(RescueApplyResult { receipt })
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_write(
        &self,
        plan_id: &str,
        plan_digest: Sha256Digest,
        device: &RescueDevice,
        port: &dyn RockUsbPort,
        partition: &str,
        begin_sector: u64,
        partition_sectors: u64,
        image_digest: Sha256Digest,
        image_size_bytes: u64,
        planned_layout_digest: Sha256Digest,
        now_epoch_ms: u64,
    ) -> Result<RescueReceipt, (RescueDisposition, String, Sha256Digest)> {
        let capacity = mutation_precondition(port.capacity_sectors(), "read device capacity")?;
        let table = mutation_precondition(port.read_partition_table(), "read partition table")?;
        if let Err(error) = validate_partition_table_for_profile(&table.value, &self.profile) {
            return Err(confirmed_no_effect(error.to_string()));
        }
        let current_layout_digest = observed_layout_digest(&table.value);
        if current_layout_digest != planned_layout_digest {
            return Err(confirmed_no_effect(format!(
                "device layout changed from {planned_layout_digest} to {current_layout_digest}"
            )));
        }
        let Some(entry) = table
            .value
            .entries
            .iter()
            .find(|entry| entry.name == partition)
        else {
            return Err(confirmed_no_effect(format!(
                "the device no longer declares partition {partition:?}"
            )));
        };
        let current_sectors = entry
            .size_sectors
            .unwrap_or_else(|| capacity.value.saturating_sub(entry.offset_sectors));
        if entry.offset_sectors != begin_sector || current_sectors != partition_sectors {
            return Err(confirmed_no_effect(format!(
                "partition {partition} changed from {begin_sector}+{partition_sectors} to {}+{current_sectors}",
                entry.offset_sectors
            )));
        }

        let store = self
            .open_store()
            .map_err(|error| confirmed_no_effect(error.to_string()))?;
        if store
            .object_size(&image_digest)
            .map_err(|error| confirmed_no_effect(error.to_string()))?
            != image_size_bytes
            || !store
                .verify_object(&image_digest)
                .map_err(|error| confirmed_no_effect(error.to_string()))?
        {
            return Err(confirmed_no_effect(
                "the staged rescue image no longer matches its content address".into(),
            ));
        }
        let staging_dir = self.work_dir().join(plan_digest.to_string());
        create_private_dir(&staging_dir).map_err(|error| confirmed_no_effect(error.to_string()))?;
        let staging_path = staging_dir.join("image.bin");
        let mut source = store
            .open_object(&image_digest)
            .map_err(|error| confirmed_no_effect(error.to_string()))?;
        let mut target = create_private_file_new(&staging_path)
            .map_err(|error| confirmed_no_effect(error.to_string()))?;
        std::io::copy(&mut source, &mut target)
            .and_then(|_| target.sync_all())
            .map_err(|error| confirmed_no_effect(error.to_string()))?;
        let image = StagedImage {
            member: format!("rescue:{partition}"),
            path: staging_path,
            size_bytes: image_size_bytes,
            sha256: image_digest,
        };
        let mut validated = image
            .open_and_revalidate()
            .map_err(|error| confirmed_no_effect(error.to_string()))?;

        match port.write_partition(partition, begin_sector, &mut validated) {
            Ok(receipt)
                if receipt.semantic_success
                    && receipt.progress.as_ref().is_some_and(|progress| {
                        progress.payload_bytes == image_size_bytes
                            && progress.payload_digest == image_digest
                    }) =>
            {
                Ok(receipt_from_mutation(
                    plan_id,
                    plan_digest,
                    device,
                    "write-partition",
                    receipt,
                    now_epoch_ms,
                ))
            }
            Ok(receipt) => Err(outcome_unknown(format!(
                "Native write returned incomplete semantic evidence: {}",
                receipt.detail
            ))),
            Err(RockUsbPortFailure::BeforeIo(message)) => Err(confirmed_no_effect(message)),
            Err(RockUsbPortFailure::AfterIo(message)) => Err(outcome_unknown(message)),
        }
    }

    fn apply_reset(
        &self,
        plan_id: &str,
        plan_digest: Sha256Digest,
        device: &RescueDevice,
        port: &dyn RockUsbPort,
        now_epoch_ms: u64,
    ) -> Result<RescueReceipt, (RescueDisposition, String, Sha256Digest)> {
        match port.reset_device() {
            Ok(receipt) if receipt.semantic_success && receipt.progress.is_none() => {
                Ok(receipt_from_mutation(
                    plan_id,
                    plan_digest,
                    device,
                    "reset-device",
                    receipt,
                    now_epoch_ms,
                ))
            }
            Ok(receipt) => Err(outcome_unknown(format!(
                "Native reset returned incomplete semantic evidence: {}",
                receipt.detail
            ))),
            Err(RockUsbPortFailure::BeforeIo(message)) => Err(confirmed_no_effect(message)),
            Err(RockUsbPortFailure::AfterIo(message)) => Err(outcome_unknown(message)),
        }
    }
}
