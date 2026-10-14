//! Windows USB connector identity, shared by WinUSB and hub-only observation.
//!
//! A USB 3 connector has distinct high-speed and SuperSpeed ports. Only the
//! hub driver's reciprocal companion-port relationship may collapse them;
//! similar ACPI names, matching device serials and caller aliases may not.

use super::*;

const CONNECTOR_BYTES: usize = 18;
const HUB_SUFFIX: &str = "#{F18A0E88-C30C-11D0-8815-00A0C906BED8}";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Endpoint {
    hub: String,
    port: u32,
}

impl Endpoint {
    fn new(hub: &str, port: u32) -> Result<Self, UsbError> {
        let upper = hub.to_ascii_uppercase();
        let key = [r"\\?\", r"\\.\", r"\??\"]
            .into_iter()
            .find_map(|prefix| upper.strip_prefix(prefix))
            .unwrap_or(&upper);
        if !(1..=255).contains(&port)
            || !key.starts_with("USB#")
            || !key.ends_with(HUB_SUFFIX)
            || key.contains(['\\', '/', '\0'])
            || key.encode_utf16().count() > MAX_PROPERTY_BYTES / 2
        {
            return Err(invalid("invalid USB companion hub or port"));
        }
        Ok(Self {
            hub: key.into(),
            port,
        })
    }

    fn native_path(&self) -> Vec<u16> {
        format!(r"\\?\{}", self.hub)
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect()
    }
}

pub(super) fn location(hub: &[u16], port: u32, budget: &Budget) -> Result<u32, UsbError> {
    let Some((&0, text)) = hub.split_last() else {
        return Err(invalid("unterminated physical hub path"));
    };
    let text = String::from_utf16(text).map_err(|_| invalid("invalid physical hub path"))?;
    let endpoint = Endpoint::new(&text, port)?;
    resolve(&endpoint, |endpoint| {
        let mut hub = NativeHub::open(&endpoint.native_path(), budget)?;
        companions(&mut hub, endpoint.port, budget)
    })
}

fn resolve(
    endpoint: &Endpoint,
    mut query: impl FnMut(&Endpoint) -> Result<Option<Endpoint>, UsbError>,
) -> Result<u32, UsbError> {
    let companion = query(endpoint)?;
    let mut ports = vec![endpoint.clone()];
    if let Some(other) = &companion {
        if other == endpoint || query(other)?.as_ref() != Some(endpoint) {
            return Err(invalid("USB companion relationship is not reciprocal"));
        }
        ports.push(other.clone());
    }
    // Do not seal a relationship that changed while its reverse was observed.
    if query(endpoint)? != companion {
        return Err(invalid(
            "USB companion relationship changed during observation",
        ));
    }
    if let Some(other) = &companion
        && query(other)?.as_ref() != Some(endpoint)
    {
        return Err(invalid(
            "USB companion relationship changed during observation",
        ));
    }
    ports.sort();
    let mut hash = 0x811c_9dc5u32;
    let mut feed = |unit: u16| {
        hash ^= u32::from(unit);
        hash = hash.wrapping_mul(0x0100_0193);
    };
    for unit in "arkforge/windows-usb-connector/v1\0".encode_utf16() {
        feed(unit);
    }
    for port in ports {
        for unit in port.hub.encode_utf16().chain(std::iter::once(0)) {
            feed(unit);
        }
        feed(port.port as u16);
        feed(0);
    }
    Ok(hash)
}

fn companions(
    hub: &mut impl Hub,
    port: u32,
    budget: &Budget,
) -> Result<Option<Endpoint>, UsbError> {
    // USB 3/xHCI requires CompanionIndex = 0. The response's explicit
    // PortHasMultipleCompanions flag rejects more complicated EHCI routing.
    query_companion(hub, port, 0, budget)
}

fn query_companion(
    hub: &mut impl Hub,
    port: u32,
    index: u16,
    budget: &Budget,
) -> Result<Option<Endpoint>, UsbError> {
    let mut request = vec![0; MAX_PROPERTY_BYTES];
    request[..4].copy_from_slice(&port.to_le_bytes());
    request[12..14].copy_from_slice(&index.to_le_bytes());
    parse(&hub.query(Query::Connector, request, budget)?, port, index)
}

fn parse(bytes: &[u8], port: u32, index: u16) -> Result<Option<Endpoint>, UsbError> {
    if bytes.len() < CONNECTOR_BYTES || read_u32(bytes, 0)? != port || read_u16(bytes, 12)? != index
    {
        return Err(invalid("incomplete or mismatched USB connector response"));
    }
    let flags = read_u32(bytes, 8)?;
    if flags & !0x0f != 0 || flags & 4 != 0 {
        return Err(invalid(
            "multiple or unknown USB companion routing is unsupported",
        ));
    }
    let length = read_u32(bytes, 4)? as usize;
    if !(CONNECTOR_BYTES..=MAX_PROPERTY_BYTES).contains(&length)
        || length > bytes.len()
        || !length.is_multiple_of(2)
        || bytes[length..].iter().any(|value| *value != 0)
    {
        return Err(invalid("invalid USB connector response length"));
    }
    let units = bytes[16..length].as_chunks::<2>().0;
    let end = units
        .iter()
        .position(|unit| *unit == [0, 0])
        .ok_or_else(|| invalid("unterminated USB companion hub"))?;
    if units[end..].iter().any(|unit| *unit != [0, 0]) {
        return Err(invalid("USB companion hub has data after its terminator"));
    }
    let other_port = read_u16(bytes, 14)?;
    if other_port == 0 && end == 0 {
        return Ok(None);
    }
    let name = decode_utf16(&bytes[16..16 + end * 2])?;
    Endpoint::new(&name, u32::from(other_port)).map(Some)
}

#[cfg(test)]
#[path = "windows_topology_tests.rs"]
mod tests;
