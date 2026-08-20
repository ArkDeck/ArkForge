//! Real read-only USB observation.
//!
//! architecture.md 11. This transport observes; it never claims a device, never
//! sends a request, and has no code path that could. It is how ArkForge sees a
//! board that is actually plugged in.
//!
//! Two boundaries it keeps:
//!
//! - **VID/PID indicate a mode, never a device.** The mapping comes from the
//!   DeviceProfile, which measured it under a named evidence entry
//!   (architecture.md 11.2). A device this Profile has no identity for is not
//!   reported as an unknown-mode device; it is not this Profile's device.
//! - **Serials are hashed, not stored.** A transcript records identity digests
//!   (architecture.md 11.4), so an exported observation cannot leak the board's
//!   serial into a report.
//!
//! Enumeration goes through [`UsbEnumerator`] so the OS-specific part is one
//! swappable implementation. On macOS that is `ioreg`, the read-only view of
//! IOKit: this build carries no third-party dependency (AFD-0001), so there is
//! no libusb and no FFI, and the OS's own read-only query is the honest
//! substrate. When a native IOKit binding arrives, only the enumerator changes.

use crate::{
    DeviceObservation, DeviceTransport, IdentityEvidenceStrength, ProtocolIdentityFact,
    RebindExpectation, RebindOutcome, SerialEvidence, TransportError, TransportSession,
    TypedDiscoveryFilter, evaluate_rebind,
};
use arkforge_core::digest::{Domain, Sha256Digest, digest_in_domain};
use arkforge_core::ids::{ObservationId, OpaqueId};
use arkforge_core::profile::DeviceProfile;
use core::fmt;
use std::sync::Arc;

/// One USB device as the operating system describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbDeviceRecord {
    pub vendor_id: u16,
    pub product_id: u16,
    /// The port path. This is the topology fact: the same board on another
    /// port is a different location.
    pub location_id: u32,
    pub serial: Option<String>,
    pub product_name: Option<String>,
    pub vendor_name: Option<String>,
    pub bcd_device: Option<u16>,
}

impl UsbDeviceRecord {
    /// Hash of the port path.
    pub fn topology_digest(&self) -> Sha256Digest {
        digest_in_domain(Domain::DeviceFacts, &self.location_id.to_be_bytes())
    }

    /// Hash of the descriptor facts, excluding the serial.
    ///
    /// The serial is separate because it is the field a rebind policy is
    /// allowed to let change (DAYU200 changes it between Loader and
    /// HDC-normal); folding it in here would make every descriptor comparison
    /// fail across that transition.
    pub fn descriptor_digest(&self) -> Sha256Digest {
        let mut payload = Vec::new();
        payload.extend_from_slice(&self.vendor_id.to_be_bytes());
        payload.extend_from_slice(&self.product_id.to_be_bytes());
        payload.extend_from_slice(&self.bcd_device.unwrap_or(0).to_be_bytes());
        payload.push(0);
        payload.extend_from_slice(self.vendor_name.as_deref().unwrap_or("").as_bytes());
        payload.push(0);
        payload.extend_from_slice(self.product_name.as_deref().unwrap_or("").as_bytes());
        digest_in_domain(Domain::DeviceFacts, &payload)
    }

    pub fn serial_evidence(&self) -> SerialEvidence {
        match &self.serial {
            None => SerialEvidence::Absent,
            Some(serial) => SerialEvidence::Descriptor {
                digest: digest_in_domain(Domain::DeviceFacts, serial.as_bytes()),
            },
        }
    }
}

/// Where USB device records come from.
pub trait UsbEnumerator: fmt::Debug + Send + Sync {
    fn enumerate(&self) -> Result<Vec<UsbDeviceRecord>, TransportError>;
}

/// Reads the macOS IOKit USB tree through `ioreg`.
///
/// Read-only: `ioreg` queries the registry the OS already maintains. It does
/// not open, claim or configure a device, so running it cannot disturb another
/// process that holds one — which matters here, because the HDC server owns the
/// board (architecture.md 9.1).
#[derive(Debug, Default, Clone, Copy)]
pub struct IoRegEnumerator;

impl UsbEnumerator for IoRegEnumerator {
    fn enumerate(&self) -> Result<Vec<UsbDeviceRecord>, TransportError> {
        let output = std::process::Command::new("ioreg")
            .args(["-c", "IOUSBHostDevice", "-r", "-l", "-w", "0"])
            .output()
            .map_err(|error| TransportError::Evidence(format!("ioreg failed: {error}")))?;
        if !output.status.success() {
            return Err(TransportError::Evidence(
                "ioreg exited non-zero".to_string(),
            ));
        }
        Ok(parse_ioreg(&String::from_utf8_lossy(&output.stdout)))
    }
}

/// A fixed record set, for tests and for replaying a captured enumeration.
#[derive(Debug, Clone)]
pub struct StaticEnumerator(pub Vec<UsbDeviceRecord>);

impl UsbEnumerator for StaticEnumerator {
    fn enumerate(&self) -> Result<Vec<UsbDeviceRecord>, TransportError> {
        Ok(self.0.clone())
    }
}

/// Parses `ioreg -c IOUSBHostDevice -r -l -w 0`.
///
/// A device block opens at a `+-o …<class IOUSBHostDevice` line. Nested
/// interface nodes repeat `idVendor`, so fields are only taken from the first
/// property block after that header — otherwise an interface's copy would
/// overwrite the device's.
pub fn parse_ioreg(text: &str) -> Vec<UsbDeviceRecord> {
    let mut records = Vec::new();
    let mut current: Option<PartialRecord> = None;

    for line in text.lines() {
        let trimmed = line.trim_start_matches([' ', '|', '+']).trim_start();
        if trimmed.starts_with("-o ") && line.contains("class IOUSBHostDevice") {
            if let Some(partial) = current.take() {
                records.extend(partial.finish());
            }
            current = Some(PartialRecord::default());
            continue;
        }
        let Some(partial) = current.as_mut() else {
            continue;
        };
        if partial.closed {
            continue;
        }
        // The device's own property block ends at the first `}` at its depth;
        // everything after belongs to child nodes.
        if trimmed == "}" {
            partial.closed = true;
            continue;
        }
        let Some((key, value)) = parse_property(trimmed) else {
            continue;
        };
        match key {
            "idVendor" => partial.vendor_id = value.parse().ok(),
            "idProduct" => partial.product_id = value.parse().ok(),
            "locationID" => partial.location_id = value.parse().ok(),
            "bcdDevice" => partial.bcd_device = value.parse().ok(),
            "USB Serial Number" => partial.serial = Some(unquote(value)),
            "USB Product Name" => partial.product_name = Some(unquote(value)),
            "USB Vendor Name" => partial.vendor_name = Some(unquote(value)),
            _ => {}
        }
    }
    if let Some(partial) = current.take() {
        records.extend(partial.finish());
    }
    records
}

#[derive(Debug, Default)]
struct PartialRecord {
    closed: bool,
    vendor_id: Option<u32>,
    product_id: Option<u32>,
    location_id: Option<u32>,
    bcd_device: Option<u32>,
    serial: Option<String>,
    product_name: Option<String>,
    vendor_name: Option<String>,
}

impl PartialRecord {
    fn finish(self) -> Option<UsbDeviceRecord> {
        Some(UsbDeviceRecord {
            vendor_id: self.vendor_id? as u16,
            product_id: self.product_id? as u16,
            location_id: self.location_id.unwrap_or(0),
            serial: self.serial,
            product_name: self.product_name,
            vendor_name: self.vendor_name,
            bcd_device: self.bcd_device.map(|value| value as u16),
        })
    }
}

fn parse_property(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix('"')?;
    let end = rest.find('"')?;
    let key = &rest[..end];
    let after = rest[end + 1..].trim_start();
    let value = after.strip_prefix('=')?.trim();
    Some((key, value))
}

/// `ioreg` renders a string property that itself contains quotes as `""x""`.
fn unquote(value: &str) -> String {
    let mut text = value.trim();
    while text.len() >= 2 && text.starts_with('"') && text.ends_with('"') {
        text = &text[1..text.len() - 1];
    }
    text.to_string()
}

/// A read-only USB transport scoped to one DeviceProfile.
#[derive(Debug)]
pub struct UsbTransport {
    id: OpaqueId,
    enumerator: Arc<dyn UsbEnumerator>,
    /// `(vendor_id, product_id) -> mode`, from the Profile.
    identities: Vec<(u16, u16, arkforge_core::effect::DeviceMode)>,
    profile_id: OpaqueId,
}

impl UsbTransport {
    /// Builds a transport that can recognize exactly the identities the Profile
    /// has measured.
    pub fn new(profile: &DeviceProfile, enumerator: Box<dyn UsbEnumerator>) -> Self {
        UsbTransport {
            id: OpaqueId::new("arkforge.transport.usb").expect("literal identifier"),
            enumerator: Arc::from(enumerator),
            identities: profile
                .usb_identities
                .iter()
                .map(|identity| {
                    (
                        identity.vendor_id,
                        identity.product_id,
                        identity.mode.clone(),
                    )
                })
                .collect(),
            profile_id: profile.id.clone(),
        }
    }

    pub fn with_ioreg(profile: &DeviceProfile) -> Self {
        Self::new(profile, Box::new(IoRegEnumerator))
    }

    /// Every USB device the host sees, whether or not this Profile knows it.
    ///
    /// Exposed for capture and diagnostics: a device the Profile cannot name is
    /// exactly what an operator needs to see when a board does not appear.
    pub fn enumerate_all(&self) -> Result<Vec<UsbDeviceRecord>, TransportError> {
        self.enumerator.enumerate()
    }

    fn observe(&self, record: &UsbDeviceRecord, at_epoch_ms: u64) -> Option<DeviceObservation> {
        observe_record(&self.identities, &self.profile_id, record, at_epoch_ms)
    }

    /// Observes now, labelling each device the Profile recognizes.
    pub fn observe_now(&self, at_epoch_ms: u64) -> Result<Vec<DeviceObservation>, TransportError> {
        Ok(self
            .enumerator
            .enumerate()?
            .iter()
            .filter_map(|record| self.observe(record, at_epoch_ms))
            .collect())
    }
}

fn observe_record(
    identities: &[(u16, u16, arkforge_core::effect::DeviceMode)],
    profile_id: &OpaqueId,
    record: &UsbDeviceRecord,
    at_epoch_ms: u64,
) -> Option<DeviceObservation> {
    let mode = identities
        .iter()
        .find(|(vendor, product, _)| *vendor == record.vendor_id && *product == record.product_id)
        .map(|(_, _, mode)| mode.clone())?;

    let serial_evidence = record.serial_evidence();
    // Serial plus a port path is the strongest claim a descriptor read can
    // make. Confirming identity through the protocol is a Provider's job,
    // and this transport does not speak one.
    let identity_strength = match serial_evidence {
        SerialEvidence::Absent => IdentityEvidenceStrength::ClassOnly,
        _ => IdentityEvidenceStrength::SerialAndTopology,
    };

    let observation_id = ObservationId::new(format!(
        "USB-{:04x}-{:04x}-{:08x}",
        record.vendor_id, record.product_id, record.location_id
    ))
    .ok()?;

    let mut protocol_identity = Vec::new();
    if let Some(name) = &record.product_name {
        protocol_identity.push(ProtocolIdentityFact {
            key: OpaqueId::new("usb.productName").expect("literal identifier"),
            value: name.clone(),
        });
    }
    if let Some(name) = &record.vendor_name {
        protocol_identity.push(ProtocolIdentityFact {
            key: OpaqueId::new("usb.vendorName").expect("literal identifier"),
            value: name.clone(),
        });
    }
    protocol_identity.push(ProtocolIdentityFact {
        key: OpaqueId::new("usb.identity").expect("literal identifier"),
        value: format!("{:#06x}:{:#06x}", record.vendor_id, record.product_id),
    });
    protocol_identity.push(ProtocolIdentityFact {
        key: OpaqueId::new("profile").expect("literal identifier"),
        value: profile_id.to_string(),
    });
    protocol_identity.sort();

    Some(DeviceObservation {
        observation_id,
        observed_at_epoch_ms: at_epoch_ms,
        mode,
        topology_digest: record.topology_digest(),
        descriptor_digest: record.descriptor_digest(),
        serial_evidence,
        protocol_identity,
        provider_candidates: Vec::new(),
        identity_strength,
        // A descriptor that ioreg rendered is by construction a settled one:
        // the OS finished enumerating it. Transient malformed reads are a claim
        // only a live protocol read can make.
        malformed_descriptor: false,
    })
}

impl DeviceTransport for UsbTransport {
    fn transport_id(&self) -> &OpaqueId {
        &self.id
    }

    fn discover(
        &self,
        filter: &TypedDiscoveryFilter,
        deadline_epoch_ms: u64,
    ) -> Result<Vec<DeviceObservation>, TransportError> {
        Ok(self
            .observe_now(deadline_epoch_ms)?
            .into_iter()
            .filter(|observation| filter.accepts(observation))
            .collect())
    }

    fn open_exact(
        &self,
        observation: &DeviceObservation,
    ) -> Result<Box<dyn TransportSession>, TransportError> {
        // "Open" here means: confirm the exact device is still present and
        // start a continuity session over it. This transport never claims the
        // USB interface — the HDC server owns it, and taking it would be the
        // one thing architecture.md 9.1 reserves to the authority.
        let present = self
            .observe_now(observation.observed_at_epoch_ms)?
            .into_iter()
            .find(|candidate| {
                candidate.descriptor_digest == observation.descriptor_digest
                    && candidate.topology_digest == observation.topology_digest
            });
        match present {
            Some(observation) => {
                let session_digest = digest_in_domain(
                    Domain::TransportSession,
                    &[
                        observation.descriptor_digest.as_bytes().as_slice(),
                        observation.topology_digest.as_bytes().as_slice(),
                    ]
                    .concat(),
                );
                Ok(Box::new(UsbObservationSession {
                    session_digest,
                    observation,
                    enumerator: Arc::clone(&self.enumerator),
                    identities: self.identities.clone(),
                    profile_id: self.profile_id.clone(),
                    detached: false,
                }))
            }
            None => Err(TransportError::NoDevice),
        }
    }

    fn wait_for_rebind(
        &self,
        expectation: &RebindExpectation,
        previous: &DeviceObservation,
    ) -> Result<RebindOutcome, TransportError> {
        // One sweep. Polling belongs to the caller, which owns the deadline and
        // the tolerance window; a transport that slept inside this call would
        // hide both from the journal.
        let observations = self.observe_now(previous.observed_at_epoch_ms)?;
        Ok(evaluate_rebind(expectation, previous, &observations))
    }
}

#[derive(Debug)]
struct UsbObservationSession {
    session_digest: Sha256Digest,
    observation: DeviceObservation,
    enumerator: Arc<dyn UsbEnumerator>,
    identities: Vec<(u16, u16, arkforge_core::effect::DeviceMode)>,
    profile_id: OpaqueId,
    detached: bool,
}

impl TransportSession for UsbObservationSession {
    fn session_digest(&self) -> Sha256Digest {
        self.session_digest
    }

    fn observation(&self) -> &DeviceObservation {
        &self.observation
    }

    fn reread_identity(&mut self) -> Result<DeviceObservation, TransportError> {
        let records = self.enumerator.enumerate()?;
        let mut at_same_topology = records
            .iter()
            .filter(|record| record.topology_digest() == self.observation.topology_digest)
            .filter_map(|record| {
                observe_record(&self.identities, &self.profile_id, record, now_ms())
            });
        let Some(observation) = at_same_topology.next() else {
            self.detached = true;
            return Err(TransportError::NoDevice);
        };
        if at_same_topology.next().is_some() {
            self.detached = true;
            return Err(TransportError::Ambiguous(2));
        }
        self.observation = observation.clone();
        Ok(observation)
    }

    fn saw_detach(&self) -> bool {
        self.detached
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|delta| delta.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A verbatim excerpt of `ioreg -c IOUSBHostDevice -r -l -w 0` taken from
    /// the DAYU200 attached on 2026-08-14, trimmed to two devices.
    const IOREG_SAMPLE: &str = r#"
+-o "USB2.0 Hub"@01100000  <class IOUSBHostDevice, id 0x100024100, registered, matched, active, busy 0 (5 ms), retain 20>
  | {
  |   "idProduct" = 1544
  |   "USB Product Name" = "USB2.0 Hub"
  |   "idVendor" = 1507
  |   "locationID" = 17825792
  | }
  |
  +-o AppleUSBHostCompositeDevice  <class AppleUSBHostCompositeDevice, id 0x100024105, !registered>
  |   {
  |     "idVendor" = 9999
  |   }
  |

+-o "HDC Device"@01200000  <class IOUSBHostDevice, id 0x1000241e5, registered, matched, active, busy 0 (14 ms), retain 40>
  | {
  |   "kUSBSerialNumberString" = "150100424a544434520325834a7c4900"
  |   "USB Serial Number" = "150100424a544434520325834a7c4900"
  |   "USB Vendor Name" = "Rockchip"
  |   "kUSBProductString" = ""HDC Device""
  |   "USB Product Name" = ""HDC Device""
  |   "idVendor" = 8711
  |   "idProduct" = 20480
  |   "bcdDevice" = 547
  |   "locationID" = 18874368
  | }
  |
  +-o AppleUSBHostCompositeDevice  <class AppleUSBHostCompositeDevice, id 0x1000241ea, !registered>
  |   {
  |     "idVendor" = 8711
  |     "idProduct" = 20480
  |     "USB Product Name" = "an interface node that must not overwrite the device"
  |   }
"#;

    fn dayu200_profile() -> DeviceProfile {
        arkforge_core::profile::load(include_str!("../../../profiles/dayu200.yaml")).unwrap()
    }

    #[test]
    fn parses_the_real_ioreg_output() {
        let records = parse_ioreg(IOREG_SAMPLE);
        assert_eq!(records.len(), 2, "{records:#?}");

        let board = records
            .iter()
            .find(|record| record.vendor_id == 0x2207)
            .expect("the Rockchip device");
        assert_eq!(board.product_id, 0x5000);
        assert_eq!(board.location_id, 0x0120_0000);
        assert_eq!(board.bcd_device, Some(547));
        assert_eq!(
            board.serial.as_deref(),
            Some("150100424a544434520325834a7c4900")
        );
        // ioreg double-quotes a string that itself contains quotes.
        assert_eq!(board.product_name.as_deref(), Some("HDC Device"));
        assert_eq!(board.vendor_name.as_deref(), Some("Rockchip"));
    }

    #[test]
    fn an_interface_node_does_not_overwrite_its_device() {
        let records = parse_ioreg(IOREG_SAMPLE);
        let board = records
            .iter()
            .find(|record| record.vendor_id == 0x2207)
            .unwrap();
        assert_eq!(
            board.product_name.as_deref(),
            Some("HDC Device"),
            "the child interface node must not win"
        );
    }

    #[test]
    fn only_devices_the_profile_has_measured_are_observed() {
        let profile = dayu200_profile();
        let transport = UsbTransport::new(
            &profile,
            Box::new(StaticEnumerator(parse_ioreg(IOREG_SAMPLE))),
        );
        let observations = transport.observe_now(1_000).unwrap();
        assert_eq!(
            observations.len(),
            1,
            "the USB hub is not this profile's device"
        );
        assert_eq!(observations[0].mode.as_str(), "hdc-normal");
        assert_eq!(
            observations[0].identity_strength,
            IdentityEvidenceStrength::SerialAndTopology
        );
        // But everything the host sees is still available for diagnostics.
        assert_eq!(transport.enumerate_all().unwrap().len(), 2);
    }

    #[test]
    fn the_serial_never_appears_in_an_observation() {
        let profile = dayu200_profile();
        let transport = UsbTransport::new(
            &profile,
            Box::new(StaticEnumerator(parse_ioreg(IOREG_SAMPLE))),
        );
        let observation = &transport.observe_now(1_000).unwrap()[0];
        let rendered = format!("{observation:?}");
        assert!(
            !rendered.contains("150100424a544434520325834a7c4900"),
            "an observation must carry the serial's digest, not the serial"
        );
        assert!(matches!(
            observation.serial_evidence,
            SerialEvidence::Descriptor { .. }
        ));
    }

    #[test]
    fn the_same_board_on_another_port_has_a_different_topology() {
        let mut moved = parse_ioreg(IOREG_SAMPLE);
        let board = moved
            .iter_mut()
            .find(|record| record.vendor_id == 0x2207)
            .unwrap();
        let original = board.topology_digest();
        board.location_id = 0x0140_0000;
        assert_ne!(original, board.topology_digest());
        // …but the same descriptor, because the descriptor is not the port.
        assert_eq!(
            board.descriptor_digest(),
            parse_ioreg(IOREG_SAMPLE)
                .iter()
                .find(|record| record.vendor_id == 0x2207)
                .unwrap()
                .descriptor_digest()
        );
    }

    #[test]
    fn opening_a_device_that_is_no_longer_present_fails() {
        let profile = dayu200_profile();
        let present = UsbTransport::new(
            &profile,
            Box::new(StaticEnumerator(parse_ioreg(IOREG_SAMPLE))),
        );
        let observation = present.observe_now(1_000).unwrap().remove(0);

        let unplugged = UsbTransport::new(&profile, Box::new(StaticEnumerator(Vec::new())));
        assert_eq!(
            unplugged.open_exact(&observation).unwrap_err(),
            TransportError::NoDevice
        );
    }

    /// AD-013: on 2026-08-14 the retired mode probe reported `Mode=Maskrom`
    /// for this exact device — PID 0x5000, product string "HDC Device", HDC
    /// answering `param get` — three times running.
    ///
    /// Maskrom is the stage where a loader is written into SRAM. A discovery
    /// path that believed the tool's mode word would act on a booted system as
    /// though it were in Maskrom. This transport is immune by construction: the
    /// mode comes from the Profile's measured VID/PID, and no vendor tool's
    /// output reaches it.
    #[test]
    fn a_pid_a_legacy_probe_misreported_still_resolves_by_profile() {
        let profile = dayu200_profile();
        let transport = UsbTransport::new(
            &profile,
            Box::new(StaticEnumerator(parse_ioreg(IOREG_SAMPLE))),
        );
        let observation = &transport.observe_now(1_000).unwrap()[0];

        // The tool says Maskrom. The Profile's measured identity says otherwise,
        // and the Profile is what this transport reads.
        assert_eq!(observation.mode.as_str(), "hdc-normal");
        assert_eq!(
            profile
                .mode_for_usb_identity(0x2207, 0x5000)
                .map(|mode| mode.as_str()),
            Some("hdc-normal")
        );

        // Nothing in an observation carries the retired probe's vocabulary.
        // These are its mode word and record-shape fields, which are output a
        // mode must never be derived from.
        // (The tool's *name* is deliberately not listed: the architecture guard
        // forbids naming it in this crate, and the property under test is about
        // the tool's output, not its name.)
        let rendered = format!("{observation:?}").to_lowercase();
        for tool_word in ["maskrom", "devno", "locationid"] {
            assert!(
                !rendered.contains(tool_word),
                "an observation must not carry a vendor tool's vocabulary: {tool_word}"
            );
        }
    }

    /// The Profile is the only source of the VID/PID -> mode mapping, so a
    /// device whose identity nobody measured resolves to no mode at all —
    /// which is what happened in Loader before 0x350a was measured.
    #[test]
    fn an_unmeasured_identity_resolves_to_no_mode() {
        let profile = dayu200_profile();
        let mut records = parse_ioreg(IOREG_SAMPLE);
        let board = records
            .iter_mut()
            .find(|record| record.vendor_id == 0x2207)
            .unwrap();
        board.product_id = 0x9999;

        let transport = UsbTransport::new(&profile, Box::new(StaticEnumerator(records)));
        assert!(
            transport.observe_now(1_000).unwrap().is_empty(),
            "an unmeasured VID/PID names no mode"
        );
    }

    #[test]
    fn garbage_input_parses_to_nothing_rather_than_panicking() {
        for input in [
            "",
            "not ioreg output",
            "+-o \"x\"@1 <class IOUSBHostDevice",
            "{}{}{}",
        ] {
            let _ = parse_ioreg(input);
        }
    }
}
