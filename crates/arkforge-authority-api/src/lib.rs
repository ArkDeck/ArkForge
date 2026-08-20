//! # arkforge-authority-api
//!
//! The neutral authority boundary.
//!
//! architecture.md 8 and 9. ArkForge asks; an authority decides. Nothing in
//! this crate names ArkDeck, and nothing here can mint permission — the two
//! properties that let the same daemon serve a different authority later
//! without weakening this one.
//!
//! The asymmetry is deliberate and enforced by module placement:
//! [`verify_permit`] lives here, [`authority_side::mint_integrity_tag`] lives
//! behind a module the daemon must not reference. An architecture guard test
//! asserts `arkforged` never names it.

#![forbid(unsafe_code)]

use arkforge_core::digest::{
    CanonicalCbor, CborError, CborValue, Domain, Sha256Digest, constant_time_eq, decode_canonical,
    digest_canonical, hmac_sha256,
};
use arkforge_core::effect::EffectSet;
use arkforge_core::ids::{
    ActionId, AttemptId, ControllerSessionId, JobId, OpaqueId, PermitId, PlanId, RequestId, StepId,
};
use arkforge_core::{AuthorityBindingRef, AuthorityNamespace};
use core::fmt;

/// Facts read immediately before asking for a permit (architecture.md 8.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepAdmissionSnapshot {
    pub captured_at_epoch_ms: u64,
    /// The wall-clock backstop. It is a ceiling on the whole
    /// snapshot → re-verify → permit → dispatch round trip, budgeted per step
    /// kind — never a short global constant (architecture.md 8.3).
    pub freshness_deadline_epoch_ms: u64,
    pub device_facts_digest: Sha256Digest,
    /// The continuity fact. While this is unchanged and no detach was seen, the
    /// device under the handle is the device that was admitted.
    pub transport_session_digest: Option<Sha256Digest>,
    pub provider_facts_digest: Sha256Digest,
    pub toolchain_facts_digest: Sha256Digest,
    pub artifact_facts_digest: Sha256Digest,
}

impl StepAdmissionSnapshot {
    pub fn digest(&self) -> Result<Sha256Digest, CborError> {
        digest_canonical(Domain::AdmissionSnapshot, self)
    }
}

impl CanonicalCbor for StepAdmissionSnapshot {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            (
                "capturedAtEpochMs",
                CborValue::Unsigned(self.captured_at_epoch_ms),
            ),
            (
                "freshnessDeadlineEpochMs",
                CborValue::Unsigned(self.freshness_deadline_epoch_ms),
            ),
            ("deviceFactsDigest", self.device_facts_digest.to_cbor()),
            (
                "transportSessionDigest",
                match self.transport_session_digest {
                    Some(digest) => digest.to_cbor(),
                    None => CborValue::Null,
                },
            ),
            ("providerFactsDigest", self.provider_facts_digest.to_cbor()),
            (
                "toolchainFactsDigest",
                self.toolchain_facts_digest.to_cbor(),
            ),
            ("artifactFactsDigest", self.artifact_facts_digest.to_cbor()),
        ])
    }
}

/// What a re-check of the facts concluded just before dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreshnessVerdict {
    /// Continuity holds and the backstop has not expired. Dispatch may proceed.
    Fresh,
    /// The wall clock ran out while continuity held.
    ///
    /// This is a stale snapshot, not a device fault: re-snapshot and ask again.
    /// It must not consume a destructive budget, because nothing about the
    /// device went wrong (architecture.md 8.3).
    StaleSnapshot { elapsed_ms: u64 },
    /// The device under the handle is not provably the admitted device any
    /// more. Zero dispatch.
    ContinuityBroken(ContinuityBreak),
}

impl FreshnessVerdict {
    pub fn permits_dispatch(&self) -> bool {
        matches!(self, FreshnessVerdict::Fresh)
    }

    /// Whether the operation may simply retry admission with fresh facts.
    pub fn is_retryable_without_device_blame(&self) -> bool {
        matches!(self, FreshnessVerdict::StaleSnapshot { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuityBreak {
    SessionChanged,
    DetachObserved,
    DeviceFactsChanged,
    ProviderFactsChanged,
    ToolchainFactsChanged,
    ArtifactFactsChanged,
}

/// The facts as they are *now*, to compare against the admitted snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentFacts {
    pub now_epoch_ms: u64,
    pub device_facts_digest: Sha256Digest,
    pub transport_session_digest: Option<Sha256Digest>,
    pub saw_detach_since_snapshot: bool,
    pub provider_facts_digest: Sha256Digest,
    pub toolchain_facts_digest: Sha256Digest,
    pub artifact_facts_digest: Sha256Digest,
}

/// Re-checks freshness before dispatch (architecture.md 8.5 step 6–7).
///
/// Continuity is the primary fact and the wall clock is the backstop, in that
/// order: a slow host must not be reported as a device that moved.
pub fn evaluate_freshness(
    snapshot: &StepAdmissionSnapshot,
    current: &CurrentFacts,
) -> FreshnessVerdict {
    if current.saw_detach_since_snapshot {
        return FreshnessVerdict::ContinuityBroken(ContinuityBreak::DetachObserved);
    }
    if snapshot.transport_session_digest != current.transport_session_digest {
        return FreshnessVerdict::ContinuityBroken(ContinuityBreak::SessionChanged);
    }
    if snapshot.device_facts_digest != current.device_facts_digest {
        return FreshnessVerdict::ContinuityBroken(ContinuityBreak::DeviceFactsChanged);
    }
    if snapshot.provider_facts_digest != current.provider_facts_digest {
        return FreshnessVerdict::ContinuityBroken(ContinuityBreak::ProviderFactsChanged);
    }
    if snapshot.toolchain_facts_digest != current.toolchain_facts_digest {
        return FreshnessVerdict::ContinuityBroken(ContinuityBreak::ToolchainFactsChanged);
    }
    if snapshot.artifact_facts_digest != current.artifact_facts_digest {
        return FreshnessVerdict::ContinuityBroken(ContinuityBreak::ArtifactFactsChanged);
    }
    if current.now_epoch_ms >= snapshot.freshness_deadline_epoch_ms {
        return FreshnessVerdict::StaleSnapshot {
            elapsed_ms: current
                .now_epoch_ms
                .saturating_sub(snapshot.captured_at_epoch_ms),
        };
    }
    FreshnessVerdict::Fresh
}

/// What ArkForge asks the authority for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepAdmissionRequest {
    pub request_id: RequestId,
    pub controller_session_id: ControllerSessionId,
    pub job_id: JobId,
    pub plan_id: PlanId,
    pub plan_digest: Sha256Digest,
    pub step_id: StepId,
    pub attempt_id: AttemptId,
    pub public_step_digest: Sha256Digest,
    pub private_action_digest: Sha256Digest,
    pub effect_set_digest: Sha256Digest,
    pub authority_binding: AuthorityBindingRef,
    pub admission_snapshot: StepAdmissionSnapshot,
    pub requested_at_epoch_ms: u64,
}

/// The authority's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepPermitDecision {
    Granted(Box<StepPermit>),
    Refused { reason: String },
}

/// A single-use permission to perform exactly one private action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepPermit {
    pub permit_id: PermitId,
    pub authority_namespace: AuthorityNamespace,
    pub controller_session_id: ControllerSessionId,
    pub job_id: JobId,
    pub plan_id: PlanId,
    pub plan_digest: Sha256Digest,
    pub step_id: StepId,
    pub attempt_id: AttemptId,
    pub public_step_digest: Sha256Digest,
    pub private_action_digest: Sha256Digest,
    pub effect_set_digest: Sha256Digest,
    pub authority_binding: AuthorityBindingRef,
    pub admitted_device_facts_digest: Sha256Digest,
    pub issued_at_epoch_ms: u64,
    pub expires_at_epoch_ms: u64,
    pub single_use: bool,
    pub integrity_tag: PermitIntegrityTag,
}

impl StepPermit {
    /// The bytes the integrity tag covers: everything but the tag itself.
    pub fn signing_body(&self) -> Result<Vec<u8>, CborError> {
        permit_body(self).to_canonical_bytes()
    }

    /// Reads a permit back from the exact canonical bytes an authority signed.
    ///
    /// The integrity tag is **not** in these bytes and is not set here: it
    /// travels beside them and is checked by [`verify_permit`]. A permit
    /// carrying its own tag inside the body it signs would be signing a claim
    /// about itself.
    ///
    /// Re-encoding this value must reproduce `input` byte for byte, and
    /// [`Self::from_canonical_bytes`] enforces that rather than trusting it: a
    /// permit whose encoding differs from what the authority signed is a
    /// different permit, and the tag would be verified against the wrong bytes.
    /// Nothing here defaults — a missing field is a malformed permit, because
    /// the alternative is executing under a permit that says less than the one
    /// the authority meant to sign.
    pub fn from_canonical_bytes(input: &[u8]) -> Result<StepPermit, PermitDecodeError> {
        let value = decode_canonical(input).map_err(PermitDecodeError::Cbor)?;
        let CborValue::Map(entries) = value else {
            return Err(PermitDecodeError::NotAMap);
        };
        let field = |name: &str| -> Option<&CborValue> {
            entries
                .iter()
                .find(|(key, _)| matches!(key, CborValue::Text(text) if text == name))
                .map(|(_, value)| value)
        };
        let text = |name: &'static str| -> Result<&str, PermitDecodeError> {
            match field(name) {
                Some(CborValue::Text(value)) => Ok(value.as_str()),
                _ => Err(PermitDecodeError::Field(name)),
            }
        };
        let unsigned = |name: &'static str| -> Result<u64, PermitDecodeError> {
            match field(name) {
                Some(CborValue::Unsigned(value)) => Ok(*value),
                _ => Err(PermitDecodeError::Field(name)),
            }
        };
        let boolean = |name: &'static str| -> Result<bool, PermitDecodeError> {
            match field(name) {
                Some(CborValue::Bool(value)) => Ok(*value),
                _ => Err(PermitDecodeError::Field(name)),
            }
        };
        let digest = |name: &'static str| -> Result<Sha256Digest, PermitDecodeError> {
            match field(name) {
                Some(CborValue::Bytes(bytes)) if bytes.len() == 32 => {
                    let mut array = [0u8; 32];
                    array.copy_from_slice(bytes);
                    Ok(Sha256Digest::from_bytes(array))
                }
                _ => Err(PermitDecodeError::Field(name)),
            }
        };
        let binding = match field("authorityBinding") {
            Some(CborValue::Map(pairs)) => {
                let sub = |name: &str| -> Option<&CborValue> {
                    pairs
                        .iter()
                        .find(|(key, _)| matches!(key, CborValue::Text(text) if text == name))
                        .map(|(_, value)| value)
                };
                let namespace = match sub("authorityNamespace") {
                    Some(CborValue::Text(value)) => AuthorityNamespace::new(value)
                        .map_err(|_| PermitDecodeError::Field("authorityBinding"))?,
                    _ => return Err(PermitDecodeError::Field("authorityBinding")),
                };
                let binding_id = match sub("bindingId") {
                    Some(CborValue::Text(value)) => OpaqueId::new(value)
                        .map_err(|_| PermitDecodeError::Field("authorityBinding"))?,
                    _ => return Err(PermitDecodeError::Field("authorityBinding")),
                };
                let revision = match sub("bindingRevision") {
                    Some(CborValue::Unsigned(value)) => *value,
                    _ => return Err(PermitDecodeError::Field("authorityBinding")),
                };
                let identity = match sub("stableIdentityDigest") {
                    Some(CborValue::Bytes(bytes)) if bytes.len() == 32 => {
                        let mut array = [0u8; 32];
                        array.copy_from_slice(bytes);
                        Sha256Digest::from_bytes(array)
                    }
                    _ => return Err(PermitDecodeError::Field("authorityBinding")),
                };
                AuthorityBindingRef {
                    authority_namespace: namespace,
                    binding_id,
                    binding_revision: revision,
                    stable_identity_digest: identity,
                }
            }
            _ => return Err(PermitDecodeError::Field("authorityBinding")),
        };

        let permit = StepPermit {
            permit_id: PermitId::new(text("permitId")?)
                .map_err(|_| PermitDecodeError::Field("permitId"))?,
            authority_namespace: AuthorityNamespace::new(text("authorityNamespace")?)
                .map_err(|_| PermitDecodeError::Field("authorityNamespace"))?,
            controller_session_id: ControllerSessionId::new(text("controllerSessionId")?)
                .map_err(|_| PermitDecodeError::Field("controllerSessionId"))?,
            job_id: JobId::new(text("jobId")?).map_err(|_| PermitDecodeError::Field("jobId"))?,
            plan_id: PlanId::new(text("planId")?)
                .map_err(|_| PermitDecodeError::Field("planId"))?,
            plan_digest: digest("planDigest")?,
            step_id: StepId::new(text("stepId")?)
                .map_err(|_| PermitDecodeError::Field("stepId"))?,
            attempt_id: AttemptId::new(text("attemptId")?)
                .map_err(|_| PermitDecodeError::Field("attemptId"))?,
            public_step_digest: digest("publicStepDigest")?,
            private_action_digest: digest("privateActionDigest")?,
            effect_set_digest: digest("effectSetDigest")?,
            authority_binding: binding,
            admitted_device_facts_digest: digest("admittedDeviceFactsDigest")?,
            issued_at_epoch_ms: unsigned("issuedAtEpochMs")?,
            expires_at_epoch_ms: unsigned("expiresAtEpochMs")?,
            single_use: boolean("singleUse")?,
            // Set by the caller from the tag that travelled beside the bytes.
            integrity_tag: PermitIntegrityTag {
                epoch: PairingEpoch(0),
                tag: Sha256Digest::from_bytes([0u8; 32]),
            },
        };

        // The round trip is the check. If re-encoding differs, the bytes the
        // authority signed are not the bytes this value stands for, and the
        // tag would be verified against something else.
        let reencoded = permit.signing_body().map_err(PermitDecodeError::Cbor)?;
        if reencoded != input {
            return Err(PermitDecodeError::NotCanonical);
        }
        Ok(permit)
    }
}

/// Why a permit could not be read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermitDecodeError {
    NotAMap,
    /// A field is missing or has the wrong shape. Named, so a malformed permit
    /// says which field rather than "invalid".
    Field(&'static str),
    /// The bytes decode, but re-encoding them produces something else — so
    /// they are not the deterministic encoding the tag was computed over.
    NotCanonical,
    Cbor(CborError),
}

impl fmt::Display for PermitDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PermitDecodeError::NotAMap => f.write_str("the permit is not a CBOR map"),
            PermitDecodeError::Field(name) => {
                write!(f, "permit field {name:?} is missing or malformed")
            }
            PermitDecodeError::NotCanonical => f.write_str(
                "the permit bytes are not the deterministic encoding the integrity tag covers; \
                 re-encoding them produces different bytes (RFC 8949 4.2.1)",
            ),
            PermitDecodeError::Cbor(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for PermitDecodeError {}

fn permit_body(permit: &StepPermit) -> CborValue {
    CborValue::map(vec![
        ("permitId", permit.permit_id.to_cbor()),
        ("authorityNamespace", permit.authority_namespace.to_cbor()),
        (
            "controllerSessionId",
            permit.controller_session_id.to_cbor(),
        ),
        ("jobId", permit.job_id.to_cbor()),
        ("planId", permit.plan_id.to_cbor()),
        ("planDigest", permit.plan_digest.to_cbor()),
        ("stepId", permit.step_id.to_cbor()),
        ("attemptId", permit.attempt_id.to_cbor()),
        ("publicStepDigest", permit.public_step_digest.to_cbor()),
        (
            "privateActionDigest",
            permit.private_action_digest.to_cbor(),
        ),
        ("effectSetDigest", permit.effect_set_digest.to_cbor()),
        ("authorityBinding", permit.authority_binding.to_cbor()),
        (
            "admittedDeviceFactsDigest",
            permit.admitted_device_facts_digest.to_cbor(),
        ),
        (
            "issuedAtEpochMs",
            CborValue::Unsigned(permit.issued_at_epoch_ms),
        ),
        (
            "expiresAtEpochMs",
            CborValue::Unsigned(permit.expires_at_epoch_ms),
        ),
        ("singleUse", CborValue::Bool(permit.single_use)),
    ])
}

/// An HMAC over the permit body, keyed by the controller pairing secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermitIntegrityTag {
    pub epoch: PairingEpoch,
    pub tag: Sha256Digest,
}

/// Identifies which pairing secret minted a tag.
///
/// The epoch rotates whenever either process restarts. An unconsumed permit
/// endorsed by an old epoch can never be consumed for the first time: it is
/// void and admission has to run again (architecture.md 8.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PairingEpoch(pub u64);

/// The shared secret established when the authority starts the daemon.
///
/// Held in memory only, never written to disk in the clear (architecture.md
/// 15.2). `ControllerPairingSecret` can verify; only the authority-side module
/// can mint.
#[derive(Clone)]
pub struct ControllerPairingSecret {
    epoch: PairingEpoch,
    secret: Vec<u8>,
}

impl fmt::Debug for ControllerPairingSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never render the bytes.
        f.debug_struct("ControllerPairingSecret")
            .field("epoch", &self.epoch)
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl ControllerPairingSecret {
    pub fn new(epoch: PairingEpoch, secret: Vec<u8>) -> Self {
        ControllerPairingSecret { epoch, secret }
    }

    pub fn epoch(&self) -> PairingEpoch {
        self.epoch
    }

    fn compute(&self, body: &[u8]) -> Sha256Digest {
        hmac_sha256(&self.secret, body)
    }
}

/// A permit that has been checked and may be handed to a provider.
///
/// It cannot be constructed except by [`verify_permit`], so a provider cannot
/// be given an unverified permit by mistake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedStepPermit {
    permit: StepPermit,
}

impl VerifiedStepPermit {
    pub fn permit(&self) -> &StepPermit {
        &self.permit
    }

    pub fn private_action_digest(&self) -> Sha256Digest {
        self.permit.private_action_digest
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermitVerificationError {
    /// The tag does not verify. This is an authority-boundary event: zero
    /// dispatch, journal it, fail closed (architecture.md 8.6).
    IntegrityTagInvalid,
    /// Endorsed by a secret from before a restart.
    StalePairingEpoch {
        permit: PairingEpoch,
        current: PairingEpoch,
    },
    Expired {
        expires_at_epoch_ms: u64,
        now_epoch_ms: u64,
    },
    /// The permit does not authorize the action about to run.
    ActionMismatch {
        expected: Sha256Digest,
        found: Sha256Digest,
    },
    PlanMismatch {
        expected: Sha256Digest,
        found: Sha256Digest,
    },
    /// A signed field is authentic but does not describe the dispatch that is
    /// currently about to happen. Authenticity is not authorization for a
    /// different job, step, attempt, binding, session, effect set, or device.
    ContextMismatch {
        field: &'static str,
    },
    /// The permit was already consumed. Return the original receipt; do not
    /// dispatch again (architecture.md 8.5).
    AlreadyConsumed,
    NotSingleUse,
    Cbor(CborError),
}

impl fmt::Display for PermitVerificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PermitVerificationError::IntegrityTagInvalid => {
                f.write_str("permit integrity tag does not verify")
            }
            PermitVerificationError::StalePairingEpoch { permit, current } => write!(
                f,
                "permit was endorsed in pairing epoch {} but the current epoch is {}",
                permit.0, current.0
            ),
            PermitVerificationError::Expired {
                expires_at_epoch_ms,
                now_epoch_ms,
            } => write!(
                f,
                "permit expired at {expires_at_epoch_ms}; now is {now_epoch_ms}"
            ),
            PermitVerificationError::ActionMismatch { expected, found } => write!(
                f,
                "permit authorizes action {found} but {expected} is about to run"
            ),
            PermitVerificationError::PlanMismatch { expected, found } => {
                write!(f, "permit is for plan {found}, not {expected}")
            }
            PermitVerificationError::ContextMismatch { field } => {
                write!(f, "permit {field} does not match the pending dispatch")
            }
            PermitVerificationError::AlreadyConsumed => {
                f.write_str("permit was already consumed; return the original receipt")
            }
            PermitVerificationError::NotSingleUse => {
                f.write_str("a permit for an external effect must be single use")
            }
            PermitVerificationError::Cbor(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for PermitVerificationError {}

/// What the daemon is about to do, checked against what the permit allows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchIntent {
    pub controller_session_id: ControllerSessionId,
    pub job_id: JobId,
    pub plan_id: PlanId,
    pub plan_digest: Sha256Digest,
    pub step_id: StepId,
    pub attempt_id: AttemptId,
    pub public_step_digest: Sha256Digest,
    pub private_action_digest: Sha256Digest,
    pub effect_set_digest: Sha256Digest,
    pub authority_binding: AuthorityBindingRef,
    pub admitted_device_facts_digest: Sha256Digest,
    pub now_epoch_ms: u64,
}

/// Verifies a permit. The only way to obtain a [`VerifiedStepPermit`].
pub fn verify_permit(
    permit: &StepPermit,
    secret: &ControllerPairingSecret,
    intent: &DispatchIntent,
    already_consumed: bool,
) -> Result<VerifiedStepPermit, PermitVerificationError> {
    if already_consumed {
        return Err(PermitVerificationError::AlreadyConsumed);
    }
    if !permit.single_use {
        return Err(PermitVerificationError::NotSingleUse);
    }
    if permit.integrity_tag.epoch != secret.epoch() {
        return Err(PermitVerificationError::StalePairingEpoch {
            permit: permit.integrity_tag.epoch,
            current: secret.epoch(),
        });
    }

    let body = permit
        .signing_body()
        .map_err(PermitVerificationError::Cbor)?;
    let expected = secret.compute(&body);
    if !constant_time_eq(&expected, &permit.integrity_tag.tag) {
        return Err(PermitVerificationError::IntegrityTagInvalid);
    }

    if intent.now_epoch_ms >= permit.expires_at_epoch_ms {
        return Err(PermitVerificationError::Expired {
            expires_at_epoch_ms: permit.expires_at_epoch_ms,
            now_epoch_ms: intent.now_epoch_ms,
        });
    }
    if permit.plan_digest != intent.plan_digest {
        return Err(PermitVerificationError::PlanMismatch {
            expected: intent.plan_digest,
            found: permit.plan_digest,
        });
    }
    if permit.private_action_digest != intent.private_action_digest {
        return Err(PermitVerificationError::ActionMismatch {
            expected: intent.private_action_digest,
            found: permit.private_action_digest,
        });
    }
    if permit.authority_namespace != intent.authority_binding.authority_namespace {
        return Err(PermitVerificationError::ContextMismatch {
            field: "authorityNamespace",
        });
    }
    for (matches, field) in [
        (
            permit.controller_session_id == intent.controller_session_id,
            "controllerSessionId",
        ),
        (permit.job_id == intent.job_id, "jobId"),
        (permit.plan_id == intent.plan_id, "planId"),
        (permit.step_id == intent.step_id, "stepId"),
        (permit.attempt_id == intent.attempt_id, "attemptId"),
        (
            permit.public_step_digest == intent.public_step_digest,
            "publicStepDigest",
        ),
        (
            permit.effect_set_digest == intent.effect_set_digest,
            "effectSetDigest",
        ),
        (
            permit.authority_binding == intent.authority_binding,
            "authorityBinding",
        ),
        (
            permit.admitted_device_facts_digest == intent.admitted_device_facts_digest,
            "admittedDeviceFactsDigest",
        ),
    ] {
        if !matches {
            return Err(PermitVerificationError::ContextMismatch { field });
        }
    }

    Ok(VerifiedStepPermit {
        permit: permit.clone(),
    })
}

/// Minting. Authority adapters only.
///
/// `arkforged` must never reference this module: it verifies, it does not mint
/// (architecture.md 8.6). The architecture guard test enforces that.
pub mod authority_side {
    use super::{ControllerPairingSecret, PermitIntegrityTag, StepPermit};
    use arkforge_core::digest::CborError;

    /// Mints the integrity tag for a permit the authority has decided to grant.
    ///
    /// The authority must persist the complete permit, tag included, before
    /// returning it. A retransmission replays the stored bytes; deterministic
    /// re-derivation is forbidden, because two byte-different copies of "the
    /// same" permit is exactly the ambiguity the tag exists to remove
    /// (architecture.md 8.6).
    pub fn mint_integrity_tag(
        permit: &StepPermit,
        secret: &ControllerPairingSecret,
    ) -> Result<PermitIntegrityTag, CborError> {
        let body = permit.signing_body()?;
        Ok(PermitIntegrityTag {
            epoch: secret.epoch(),
            tag: super::hmac_sha256(secret_bytes(secret), &body),
        })
    }

    fn secret_bytes(secret: &ControllerPairingSecret) -> &[u8] {
        secret.raw()
    }
}

impl ControllerPairingSecret {
    /// Exposed only within this crate's authority-side module.
    fn raw(&self) -> &[u8] {
        &self.secret
    }
}

/// The neutral authority interface (architecture.md 8.1).
///
/// Only the Engine calls it. Providers have no handle to it, by construction:
/// nothing in the provider SPI takes one.
pub trait ExecutionAuthority: fmt::Debug + Send + Sync {
    fn request_step_permit(
        &self,
        request: StepAdmissionRequest,
    ) -> Result<StepPermitDecision, AuthorityError>;

    fn acknowledge_receipt(&self, receipt: ActionReceiptSummary) -> Result<(), AuthorityError>;
}

/// Actions the authority performs on ArkForge's behalf, because it owns the
/// device control channel (architecture.md 9.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ManagedDeviceControlAction {
    EnterUpdater,
    RebootToNormal,
    ReadProductFacts,
    ReadBuildFacts,
}

impl ManagedDeviceControlAction {
    pub fn as_str(self) -> &'static str {
        match self {
            ManagedDeviceControlAction::EnterUpdater => "enter-updater",
            ManagedDeviceControlAction::RebootToNormal => "reboot-to-normal",
            ManagedDeviceControlAction::ReadProductFacts => "read-product-facts",
            ManagedDeviceControlAction::ReadBuildFacts => "read-build-facts",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "enter-updater" => Some(ManagedDeviceControlAction::EnterUpdater),
            "reboot-to-normal" => Some(ManagedDeviceControlAction::RebootToNormal),
            "read-product-facts" => Some(ManagedDeviceControlAction::ReadProductFacts),
            "read-build-facts" => Some(ManagedDeviceControlAction::ReadBuildFacts),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedControlReceipt {
    pub action: ManagedDeviceControlAction,
    pub accepted: bool,
    pub facts: Vec<(OpaqueId, String)>,
    pub evidence_digest: Sha256Digest,
}

/// The typed control port.
///
/// ArkForge names a semantic action. It never receives an executable path, a
/// connect key, an argv, a shell, or any server lifecycle control
/// (architecture.md 9.2) — none of those appear in this signature.
pub trait ManagedDeviceControlPort: fmt::Debug + Send + Sync {
    fn execute(
        &self,
        action: ManagedDeviceControlAction,
        target: &AuthorityBindingRef,
        permit: &VerifiedStepPermit,
    ) -> Result<ManagedControlReceipt, AuthorityError>;
}

/// What ArkForge tells the authority happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionReceiptSummary {
    pub job_id: JobId,
    pub plan_id: PlanId,
    pub step_id: StepId,
    pub action_id: ActionId,
    pub attempt_id: AttemptId,
    pub permit_id: PermitId,
    pub disposition: ActionDisposition,
    pub observed_effects: EffectSet,
    pub possible_effects: Option<PossibleEffectSet>,
    pub receipt_digest: Sha256Digest,
}

/// How an action ended.
///
/// Defined in Core rather than here: a disposition is domain vocabulary that a
/// Provider produces and an authority consumes, and duplicating a four-variant
/// enum on both sides of that boundary is how the two spellings drift apart.
pub use arkforge_core::outcome::ActionDisposition;

/// How completely the possible effects of an unresolved action are bounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EffectSetCompleteness {
    /// Every possible effect is enumerated.
    Bounded,
    /// The effects cannot be bounded. Recovery eligibility is false
    /// (architecture.md 14.3).
    Unbounded,
}

/// The conservative union of what an unresolved action might have done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PossibleEffectSet {
    pub effects: EffectSet,
    pub completeness: EffectSetCompleteness,
    pub source_action_ids: Vec<ActionId>,
}

impl PossibleEffectSet {
    pub fn digest(&self) -> Result<Sha256Digest, CborError> {
        digest_canonical(Domain::PossibleEffectSet, self)
    }

    /// Recovery may only be considered when the effects are bounded.
    pub fn permits_recovery_assessment(&self) -> bool {
        self.completeness == EffectSetCompleteness::Bounded
    }
}

impl CanonicalCbor for PossibleEffectSet {
    fn to_cbor(&self) -> CborValue {
        CborValue::map(vec![
            ("effects", self.effects.to_cbor()),
            (
                "completeness",
                CborValue::text(match self.completeness {
                    EffectSetCompleteness::Bounded => "bounded",
                    EffectSetCompleteness::Unbounded => "unbounded",
                }),
            ),
            (
                "sourceActionIds",
                CborValue::array(
                    self.source_action_ids
                        .iter()
                        .map(|id| id.to_cbor())
                        .collect(),
                ),
            ),
        ])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorityError {
    Refused(String),
    Unavailable(String),
    Protocol(String),
}

impl fmt::Display for AuthorityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuthorityError::Refused(reason) => write!(f, "authority refused: {reason}"),
            AuthorityError::Unavailable(reason) => write!(f, "authority unavailable: {reason}"),
            AuthorityError::Protocol(reason) => write!(f, "authority protocol error: {reason}"),
        }
    }
}

impl std::error::Error for AuthorityError {}

#[cfg(test)]
mod tests {
    use super::*;
    use arkforge_core::digest::sha256;

    fn secret(epoch: u64) -> ControllerPairingSecret {
        ControllerPairingSecret::new(PairingEpoch(epoch), b"pairing-secret-bytes".to_vec())
    }

    fn permit(secret: &ControllerPairingSecret) -> StepPermit {
        let mut permit = StepPermit {
            permit_id: PermitId::new("PERMIT-001").unwrap(),
            authority_namespace: AuthorityNamespace::new("test-authority").unwrap(),
            controller_session_id: ControllerSessionId::new("SESSION-1").unwrap(),
            job_id: JobId::new("JOB-1").unwrap(),
            plan_id: PlanId::new("PLAN-1").unwrap(),
            plan_digest: sha256(b"plan"),
            step_id: StepId::new("STEP-005").unwrap(),
            attempt_id: AttemptId::new("ATTEMPT-1").unwrap(),
            public_step_digest: sha256(b"public step"),
            private_action_digest: sha256(b"private action"),
            effect_set_digest: sha256(b"effects"),
            authority_binding: AuthorityBindingRef {
                authority_namespace: AuthorityNamespace::new("test-authority").unwrap(),
                binding_id: OpaqueId::new("TGT-1").unwrap(),
                binding_revision: 2,
                stable_identity_digest: sha256(b"device"),
            },
            admitted_device_facts_digest: sha256(b"facts"),
            issued_at_epoch_ms: 1_000,
            expires_at_epoch_ms: 61_000,
            single_use: true,
            integrity_tag: PermitIntegrityTag {
                epoch: secret.epoch(),
                tag: sha256(b"placeholder"),
            },
        };
        permit.integrity_tag =
            authority_side::mint_integrity_tag(&permit, secret).expect("mintable");
        permit
    }

    fn intent(permit: &StepPermit, now: u64) -> DispatchIntent {
        DispatchIntent {
            controller_session_id: permit.controller_session_id.clone(),
            job_id: permit.job_id.clone(),
            plan_id: permit.plan_id.clone(),
            plan_digest: permit.plan_digest,
            step_id: permit.step_id.clone(),
            attempt_id: permit.attempt_id.clone(),
            public_step_digest: permit.public_step_digest,
            private_action_digest: permit.private_action_digest,
            effect_set_digest: permit.effect_set_digest,
            authority_binding: permit.authority_binding.clone(),
            admitted_device_facts_digest: permit.admitted_device_facts_digest,
            now_epoch_ms: now,
        }
    }

    #[test]
    fn a_freshly_minted_permit_verifies() {
        let secret = secret(1);
        let permit = permit(&secret);
        let verified = verify_permit(&permit, &secret, &intent(&permit, 2_000), false).unwrap();
        assert_eq!(
            verified.private_action_digest(),
            permit.private_action_digest
        );
    }

    #[test]
    fn any_edit_to_the_permit_body_invalidates_the_tag() {
        let secret = secret(1);
        let base = permit(&secret);
        for mutate in [
            (|p: &mut StepPermit| p.private_action_digest = sha256(b"other action"))
                as fn(&mut StepPermit),
            |p: &mut StepPermit| p.expires_at_epoch_ms = u64::MAX,
            |p: &mut StepPermit| p.authority_binding.binding_revision = 3,
            |p: &mut StepPermit| p.step_id = StepId::new("STEP-006").unwrap(),
            |p: &mut StepPermit| p.admitted_device_facts_digest = sha256(b"another device"),
        ] {
            let mut tampered = base.clone();
            mutate(&mut tampered);
            let result = verify_permit(&tampered, &secret, &intent(&tampered, 2_000), false);
            assert!(
                matches!(result, Err(PermitVerificationError::IntegrityTagInvalid)),
                "tamper produced {result:?}"
            );
        }
    }

    #[test]
    fn a_permit_from_a_previous_pairing_epoch_cannot_be_consumed() {
        let old = secret(1);
        let permit = permit(&old);
        let rotated = ControllerPairingSecret::new(PairingEpoch(2), b"a different secret".to_vec());
        assert!(matches!(
            verify_permit(&permit, &rotated, &intent(&permit, 2_000), false),
            Err(PermitVerificationError::StalePairingEpoch { .. })
        ));
    }

    #[test]
    fn an_expired_permit_cannot_be_consumed_for_the_first_time() {
        let secret = secret(1);
        let permit = permit(&secret);
        assert!(matches!(
            verify_permit(&permit, &secret, &intent(&permit, 61_000), false),
            Err(PermitVerificationError::Expired { .. })
        ));
    }

    #[test]
    fn a_consumed_permit_is_refused_rather_than_re_dispatched() {
        let secret = secret(1);
        let permit = permit(&secret);
        assert!(matches!(
            verify_permit(&permit, &secret, &intent(&permit, 2_000), true),
            Err(PermitVerificationError::AlreadyConsumed)
        ));
    }

    #[test]
    fn a_permit_for_another_action_does_not_authorize_this_one() {
        let secret = secret(1);
        let permit = permit(&secret);
        let mut wrong = intent(&permit, 2_000);
        wrong.private_action_digest = sha256(b"a different action");
        assert!(matches!(
            verify_permit(&permit, &secret, &wrong, false),
            Err(PermitVerificationError::ActionMismatch { .. })
        ));
    }

    #[test]
    fn every_exact_context_field_is_checked_against_the_pending_dispatch() {
        let secret = secret(1);
        let permit = permit(&secret);
        type IntentMutation = fn(&mut DispatchIntent);
        let cases: [(IntentMutation, &'static str); 9] = [
            (
                |intent| {
                    intent.controller_session_id = ControllerSessionId::new("SESSION-2").unwrap()
                },
                "controllerSessionId",
            ),
            (
                |intent| intent.job_id = JobId::new("JOB-2").unwrap(),
                "jobId",
            ),
            (
                |intent| intent.plan_id = PlanId::new("PLAN-2").unwrap(),
                "planId",
            ),
            (
                |intent| intent.step_id = StepId::new("STEP-006").unwrap(),
                "stepId",
            ),
            (
                |intent| intent.attempt_id = AttemptId::new("ATTEMPT-2").unwrap(),
                "attemptId",
            ),
            (
                |intent| intent.public_step_digest = sha256(b"other public step"),
                "publicStepDigest",
            ),
            (
                |intent| intent.effect_set_digest = sha256(b"other effects"),
                "effectSetDigest",
            ),
            (
                |intent| intent.authority_binding.binding_revision += 1,
                "authorityBinding",
            ),
            (
                |intent| intent.admitted_device_facts_digest = sha256(b"other facts"),
                "admittedDeviceFactsDigest",
            ),
        ];
        for (mutate, field) in cases {
            let mut pending = intent(&permit, 2_000);
            mutate(&mut pending);
            assert_eq!(
                verify_permit(&permit, &secret, &pending, false),
                Err(PermitVerificationError::ContextMismatch { field })
            );
        }
    }

    #[test]
    fn a_non_single_use_permit_is_refused() {
        let secret = secret(1);
        let mut permit = permit(&secret);
        permit.single_use = false;
        permit.integrity_tag = authority_side::mint_integrity_tag(&permit, &secret).unwrap();
        assert!(matches!(
            verify_permit(&permit, &secret, &intent(&permit, 2_000), false),
            Err(PermitVerificationError::NotSingleUse)
        ));
    }

    fn snapshot() -> StepAdmissionSnapshot {
        StepAdmissionSnapshot {
            captured_at_epoch_ms: 1_000,
            freshness_deadline_epoch_ms: 121_000,
            device_facts_digest: sha256(b"device"),
            transport_session_digest: Some(sha256(b"session")),
            provider_facts_digest: sha256(b"provider"),
            toolchain_facts_digest: sha256(b"toolchain"),
            artifact_facts_digest: sha256(b"artifact"),
        }
    }

    fn current(now: u64) -> CurrentFacts {
        CurrentFacts {
            now_epoch_ms: now,
            device_facts_digest: sha256(b"device"),
            transport_session_digest: Some(sha256(b"session")),
            saw_detach_since_snapshot: false,
            provider_facts_digest: sha256(b"provider"),
            toolchain_facts_digest: sha256(b"toolchain"),
            artifact_facts_digest: sha256(b"artifact"),
        }
    }

    #[test]
    fn continuity_intact_and_inside_the_backstop_is_fresh() {
        assert_eq!(
            evaluate_freshness(&snapshot(), &current(60_000)),
            FreshnessVerdict::Fresh
        );
    }

    #[test]
    fn a_slow_host_is_a_stale_snapshot_not_a_device_fault() {
        // The whole point of architecture.md 8.3: a wall-clock expiry with
        // continuity intact must not burn a destructive budget or blame the
        // board. This is the flake class PR #1008/#1080 came from.
        let verdict = evaluate_freshness(&snapshot(), &current(200_000));
        assert!(matches!(verdict, FreshnessVerdict::StaleSnapshot { .. }));
        assert!(!verdict.permits_dispatch());
        assert!(verdict.is_retryable_without_device_blame());
    }

    #[test]
    fn a_detach_breaks_continuity_even_inside_the_deadline() {
        let mut now = current(2_000);
        now.saw_detach_since_snapshot = true;
        assert_eq!(
            evaluate_freshness(&snapshot(), &now),
            FreshnessVerdict::ContinuityBroken(ContinuityBreak::DetachObserved)
        );
    }

    #[test]
    fn continuity_is_checked_before_the_clock() {
        // Both wrong: the answer must name the device change, not the clock,
        // because the two lead to different next steps.
        let mut now = current(200_000);
        now.device_facts_digest = sha256(b"another device");
        assert_eq!(
            evaluate_freshness(&snapshot(), &now),
            FreshnessVerdict::ContinuityBroken(ContinuityBreak::DeviceFactsChanged)
        );
    }

    #[test]
    fn every_fact_class_can_break_continuity() {
        type ContinuityCase = (fn(&mut CurrentFacts), ContinuityBreak);
        let cases: [ContinuityCase; 4] = [
            (
                |facts| facts.transport_session_digest = Some(sha256(b"another session")),
                ContinuityBreak::SessionChanged,
            ),
            (
                |facts| facts.provider_facts_digest = sha256(b"another provider"),
                ContinuityBreak::ProviderFactsChanged,
            ),
            (
                |facts| facts.toolchain_facts_digest = sha256(b"another tool"),
                ContinuityBreak::ToolchainFactsChanged,
            ),
            (
                |facts| facts.artifact_facts_digest = sha256(b"another artifact"),
                ContinuityBreak::ArtifactFactsChanged,
            ),
        ];
        for (mutate, expected) in cases {
            let mut now = current(2_000);
            mutate(&mut now);
            assert_eq!(
                evaluate_freshness(&snapshot(), &now),
                FreshnessVerdict::ContinuityBroken(expected)
            );
        }
    }

    #[test]
    fn no_disposition_permits_redispatch() {
        for disposition in [
            ActionDisposition::SemanticSuccess,
            ActionDisposition::ConfirmedNoEffect,
            ActionDisposition::ConfirmedPartialEffect,
            ActionDisposition::OutcomeUnknown,
        ] {
            assert!(!disposition.permits_redispatch(), "{disposition:?}");
        }
    }

    #[test]
    fn unbounded_possible_effects_block_recovery_assessment() {
        let unbounded = PossibleEffectSet {
            effects: EffectSet::read_only(),
            completeness: EffectSetCompleteness::Unbounded,
            source_action_ids: vec![ActionId::new("ACT-1").unwrap()],
        };
        assert!(!unbounded.permits_recovery_assessment());

        let bounded = PossibleEffectSet {
            completeness: EffectSetCompleteness::Bounded,
            ..unbounded
        };
        assert!(bounded.permits_recovery_assessment());
    }

    #[test]
    fn the_pairing_secret_never_renders_its_bytes() {
        let rendered = format!("{:?}", secret(7));
        assert!(rendered.contains("redacted"));
        assert!(!rendered.contains("pairing-secret-bytes"));
    }
}
