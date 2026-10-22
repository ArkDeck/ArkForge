use super::*;

impl<B: RescueBackend> RescueManager<B> {
    pub(super) fn store_plan(&self, plan: RescuePlan) -> Result<RescuePlanSummary, RescueError> {
        plan.validate_shape()?;
        let bytes = plan
            .to_canonical_bytes()
            .map_err(|error| RescueError::io("encode rescue plan", error))?;
        let digest = digest_in_domain(Domain::RescuePlan, &bytes);
        let plan_id = format!("rescue-plan:{digest}");
        let path = self.plans_dir().join(format!("{digest}.cbor"));
        create_private_dir(&self.plans_dir())?;
        write_content_addressed(&path, &bytes)?;
        Ok(RescuePlanSummary {
            plan_id,
            plan_sha256: digest,
            plan,
        })
    }

    pub(super) fn load_plan(
        &self,
        plan_id: &str,
    ) -> Result<(RescuePlan, Sha256Digest, Vec<u8>), RescueError> {
        let digest_text = plan_id.strip_prefix("rescue-plan:").ok_or_else(|| {
            RescueError::invalid("Use a rescue plan ID in the form rescue-plan:<sha256>.")
        })?;
        let id_digest = Sha256Digest::parse_hex(digest_text).map_err(|error| {
            RescueError::invalid(format!("Rescue plan ID has an invalid digest: {error}"))
        })?;
        let path = self.plans_dir().join(format!("{id_digest}.cbor"));
        let bytes = fs::read(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                RescueError::new(
                    "PLAN_NOT_FOUND",
                    format!("No stored rescue plan matches {plan_id}."),
                    5,
                )
            } else {
                RescueError::io(&format!("read {}", path.display()), error)
            }
        })?;
        let observed = digest_in_domain(Domain::RescuePlan, &bytes);
        if observed != id_digest {
            return Err(RescueError::refused(
                "PLAN_DIGEST_MISMATCH",
                format!("Stored plan hashes to {observed}; its ID names {id_digest}."),
            ));
        }
        let plan = RescuePlan::from_canonical_bytes(&bytes)?;
        Ok((plan, observed, bytes))
    }

    pub(super) fn record_intent(
        &self,
        plan_id: &str,
        plan_digest: Sha256Digest,
        plan_bytes: &[u8],
        now_epoch_ms: u64,
    ) -> Result<(), RescueError> {
        create_private_dir(&self.intents_dir())?;
        let path = self
            .intents_dir()
            .join(format!("{plan_digest}.intent.cbor"));
        let intent = CborValue::map(vec![
            ("schemaVersion", CborValue::text(RESCUE_INTENT_SCHEMA)),
            ("planId", CborValue::text(plan_id)),
            ("planDigest", plan_digest.to_cbor()),
            ("planBytesDigest", sha256(plan_bytes).to_cbor()),
            ("recordedAtEpochMs", CborValue::Unsigned(now_epoch_ms)),
        ])
        .to_canonical_bytes()
        .map_err(|error| RescueError::io("encode rescue intent", error))?;
        match create_private_file_new(&path) {
            Ok(mut file) => {
                file.write_all(&intent)
                    .and_then(|_| file.sync_all())
                    .map_err(|error| RescueError::io("persist rescue intent", error))?;
                sync_parent(&path)?;
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(RescueError::new(
                    "RESCUE_PLAN_ALREADY_APPLIED",
                    format!(
                        "{plan_id} already has a durable intent. Do not replay it; inspect the stored receipt or create a new plan after reconciling the device."
                    ),
                    6,
                ))
            }
            Err(error) => Err(RescueError::io("create rescue intent", error)),
        }
    }

    pub(super) fn store_receipt(&self, receipt: &RescueReceipt) -> Result<(), RescueError> {
        create_private_dir(&self.receipts_dir())?;
        let bytes = receipt
            .to_canonical_bytes()
            .map_err(|error| RescueError::io("encode rescue receipt", error))?;
        let path = self
            .receipts_dir()
            .join(format!("{}.cbor", receipt.plan_digest));
        write_content_addressed(&path, &bytes)
    }

    pub(super) fn open_store(&self) -> Result<ContentAddressedStore, RescueError> {
        ContentAddressedStore::open(
            self.rescue_root().join("store"),
            CasQuota::dayu200_default(),
        )
        .map_err(|error| RescueError::new("RESCUE_STORE_UNAVAILABLE", error.to_string(), 10))
    }

    pub(super) fn rescue_root(&self) -> PathBuf {
        self.runtime_root.join("rescue")
    }

    pub(super) fn plans_dir(&self) -> PathBuf {
        self.rescue_root().join("plans")
    }

    pub(super) fn intents_dir(&self) -> PathBuf {
        self.rescue_root().join("intents")
    }

    pub(super) fn receipts_dir(&self) -> PathBuf {
        self.rescue_root().join("receipts")
    }

    pub(super) fn work_dir(&self) -> PathBuf {
        self.rescue_root().join("work")
    }
}
