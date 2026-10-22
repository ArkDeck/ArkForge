impl Service {

    fn materialize_plan(&mut self, session: SessionKind, request: &Request) -> Response {
        let artifact_id = first_string_field(&request.payload, 1).unwrap_or_default();
        let profile_id = first_string_field(&request.payload, 2).unwrap_or_default();
        let observation_id = first_string_field(&request.payload, 3).unwrap_or_default();
        let intent = first_string_field(&request.payload, 4).unwrap_or_default();
        let requested_toolchain_id = first_string_field(&request.payload, 5).unwrap_or_default();
        let authority_namespace = first_string_field(&request.payload, 6).unwrap_or_default();
        let binding_id = first_string_field(&request.payload, 7).unwrap_or_default();
        let binding_revision = first_u64_field(&request.payload, 8).unwrap_or_default();
        let stable_identity = first_bytes_field(&request.payload, 9).unwrap_or_default();
        let execution_purpose = first_string_field(&request.payload, 10).unwrap_or_default();
        let authority_support_key = first_bytes_field(&request.payload, 11).unwrap_or_default();
        let authority_support_state = first_string_field(&request.payload, 12).unwrap_or_default();
        let authority_support_detail = first_string_field(&request.payload, 13).unwrap_or_default();

        let intent = match intent.as_str() {
            "fullRestore" => FlashIntent::FullRestore,
            "restoreRamdisk" => FlashIntent::RestoreRamdisk,
            _ => {
                return self.refuse(
                    request,
                    Status::InvalidArgument,
                    "UNSUPPORTED_FLASH_INTENT",
                    "materializePlan requires intent=fullRestore or restoreRamdisk",
                );
            }
        };
        let execution_purpose = if session == SessionKind::Public {
            // The public face is semantic and assessment-only. Authority
            // binding and execution purpose are controller concerns and must
            // not be invented by a diagnostic client.
            ExecutionPurpose::PrimaryFlash
        } else {
            let Some(execution_purpose) = ExecutionPurpose::parse(&execution_purpose) else {
                return self.refuse(
                    request,
                    Status::InvalidArgument,
                    "UNSUPPORTED_EXECUTION_PURPOSE",
                    "materializePlan requires executionPurpose=primaryFlash or supersedingRecovery",
                );
            };
            execution_purpose
        };

        if !self.manifests.contains_key(&artifact_id) {
            let Ok(digest) = Sha256Digest::parse_hex(&artifact_id) else {
                return self.refuse(
                    request,
                    Status::InvalidArgument,
                    "MALFORMED_ARTIFACT_ID",
                    "materializePlan requires a 64-hex content-addressed artifact id",
                );
            };
            let manifest = match inspect_stored_container(&self.store, &digest) {
                Ok(manifest) => manifest,
                Err(error) => {
                    return self.refuse(request, Status::Refused, "ARTIFACT_REJECTED", &error);
                }
            };
            self.manifests.insert(artifact_id.clone(), manifest);
        }
        let manifest = self
            .manifests
            .get(&artifact_id)
            .cloned()
            .expect("inserted or already present");
        let Some(profile) = self.profiles.get(&profile_id).cloned() else {
            return self.refuse(
                request,
                Status::NotFound,
                "PROFILE_NOT_FOUND",
                &format!("no loaded profile {profile_id}"),
            );
        };
        if execution_purpose == ExecutionPurpose::SupersedingRecovery
            && !profile.recovery.supports_complete_overwrite
        {
            return self.refuse(
                request,
                Status::Refused,
                "RECOVERY_CONTRACT_UNAVAILABLE",
                "the selected profile publishes no complete-overwrite recovery contract",
            );
        }
        let toolchain = if profile
            .artifact_formats
            .iter()
            .any(|format| format.as_str() == pac::FORMAT_ID)
        {
            research_toolchain_identity()
        } else {
            self.bound_rockchip_toolchain_identity()
        };
        if session == SessionKind::Controller && requested_toolchain_id != toolchain.id.as_str() {
            return self.refuse(
                request,
                Status::Refused,
                "TOOLCHAIN_ID_MISMATCH",
                &format!(
                    "materializePlan requested toolchain {requested_toolchain_id}; this daemon binds {}",
                    toolchain.id
                ),
            );
        }
        let Some(provider) = provider_for(&profile, &self.rockchip, &self.unisoc) else {
            return self.refuse(
                request,
                Status::NotFound,
                "NO_PROVIDER_FOR_PROFILE",
                &format!(
                    "no registered provider handles the artifact formats profile {} declares",
                    profile.id
                ),
            );
        };

        let mut probed = None;
        for (transport_index, transport) in self.transports.iter().enumerate() {
            let Ok(observations) =
                transport.discover(&TypedDiscoveryFilter::default(), self.clock.now_epoch_ms())
            else {
                continue;
            };
            if let Some(observation) = observations
                .iter()
                .find(|candidate| candidate.observation_id.as_str() == observation_id)
            {
                probed = provider
                    .probe(&ProbeContext {
                        transport: transport.as_ref(),
                        observation,
                        profile: &profile,
                    })
                    .ok()
                    .map(|probe| (probe, transport_index, observation.clone()));
                break;
            }
        }
        let Some((probe, transport_index, plan_observation)) = probed else {
            return self.refuse(
                request,
                Status::NotFound,
                "OBSERVATION_NOT_FOUND",
                &format!("no probe for observation {observation_id}"),
            );
        };

        let authority_binding = if session == SessionKind::Public {
            AuthorityBindingRef {
                authority_namespace: AuthorityNamespace::new("public-assessment")
                    .expect("literal identifier"),
                binding_id: OpaqueId::new("PUBLIC-ASSESSMENT").expect("literal identifier"),
                binding_revision: 1,
                stable_identity_digest: probe.facts_digest,
            }
        } else {
            let Ok(authority_namespace) = AuthorityNamespace::new(authority_namespace) else {
                return self.refuse(
                    request,
                    Status::InvalidArgument,
                    "BAD_AUTHORITY_NAMESPACE",
                    "authorityNamespace is not a usable identifier",
                );
            };
            let Ok(binding_id) = OpaqueId::new(binding_id) else {
                return self.refuse(
                    request,
                    Status::InvalidArgument,
                    "BAD_BINDING_ID",
                    "bindingId is not a usable identifier",
                );
            };
            let Some(stable_identity_digest) = digest_from_bytes(&stable_identity) else {
                return self.refuse(
                    request,
                    Status::InvalidArgument,
                    "BAD_STABLE_IDENTITY_DIGEST",
                    "stableIdentitySha256 must contain exactly 32 bytes",
                );
            };
            if binding_revision == 0 {
                return self.refuse(
                    request,
                    Status::InvalidArgument,
                    "BAD_BINDING_REVISION",
                    "bindingRevision must be greater than zero",
                );
            }
            AuthorityBindingRef {
                authority_namespace,
                binding_id,
                binding_revision,
                stable_identity_digest,
            }
        };

        let authority_support = if session == SessionKind::Public {
            AuthoritySupportBinding {
                key_digest: sha256(b"public-assessment-has-no-authority-support"),
                state: AuthoritySupportState::HardwareGated {
                    blocker: "public sessions cannot publish authority support".into(),
                },
            }
        } else {
            let Some(key_digest) = digest_from_bytes(&authority_support_key) else {
                return self.refuse(
                    request,
                    Status::InvalidArgument,
                    "BAD_AUTHORITY_SUPPORT_KEY",
                    "authoritySupportKeySha256 must contain exactly 32 bytes",
                );
            };
            let state = match authority_support_state.as_str() {
                "productionVerified" => AuthoritySupportState::ProductionVerified,
                "hardwareCampaign" if !authority_support_detail.is_empty() => {
                    AuthoritySupportState::HardwareCampaign {
                        campaign: authority_support_detail,
                    }
                }
                "hardwareGated" if !authority_support_detail.is_empty() => {
                    AuthoritySupportState::HardwareGated {
                        blocker: authority_support_detail,
                    }
                }
                _ => {
                    return self.refuse(
                        request,
                        Status::InvalidArgument,
                        "BAD_AUTHORITY_SUPPORT_STATE",
                        "authoritySupportState must be productionVerified, or hardwareCampaign/hardwareGated with a non-empty detail",
                    );
                }
            };
            AuthoritySupportBinding { key_digest, state }
        };

        let created_at_epoch_ms = self.clock.now_epoch_ms();
        let mut plan_identity = Vec::new();
        plan_identity.extend_from_slice(artifact_id.as_bytes());
        plan_identity.push(0);
        plan_identity.extend_from_slice(execution_purpose.as_str().as_bytes());
        plan_identity.push(0);
        plan_identity.extend_from_slice(authority_binding.binding_id.as_str().as_bytes());
        plan_identity.extend_from_slice(&created_at_epoch_ms.to_be_bytes());
        let plan_suffix = sha256(&plan_identity).to_hex();

        let materialize = MaterializeRequest {
            plan_id: PlanId::new(format!("PLAN-{}", &plan_suffix[..24]))
                .unwrap_or_else(|_| PlanId::new("PLAN-UNNAMED").expect("literal")),
            execution_purpose,
            intent,
            artifact: &manifest,
            artifact_id: OpaqueId::new(&artifact_id[..32])
                .unwrap_or_else(|_| OpaqueId::new("ART-UNNAMED").expect("literal identifier")),
            profile: &profile,
            probe: &probe,
            authority_binding,
            authority_support: authority_support.clone(),
            // The native implementation this daemon actually bound, not a
            // source-revision constant. A plan materialized for one build and
            // started on another is refused because the executable digest is
            // part of the maturity combination.
            toolchain,
            host_platform: HostPlatform::current(),
            driver_facts_digest: driver_facts_digest(),
            evidence_set_digest: evidence_set_digest(),
            created_at_epoch_ms,
            plan_lifetime_ms: 3_600_000,
        };

        // Public callers always see a non-executable assessment, even when
        // this daemon has published execution maturity for the exact
        // combination. An empty registry preserves the full projected steps
        // and effects while making executability structurally unavailable.
        let public_maturity = MaturityRegistry::new();
        let maturity = if session == SessionKind::Public {
            &public_maturity
        } else {
            &self.maturity
        };
        let mechanics_key = MaturityKey {
            provider: provider.descriptor().identity,
            profile: match profile.identity() {
                Ok(identity) => identity,
                Err(error) => {
                    return self.refuse(
                        request,
                        Status::Internal,
                        "PROFILE_IDENTITY_INVALID",
                        &error.to_string(),
                    );
                }
            },
            artifact_format: provider.descriptor().artifact_formats[0].clone(),
            toolchain: materialize.toolchain.clone(),
            host_platform: materialize.host_platform.clone(),
            driver_facts_digest: materialize.driver_facts_digest,
            evidence_set_digest: materialize.evidence_set_digest,
        };
        let mechanics_key_digest = match mechanics_key.digest() {
            Ok(digest) => digest,
            Err(error) => {
                return self.refuse(
                    request,
                    Status::Internal,
                    "MATURITY_KEY_NOT_ENCODABLE",
                    &error.to_string(),
                );
            }
        };
        let mechanics_state = maturity.lookup(&mechanics_key);
        let materialized = match provider.materialize_with_private_plan(&materialize, maturity) {
            Ok(materialized) => materialized,
            Err(error) => {
                return self.refuse(
                    request,
                    Status::Refused,
                    "MATERIALIZATION_REFUSED",
                    &error.to_string(),
                );
            }
        };
        if session == SessionKind::Public
            && matches!(
                materialized.materialization,
                PlanMaterialization::Executable(_)
            )
        {
            return self.refuse(
                request,
                Status::Internal,
                "PUBLIC_PLAN_FORBIDDEN",
                "the public session cannot receive or store an executable plan",
            );
        }
        // Store the executable plan and its private half together. A job can
        // only ever be started against a plan the daemon materialized itself,
        // which is what keeps `startExecution` free of anything a caller could
        // supply (architecture.md 15.3).
        if let (PlanMaterialization::Executable(envelope), Some(private_plan)) =
            (&materialized.materialization, &materialized.private_plan)
        {
            if let Err(error) = self.engine.plans_mut().insert(StoredPlan {
                envelope: (**envelope).clone(),
                private_plan: private_plan.clone(),
            }) {
                return self.refuse(
                    request,
                    Status::Internal,
                    "PLAN_NOT_STORABLE",
                    &error.to_string(),
                );
            }
            self.plan_observations.insert(
                envelope.plan_digest.to_hex(),
                PlanObservationContext {
                    transport_index,
                    observation: plan_observation,
                },
            );
        }

        match Ok::<_, arkforge_provider::ProviderError>(materialized.materialization) {
            Ok(PlanMaterialization::Assessment(assessment)) => {
                let mut message = Assessment {
                    availability: assessment.availability.as_str().to_string(),
                    unavailable_reason: match &assessment.availability {
                        arkforge_core::plan::ExecutionAvailability::Unavailable { reason } => {
                            reason.clone()
                        }
                        _ => String::new(),
                    },
                    ..Assessment::default()
                };
                for unknown in &assessment.unknowns {
                    message.unknowns.push(KeyValue {
                        key: unknown.id.to_string(),
                        value: unknown.summary.clone(),
                    });
                }
                for requirement in &assessment.evidence_requirements {
                    message.evidence_requirements.push(KeyValue {
                        key: requirement.id.to_string(),
                        value: requirement.description.clone(),
                    });
                }
                for step in &assessment.would_be_steps {
                    message.would_be_steps.push(encode_step(step));
                }
                for effect in &assessment.known_effects.persistent {
                    message.known_persistent_effects.push(encode_effect(effect));
                }
                message.data_impact = encode_data_impact(&assessment.known_effects.data_impact);
                message.mechanics_maturity_key_sha256 = mechanics_key_digest.to_hex();
                message.mechanics_maturity_state = mechanics_state.as_str().to_string();
                message.authority_support_key_sha256 = authority_support.key_digest.to_hex();
                message.authority_support_state = authority_support.state.as_str().to_string();
                self.ok(
                    request,
                    MaterializePlanResponse::Assessment(message).encode(),
                )
            }
            Ok(PlanMaterialization::Executable(envelope)) => {
                let maturity_key = MaturityKey {
                    provider: envelope.provider.clone(),
                    profile: envelope.profile.clone(),
                    artifact_format: provider.descriptor().artifact_formats[0].clone(),
                    toolchain: envelope.toolchain.clone(),
                    host_platform: HostPlatform::current(),
                    driver_facts_digest: driver_facts_digest(),
                    evidence_set_digest: evidence_set_digest(),
                };
                let mechanics_maturity_key_sha256 = match maturity_key.digest() {
                    Ok(digest) => digest.to_hex(),
                    Err(error) => {
                        return self.refuse(
                            request,
                            Status::Internal,
                            "MATURITY_KEY_NOT_ENCODABLE",
                            &error.to_string(),
                        );
                    }
                };
                let mut plan = ExecutablePlan {
                    plan_id: envelope.plan_id.to_string(),
                    plan_sha256: envelope.plan_digest.to_hex(),
                    provider_execution_plan_sha256: envelope
                        .provider_execution_plan_digest
                        .to_hex(),
                    public_projection_sha256: envelope.public_projection_digest.to_hex(),
                    expires_at_epoch_ms: envelope.expires_at_epoch_ms,
                    execution_purpose: envelope.execution_purpose.as_str().to_string(),
                    mechanics_maturity_key_sha256,
                    mechanics_maturity_state: envelope.maturity.as_str().to_string(),
                    mechanics_maturity_campaign: envelope
                        .maturity
                        .campaign()
                        .unwrap_or_default()
                        .to_string(),
                    authority_support_key_sha256: envelope.authority_support.key_digest.to_hex(),
                    authority_support_state: envelope.authority_support.state.as_str().to_string(),
                    authority_support_campaign: envelope
                        .authority_support
                        .state
                        .campaign()
                        .unwrap_or_default()
                        .to_string(),
                    ..ExecutablePlan::default()
                };
                for step in &envelope.public_steps {
                    plan.public_steps.push(encode_step(step));
                }
                for effect in &envelope.effect_set.persistent {
                    plan.persistent_effects.push(encode_effect(effect));
                }
                for effect in &envelope.effect_set.transient {
                    plan.transient_effects.push(encode_transient(effect));
                }
                plan.data_impact = encode_data_impact(&envelope.effect_set.data_impact);
                self.ok(request, MaterializePlanResponse::Plan(plan).encode())
            }
            Err(error) => self.refuse(
                request,
                Status::Refused,
                "MATERIALIZATION_REFUSED",
                &error.to_string(),
            ),
        }
    }
}
