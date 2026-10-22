
impl RescuePlan {
    pub fn digest(&self) -> Result<Sha256Digest, RescueError> {
        let bytes = self
            .to_canonical_bytes()
            .map_err(|error| RescueError::io("encode rescue plan", error))?;
        Ok(digest_in_domain(Domain::RescuePlan, &bytes))
    }

    pub fn plan_id(&self) -> Result<String, RescueError> {
        Ok(format!("rescue-plan:{}", self.digest()?))
    }

    pub fn required_acknowledgements(&self) -> Vec<String> {
        self.operation.acknowledgements()
    }

    fn validate_shape(&self) -> Result<(), RescueError> {
        if self.expires_at_epoch_ms
            != self
                .created_at_epoch_ms
                .checked_add(PLAN_LIFETIME.as_millis() as u64)
                .ok_or_else(|| {
                    RescueError::refused("PLAN_INVALID", "Rescue plan lifetime overflows.")
                })?
        {
            return Err(RescueError::refused(
                "PLAN_INVALID",
                "The rescue plan does not carry the exact v1 lifetime.",
            ));
        }
        if self.device_id != format!("rescue-device:{}", self.device_facts_digest) {
            return Err(RescueError::refused(
                "PLAN_INVALID",
                "The rescue plan device ID does not match its sealed device facts digest.",
            ));
        }
        if self.profile_id.is_empty() || self.profile_version.is_empty() {
            return Err(RescueError::refused(
                "PLAN_INVALID",
                "The rescue plan profile binding is empty.",
            ));
        }
        match &self.operation {
            RescueOperation::WritePartition {
                partition,
                begin_sector,
                partition_sectors,
                image_size_bytes,
                ..
            } => {
                if partition.is_empty()
                    || *partition_sectors == 0
                    || *image_size_bytes == 0
                    || self.observed_layout_digest.is_none()
                    || begin_sector.checked_add(*partition_sectors).is_none()
                {
                    return Err(RescueError::refused(
                        "PLAN_INVALID",
                        "Write rescue plan has an empty or overflowing target, image, or layout binding.",
                    ));
                }
                let image_sectors = image_size_bytes / ROCKUSB_SECTOR_BYTES
                    + u64::from(!image_size_bytes.is_multiple_of(ROCKUSB_SECTOR_BYTES));
                if image_sectors > *partition_sectors {
                    return Err(RescueError::refused(
                        "PLAN_INVALID",
                        "Write rescue plan image exceeds its sealed partition extent.",
                    ));
                }
            }
            RescueOperation::ResetDevice if self.observed_layout_digest.is_some() => {
                return Err(RescueError::refused(
                    "PLAN_INVALID",
                    "Reset rescue plan must not claim a partition layout binding.",
                ));
            }
            RescueOperation::ResetDevice => {}
        }
        Ok(())
    }

    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, RescueError> {
        let value = decode_canonical(bytes).map_err(|error| {
            RescueError::refused("PLAN_INVALID", format!("Rescue plan is invalid: {error}"))
        })?;
        let entries = value_map(&value, "rescue plan")?;
        ensure_fields(
            entries,
            &[
                "schemaVersion",
                "createdAtEpochMs",
                "expiresAtEpochMs",
                "deviceId",
                "deviceFactsDigest",
                "profileId",
                "profileVersion",
                "profileDigest",
                "nativeBuildDigest",
                "observedLayoutDigest",
                "operation",
            ],
            "rescue plan",
        )?;
        if text_field(entries, "schemaVersion")? != RESCUE_PLAN_SCHEMA {
            return Err(RescueError::refused(
                "PLAN_UNSUPPORTED",
                "Rescue plan schema is not supported by this build.",
            ));
        }
        let observed_layout_digest = match field(entries, "observedLayoutDigest")? {
            CborValue::Null => None,
            value => Some(digest_value(value, "observedLayoutDigest")?),
        };
        let operation = RescueOperation::from_value(field(entries, "operation")?)?;
        let plan = Self {
            created_at_epoch_ms: unsigned_field(entries, "createdAtEpochMs")?,
            expires_at_epoch_ms: unsigned_field(entries, "expiresAtEpochMs")?,
            device_id: text_field(entries, "deviceId")?.to_string(),
            device_facts_digest: digest_field(entries, "deviceFactsDigest")?,
            profile_id: text_field(entries, "profileId")?.to_string(),
            profile_version: text_field(entries, "profileVersion")?.to_string(),
            profile_digest: digest_field(entries, "profileDigest")?,
            native_build_digest: digest_field(entries, "nativeBuildDigest")?,
            observed_layout_digest,
            operation,
        };
        plan.validate_shape()?;
        let reencoded = plan
            .to_canonical_bytes()
            .map_err(|error| RescueError::io("re-encode rescue plan", error))?;
        if reencoded != bytes {
            return Err(RescueError::refused(
                "PLAN_INVALID",
                "Rescue plan does not round-trip byte for byte.",
            ));
        }
        Ok(plan)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RescuePlanSummary {
    pub plan_id: String,
    pub plan_sha256: Sha256Digest,
    pub plan: RescuePlan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RescueDisposition {
    SemanticSuccess,
    ConfirmedNoEffect,
    OutcomeUnknown,
}

impl RescueDisposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SemanticSuccess => "semantic-success",
            Self::ConfirmedNoEffect => "confirmed-no-effect",
            Self::OutcomeUnknown => "outcome-unknown",
        }
    }

    pub fn exit_code(self) -> i32 {
        match self {
            Self::SemanticSuccess => 0,
            Self::ConfirmedNoEffect => 7,
            Self::OutcomeUnknown => 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RescueReceipt {
    pub plan_id: String,
    pub plan_digest: Sha256Digest,
    pub device_id: String,
    pub operation: String,
    pub disposition: RescueDisposition,
    pub evidence_digest: Sha256Digest,
    pub completed_at_epoch_ms: u64,
    pub detail: String,
    pub payload_bytes: Option<u64>,
    pub payload_digest: Option<Sha256Digest>,
}

impl CanonicalCbor for RescueReceipt {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("schemaVersion", CborValue::text(RESCUE_RECEIPT_SCHEMA)),
            ("planId", CborValue::text(self.plan_id.clone())),
            ("planDigest", self.plan_digest.to_cbor()),
            ("deviceId", CborValue::text(self.device_id.clone())),
            ("operation", CborValue::text(self.operation.clone())),
            ("disposition", CborValue::text(self.disposition.as_str())),
            ("evidenceDigest", self.evidence_digest.to_cbor()),
            (
                "completedAtEpochMs",
                CborValue::Unsigned(self.completed_at_epoch_ms),
            ),
            ("detail", CborValue::text(self.detail.clone())),
            (
                "payloadBytes",
                self.payload_bytes
                    .map(CborValue::Unsigned)
                    .unwrap_or(CborValue::Null),
            ),
            (
                "payloadDigest",
                self.payload_digest
                    .map(|digest| digest.to_cbor())
                    .unwrap_or(CborValue::Null),
            ),
        ])
    }
}

impl RescueReceipt {
    pub fn digest(&self) -> Result<Sha256Digest, RescueError> {
        let bytes = self
            .to_canonical_bytes()
            .map_err(|error| RescueError::io("encode rescue receipt", error))?;
        Ok(digest_in_domain(Domain::RescueReceipt, &bytes))
    }

    pub fn receipt_id(&self) -> Result<String, RescueError> {
        Ok(format!("rescue-receipt:{}", self.digest()?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RescueApplyResult {
    pub receipt: RescueReceipt,
}

impl RescueApplyResult {
    pub fn exit_code(&self) -> i32 {
        self.receipt.disposition.exit_code()
    }
}

#[derive(Debug)]
pub struct RescueManager<B: RescueBackend> {
    runtime_root: PathBuf,
    profile: DeviceProfile,
    profile_digest: Sha256Digest,
    native_build_digest: Sha256Digest,
    backend: B,
}
