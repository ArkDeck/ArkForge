use super::*;

/// What the medium's read face turned out to be.
///
/// architecture.md 16.4: the read window is a runtime-measured fact, never a
/// Profile constant. This is the measurement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeasuredReadDomain {
    /// The backup table at the far end of the medium read back as real data,
    /// so the read face reaches the whole medium.
    Full,
    /// The far end read back as uniform filler while the near end read real
    /// data. Reads past the window return filler regardless of what is on the
    /// medium, so filler carries no information about content out there.
    Windowed { detail: String },
}

impl MeasuredReadDomain {
    pub fn summary(&self) -> &'static str {
        match self {
            MeasuredReadDomain::Full => "full",
            MeasuredReadDomain::Windowed { .. } => "windowed",
        }
    }
}

/// What one action turned out to be, once decoded from its stored body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredAction {
    /// Handled by the authority's control port, not by this Provider
    /// (architecture.md 9.2). Decoded so the caller knows what to ask for, and
    /// what answer would satisfy it.
    ManagedControl {
        control_action: String,
        /// Facts the booted device must answer with, for a postflight probe.
        /// Empty for a control action that only changes state.
        expect: Vec<(String, String)>,
    },
    ProbeLoader,
    ValidatePartitionTable {
        expected_layout_digest: Sha256Digest,
    },
    WritePartition {
        partition: String,
        member: String,
        begin_sector: u64,
    },
    ReadbackPartition {
        partition: String,
        begin_sector: u64,
        max_strength_when_readable: VerificationStrength,
        erased_medium_filler: Option<u8>,
    },
    CharacterizeReadDomain,
    ResetDevice,
}

impl StoredAction {
    /// Decodes the Provider's own stored body.
    ///
    /// Nothing is inferred and nothing defaults: a body this Provider did not
    /// write is an error, because the alternative is executing a half-understood
    /// instruction against a device.
    pub fn decode(record: &PrivateActionRecord) -> Result<StoredAction, ExecutionError> {
        let CborValue::Map(entries) = &record.body else {
            return Err(ExecutionError::ActionUndecodable(
                "body is not a map".into(),
            ));
        };
        let field = |name: &str| -> Option<&CborValue> {
            entries
                .iter()
                .find(|(key, _)| matches!(key, CborValue::Text(text) if text == name))
                .map(|(_, value)| value)
        };
        let text = |name: &str| -> Result<String, ExecutionError> {
            match field(name) {
                Some(CborValue::Text(value)) => Ok(value.clone()),
                _ => Err(ExecutionError::ActionUndecodable(format!(
                    "field {name:?} is missing or not text"
                ))),
            }
        };
        let unsigned = |name: &str| -> Result<u64, ExecutionError> {
            match field(name) {
                Some(CborValue::Unsigned(value)) => Ok(*value),
                _ => Err(ExecutionError::ActionUndecodable(format!(
                    "field {name:?} is missing or not an unsigned integer"
                ))),
            }
        };
        let digest = |name: &str| -> Result<Sha256Digest, ExecutionError> {
            match field(name) {
                Some(CborValue::Bytes(bytes)) if bytes.len() == 32 => {
                    let mut array = [0u8; 32];
                    array.copy_from_slice(bytes);
                    Ok(Sha256Digest::from_bytes(array))
                }
                _ => Err(ExecutionError::ActionUndecodable(format!(
                    "field {name:?} is missing or not a 32-byte digest"
                ))),
            }
        };

        match text("action")?.as_str() {
            "enter-loader" => Ok(StoredAction::ManagedControl {
                control_action: text("controlAction")?,
                expect: Vec::new(),
            }),
            "verify-hdc-postflight" => Ok(StoredAction::ManagedControl {
                control_action: text("controlAction")?,
                expect: match field("expect") {
                    Some(CborValue::Map(pairs)) => pairs
                        .iter()
                        .map(|(key, value)| match (key, value) {
                            (CborValue::Text(key), CborValue::Text(value)) => {
                                Ok((key.clone(), value.clone()))
                            }
                            _ => Err(ExecutionError::ActionUndecodable(
                                "expect holds a non-text entry".into(),
                            )),
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                    _ => {
                        return Err(ExecutionError::ActionUndecodable(
                            "verify-hdc-postflight carries no expect map".into(),
                        ));
                    }
                },
            }),
            "probe-loader" => Ok(StoredAction::ProbeLoader),
            "validate-partition-table" => Ok(StoredAction::ValidatePartitionTable {
                expected_layout_digest: digest("expectedLayoutDigest")?,
            }),
            "write-partition" => Ok(StoredAction::WritePartition {
                partition: text("partition")?,
                member: text("member")?,
                begin_sector: unsigned("beginSector")?,
            }),
            "readback-partition" => Ok(StoredAction::ReadbackPartition {
                partition: text("partition")?,
                begin_sector: unsigned("beginSector")?,
                max_strength_when_readable: parse_strength(&text("maxStrengthWhenReadable")?)?,
                erased_medium_filler: match field("erasedMediumFiller") {
                    Some(CborValue::Unsigned(byte)) if *byte <= 0xFF => Some(*byte as u8),
                    Some(CborValue::Null) => None,
                    _ => {
                        return Err(ExecutionError::ActionUndecodable(
                            "erasedMediumFiller is neither a byte nor null".into(),
                        ));
                    }
                },
            }),
            "characterize-read-domain" => Ok(StoredAction::CharacterizeReadDomain),
            "reset-device" => Ok(StoredAction::ResetDevice),
            other => Err(ExecutionError::ActionUndecodable(format!(
                "unknown stored action {other:?}"
            ))),
        }
    }
}

pub(super) fn parse_strength(text: &str) -> Result<VerificationStrength, ExecutionError> {
    match text {
        "fullHash" => Ok(VerificationStrength::FullHash),
        "sampledRanges" => Ok(VerificationStrength::SampledRanges),
        "prefixHash" => Ok(VerificationStrength::PrefixHash),
        "semanticOnly" => Ok(VerificationStrength::SemanticOnly),
        other => Err(ExecutionError::ActionUndecodable(format!(
            "unknown verification strength {other:?}"
        ))),
    }
}

/// What executing one action produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionOutcome {
    pub action_id: ActionId,
    pub step_id: StepId,
    pub disposition: ActionDisposition,
    pub facts: Vec<(OpaqueId, String)>,
    pub evidence_digest: Sha256Digest,
    /// Present for verification actions only.
    pub verification: Option<VerificationOutcome>,
}

/// Carries what earlier actions established into the ones that need it.
///
/// A write may not be dispatched until this holds an observed partition table,
/// and a readback's verdict depends on the measured read domain. Keeping both
/// here, rather than re-deriving them per action, is what stops a write from
/// running against a table nobody looked at.
#[derive(Debug, Default)]
pub struct ExecutionSession {
    pub(super) observed_table: Option<PartitionTableFact>,
    pub(super) read_domain: Option<MeasuredReadDomain>,
    /// Sectors that returned real (non-uniform) data before any write. A
    /// partition start in here is inside the read face, so filler read there
    /// afterwards is a real failure rather than an unreadable address.
    pub(super) demonstrated_readable: Vec<u64>,
    pub(super) staged: BTreeMap<String, StagedImage>,
    pub(super) validating: BTreeMap<String, JoinHandle<Result<ValidatedImage, ExecutionError>>>,
}

impl ExecutionSession {
    pub fn new(staged: BTreeMap<String, StagedImage>) -> Self {
        ExecutionSession {
            observed_table: None,
            read_domain: None,
            demonstrated_readable: Vec::new(),
            staged,
            validating: BTreeMap::new(),
        }
    }

    pub fn observed_table(&self) -> Option<&PartitionTableFact> {
        self.observed_table.as_ref()
    }

    pub fn read_domain(&self) -> Option<&MeasuredReadDomain> {
        self.read_domain.as_ref()
    }

    pub fn note_readable_sector(&mut self, sector: u64) {
        if !self.demonstrated_readable.contains(&sector) {
            self.demonstrated_readable.push(sector);
        }
    }

    pub fn set_read_domain(&mut self, domain: MeasuredReadDomain) {
        self.read_domain = Some(domain);
    }

    pub fn set_observed_table(&mut self, table: PartitionTableFact) {
        self.observed_table = Some(table);
    }

    /// Records an image that has been extracted and verified.
    ///
    /// Staging happens outside this module — it needs the content store, which
    /// a Provider has no business holding — so the session receives the result
    /// rather than producing it.
    pub fn stage(&mut self, member: String, image: StagedImage) {
        self.staged.insert(member, image);
    }

    /// Starts one full SHA-256 worker per independent staged image. The
    /// resulting proof retains the exact open descriptor, so waiting for a
    /// later partition write does not reopen a pathname or trust metadata in
    /// place of bytes.
    pub fn begin_parallel_staged_validation(&mut self) {
        for (member, image) in &self.staged {
            if self.validating.contains_key(member) {
                continue;
            }
            let image = image.clone();
            self.validating.insert(
                member.clone(),
                std::thread::spawn(move || image.open_and_revalidate()),
            );
        }
    }

    pub(super) fn take_validated_image(
        &mut self,
        member: &str,
    ) -> Result<(ValidatedImage, u64, bool), ExecutionError> {
        let waited = Instant::now();
        let (result, parallel) = match self.validating.remove(member) {
            Some(worker) => (
                worker.join().map_err(|_| {
                    ExecutionError::StagingChanged(format!(
                        "the SHA-256 worker for {member} terminated unexpectedly"
                    ))
                })?,
                true,
            ),
            None => (
                self.staged
                    .get(member)
                    .ok_or_else(|| ExecutionError::ImageNotStaged(member.to_string()))?
                    .open_and_revalidate(),
                false,
            ),
        };
        Ok((result?, waited.elapsed().as_millis() as u64, parallel))
    }
}
