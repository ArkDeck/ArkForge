use super::*;
use std::collections::VecDeque;

fn endpoint(hub: &str, port: u32) -> Endpoint {
    Endpoint::new(&format!("USB#ROOT_HUB30#{hub}{HUB_SUFFIX}"), port).unwrap()
}

fn pair(a: &Endpoint, b: &Endpoint, start: &Endpoint) -> u32 {
    resolve(start, |value| {
        Ok(Some(if value == a {
            b.clone()
        } else {
            assert_eq!(value, b);
            a.clone()
        }))
    })
    .unwrap()
}

fn response(port: u32, index: u16, companion: Option<&Endpoint>) -> Vec<u8> {
    let mut bytes = vec![0; 16];
    bytes[..4].copy_from_slice(&port.to_le_bytes());
    bytes[12..14].copy_from_slice(&index.to_le_bytes());
    if let Some(companion) = companion {
        bytes[14..16].copy_from_slice(&(companion.port as u16).to_le_bytes());
        bytes.extend(companion.hub.encode_utf16().flat_map(u16::to_le_bytes));
    }
    bytes.extend([0, 0]);
    let length = bytes.len() as u32;
    bytes[4..8].copy_from_slice(&length.to_le_bytes());
    bytes
}

#[test]
fn speed_companions_share_a_connector_but_other_ports_and_controllers_do_not() {
    let normal = endpoint("CONTROLLER-A", 26);
    let loader = endpoint("CONTROLLER-A", 10);
    let physical = pair(&normal, &loader, &normal);
    assert_eq!(physical, pair(&normal, &loader, &loader));
    let neighbor_normal = endpoint("CONTROLLER-A", 27);
    let neighbor_loader = endpoint("CONTROLLER-A", 11);
    assert_ne!(
        physical,
        pair(&neighbor_normal, &neighbor_loader, &neighbor_normal)
    );
    assert_ne!(
        physical,
        pair(
            &endpoint("CONTROLLER-B", 26),
            &endpoint("CONTROLLER-B", 10),
            &endpoint("CONTROLLER-B", 26)
        )
    );
    let external_hub_companion = endpoint("OTHER-HUB", 2);
    assert_eq!(
        pair(&normal, &external_hub_companion, &normal),
        pair(&normal, &external_hub_companion, &external_hub_companion)
    );
}

#[test]
fn one_port_connector_stays_distinct_from_a_pair() {
    let current = endpoint("A", 10);
    let alone = resolve(&current, |_| Ok(None)).unwrap();
    assert_ne!(alone, pair(&current, &endpoint("A", 26), &current));
    assert_ne!(alone, resolve(&endpoint("A", 11), |_| Ok(None)).unwrap());
}

#[test]
fn only_reciprocal_unchanged_companion_relations_are_accepted() {
    let a = endpoint("A", 26);
    let b = endpoint("A", 10);
    let c = endpoint("A", 11);
    for replies in [
        vec![Some(a.clone())],
        vec![Some(b.clone()), None],
        vec![Some(b.clone()), Some(c.clone())],
        vec![Some(b.clone()), Some(a.clone()), None],
        vec![Some(b.clone()), Some(a.clone()), Some(b.clone()), Some(c)],
        vec![None, Some(b)],
    ] {
        let mut replies = VecDeque::from(replies);
        assert!(resolve(&a, |_| Ok(replies.pop_front().expect("unexpected query"))).is_err());
        assert!(replies.is_empty());
    }
    assert!(resolve(&a, |_| Err(invalid("query unavailable"))).is_err());
}

#[test]
fn hub_names_accept_only_exact_windows_usb_hub_interfaces() {
    let value = endpoint("A", 10);
    for prefix in ["", r"\\?\", r"\\.\", r"\??\"] {
        assert_eq!(
            value,
            Endpoint::new(&format!("{prefix}{}", value.hub.to_ascii_lowercase()), 10).unwrap()
        );
    }
    for path in ["", "USB#A", r"\\.\PhysicalDrive0", "USB#A\0", "USB#A/../B"] {
        assert!(Endpoint::new(path, 10).is_err());
    }
    for port in [0, 256, u32::MAX] {
        assert!(Endpoint::new(&value.hub, port).is_err());
    }
    assert!(Endpoint::new(&format!("USB#A\\B{HUB_SUFFIX}"), 10).is_err());
}

#[test]
fn connector_response_checks_identity_lengths_names_and_zero_companion() {
    let a = endpoint("A", 26);
    let valid = response(10, 0, Some(&a));
    assert_eq!(parse(&valid, 10, 0).unwrap(), Some(a));
    assert_eq!(parse(&response(10, 1, None), 10, 1).unwrap(), None);
    let mut padded = valid.clone();
    padded.extend([0; 4]);
    assert!(parse(&padded, 10, 0).is_ok());
    for offset in [0, 4, 12, 14] {
        let mut bad = valid.clone();
        bad[offset..offset + 2].copy_from_slice(&0xffffu16.to_le_bytes());
        assert!(parse(&bad, 10, 0).is_err(), "offset {offset}");
    }
    for length in [0, 1, 16, 17, valid.len() - 1] {
        assert!(parse(&valid[..length], 10, 0).is_err());
    }
    let mut unterminated = valid.clone();
    let last = unterminated.len() - 2;
    unterminated[last] = 65;
    assert!(parse(&unterminated, 10, 0).is_err());
    let mut invalid_utf16 = valid.clone();
    invalid_utf16[16..18].copy_from_slice(&0xd800u16.to_le_bytes());
    assert!(parse(&invalid_utf16, 10, 0).is_err());
    let mut no_port = valid;
    no_port[14..16].fill(0);
    assert!(parse(&no_port, 10, 0).is_err());
    let mut no_name = response(10, 0, None);
    no_name[14] = 26;
    assert!(parse(&no_name, 10, 0).is_err());
    let mut garbage = padded;
    *garbage.last_mut().unwrap() = 1;
    assert!(parse(&garbage, 10, 0).is_err());
}

struct HubScript(VecDeque<(u16, Vec<u8>)>);

impl Hub for HubScript {
    fn query(
        &mut self,
        query: Query,
        bytes: Vec<u8>,
        budget: &Budget,
    ) -> Result<Vec<u8>, UsbError> {
        budget.check()?;
        assert_eq!(query, Query::Connector);
        assert_eq!(query.code(), 0x220458);
        assert_eq!(read_u32(&bytes, 0).unwrap(), 10);
        assert_eq!(bytes.len(), MAX_PROPERTY_BYTES);
        let (index, response) = self.0.pop_front().expect("unexpected hub query");
        assert_eq!(read_u16(&bytes, 12).unwrap(), index);
        Ok(response)
    }
}

#[test]
fn native_query_vocabulary_rejects_multiple_companions() {
    let budget = Budget {
        started: Instant::now(),
        cancelled: Arc::new(AtomicBool::new(false)),
    };
    let a = endpoint("A", 26);
    let mut one = HubScript(VecDeque::from([(0, response(10, 0, Some(&a)))]));
    assert_eq!(companions(&mut one, 10, &budget).unwrap(), Some(a.clone()));
    assert!(one.0.is_empty());
    let mut reply = response(10, 0, Some(&a));
    reply[8] = 4;
    let mut multiple = HubScript(VecDeque::from([(0, reply)]));
    assert!(companions(&mut multiple, 10, &budget).is_err());
    assert!(multiple.0.is_empty());
}
