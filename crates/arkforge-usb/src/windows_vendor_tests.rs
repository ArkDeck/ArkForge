use super::*;
use std::collections::VecDeque;

fn budget() -> Budget {
    Budget {
        started: Instant::now(),
        cancelled: Arc::new(AtomicBool::new(false)),
    }
}

fn candidate() -> Candidate {
    Candidate {
        instance_id: "USB\\VID_2207&PID_350A\\fixture".into(),
        driver_key: "{fixture-driver}\\0001".into(),
        hub_path: vec![0],
        port: 3,
    }
}

fn connection_bytes(loader: bool) -> Vec<u8> {
    let mut bytes = vec![0; CONNECTION_BYTES];
    bytes[..4].copy_from_slice(&3u32.to_le_bytes());
    let device = &mut bytes[4..22];
    device[0] = 18;
    device[1] = 1;
    device[2..4].copy_from_slice(&(if loader { 0x0201u16 } else { 0x0200u16 }).to_le_bytes());
    device[7] = 64;
    device[8..10].copy_from_slice(&0x2207u16.to_le_bytes());
    device[10..12].copy_from_slice(&0x350au16.to_le_bytes());
    device[12..14].copy_from_slice(&0x0200u16.to_le_bytes());
    device[14..18].copy_from_slice(&[1, 2, 3, 1]);
    bytes[22] = 1;
    bytes[25..27].copy_from_slice(&4u16.to_le_bytes());
    bytes[31..35].copy_from_slice(&1u32.to_le_bytes());
    bytes
}

fn config_bytes() -> Vec<u8> {
    vec![
        9, 2, 32, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 2, 0xff, 6, 5, 0, 7, 5, 0x81, 2, 0, 2, 0, 7, 5,
        0x02, 2, 0, 2, 0,
    ]
}

fn wide_bytes(value: &str) -> Vec<u8> {
    value.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn driver_bytes(key: &str) -> Vec<u8> {
    let mut bytes = vec![0; 8];
    bytes[..4].copy_from_slice(&3u32.to_le_bytes());
    bytes.extend(wide_bytes(key));
    bytes.extend([0, 0]);
    let len = bytes.len() as u32;
    bytes[4..8].copy_from_slice(&len.to_le_bytes());
    bytes
}

fn string_bytes(value: &str) -> Vec<u8> {
    let data = wide_bytes(value);
    let mut result = vec![(data.len() + 2) as u8, 3];
    result.extend(data);
    result
}

struct Reply {
    query: Query,
    descriptor: Option<(u8, u8, u16)>,
    bytes: Vec<u8>,
}

struct ScriptedHub {
    replies: VecDeque<Reply>,
    seen: Vec<Query>,
}

impl Hub for ScriptedHub {
    fn query(
        &mut self,
        query: Query,
        request: Vec<u8>,
        budget: &Budget,
    ) -> Result<Vec<u8>, UsbError> {
        budget.check()?;
        self.seen.push(query);
        let next = self
            .replies
            .pop_front()
            .expect("unexpected extra hub query");
        assert_eq!(next.query, query);
        assert_eq!(read_u32(&request, 0).unwrap(), 3);
        if let Some((kind, index, language)) = next.descriptor {
            assert_eq!(request[4..8], [0x80, 6, index, kind]);
            assert_eq!(read_u16(&request, 8).unwrap(), language);
            assert_eq!(
                request.len(),
                DESCRIPTOR_HEADER_BYTES + read_u16(&request, 10).unwrap() as usize
            );
            let mut result = request[..DESCRIPTOR_HEADER_BYTES].to_vec();
            result.extend(next.bytes);
            Ok(result)
        } else {
            Ok(next.bytes)
        }
    }
}

fn scripted(loader: bool) -> ScriptedHub {
    let direct = |query, bytes| Reply {
        query,
        descriptor: None,
        bytes,
    };
    let desc = |kind, index, language, bytes| Reply {
        query: Query::Descriptor,
        descriptor: Some((kind, index, language)),
        bytes,
    };
    ScriptedHub {
        replies: VecDeque::from([
            direct(Query::Connection, connection_bytes(loader)),
            direct(Query::DriverKey, driver_bytes(&candidate().driver_key)),
            desc(2, 0, 0, config_bytes()),
            desc(3, 0, 0, vec![4, 3, 9, 4]),
            desc(3, 1, 0x0409, string_bytes("Rockchip")),
            desc(3, 2, 0x0409, string_bytes("USB download gadget")),
            desc(3, 3, 0x0409, string_bytes("fixture-serial")),
            desc(2, 0, 0, config_bytes()),
            desc(3, 0, 0, vec![4, 3, 9, 4]),
            desc(3, 1, 0x0409, string_bytes("Rockchip")),
            desc(3, 2, 0x0409, string_bytes("USB download gadget")),
            desc(3, 3, 0x0409, string_bytes("fixture-serial")),
            direct(Query::Connection, connection_bytes(loader)),
            direct(Query::DriverKey, driver_bytes(&candidate().driver_key)),
        ]),
        seen: Vec::new(),
    }
}

#[test]
fn complete_hub_observation_retains_exact_identity_fields() {
    let mut hub = scripted(true);
    let record = observe_candidate(&mut hub, &candidate(), &budget())
        .unwrap()
        .unwrap();
    assert_eq!(record.vendor_id, 0x2207);
    assert_eq!(record.product_id, 0x350a);
    assert_eq!(record.usb_specification, 0x0201);
    assert_eq!(record.device_release, 0x0200);
    assert_eq!(
        record.location_id,
        instance_location(&candidate().instance_id)
    );
    assert_eq!(
        (
            record.interface_number,
            record.interface_class,
            record.interface_subclass,
            record.interface_protocol
        ),
        (0, 0xff, 6, 5)
    );
    assert_eq!(record.vendor_name.as_deref(), Some("Rockchip"));
    assert_eq!(record.product_name.as_deref(), Some("USB download gadget"));
    assert_eq!(record.serial.as_deref(), Some("fixture-serial"));
    assert!(hub.replies.is_empty());
}

#[test]
fn maskrom_is_not_reported_as_loader_from_shared_vid_pid() {
    let mut hub = scripted(false);
    assert!(
        observe_candidate(&mut hub, &candidate(), &budget())
            .unwrap()
            .is_none()
    );
}

#[test]
fn wrong_driver_key_stops_before_any_descriptor_request() {
    let mut hub = scripted(true);
    hub.replies[1].bytes = driver_bytes("another-device");
    assert!(observe_candidate(&mut hub, &candidate(), &budget()).is_err());
    assert_eq!(hub.seen, [Query::Connection, Query::DriverKey]);
}

#[test]
fn reenumeration_or_driver_change_during_observation_refuses() {
    for mutate in 0..5 {
        let mut hub = scripted(true);
        match mutate {
            0 => hub.replies[12].bytes[25] = 5, // USB address changed on the same port.
            1 => hub.replies[12].bytes[6] = 0,  // Loader bit changed.
            2 => hub.replies[13].bytes = driver_bytes("new-driver-instance"),
            3 => hub.replies[11].bytes = string_bytes("different-serial"),
            _ => hub.replies[7].bytes[5] = 2, // Active configuration changed.
        }
        assert!(observe_candidate(&mut hub, &candidate(), &budget()).is_err());
    }
}

#[test]
fn malformed_or_disconnected_connection_never_supplies_identity() {
    for mutate in 0..8 {
        let mut hub = scripted(true);
        let bytes = &mut hub.replies[0].bytes;
        match mutate {
            0 => bytes.truncate(34),
            1 => bytes[0] = 4,
            2 => bytes[31] = 0,
            3 => bytes[24] = 1,
            4 => bytes[4] = 17,
            5 => bytes[12] = 8,
            6 => bytes[21] = 2,
            _ => bytes[22] = 0,
        }
        assert!(observe_candidate(&mut hub, &candidate(), &budget()).is_err());
        assert_eq!(hub.seen, [Query::Connection]);
    }
}

#[test]
fn every_truncated_configuration_refuses() {
    let bytes = config_bytes();
    for length in 0..bytes.len() {
        assert!(
            config_interface(&bytes[..length], 1).is_err(),
            "length {length}"
        );
    }
    assert_eq!(config_interface(&bytes, 1).unwrap(), [0, 0xff, 6, 5]);
}

#[test]
fn ambiguous_alternate_or_malformed_endpoint_configuration_refuses() {
    for (offset, value) in [
        (2, 31),
        (4, 2),
        (5, 2),
        (9, 0),
        (12, 1),
        (13, 1),
        (18, 1),
        (20, 0x80),
        (21, 3),
        (27, 0x81),
    ] {
        let mut bytes = config_bytes();
        bytes[offset] = value;
        assert!(config_interface(&bytes, 1).is_err(), "offset {offset}");
    }
    let mut duplicate = config_bytes();
    duplicate.extend_from_slice(&[9, 4, 0, 0, 2, 0xff, 6, 5, 0]);
    let size = duplicate.len() as u16;
    duplicate[2..4].copy_from_slice(&size.to_le_bytes());
    assert!(config_interface(&duplicate, 1).is_err());
}

#[test]
fn absent_string_indices_stay_absent_instead_of_inventing_pnp_serial() {
    let mut hub = scripted(true);
    hub.replies[0].bytes[18..21].fill(0);
    hub.replies[12].bytes[18..21].fill(0);
    hub.replies.drain(8..12);
    hub.replies.drain(3..7);
    let record = observe_candidate(&mut hub, &candidate(), &budget())
        .unwrap()
        .unwrap();
    assert_eq!(record.serial, None);
    assert_eq!(record.product_name, None);
    assert_eq!(record.vendor_name, None);
    assert!(hub.replies.is_empty());
}

#[test]
fn declared_but_unreadable_serial_refuses_instead_of_weakening_identity() {
    let mut hub = scripted(true);
    hub.replies[6].bytes = vec![4, 3, 0, 0xd8]; // Unpaired UTF-16 surrogate.
    assert!(observe_candidate(&mut hub, &candidate(), &budget()).is_err());
}

#[test]
fn malformed_string_lengths_nulls_and_surrogates_refuse() {
    for bytes in [
        vec![],
        vec![2, 3],
        vec![4, 2, 65, 0],
        vec![5, 3, 65, 0],
        vec![4, 3, 0, 0],
        vec![4, 3, 0, 0xd8],
        vec![5, 3, 65, 0, 0],
    ] {
        assert!(usb_string(&bytes).is_err());
    }
    assert_eq!(usb_string(&string_bytes("设备")), Ok("设备".into()));
    assert!(utf16_string(&[65, 0]).is_err());
    assert!(utf16_string(&[65, 0, 0, 0, 66, 0, 0, 0]).is_err());
    assert_eq!(utf16_string(&[65, 0, 0, 0]), Ok("A".into()));
}

#[test]
fn language_descriptor_is_checked_before_selecting_a_language() {
    for bytes in [
        vec![],
        vec![2, 3],
        vec![4, 3, 0, 0],
        vec![4, 2, 9, 4],
        vec![5, 3, 9, 4, 0],
    ] {
        assert!(string_language(&bytes).is_err());
    }
    assert_eq!(string_language(&[4, 3, 9, 4]), Ok(0x0409));
}

#[test]
fn pnp_prefilter_is_bounded_to_the_exact_vid_pid() {
    assert!(candidate_id("USB\\VID_2207&PID_350A\\one"));
    assert!(candidate_id("usb\\vid_2207&pid_350a&rev_0200\\one"));
    for value in [
        "USB\\VID_2207&PID_350A1\\one",
        "USB\\VID_2207&PID_350B\\one",
        "USB\\VID_2207&PID_5000\\one",
        "OTHER\\VID_2207&PID_350A\\one",
        "USB\\VID_2207&PID_350A",
    ] {
        assert!(!candidate_id(value));
    }
}

#[test]
fn expired_or_cancelled_observation_issues_zero_queries() {
    for expired in [false, true] {
        let mut budget = budget();
        if expired {
            budget.started -= OBSERVATION_BUDGET;
        } else {
            budget.cancelled.store(true, Ordering::Release);
        }
        let mut hub = scripted(true);
        assert!(observe_candidate(&mut hub, &candidate(), &budget).is_err());
        assert!(hub.seen.is_empty());
    }
}

#[test]
fn timed_out_query_retains_its_resources_and_blocks_another_worker() {
    struct HeldQuery(mpsc::Sender<()>);
    impl Drop for HeldQuery {
        fn drop(&mut self) {
            self.0.send(()).unwrap();
        }
    }
    let running = Arc::new(AtomicBool::new(false));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (dropped_tx, dropped_rx) = mpsc::channel();
    let (cancelled_tx, cancelled_rx) = mpsc::channel();
    // A zero caller wait makes this deterministic without a scheduler sleep.
    // The simulated in-flight OS call owns its resources until explicitly released.
    assert!(
        bounded_observation(Arc::clone(&running), Duration::ZERO, move |budget| {
            let _held = HeldQuery(dropped_tx);
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            let refused = budget.check().is_err();
            cancelled_tx.send(refused).unwrap();
            budget.check()?;
            panic!("a cancelled query must not dispatch its next operation");
        })
        .is_err()
    );
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(matches!(
        dropped_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert!(running.load(Ordering::Acquire));
    assert!(
        bounded_observation(Arc::clone(&running), Duration::ZERO, |_| {
            panic!("a second observation worker must not start");
        })
        .is_err()
    );
    release_tx.send(()).unwrap();
    assert!(cancelled_rx.recv_timeout(Duration::from_secs(2)).unwrap());
    dropped_rx.recv_timeout(Duration::from_secs(2)).unwrap();
}

#[test]
fn completed_or_panicked_worker_releases_its_gate() {
    let running = Arc::new(AtomicBool::new(false));
    assert!(
        bounded_observation(Arc::clone(&running), Duration::from_secs(2), |_| {
            Ok(Vec::new())
        })
        .unwrap()
        .is_empty()
    );
    assert!(!running.load(Ordering::Acquire));
    assert!(
        bounded_observation(Arc::clone(&running), Duration::from_secs(2), |_| {
            panic!("simulated worker failure");
        })
        .is_err()
    );
    assert!(!running.load(Ordering::Acquire));
}

#[test]
fn incomplete_driver_key_refuses_before_identity_queries() {
    for mutate in 0..4 {
        let mut hub = scripted(true);
        let bytes = &mut hub.replies[1].bytes;
        match mutate {
            0 => bytes.truncate(8),
            1 => bytes[4] -= 2,
            2 => bytes[0] = 4,
            _ => *bytes.last_mut().unwrap() = 1,
        }
        assert!(observe_candidate(&mut hub, &candidate(), &budget()).is_err());
        assert_eq!(hub.seen, [Query::Connection, Query::DriverKey]);
    }
}

#[test]
fn only_documented_read_only_hub_ioctl_codes_are_available() {
    assert_eq!(Query::Connection.code(), 0x220448);
    assert_eq!(Query::DriverKey.code(), 0x220420);
    assert_eq!(Query::Descriptor.code(), 0x220410);
}

fn service(descriptor: UsbInterfaceDescriptor) -> super::super::ServiceRecord {
    super::super::ServiceRecord {
        path: vec![0],
        descriptor,
        binding: super::super::Binding::RockchipObservationOnly,
    }
}

#[test]
fn observed_vendor_loader_can_never_open_a_bulk_transport() {
    let descriptor = observe_candidate(&mut scripted(true), &candidate(), &budget())
        .unwrap()
        .unwrap();
    assert!(matches!(
        super::super::open_service(service(descriptor), 30_000),
        Err(UsbError::Claim(_))
    ));
}

#[test]
fn duplicate_and_identity_drift_are_refused_before_any_claim() {
    let descriptor = observe_candidate(&mut scripted(true), &candidate(), &budget())
        .unwrap()
        .unwrap();
    let selector = UsbInterfaceSelector::dayu200_loader();
    let duplicated = vec![service(descriptor.clone()), service(descriptor.clone())];
    assert!(
        matches!(super::super::select_service(duplicated, selector, None), Err(UsbError::AmbiguousInterfaces(records)) if records.len() == 2)
    );
    let mut changed = descriptor.clone();
    changed.serial = Some("different-fixture-serial".into());
    assert!(matches!(
        super::super::select_service(vec![service(changed)], selector, Some(&descriptor)),
        Err(UsbError::NoExactInterface(_))
    ));
}
