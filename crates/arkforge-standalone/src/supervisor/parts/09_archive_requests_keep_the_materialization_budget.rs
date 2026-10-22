
#[cfg(test)]
mod tests {
    use super::*;
    use arkforge_ipc::messages::KeyValue;

    #[test]
    fn archive_requests_keep_the_materialization_budget() {
        for verb in [
            "assess-plan",
            "materialize-plan",
            "materialize-recovery-plan",
        ] {
            assert_eq!(
                request_read_timeout(verb),
                ControllerClient::MATERIALIZATION_READ_TIMEOUT
            );
        }
        for verb in ["status", "stop", "apply-plan", "cancel-job", "unknown"] {
            assert_eq!(request_read_timeout(verb), REQUEST_TIMEOUT);
        }
    }

    #[test]
    fn an_archive_reply_can_arrive_after_the_lifecycle_deadline() {
        #[cfg(target_os = "macos")]
        let temporary_base = PathBuf::from("/private/tmp");
        #[cfg(not(target_os = "macos"))]
        let temporary_base = std::env::temp_dir();
        let root = temporary_base.join(format!(
            "arkforge-supervisor-archive-deadline-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let endpoint = LocalEndpoint::for_runtime(&root, LocalChannel::Supervisor);
        let mut listener = LocalListener::bind(&endpoint).unwrap();
        let (release, pending_work) = std::sync::mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            let mut stream = listener.accept().unwrap();
            let frame = read_frame(&mut stream).unwrap().unwrap();
            assert_eq!(decode_request(&frame).unwrap().0, "assess-plan");
            // Model a quiet parser, past the lifecycle deadline. This waits on
            // pending work, not an increased retry delay or a device operation.
            assert!(matches!(
                pending_work.recv_timeout(REQUEST_TIMEOUT + Duration::from_secs(1)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ));
            write_reply(&mut stream, Ok(vec![42]))
        });
        let response = request(&root, "assess-plan", &["artifact", "profile", "device"]);
        let sent = server.join().unwrap();
        drop(release);
        let _ = std::fs::remove_dir_all(root);
        assert_eq!(response.unwrap(), vec![42]);
        sent.unwrap();
    }

    fn stable_observation(observed_at_epoch_ms: u64) -> DeviceObservationView {
        DeviceObservationView {
            observation_id: "USB-2207-5000-01120000".into(),
            observed_at_epoch_ms,
            mode: "hdc-normal".into(),
            topology_sha256: sha256(b"topology").to_hex(),
            descriptor_sha256: sha256(b"descriptor").to_hex(),
            identity_strength: "serialAndTopology".into(),
            malformed_descriptor: false,
            protocol_identity: vec![KeyValue {
                key: "profile".into(),
                value: "org.openharmony.dayu200".into(),
            }],
            serial_sha256: sha256(b"serial").to_hex(),
            serial_evidence_kind: "descriptor".into(),
        }
    }

    fn stable_probe(observed_at_epoch_ms: u64, facts_sha256: &str) -> DeviceProbeView {
        DeviceProbeView {
            observation: stable_observation(observed_at_epoch_ms),
            protocol_facts: vec![KeyValue {
                key: "transport".into(),
                value: "arkforge.transport.usb".into(),
            }],
            profile_id: "org.openharmony.dayu200".into(),
            facts_sha256: facts_sha256.into(),
        }
    }

    fn admission_for(observation: &DeviceObservationView) -> StepAdmissionSnapshot {
        StepAdmissionSnapshot {
            step_id: "STEP-022".into(),
            private_action_sha256: sha256(b"postflight action").as_bytes().to_vec(),
            observed_mode: observation.mode.clone(),
            topology_sha256: Sha256Digest::parse_hex(&observation.topology_sha256)
                .unwrap()
                .as_bytes()
                .to_vec(),
            descriptor_sha256: Sha256Digest::parse_hex(&observation.descriptor_sha256)
                .unwrap()
                .as_bytes()
                .to_vec(),
            serial_sha256: Sha256Digest::parse_hex(&observation.serial_sha256)
                .unwrap()
                .as_bytes()
                .to_vec(),
            serial_evidence_kind: observation.serial_evidence_kind.clone(),
            protocol_identity: observation.protocol_identity.clone(),
            identity_strength: observation.identity_strength.clone(),
            malformed_descriptor: observation.malformed_descriptor,
            ..StepAdmissionSnapshot::default()
        }
    }

    fn pending_authority_assessment() -> arkforge_ipc::messages::Assessment {
        arkforge_ipc::messages::Assessment {
            availability: "unavailable".into(),
            unavailable_reason: "pending authority support".into(),
            mechanics_maturity_state: "hardwareCampaign".into(),
            unknowns: vec![KeyValue {
                key: "RK-A01".into(),
                value: "authority implementation is hardwareGated".into(),
            }],
            evidence_requirements: vec![KeyValue {
                key: "EVR-RK-A01".into(),
                value: "close authority support".into(),
            }],
            ..arkforge_ipc::messages::Assessment::default()
        }
    }

    #[test]
    fn stable_identity_ignores_fresh_probe_time_but_binds_explicit_identity_facts() {
        let discovery = stable_observation(100);
        let first = stable_probe(101, &sha256(b"full observation at 101").to_hex());
        let fresh = stable_probe(202, &sha256(b"full observation at 202").to_hex());

        assert_eq!(
            stable_identity_digest(&discovery, &first),
            stable_identity_digest(&discovery, &fresh),
            "a timestamp-only fresh probe must preserve the sealed target binding"
        );

        let mut replaced = fresh.clone();
        replaced.observation.descriptor_sha256 = sha256(b"replacement descriptor").to_hex();
        assert_ne!(
            stable_identity_digest(&discovery, &first),
            stable_identity_digest(&discovery, &replaced),
            "same-handle replacement evidence must still invalidate the binding"
        );

        let mut changed_protocol = fresh;
        changed_protocol.protocol_facts[0].value = "arkforge.transport.replacement".into();
        assert_ne!(
            stable_identity_digest(&discovery, &first),
            stable_identity_digest(&discovery, &changed_protocol),
            "provider protocol facts remain effect-relevant binding material"
        );
    }

    #[test]
    fn postflight_rebind_requires_the_immediately_prior_sealed_reboot_edge() {
        let observation = stable_observation(100);
        let snapshot = admission_for(&observation);
        let mut plan = ExecutablePlan {
            public_steps: vec![
                arkforge_ipc::messages::PublicStep {
                    step_id: "STEP-021".into(),
                    kind: "reboot".into(),
                    expected_mode_before: "rockusb-loader".into(),
                    expected_mode_after: "hdc-normal".into(),
                    ..arkforge_ipc::messages::PublicStep::default()
                },
                arkforge_ipc::messages::PublicStep {
                    step_id: "STEP-022".into(),
                    kind: "postflightProbe".into(),
                    expected_mode_before: "hdc-normal".into(),
                    expected_mode_after: "hdc-normal".into(),
                    private_action_sha256: sha256(b"postflight action").to_hex(),
                    ..arkforge_ipc::messages::PublicStep::default()
                },
            ],
            ..ExecutablePlan::default()
        };

        assert!(prior_reboot_authorizes_rebind(&plan, &snapshot));

        plan.public_steps[0].kind = "verifyTarget".into();
        assert!(!prior_reboot_authorizes_rebind(&plan, &snapshot));
        plan.public_steps[0].kind = "reboot".into();
        plan.public_steps[0].expected_mode_after = "rockusb-loader".into();
        assert!(!prior_reboot_authorizes_rebind(&plan, &snapshot));
    }

    #[test]
    fn postflight_rebind_matches_every_typed_identity_field_but_not_probe_time() {
        let observation = stable_observation(100);
        let snapshot = admission_for(&observation);
        let mut fresh = observation.clone();
        fresh.observation_id = "USB-2207-5000-fresh".into();
        fresh.observed_at_epoch_ms = 200;

        assert!(observation_matches_admission(&fresh, &snapshot));

        fresh.descriptor_sha256 = sha256(b"replacement descriptor").to_hex();
        assert!(!observation_matches_admission(&fresh, &snapshot));
        fresh = observation.clone();
        fresh.protocol_identity[0].value = "org.openharmony.replacement".into();
        assert!(!observation_matches_admission(&fresh, &snapshot));
    }

    #[test]
    fn assessment_closes_only_the_resolved_authority_blocker() {
        let mut ready = pending_authority_assessment();
        close_resolved_authority_blocker(&mut ready);
        assert_eq!(ready.availability, "available");
        assert!(ready.unavailable_reason.is_empty());
        assert!(ready.unknowns.is_empty());
        assert!(ready.evidence_requirements.is_empty());

        let mut still_blocked = pending_authority_assessment();
        still_blocked.unknowns.push(KeyValue {
            key: "RK-V10".into(),
            value: "device mode is not declared".into(),
        });
        close_resolved_authority_blocker(&mut still_blocked);
        assert_eq!(still_blocked.availability, "unavailable");
        assert_eq!(still_blocked.unknowns.len(), 1);
        assert_eq!(still_blocked.unknowns[0].key, "RK-V10");
    }

    #[test]
    fn daemon_options_require_exact_hdc_path_and_digest_together() {
        let absolute_hdc = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(DaemonOptions::parse(&["--hdc".into(), absolute_hdc.clone()]).is_err());
        assert!(DaemonOptions::parse(&["--expect-hdc-sha256".into(), "0".repeat(64),]).is_err());
        assert!(
            DaemonOptions::parse(&[
                "--hdc".into(),
                "relative-hdc".into(),
                "--expect-hdc-sha256".into(),
                "0".repeat(64),
            ])
            .is_err()
        );
        assert!(
            DaemonOptions::parse(&[
                "--hdc".into(),
                absolute_hdc,
                "--expect-hdc-sha256".into(),
                "0".repeat(64),
            ])
            .is_ok()
        );
        assert!(
            DaemonOptions::parse(&[
                "--hardware-campaign".into(),
                "CLI-AC-28".into(),
                "--hardware-campaign".into(),
                "CLI-AC-29".into(),
            ])
            .is_err()
        );
    }

    #[test]
    fn status_protocol_keeps_an_empty_blocker_field() {
        let original = DaemonStatus {
            supervisor_pid: 12,
            daemon_pid: 13,
            epoch: 14,
            protocol_major: 1,
            protocol_minor: 0,
            daemon_version: "0.1.0".into(),
            mechanics_ready: true,
            authority_support_available: false,
            hdc_bound: true,
            hdc_sha256: sha256(b"hdc").to_hex(),
            hardware_campaign: "CLI-AC-28".into(),
            active_jobs: 0,
            blockers: Vec::new(),
        };
        let status = decode_status(&encode_status(&original)).unwrap();
        assert_eq!(status, original);
        assert_eq!(status.supervisor_pid, 12);
        assert_eq!(status.daemon_pid, 13);
        assert_eq!(status.epoch, 14);
        assert!(status.mechanics_ready);
        assert!(status.blockers.is_empty());
    }

    #[test]
    fn stop_refusal_preserves_active_job_count() {
        let mut encoded = Vec::new();
        wire::write_uint32(&mut encoded, 1, 2);
        wire::write_string(&mut encoded, 2, "ACTIVE_JOBS");
        wire::write_string(&mut encoded, 3, "The runtime has 2 active jobs.");
        wire::write_uint32(&mut encoded, 5, 6);
        wire::write_bool(&mut encoded, 6, true);
        let error = decode_reply(&encoded).unwrap_err();
        assert_eq!(error.code, "ACTIVE_JOBS");
        assert_eq!(error.exit_code, 6);
        assert!(error.message.contains('2'));
    }

    #[test]
    fn supervisor_error_wire_preserves_required_acknowledgements() {
        let mut encoded = Vec::new();
        wire::write_uint32(&mut encoded, 1, 2);
        wire::write_string(&mut encoded, 2, "ACKNOWLEDGEMENT_REQUIRED");
        wire::write_string(&mut encoded, 3, "The exact token set is incomplete.");
        wire::write_uint32(&mut encoded, 5, 4);
        wire::write_bool(&mut encoded, 6, true);
        wire::write_string(&mut encoded, 7, "data-loss:userdata");
        let error = decode_reply(&encoded).unwrap_err();
        assert_eq!(error.exit_code, 4);
        assert!(error.retryable);
        assert_eq!(error.required_acknowledgements, vec!["data-loss:userdata"]);
    }

    #[test]
    fn apply_rechecks_the_exact_current_authority_support_axes() {
        let mechanics = sha256(b"mechanics key");
        let hdc = sha256(b"hdc build");
        let (support_key, support_state) =
            current_authority_support(mechanics, Some(hdc), Some("CLI-AC-28")).unwrap();
        let plan = ExecutablePlan {
            mechanics_maturity_key_sha256: mechanics.to_hex(),
            authority_support_key_sha256: support_key.to_hex(),
            authority_support_state: support_state.as_str().into(),
            authority_support_campaign: support_state.campaign().unwrap().into(),
            ..ExecutablePlan::default()
        };

        require_current_authority_support(&plan, Some(hdc), Some("CLI-AC-28")).unwrap();
        assert_eq!(
            require_current_authority_support(
                &plan,
                Some(sha256(b"different hdc")),
                Some("CLI-AC-28")
            )
            .unwrap_err()
            .code,
            "AUTHORITY_SUPPORT_SEAL_MISMATCH"
        );
        assert_eq!(
            require_current_authority_support(&plan, Some(hdc), Some("CLI-AC-29"))
                .unwrap_err()
                .code,
            "AUTHORITY_SUPPORT_SEAL_MISMATCH"
        );
        assert_eq!(
            require_current_authority_support(&plan, None, Some("CLI-AC-28"))
                .unwrap_err()
                .code,
            "AUTHORITY_SUPPORT_UNAVAILABLE"
        );
    }

    #[test]
    fn pairing_epoch_is_durable_and_strictly_rotates_before_pairing() {
        let root = std::env::temp_dir().join(format!(
            "arkforge-cli-pairing-epoch-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let (first_epoch, first_secret) = fresh_pairing(&root).unwrap();
        let (second_epoch, second_secret) = fresh_pairing(&root).unwrap();
        assert_eq!(first_epoch, 1);
        assert_eq!(second_epoch, 2);
        assert_ne!(first_secret, second_secret);
        assert_eq!(
            std::fs::read_to_string(root.join("authority/pairing-epoch")).unwrap(),
            "2"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(root.join("authority/pairing-epoch"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn permit_is_durable_before_submission_and_same_epoch_retry_is_byte_exact() {
        let root =
            std::env::temp_dir().join(format!("arkforge-cli-permit-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let plan_digest = sha256(b"plan");
        let stable = sha256(b"stable target");
        let admitted = sha256(b"admitted device");
        let snapshot = StepAdmissionSnapshot {
            job_id: "JOB-TEST".into(),
            plan_id: "PLAN-TEST".into(),
            plan_sha256: plan_digest.as_bytes().to_vec(),
            step_id: "STEP-TEST".into(),
            attempt_id: "ATTEMPT-1".into(),
            public_step_sha256: sha256(b"public").as_bytes().to_vec(),
            private_action_sha256: sha256(b"private").as_bytes().to_vec(),
            effect_set_sha256: sha256(b"effects").as_bytes().to_vec(),
            admitted_device_facts_sha256: admitted.as_bytes().to_vec(),
            request_id: "REQUEST-TEST".into(),
            ..StepAdmissionSnapshot::default()
        };
        let record = AuthorityPlanRecord {
            plan: ExecutablePlan {
                plan_id: "PLAN-TEST".into(),
                plan_sha256: plan_digest.to_hex(),
                ..ExecutablePlan::default()
            },
            binding_id: "CLI-BIND-TEST".into(),
            stable_identity_sha256: stable.as_bytes().to_vec(),
            device_id: "OBS-TEST".into(),
            profile_id: "profile@1.0.0".into(),
            supersedes_job_id: String::new(),
            topology_sha256: "topology".into(),
            toolchain_id: "toolchain-a".into(),
        };
        let pairing = ControllerPairingSecret::new(PairingEpoch(7), b"secret".to_vec());
        let first =
            load_or_create_permit(&root, 7, &pairing, &snapshot, &record, admitted, 100, 200)
                .unwrap();
        let retried =
            load_or_create_permit(&root, 7, &pairing, &snapshot, &record, admitted, 101, 199)
                .unwrap();
        assert_eq!(retried.permit_cbor, first.permit_cbor);
        assert_eq!(retried.integrity_tag, first.integrity_tag);
        let permit = StepPermit::from_canonical_bytes(&first.permit_cbor).unwrap();
        let expected = mint_integrity_tag(&permit, &pairing).unwrap();
        assert_eq!(first.integrity_tag, expected.tag.as_bytes());
        let rotated_pairing =
            ControllerPairingSecret::new(PairingEpoch(8), b"rotated secret".to_vec());
        let rotated = load_or_create_permit(
            &root,
            8,
            &rotated_pairing,
            &snapshot,
            &record,
            admitted,
            101,
            201,
        )
        .unwrap();
        assert_ne!(rotated.permit_cbor, first.permit_cbor);
        assert_ne!(rotated.integrity_tag, first.integrity_tag);
        let rotated_permit = StepPermit::from_canonical_bytes(&rotated.permit_cbor).unwrap();
        assert_eq!(
            rotated_permit.controller_session_id.as_str(),
            "CLI-SESSION-8"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn recovery_acknowledgement_is_part_of_the_exact_apply_set() {
        let record = AuthorityPlanRecord {
            plan: ExecutablePlan {
                persistent_effects: vec![arkforge_ipc::messages::Effect {
                    target: "userdata".into(),
                    ..arkforge_ipc::messages::Effect::default()
                }],
                ..ExecutablePlan::default()
            },
            binding_id: "CLI-BIND-TEST".into(),
            stable_identity_sha256: sha256(b"stable").as_bytes().to_vec(),
            device_id: "OBS-TEST".into(),
            profile_id: "profile@1.0.0".into(),
            supersedes_job_id: "JOB-OLD".into(),
            topology_sha256: "topology".into(),
            toolchain_id: "toolchain-a".into(),
        };
        assert_eq!(
            record_acknowledgements(&record),
            vec![
                "data-loss:userdata".to_string(),
                "recovery:supersedes-job=JOB-OLD".to_string()
            ]
        );
    }
}
