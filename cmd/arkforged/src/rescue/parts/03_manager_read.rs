use super::*;

impl<B: RescueBackend> RescueManager<B> {
    pub fn new(
        runtime_root: impl Into<PathBuf>,
        native_build_digest: Sha256Digest,
        backend: B,
    ) -> Result<Self, RescueError> {
        let profile = arkforge_core::profile::load(PROFILE_SOURCE).map_err(|error| {
            RescueError::new(
                "SHIPPED_PROFILE_INVALID",
                format!("The shipped DAYU200 profile cannot be loaded: {error}"),
                10,
            )
        })?;
        let profile_digest = profile
            .digest()
            .map_err(|error| RescueError::io("digest shipped DAYU200 profile", error))?;
        Ok(Self {
            runtime_root: runtime_root.into(),
            profile,
            profile_digest,
            native_build_digest,
            backend,
        })
    }

    pub fn list_devices(&self) -> Result<Vec<RescueDevice>, RescueError> {
        self.backend.list_devices()
    }

    pub fn inspect(&self, device_id: &str) -> Result<RescueInspection, RescueError> {
        let (device, port) = self.backend.open_device(device_id)?;
        let capacity = port
            .capacity_sectors()
            .map_err(|error| port_refusal("read device capacity", error))?;
        let table = port
            .read_partition_table()
            .map_err(|error| port_refusal("read the device partition table", error))?;
        let compatibility = validate_partition_table_for_profile(&table.value, &self.profile);
        let (profile_compatible, profile_blocker) = match compatibility {
            Ok(()) => (true, None),
            Err(error) => (false, Some(error.to_string())),
        };
        Ok(RescueInspection {
            device,
            capacity_sectors: capacity.value,
            capacity_evidence_digest: capacity.evidence_digest,
            layout_digest: observed_layout_digest(&table.value),
            layout_evidence_digest: table.evidence_digest,
            table: table.value,
            profile_compatible,
            profile_blocker,
        })
    }

    pub fn read_sectors(
        &self,
        device_id: &str,
        begin_sector: u64,
        sector_count: u64,
        output: &Path,
    ) -> Result<RescueReadReceipt, RescueError> {
        if sector_count == 0 {
            return Err(RescueError::invalid(
                "Use a sector count greater than zero.",
            ));
        }
        let bytes_requested = sector_count
            .checked_mul(ROCKUSB_SECTOR_BYTES)
            .ok_or_else(|| {
                RescueError::invalid("The requested sector range overflows a byte count.")
            })?;
        if bytes_requested > MAX_READ_BYTES {
            return Err(RescueError::refused(
                "READ_TOO_LARGE",
                format!(
                    "One rescue read is limited to {MAX_READ_BYTES} bytes. Split the request into smaller ranges."
                ),
            ));
        }
        let end = begin_sector.checked_add(sector_count).ok_or_else(|| {
            RescueError::invalid("The requested sector range overflows the address space.")
        })?;
        if output.exists() {
            return Err(RescueError::refused(
                "OUTPUT_EXISTS",
                format!(
                    "{} already exists. Choose a new --out path; rescue read never overwrites a file.",
                    output.display()
                ),
            ));
        }
        if let Some(parent) = output.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            return Err(RescueError::refused(
                "OUTPUT_PARENT_MISSING",
                format!("Create {} before reading.", parent.display()),
            ));
        }

        let (device, port) = self.backend.open_device(device_id)?;
        let capacity = port
            .capacity_sectors()
            .map_err(|error| port_refusal("read device capacity", error))?;
        if end > capacity.value {
            return Err(RescueError::refused(
                "READ_OUT_OF_RANGE",
                format!(
                    "The device has {} sectors; requested range {begin_sector}+{sector_count} ends at {end}.",
                    capacity.value
                ),
            ));
        }
        let scratch = self.rescue_root().join("read-scratch");
        create_private_dir(&scratch)?;
        let observation = port
            .read_sectors(begin_sector, sector_count, &scratch)
            .map_err(|error| port_refusal("read sectors", error))?;
        if observation.value.len() as u64 != bytes_requested {
            return Err(RescueError::new(
                "NATIVE_READ_LENGTH_MISMATCH",
                format!(
                    "Native RockUSB returned {} bytes; this request requires exactly {bytes_requested}.",
                    observation.value.len()
                ),
                7,
            ));
        }
        let output_digest = sha256(&observation.value);
        if output_digest != observation.evidence_digest {
            return Err(RescueError::new(
                "NATIVE_READ_EVIDENCE_MISMATCH",
                format!(
                    "Native read evidence names {}; returned bytes hash to {output_digest}.",
                    observation.evidence_digest
                ),
                7,
            ));
        }
        let mut file = create_private_file_new(output)
            .map_err(|error| RescueError::io(&format!("create {}", output.display()), error))?;
        if let Err(error) = file
            .write_all(&observation.value)
            .and_then(|_| file.sync_all())
        {
            drop(file);
            let _ = fs::remove_file(output);
            return Err(RescueError::io(
                &format!("write {}", output.display()),
                error,
            ));
        }
        Ok(RescueReadReceipt {
            device,
            begin_sector,
            sector_count,
            bytes: observation.value.len() as u64,
            sha256: output_digest,
            output: output.to_path_buf(),
        })
    }
}
