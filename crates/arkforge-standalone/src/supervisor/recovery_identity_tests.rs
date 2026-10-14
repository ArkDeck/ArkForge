use super::*;
use arkforge_ipc::messages::KeyValue;

fn facts() -> (ActiveTargetLineage, DeviceObservationView, DeviceProbeView) {
    let old = DeviceObservationView {
        observation_id: "USB-2207-5000-12345678".into(),
        mode: "hdc-normal".into(),
        topology_sha256: sha256(b"retired location encoding").to_hex(),
        descriptor_sha256: sha256(b"same descriptor").to_hex(),
        serial_sha256: sha256(b"same serial").to_hex(),
        serial_evidence_kind: "usbDescriptor".into(),
        identity_strength: "serialAndTopology".into(),
        ..Default::default()
    };
    let old_probe = DeviceProbeView {
        observation: old.clone(),
        protocol_facts: vec![KeyValue {
            key: "mode".into(),
            value: "normal".into(),
        }],
        profile_id: "fixture@1.0.0".into(),
        facts_sha256: sha256(b"timestamped probe").to_hex(),
    };
    let lineage = ActiveTargetLineage {
        plan_id: "PLAN-OLD".into(),
        current_device_id: old.observation_id.clone(),
        current_stable_identity_sha256: stable_identity_digest(&old, &old_probe)
            .as_bytes()
            .to_vec(),
        topology_sha256: old.topology_sha256.clone(),
        revision: 1,
    };
    let mut current = old;
    current.observation_id = "USB-2207-5000-87654321".into();
    current.topology_sha256 = sha256(b"canonical connector").to_hex();
    current.observed_at_epoch_ms = 1000;
    let mut probe = old_probe;
    probe.observation = current.clone();
    (lineage, current, probe)
}

fn projection(
    lineage: &ActiveTargetLineage,
    observed: &DeviceObservationView,
    probe: &DeviceProbeView,
) -> Result<Migration, StandaloneError> {
    prove(
        lineage,
        observed,
        probe,
        &lineage.current_device_id,
        &lineage.topology_sha256,
    )
}

#[test]
fn same_normal_device_recomputes_exact_original_identity_without_mutating_any_facts() {
    let (lineage, observation, probe) = facts();
    assert!(validate_observations(&lineage, &observation, &probe).is_err());
    let migration = projection(&lineage, &observation, &probe).unwrap();
    assert_eq!(
        migration.source_stable,
        lineage.current_stable_identity_sha256
    );
    assert_eq!(
        migration.target_stable,
        stable_identity_digest(&observation, &probe).as_bytes()
    );
    assert_eq!(Migration::decode(&migration.encode()).unwrap(), migration);
    let record = AuthorityPlanRecord {
        plan: ExecutablePlan::default(),
        binding_id: "BIND-NEW".into(),
        stable_identity_sha256: migration.target_stable.clone(),
        device_id: observation.observation_id.clone(),
        profile_id: probe.profile_id.clone(),
        supersedes_job_id: "JOB-OLD".into(),
        topology_sha256: observation.topology_sha256.clone(),
        toolchain_id: "new".into(),
    };
    migration.validate(&lineage, &record).unwrap();
    let mut other = record.clone();
    other.stable_identity_sha256 = sha256(b"other board").as_bytes().to_vec();
    assert!(migration.validate(&lineage, &other).is_err());
    let mut other = lineage.clone();
    other.topology_sha256 = sha256(b"other original port").to_hex();
    assert!(migration.validate(&other, &record).is_err());
    let mut later = observation.clone();
    later.observed_at_epoch_ms += 1000;
    assert_eq!(projection(&lineage, &later, &probe).unwrap(), migration);
}

#[test]
fn projection_cannot_bridge_mode_serial_descriptor_protocol_or_strength_drift() {
    let (lineage, observed, probe) = facts();
    for mutate in [
        |value: &mut DeviceObservationView| value.mode = "rockusb-loader".into(),
        |value: &mut DeviceObservationView| value.serial_sha256 = sha256(b"other serial").to_hex(),
        |value: &mut DeviceObservationView| {
            value.descriptor_sha256 = sha256(b"other descriptor").to_hex()
        },
        |value: &mut DeviceObservationView| value.identity_strength = "topologyOnly".into(),
        |value: &mut DeviceObservationView| value.serial_evidence_kind = "protocol".into(),
        |value: &mut DeviceObservationView| value.malformed_descriptor = true,
        |value: &mut DeviceObservationView| {
            value.protocol_identity.push(KeyValue {
                key: "changed".into(),
                value: "fact".into(),
            })
        },
    ] {
        let mut changed = observed.clone();
        let mut changed_probe = probe.clone();
        mutate(&mut changed);
        assert!(projection(&lineage, &changed, &changed_probe).is_err());
        mutate(&mut changed_probe.observation);
        assert!(projection(&lineage, &changed, &changed_probe).is_err());
    }
    let mut changed = probe.clone();
    changed.protocol_facts[0].value = "different".into();
    assert!(projection(&lineage, &observed, &changed).is_err());
    changed = probe.clone();
    changed.profile_id = "other@1.0.0".into();
    assert!(projection(&lineage, &observed, &changed).is_err());
    assert!(
        prove(
            &lineage,
            &observed,
            &probe,
            "USB-2207-5000-00000000",
            &lineage.topology_sha256
        )
        .is_err()
    );
    assert!(
        prove(
            &lineage,
            &observed,
            &probe,
            &lineage.current_device_id,
            &sha256(b"other legacy port").to_hex()
        )
        .is_err()
    );
}

#[test]
fn migration_seal_is_canonical_and_rejects_other_policies_or_modes() {
    let (lineage, observed, probe) = facts();
    let migration = projection(&lineage, &observed, &probe).unwrap();
    for mutate in [
        |value: &mut Migration| value.source_device = "USB-2207-350a-12345678".into(),
        |value: &mut Migration| value.target_device = "USB-2207-5000-ABCDEFFF".into(),
        |value: &mut Migration| value.source_stable.clear(),
        |value: &mut Migration| value.target_topology = value.source_topology.clone(),
    ] {
        let mut changed = migration.clone();
        mutate(&mut changed);
        assert!(Migration::decode(&changed.encode()).is_err());
    }
    let mut unknown = migration.encode();
    wire::write_string(&mut unknown, 1, "caller-supplied-override");
    assert!(Migration::decode(&unknown).is_err());
}
