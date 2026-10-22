use super::*;

impl<B: RescueBackend> RescueManager<B> {
    pub fn plan_write(
        &self,
        device_id: &str,
        partition: &str,
        image_path: &Path,
        expected_image_digest: Sha256Digest,
        now_epoch_ms: u64,
    ) -> Result<RescuePlanSummary, RescueError> {
        let target = self
            .profile
            .allowed_targets
            .iter()
            .find(|target| target.partition.as_str() == partition)
            .ok_or_else(|| {
                RescueError::refused(
                    "TARGET_NOT_ALLOWED",
                    format!(
                        "The shipped profile does not allow rescue writes to {partition:?}. Choose one of its allowed targets."
                    ),
                )
            })?;
        let image_size = fs::metadata(image_path)
            .map_err(|error| RescueError::io(&format!("read {}", image_path.display()), error))?
            .len();
        if image_size == 0 {
            return Err(RescueError::refused(
                "IMAGE_EMPTY",
                "The rescue image is empty. Choose a non-empty image.",
            ));
        }

        let inspection = self.inspect(device_id)?;
        if !inspection.profile_compatible {
            return Err(RescueError::refused(
                "LAYOUT_NOT_ALLOWED",
                inspection.profile_blocker.unwrap_or_else(|| {
                    "The device partition table does not match the shipped profile.".into()
                }),
            ));
        }
        let entry = inspection
            .table
            .entries
            .iter()
            .find(|entry| entry.name == partition)
            .ok_or_else(|| {
                RescueError::refused(
                    "PARTITION_NOT_FOUND",
                    format!("The device declares no partition named {partition:?}."),
                )
            })?;
        if entry.offset_sectors != target.offset_sectors {
            return Err(RescueError::refused(
                "LAYOUT_NOT_ALLOWED",
                format!(
                    "Partition {partition} starts at {} on the device and {} in the shipped profile.",
                    entry.offset_sectors, target.offset_sectors
                ),
            ));
        }
        let partition_sectors = entry.size_sectors.unwrap_or_else(|| {
            inspection
                .capacity_sectors
                .saturating_sub(entry.offset_sectors)
        });
        let image_sectors = image_size / ROCKUSB_SECTOR_BYTES
            + u64::from(!image_size.is_multiple_of(ROCKUSB_SECTOR_BYTES));
        if partition_sectors == 0 || image_sectors > partition_sectors {
            return Err(RescueError::refused(
                "IMAGE_OVERRUNS_PARTITION",
                format!(
                    "The image needs {image_sectors} sectors and partition {partition} has {partition_sectors}."
                ),
            ));
        }

        let store = self.open_store()?;
        let image = File::open(image_path)
            .map_err(|error| RescueError::io(&format!("open {}", image_path.display()), error))?;
        let imported = store
            .import(image, image_size, Some(expected_image_digest))
            .map_err(|error| RescueError::new("IMAGE_IMPORT_REFUSED", error.to_string(), 3))?;

        let plan = RescuePlan {
            created_at_epoch_ms: now_epoch_ms,
            expires_at_epoch_ms: now_epoch_ms.saturating_add(PLAN_LIFETIME.as_millis() as u64),
            device_id: inspection.device.device_id.clone(),
            device_facts_digest: inspection.device.facts_digest,
            profile_id: self.profile.id.to_string(),
            profile_version: self.profile.version.to_string(),
            profile_digest: self.profile_digest,
            native_build_digest: self.native_build_digest,
            observed_layout_digest: Some(inspection.layout_digest),
            operation: RescueOperation::WritePartition {
                partition: partition.to_string(),
                begin_sector: entry.offset_sectors,
                partition_sectors,
                image_digest: imported.digest,
                image_size_bytes: imported.size_bytes,
            },
        };
        self.store_plan(plan)
    }

    pub fn plan_reset(
        &self,
        device_id: &str,
        now_epoch_ms: u64,
    ) -> Result<RescuePlanSummary, RescueError> {
        let (device, _port) = self.backend.open_device(device_id)?;
        let plan = RescuePlan {
            created_at_epoch_ms: now_epoch_ms,
            expires_at_epoch_ms: now_epoch_ms.saturating_add(PLAN_LIFETIME.as_millis() as u64),
            device_id: device.device_id,
            device_facts_digest: device.facts_digest,
            profile_id: self.profile.id.to_string(),
            profile_version: self.profile.version.to_string(),
            profile_digest: self.profile_digest,
            native_build_digest: self.native_build_digest,
            observed_layout_digest: None,
            operation: RescueOperation::ResetDevice,
        };
        self.store_plan(plan)
    }
}
