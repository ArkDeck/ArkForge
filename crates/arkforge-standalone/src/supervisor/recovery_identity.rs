//! A closed migration of topology encodings on the same live Normal USB node.
//! Serial, descriptor, mode and protocol facts are never projected or weakened.
use super::*;

const POLICY: &str = "windows-location-paths-to-connector-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Migration {
    source_device: String,
    source_topology: String,
    source_stable: Vec<u8>,
    target_device: String,
    target_topology: String,
    target_stable: Vec<u8>,
}

impl Migration {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        wire::write_string(&mut out, 1, POLICY);
        wire::write_string(&mut out, 2, &self.source_device);
        wire::write_string(&mut out, 3, &self.source_topology);
        wire::write_bytes(&mut out, 4, &self.source_stable);
        wire::write_string(&mut out, 5, &self.target_device);
        wire::write_string(&mut out, 6, &self.target_topology);
        wire::write_bytes(&mut out, 7, &self.target_stable);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, StandaloneError> {
        let mut out = Self {
            source_device: String::new(),
            source_topology: String::new(),
            source_stable: Vec::new(),
            target_device: String::new(),
            target_topology: String::new(),
            target_stable: Vec::new(),
        };
        let mut reader = wire::Reader::new(bytes);
        while let Some((field, value)) = reader.next_field().map_err(blocked)? {
            match field {
                1 if value.as_str(field).map_err(blocked)? == POLICY => {}
                2 => out.source_device = value.as_str(field).map_err(blocked)?.into(),
                3 => out.source_topology = value.as_str(field).map_err(blocked)?.into(),
                4 => out.source_stable = value.as_bytes().map_err(blocked)?.to_vec(),
                5 => out.target_device = value.as_str(field).map_err(blocked)?.into(),
                6 => out.target_topology = value.as_str(field).map_err(blocked)?.into(),
                7 => out.target_stable = value.as_bytes().map_err(blocked)?.to_vec(),
                _ => return Err(blocked("Unknown topology migration policy or field.")),
            }
        }
        for id in [&out.source_device, &out.target_device] {
            let location = id
                .strip_prefix("USB-2207-5000-")
                .filter(|value| {
                    value.len() == 8
                        && value
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                })
                .ok_or_else(|| {
                    blocked("Topology migration is limited to a canonical Normal USB identity.")
                })?;
            let _ = u32::from_str_radix(location, 16).map_err(blocked)?;
        }
        if out.encode() != bytes
            || out.source_stable.len() != 32
            || out.target_stable.len() != 32
            || out.source_device == out.target_device
            || out.source_topology == out.target_topology
            || Sha256Digest::parse_hex(&out.source_topology).is_err()
            || Sha256Digest::parse_hex(&out.target_topology).is_err()
        {
            return Err(blocked(
                "Incomplete or noncanonical topology migration proof.",
            ));
        }
        Ok(out)
    }

    pub fn validate(
        &self,
        lineage: &ActiveTargetLineage,
        new: &AuthorityPlanRecord,
    ) -> Result<(), StandaloneError> {
        Self::decode(&self.encode())?;
        if self.source_device != lineage.current_device_id
            || self.source_topology != lineage.topology_sha256
            || self.source_stable != lineage.current_stable_identity_sha256
            || self.target_device != new.device_id
            || self.target_topology != new.topology_sha256
            || self.target_stable != new.stable_identity_sha256
        {
            return Err(blocked(
                "Topology migration does not bind the exact old and new identities.",
            ));
        }
        Ok(())
    }
}

pub(super) fn fresh(
    lineage: &ActiveTargetLineage,
    observation: &DeviceObservationView,
    probe: &DeviceProbeView,
) -> Result<Migration, StandaloneError> {
    if observation.mode != "hdc-normal" {
        return Err(blocked(
            "Recovery requires the original strong identity; a different USB mode without a durable rebind cannot establish it.",
        ));
    }
    let (legacy_id, legacy_topology) = arkforged::service::native_normal_topology_projection(
        &observation.observation_id,
        &observation.topology_sha256,
        &observation.descriptor_sha256,
        &observation.serial_sha256,
    )
    .map_err(blocked)?
    .ok_or_else(|| {
        blocked("No live same-device topology projection matches the current observation.")
    })?;
    prove(lineage, observation, probe, &legacy_id, &legacy_topology)
}

fn prove(
    lineage: &ActiveTargetLineage,
    observation: &DeviceObservationView,
    probe: &DeviceProbeView,
    legacy_id: &str,
    legacy_topology: &str,
) -> Result<Migration, StandaloneError> {
    let probed = &probe.observation;
    if observation.mode != "hdc-normal"
        || probed.mode != observation.mode
        || probed.observation_id != observation.observation_id
        || probed.topology_sha256 != observation.topology_sha256
        || probed.descriptor_sha256 != observation.descriptor_sha256
        || probed.serial_sha256 != observation.serial_sha256
        || probed.serial_evidence_kind != "descriptor"
        || observation.serial_evidence_kind != "descriptor"
    {
        return Err(blocked(
            "Topology projection requires identical fresh Normal identity facts.",
        ));
    }
    let mut projected = observation.clone();
    projected.observation_id = legacy_id.into();
    projected.topology_sha256 = legacy_topology.into();
    let mut projected_probe = probe.clone();
    projected_probe.observation.observation_id = legacy_id.into();
    projected_probe.observation.topology_sha256 = legacy_topology.into();
    validate_observations(lineage, &projected, &projected_probe)?;
    let migration = Migration {
        source_device: legacy_id.into(),
        source_topology: legacy_topology.into(),
        source_stable: stable_identity_digest(&projected, &projected_probe)
            .as_bytes()
            .to_vec(),
        target_device: observation.observation_id.clone(),
        target_topology: observation.topology_sha256.clone(),
        target_stable: stable_identity_digest(observation, probe)
            .as_bytes()
            .to_vec(),
    };
    Migration::decode(&migration.encode())
}

#[cfg(test)]
#[path = "recovery_identity_tests.rs"]
mod tests;
