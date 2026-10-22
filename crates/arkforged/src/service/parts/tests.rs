
#[cfg(test)]
mod tests {
    use super::*;
    use arkforge_core::identity::{MaturityKey, MaturityState};

    const DAYU200_PROFILE: &str = include_str!("../../../../../profiles/dayu200.yaml");

    #[test]
    fn legacy_topology_requires_one_present_normal_node_and_every_current_digest() {
        let record = UsbDeviceRecord {
            vendor_id: 0x2207,
            product_id: 0x5000,
            location_id: 0x1122_3344,
            serial: Some("fixture-serial".into()),
            product_name: Some("fixture-product".into()),
            vendor_name: Some("fixture-vendor".into()),
            bcd_device: Some(0x100),
        };
        let candidate = arkforge_usb::LegacyTopologyObservation {
            descriptor: arkforge_usb::UsbInterfaceDescriptor {
                vendor_id: record.vendor_id,
                product_id: record.product_id,
                usb_specification: 0x300,
                device_release: 0x100,
                location_id: record.location_id,
                interface_class: 0xff,
                interface_subclass: 0x50,
                interface_protocol: 1,
                interface_number: 0,
                serial: record.serial.clone(),
                product_name: record.product_name.clone(),
                vendor_name: record.vendor_name.clone(),
            },
            legacy_location_id: 0x5566_7788,
        };
        let current_id = "USB-2207-5000-11223344";
        let topology = record.topology_digest().to_hex();
        let descriptor = record.descriptor_digest().to_hex();
        let serial = digest_in_domain(Domain::DeviceSerial, b"fixture-serial").to_hex();
        let project = |candidates| {
            select_normal_topology_projection(
                candidates,
                current_id,
                &topology,
                &descriptor,
                &serial,
            )
        };
        assert_eq!(
            project(vec![candidate.clone()]).unwrap(),
            Some((
                "USB-2207-5000-55667788".into(),
                digest_in_domain(Domain::UsbTopology, &0x5566_7788_u32.to_be_bytes()).to_hex(),
            ))
        );
        assert!(project(vec![candidate.clone(), candidate.clone()]).is_err());
        assert_eq!(project(Vec::new()).unwrap(), None);
        for mutate in [
            |value: &mut arkforge_usb::UsbInterfaceDescriptor| value.product_id = 0x350a,
            |value: &mut arkforge_usb::UsbInterfaceDescriptor| value.location_id += 1,
            |value: &mut arkforge_usb::UsbInterfaceDescriptor| {
                value.serial = Some("different".into())
            },
            |value: &mut arkforge_usb::UsbInterfaceDescriptor| value.serial = None,
            |value: &mut arkforge_usb::UsbInterfaceDescriptor| value.device_release += 1,
        ] {
            let mut changed = candidate.clone();
            mutate(&mut changed.descriptor);
            assert_eq!(project(vec![changed]).unwrap(), None);
        }
        assert_eq!(
            select_normal_topology_projection(
                vec![candidate],
                "USB-2207-5000-00000000",
                &topology,
                &descriptor,
                &serial
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn native_hdc_target_requires_both_sealed_usb_digests() {
        let serial_digest = digest_in_domain(Domain::DeviceSerial, b"serial").to_hex();
        let topology_digest =
            digest_in_domain(Domain::UsbTopology, &0x0112_0000_u32.to_be_bytes()).to_hex();
        assert_eq!(
            select_hdc_connect_key(
                vec![
                    (0x0112_0000, Some("serial".into())),
                    (0x0112_0000, Some("serial".into())),
                    (0x0113_0000, Some("serial".into())),
                    (0x0112_0000, Some("other".into())),
                ],
                &serial_digest,
                &topology_digest,
            ),
            Ok(Some("serial".into()))
        );
    }

    fn unresolved_control_journal(action: &str) -> arkforge_engine::journal::Journal {
        let mut journal = arkforge_engine::journal::Journal::new();
        let id = |value: &str| OpaqueId::new(value).unwrap();
        for kind in [
            JournalRecordKind::StepPermitAccepted,
            JournalRecordKind::StepIntentRecorded,
            JournalRecordKind::PermitConsuming,
        ] {
            let mut facts = vec![(id("permitId"), "PERMIT-1".into())];
            if kind == JournalRecordKind::PermitConsuming {
                facts.push((id("controlAction"), action.into()));
            }
            journal
                .append(kind, 1_000, 1, id("STEP-001"), facts)
                .unwrap();
        }
        journal
    }

    #[test]
    fn rehydrated_closed_managed_control_is_bounded_without_a_private_plan() {
        let possible = recovered_managed_control_possible_effects(&unresolved_control_journal(
            "enter-updater",
        ))
        .unwrap();
        assert_eq!(possible.completeness, EffectSetCompleteness::Bounded);
        assert!(possible.effects.persistent.is_empty());
        assert!(possible.effects.transient.is_empty());

        assert!(
            recovered_managed_control_possible_effects(&unresolved_control_journal("raw-shell"))
                .is_none(),
            "an unknown action must remain unbounded after restart"
        );
    }

    #[test]
    fn reconcile_api_durably_enters_reconciling_then_returns_unknown_without_a_plan() {
        let root = std::env::temp_dir().join(format!(
            "arkforged-reconcile-api-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let jobs = root.join("jobs");
        std::fs::create_dir_all(&jobs).unwrap();
        let job_id = "JOB-RECONCILE-API";
        let id = |value: &str| OpaqueId::new(value).unwrap();
        let (mut journal, _) =
            arkforge_engine::durable::DurableJournal::open(jobs.join(format!("{job_id}.journal")))
                .unwrap();
        journal
            .append(
                JournalRecordKind::JobCreated,
                1_000,
                1,
                id(job_id),
                vec![
                    (id("jobId"), job_id.into()),
                    (id("planId"), "PLAN-RECOVERED".into()),
                    (id("planDigest"), sha256(b"missing plan").to_hex()),
                    (id("totalSteps"), "1".into()),
                    (id("controllerSessionId"), "SESSION-RECOVERED".into()),
                ],
            )
            .unwrap();
        for kind in [
            JournalRecordKind::StepPermitAccepted,
            JournalRecordKind::StepIntentRecorded,
            JournalRecordKind::PermitConsuming,
        ] {
            let mut facts = vec![
                (id("jobId"), job_id.into()),
                (id("planId"), "PLAN-RECOVERED".into()),
                (id("stepId"), "STEP-CONTROL".into()),
                (id("attemptId"), "ATTEMPT-1".into()),
                (id("permitId"), "PERMIT-1".into()),
            ];
            if kind == JournalRecordKind::PermitConsuming {
                facts.push((id("controlAction"), "enter-updater".into()));
            }
            journal
                .append(kind, 1_001, 1, id("STEP-CONTROL"), facts)
                .unwrap();
        }
        drop(journal);

        let mut service =
            Service::new(&root, Vec::new(), Vec::new(), Clock::Fixed(2_000), None).unwrap();
        assert_eq!(
            service.jobs.job(job_id).unwrap().state(),
            arkforge_engine::JobState::OutcomeUnknown
        );
        let mut payload = Vec::new();
        arkforge_ipc::wire::write_string(&mut payload, 1, job_id);
        let response = service.handle(
            SessionKind::Controller,
            &Request {
                request_id: "REQ-RECONCILE".into(),
                api: Api::ReconcileJob,
                payload,
            },
            None,
        );
        assert_eq!(response.status, Status::Ok);
        assert_eq!(
            first_string_field(&response.payload, 2).as_deref(),
            Some("stillUnknown")
        );
        assert_eq!(
            first_string_field(&response.payload, 6).as_deref(),
            Some("outcomeUnknown")
        );
        let job = service.jobs.job(job_id).unwrap();
        assert!(
            job.events_from(0)
                .iter()
                .any(|event| event.job_state == "reconciling")
        );
        assert_eq!(job.last_reconcile_assessment().unwrap().0, "stillUnknown");
        assert!(
            job.journal()
                .records()
                .iter()
                .any(|record| { record.kind == JournalRecordKind::ReadOnlyObservationRecorded })
        );
        assert!(
            !job.journal()
                .records()
                .iter()
                .any(|record| { record.kind == JournalRecordKind::ExternalDispatchStarted })
        );
        drop(service);
        let _ = std::fs::remove_dir_all(root);
    }

    fn native_binding_maturity(campaign: Option<&str>) -> MaturityState {
        let root = std::env::temp_dir().join(format!(
            "arkforged-native-maturity-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let profile = arkforge_core::profile::load(DAYU200_PROFILE).unwrap();
        let mut service = Service::new(
            &root,
            vec![profile],
            Vec::new(),
            Clock::Fixed(1_754_380_800_000),
            campaign,
        )
        .unwrap();
        let backend_digest = sha256(b"one exact arkforged build");
        service.bind_native_dispatcher(BoundToolchain {
            id: OpaqueId::new("arkforged-native-rockusb").unwrap(),
            backend_digest,
        });

        let toolchain = service.bound_rockchip_toolchain_identity();
        assert_eq!(toolchain.kind, ToolchainKind::NativeProtocol);
        assert_eq!(toolchain.backend_digest, backend_digest);
        assert_eq!(
            service
                .readiness
                .dispatcher
                .as_ref()
                .unwrap()
                .backend_digest,
            backend_digest
        );

        let profile = service.profiles.values().next().unwrap();
        let state = service.maturity.lookup(&MaturityKey {
            provider: service.rockchip.identity().clone(),
            profile: profile.identity().unwrap(),
            artifact_format: service.rockchip.descriptor().artifact_formats[0].clone(),
            toolchain,
            host_platform: HostPlatform::current(),
            driver_facts_digest: driver_facts_digest(),
            evidence_set_digest: evidence_set_digest(),
        });
        drop(service);
        let _ = std::fs::remove_dir_all(root);
        state
    }

    #[test]
    fn native_binding_without_a_campaign_is_hardware_gated() {
        assert!(matches!(
            native_binding_maturity(None),
            MaturityState::HardwareGated { .. }
        ));
    }

    #[test]
    fn afa_ac_7_publishes_the_exact_native_build_as_a_hardware_campaign() {
        assert_eq!(
            native_binding_maturity(Some("AFA-AC-7")),
            MaturityState::HardwareCampaign {
                campaign: "AFA-AC-7".into()
            }
        );
    }

    #[test]
    fn only_semantically_confirmed_native_mode_changes_open_an_exact_rebind() {
        let loader = DeviceMode::new("rockusb-loader").unwrap();
        let normal = DeviceMode::new("hdc-normal").unwrap();

        assert!(successful_dispatch_requires_rebind(
            ActionDisposition::SemanticSuccess,
            Some(&loader),
            Some(&normal),
        ));
        assert!(!successful_dispatch_requires_rebind(
            ActionDisposition::SemanticSuccess,
            Some(&loader),
            Some(&loader),
        ));
        assert!(!successful_dispatch_requires_rebind(
            ActionDisposition::OutcomeUnknown,
            Some(&loader),
            Some(&normal),
        ));
        assert!(!successful_dispatch_requires_rebind(
            ActionDisposition::SemanticSuccess,
            None,
            Some(&normal),
        ));
    }
}
