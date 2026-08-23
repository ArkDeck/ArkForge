//! Unisoc provider — DAYU600 research inspection and assessment.
//!
//! architecture.md 17. This provider can produce a `PlanAssessment` and nothing
//! else. That is not a stage it will grow out of by adding a flag: the
//! `materialize` path has no branch that constructs a `FlashPlanEnvelope`, so
//! there is no code to reach even if a caller, a config file or a future
//! refactor wanted one.
//!
//! `discover`/`probe` are conditional, exactly as architecture.md 17.1 words
//! it: allowed *when read-only identity evidence exists*. Today UNI-U05 and
//! UNI-U06 are open — nobody has measured the download-mode USB identity or a
//! stable chip identifier — so a probe can only report what a transport
//! actually observed, and it refuses to assert that the observation is a
//! DAYU600 in a download mode.

use crate::{
    FlashIntent, FlashProvider, MaterializeRequest, MaturityRegistry, ProbeContext,
    ProviderDescriptor, ProviderError, ProviderProbe, ValidationReport,
};
use arkforge_artifact::manifest::{ArtifactManifest, ParserConfidence};
use arkforge_artifact::pac;
use arkforge_core::digest::sha256;
use arkforge_core::effect::EffectSet;
use arkforge_core::identity::{
    ArtifactFormat, MaturityKey, MaturityState, ProviderIdentity, Version,
};
use arkforge_core::ids::OpaqueId;
use arkforge_core::plan::{
    EvidenceRequirement, ExecutionAvailability, ExecutionUnknown, PlanAssessment,
    PlanMaterialization, ProfileCandidate, ProviderCandidate,
};
use arkforge_core::profile::DeviceProfile;
use arkforge_core::{DataImpact, EvidenceId, Sha256Digest};

pub const PROVIDER_ID: &str = "arkforge.unisoc";
pub const BACKEND_RESEARCH: &str = "research-inspect";

/// The DAYU600 Unisoc provider.
#[derive(Debug, Clone)]
pub struct UnisocProvider {
    identity: ProviderIdentity,
}

impl Default for UnisocProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl UnisocProvider {
    pub fn new() -> Self {
        UnisocProvider {
            identity: ProviderIdentity {
                id: OpaqueId::new(PROVIDER_ID).expect("literal identifier"),
                // 0.x on purpose: this provider implements inspection, not a
                // download protocol.
                version: Version::new(0, 1, 0),
                implementation_digest: sha256(
                    concat!(
                        "arkforge.unisoc/research/v1;",
                        "capabilities=pac-research-inspect,plan-assessment;",
                        "execute=none;evidence-gate=architecture.md-17.5"
                    )
                    .as_bytes(),
                ),
            },
        }
    }

    pub fn identity(&self) -> &ProviderIdentity {
        &self.identity
    }

    pub fn artifact_format(&self) -> ArtifactFormat {
        ArtifactFormat {
            id: OpaqueId::new(pac::FORMAT_ID).expect("literal identifier"),
            version: pac::FORMAT_VERSION,
        }
    }

    /// Everything standing between this combination and an executable plan.
    ///
    /// Collected from three independent sources — the artifact, the profile and
    /// the evidence gate — so closing one does not silently look like closing
    /// all of them.
    fn blockers(
        &self,
        artifact: &ArtifactManifest,
        profile: &DeviceProfile,
    ) -> Vec<ExecutionUnknown> {
        let mut blockers = Vec::new();

        // 1. Whatever the artifact parser could not settle. For a PAC container
        //    that is the entire UNI-U list.
        blockers.extend(artifact.execution_relevant_unknowns.iter().cloned());

        if artifact.confidence != ParserConfidence::ProductionManifest {
            blockers.push(ExecutionUnknown {
                id: OpaqueId::new("UNI-M01").expect("literal identifier"),
                summary: format!(
                    "artifact manifest confidence is {}; a production manifest requires the PAC \
                     format itself to be known (UNI-U01)",
                    artifact.confidence.as_str()
                ),
            });
        }

        // 2. Whatever the profile cannot supply.
        for blocker in profile.execution_blockers() {
            blockers.push(ExecutionUnknown {
                id: OpaqueId::new(blocker.id()).expect("literal identifier"),
                summary: blocker.to_string(),
            });
        }

        // 3. The gate itself. Even with the other two closed, execution needs
        //    the eighteen gates of architecture.md 17.5 and a maintainer review.
        blockers.push(ExecutionUnknown {
            id: OpaqueId::new("UNI-G01").expect("literal identifier"),
            summary:
                "the eighteen DAYU600 evidence gates of architecture.md 17.5 have not passed; \
                      until they do there is no executable plan for this device, and no flag, \
                      configuration or caller argument creates one"
                    .to_string(),
        });

        blockers
    }
}

impl FlashProvider for UnisocProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            identity: self.identity.clone(),
            artifact_formats: vec![self.artifact_format()],
            backends: vec![OpaqueId::new(BACKEND_RESEARCH).expect("literal identifier")],
        }
    }

    /// Reports what a transport observed, and refuses to embellish it.
    ///
    /// architecture.md 17.1 permits discover/probe once read-only identity
    /// evidence exists. What exists today is an observation; what does not exist
    /// is any basis for saying the observation *is* a DAYU600 in a download
    /// mode (UNI-U05, UNI-U06). So the probe reports the observation and marks
    /// the profile candidate as unconfirmed.
    fn probe(&self, ctx: &ProbeContext<'_>) -> Result<ProviderProbe, ProviderError> {
        let mut session = ctx
            .transport
            .open_exact(ctx.observation)
            .map_err(|error| ProviderError::Unsupported(error.to_string()))?;
        let observation = session
            .reread_identity()
            .map_err(|error| ProviderError::Unsupported(error.to_string()))?;

        if ctx.profile.mode(&observation.mode).is_none() {
            return Err(ProviderError::Unsupported(format!(
                "profile {} declares no mode matching {}; this provider will not guess that an \
                 unrecognized mode is a download mode",
                ctx.profile.id, observation.mode
            )));
        }

        let mut protocol_facts: Vec<(OpaqueId, String)> = observation
            .protocol_identity
            .iter()
            .map(|fact| (fact.key.clone(), fact.value.clone()))
            .collect();
        protocol_facts.push((
            OpaqueId::new("transport").expect("literal identifier"),
            ctx.transport.transport_id().to_string(),
        ));
        // The honest headline fact of this probe.
        protocol_facts.push((
            OpaqueId::new("identityConfirmation").expect("literal identifier"),
            "unconfirmed: no download-mode USB identity or stable chip identifier has been \
             measured for this device (UNI-U05, UNI-U06)"
                .to_string(),
        ));
        protocol_facts.sort();

        let facts_digest = observation
            .facts_digest()
            .map_err(|error| ProviderError::Core(error.to_string()))?;
        let profile_candidate = ctx
            .profile
            .identity()
            .map(Some)
            .map_err(|error| ProviderError::Core(error.to_string()))?;

        Ok(ProviderProbe {
            observation,
            protocol_facts,
            profile_candidate,
            facts_digest,
        })
    }

    fn validate(
        &self,
        artifact: &ArtifactManifest,
        profile: &DeviceProfile,
        probe: &ProviderProbe,
    ) -> Result<ValidationReport, ProviderError> {
        let mut report = ValidationReport::default();

        let format_id = self.artifact_format().id;
        if artifact.format.id != format_id {
            report.violation(
                "UNI-V01",
                format!("artifact format {} is not {format_id}", artifact.format.id),
            );
        }
        if !profile.artifact_formats.contains(&artifact.format.id) {
            report.violation(
                "UNI-V02",
                format!(
                    "profile {} does not accept artifact format {}",
                    profile.id, artifact.format.id
                ),
            );
        }
        if profile.mode(&probe.observation.mode).is_none() {
            report.violation(
                "UNI-V03",
                format!(
                    "device reports mode {}, which profile {} does not declare",
                    probe.observation.mode, profile.id
                ),
            );
        }
        // There is nothing else to validate. Three-way agreement between
        // profile allowlist, device table and artifact members (the DAYU200
        // check) has no counterpart here: none of the three exists.
        Ok(report)
    }

    /// Always an assessment.
    ///
    /// There is no `PlanMaterialization::Executable` construction anywhere in
    /// this function, and the maturity argument cannot change that — it is read
    /// so the assessment can *report* the combination's state, not so it can
    /// gate a branch that does not exist.
    fn materialize(
        &self,
        request: &MaterializeRequest<'_>,
        maturity: &MaturityRegistry,
    ) -> Result<PlanMaterialization, ProviderError> {
        if request.intent != FlashIntent::FullRestore {
            return Err(ProviderError::Unsupported(format!(
                "intent {} is not defined for this device",
                request.intent.as_str()
            )));
        }

        let profile_identity = request
            .profile
            .identity()
            .map_err(|error| ProviderError::Core(error.to_string()))?;
        let maturity_state = maturity.lookup(&MaturityKey {
            provider: self.identity.clone(),
            profile: profile_identity.clone(),
            artifact_format: self.artifact_format(),
            toolchain: request.toolchain.clone(),
            host_platform: request.host_platform.clone(),
            driver_facts_digest: request.driver_facts_digest,
            evidence_set_digest: request.evidence_set_digest,
        });

        let validation = self.validate(request.artifact, request.profile, request.probe)?;
        let mut unknowns: Vec<ExecutionUnknown> = validation
            .violations
            .iter()
            .map(|violation| ExecutionUnknown {
                id: violation.id.clone(),
                summary: violation.detail.clone(),
            })
            .collect();
        unknowns.extend(self.blockers(request.artifact, request.profile));

        let evidence_requirements: Vec<EvidenceRequirement> = unknowns
            .iter()
            .map(|unknown| EvidenceRequirement {
                id: EvidenceId::new(format!("EVR-{}", unknown.id)).unwrap_or_else(|_| {
                    EvidenceId::new("EVR-UNNAMED").expect("literal identifier")
                }),
                closes: vec![unknown.id.clone()],
                description: unknown.summary.clone(),
                // Grade A: architecture.md 17.4 requires three-way agreement
                // between the parser, the official tool's behaviour and a real
                // device fact. D-grade community reversing cannot close any of
                // these on its own (architecture.md 2.3).
                minimum_grade: 'A',
            })
            .collect();

        let assessment = PlanAssessment {
            provider_candidates: vec![ProviderCandidate {
                provider: self.identity.clone(),
                maturity: maturity_state.clone(),
                rationale: "Unisoc research provider: PAC inspection and assessment only. It \
                            implements no download protocol, because none is known."
                    .into(),
            }],
            profile_candidates: vec![ProfileCandidate {
                profile: profile_identity,
                maturity: maturity_state.clone(),
                rationale: format!(
                    "research profile with {} writable targets and {} execution blockers",
                    request.profile.allowed_targets.len(),
                    request.profile.execution_blockers().len()
                ),
            }],
            would_be_steps: Vec::new(),
            // Not `EffectSet::read_only()`: that would assert every axis is
            // preserved. Nothing is known, so every axis reads unknown — and an
            // EffectSet with unknown data impact can never be sealed into a
            // plan (architecture.md 5.5).
            known_effects: EffectSet {
                persistent: Vec::new(),
                transient: Vec::new(),
                data_impact: DataImpact::all_unknown(),
            },
            unknowns,
            evidence_requirements,
            availability: ExecutionAvailability::Unavailable {
                reason: "DAYU600 execution is unavailable: the evidence gates of architecture.md \
                         17.5 have not passed and no download protocol is implemented"
                    .into(),
            },
        };

        Ok(PlanMaterialization::Assessment(Box::new(assessment)))
    }
}

/// Publishes the DAYU600 maturity state.
///
/// `ResearchOnly` for every combination, with the blocker named. There is no
/// argument to this function that could publish anything stronger — a
/// ProductionVerified DAYU600 requires evidence, and evidence is not a
/// parameter.
pub fn publish_af_v3_maturity(
    registry: &mut MaturityRegistry,
    provider: &UnisocProvider,
    profile: &DeviceProfile,
    toolchain: &arkforge_core::identity::ToolchainIdentity,
    host_platform: &arkforge_core::identity::HostPlatform,
    driver_facts_digest: Sha256Digest,
    evidence_set_digest: Sha256Digest,
) -> Result<(), ProviderError> {
    let profile_identity = profile
        .identity()
        .map_err(|error| ProviderError::Core(error.to_string()))?;
    registry.publish(
        &MaturityKey {
            provider: provider.identity().clone(),
            profile: profile_identity,
            artifact_format: provider.artifact_format(),
            toolchain: toolchain.clone(),
            host_platform: host_platform.clone(),
            driver_facts_digest,
            evidence_set_digest,
        },
        MaturityState::ResearchOnly {
            blocker: "the PAC format, the download protocol and the device's data impact are all \
                      unknown (UNI-U01..UNI-U12); architecture.md 17.5 gates production"
                .into(),
        },
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use arkforge_core::identity::{HostPlatform, ToolchainIdentity, ToolchainKind};

    fn toolchain() -> ToolchainIdentity {
        ToolchainIdentity {
            id: OpaqueId::new("research-inspect").unwrap(),
            kind: ToolchainKind::Replay,
            version: Version::new(0, 1, 0),
            backend_digest: sha256(b"research"),
            upstream_ref: None,
        }
    }

    #[test]
    fn the_provider_declares_no_executable_backend() {
        let descriptor = UnisocProvider::new().descriptor();
        assert_eq!(descriptor.backends.len(), 1);
        assert_eq!(descriptor.backends[0].as_str(), BACKEND_RESEARCH);
        assert_eq!(descriptor.identity.version.major, 0);
    }

    #[test]
    fn the_execution_side_spi_refuses() {
        let provider = UnisocProvider::new();
        assert!(provider.execute_stored_action().is_err());
        let private_plan = arkforge_core::projection::StoredProviderPlan {
            actions: Vec::new(),
        };
        assert!(
            provider
                .reconcile_read_only(&crate::ReconcileRequest {
                    private_plan: &private_plan,
                    possible_effects: &[],
                })
                .is_err()
        );
        assert!(provider.materialize_superseding_recovery().is_err());
    }

    #[test]
    fn published_maturity_is_research_only_whatever_is_passed_in() {
        let provider = UnisocProvider::new();
        let profile =
            arkforge_core::profile::load(include_str!("../../../profiles/dayu600.yaml")).unwrap();
        let mut registry = MaturityRegistry::new();
        publish_af_v3_maturity(
            &mut registry,
            &provider,
            &profile,
            &toolchain(),
            &HostPlatform::new("macos", "aarch64").unwrap(),
            sha256(b"driver"),
            sha256(b"evidence"),
        )
        .unwrap();
        let state = registry.lookup(&MaturityKey {
            provider: provider.identity().clone(),
            profile: profile.identity().unwrap(),
            artifact_format: provider.artifact_format(),
            toolchain: toolchain(),
            host_platform: HostPlatform::new("macos", "aarch64").unwrap(),
            driver_facts_digest: sha256(b"driver"),
            evidence_set_digest: sha256(b"evidence"),
        });
        assert!(matches!(state, MaturityState::ResearchOnly { .. }));
        assert!(!state.permits_executable_plan());
    }
}
