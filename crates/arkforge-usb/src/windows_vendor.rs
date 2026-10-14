//! Observe vendor-bound Loaders and HDC-normal through Windows hub queries.
//!
//! No Rockusb device/pipe is opened and no vendor IOCTL is issued. This is an
//! observation path only; the parent refuses bulk claims for these records.
//! `usbioctl.h` structures are packed to one byte, so decode their byte slices
//! explicitly rather than casting potentially unaligned device output.

use super::{
    Bool, CreateFileW, DIGCF_PRESENT, DeviceInfoSet, DeviceInfoSetGuard, Dword,
    ERROR_INSUFFICIENT_BUFFER, ERROR_NO_MORE_ITEMS, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ,
    FILE_SHARE_WRITE, GENERIC_READ, GENERIC_WRITE, Guid, Handle, INVALID_HANDLE_VALUE,
    OPEN_EXISTING, SetupDiGetClassDevsW, SpDevInfoData, UsbError, UsbInterfaceDescriptor,
    UsbInterfaceSelector, device_instance_id, enumeration_error,
};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::time::{Duration, Instant};

const DIGCF_ALLCLASSES: u32 = 4;
const SPDRP_SERVICE: u32 = 4;
const SPDRP_DRIVER: u32 = 9;
const SPDRP_ADDRESS: u32 = 28;
const SPDRP_LOCATION_PATHS: u32 = 35;
const MAX_PROPERTY_BYTES: usize = 65_536;
const MAX_CANDIDATES: usize = 16;
const OBSERVATION_BUDGET: Duration = Duration::from_secs(5);
const CONNECTION_BYTES: usize = 35;
const DESCRIPTOR_HEADER_BYTES: usize = 12;
const HUB_GUID: Guid = Guid {
    data1: 0xf18a_0e88,
    data2: 0xc30c,
    data3: 0x11d0,
    data4: [0x88, 0x15, 0x00, 0xa0, 0xc9, 0x06, 0xbe, 0xd8],
};

#[link(name = "setupapi")]
unsafe extern "system" {
    fn SetupDiEnumDeviceInfo(set: DeviceInfoSet, index: Dword, info: *mut SpDevInfoData) -> Bool;
    fn SetupDiGetDeviceRegistryPropertyW(
        set: DeviceInfoSet,
        info: *mut SpDevInfoData,
        property: Dword,
        kind: *mut Dword,
        buffer: *mut u8,
        size: Dword,
        required: *mut Dword,
    ) -> Bool;
}

#[link(name = "cfgmgr32")]
unsafe extern "system" {
    fn CM_Get_Parent(parent: *mut u32, child: u32, flags: u32) -> u32;
    fn CM_Get_Device_ID_Size(length: *mut u32, dev_inst: u32, flags: u32) -> u32;
    fn CM_Get_Device_IDW(dev_inst: u32, buffer: *mut u16, length: u32, flags: u32) -> u32;
    fn CM_Get_Device_Interface_List_SizeW(
        length: *mut u32,
        class: *const Guid,
        device: *const u16,
        flags: u32,
    ) -> u32;
    fn CM_Get_Device_Interface_ListW(
        class: *const Guid,
        device: *const u16,
        buffer: *mut u16,
        length: u32,
        flags: u32,
    ) -> u32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn DeviceIoControl(
        device: Handle,
        code: Dword,
        input: *mut core::ffi::c_void,
        input_bytes: Dword,
        output: *mut core::ffi::c_void,
        output_bytes: Dword,
        returned: *mut Dword,
        overlapped: *mut core::ffi::c_void,
    ) -> Bool;
}

fn invalid(message: &str) -> UsbError {
    UsbError::Enumeration(format!("USB hub observation: {message}"))
}

/// Only one observation worker may be outstanding. If a Windows query does
/// not return, it retains its own handle and buffer; the caller times out and
/// later observations fail closed instead of accumulating workers or freeing
/// memory still in use by Windows. A cancelled worker issues no next query.
static OBSERVATION_RUNNING: OnceLock<Arc<AtomicBool>> = OnceLock::new();

pub(super) fn observe() -> Result<Vec<UsbInterfaceDescriptor>, UsbError> {
    bounded_observation(
        Arc::clone(OBSERVATION_RUNNING.get_or_init(|| Arc::new(AtomicBool::new(false)))),
        OBSERVATION_BUDGET,
        observe_inner,
    )
}

fn bounded_observation(
    running: Arc<AtomicBool>,
    wait: Duration,
    observe: impl FnOnce(&Budget) -> Result<Vec<UsbInterfaceDescriptor>, UsbError> + Send + 'static,
) -> Result<Vec<UsbInterfaceDescriptor>, UsbError> {
    if running
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err(invalid("a previous observation is still outstanding"));
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = Arc::clone(&cancelled);
    let worker_running = Arc::clone(&running);
    let started = Instant::now();
    let (sender, receiver) = mpsc::sync_channel(1);
    let worker = std::thread::Builder::new()
        .name("arkforge-usb-observe".into())
        .spawn(move || {
            struct RunningGuard(Arc<AtomicBool>);
            impl Drop for RunningGuard {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::Release);
                }
            }
            let running = RunningGuard(worker_running);
            let budget = Budget {
                started,
                cancelled: worker_cancelled,
            };
            let result = observe(&budget).and_then(|result| {
                budget.check()?;
                Ok(result)
            });
            drop(running);
            let _ = sender.send(result);
        });
    if worker.is_err() {
        running.store(false, Ordering::Release);
        return Err(invalid("could not start the bounded observation worker"));
    }
    match receiver.recv_timeout(wait) {
        Ok(result) => result,
        Err(_) => {
            cancelled.store(true, Ordering::Release);
            Err(invalid("observation did not complete within its budget"))
        }
    }
}

struct Budget {
    started: Instant,
    cancelled: Arc<AtomicBool>,
}

impl Budget {
    fn check(&self) -> Result<(), UsbError> {
        if self.cancelled.load(Ordering::Acquire) || self.started.elapsed() >= OBSERVATION_BUDGET {
            Err(invalid("observation budget exhausted"))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Candidate {
    instance_id: String,
    product_id: u16,
    driver_key: String,
    location_paths: Vec<u8>,
    hub_path: Vec<u16>,
    port: u32,
}

fn observe_inner(budget: &Budget) -> Result<Vec<UsbInterfaceDescriptor>, UsbError> {
    let candidates = candidates(budget)?;
    let mut records = Vec::new();
    for candidate in &candidates {
        budget.check()?;
        let mut hub = NativeHub::open(&candidate.hub_path, budget)?;
        if let Some(descriptor) = observe_candidate(&mut hub, candidate, budget)? {
            records.push(descriptor);
        }
    }
    // Reject unplug/rebind/driver/parent/port changes during the whole census.
    if candidates != self::candidates(budget)? {
        return Err(invalid("PnP binding changed during observation"));
    }
    budget.check()?;
    Ok(records)
}

fn candidates(budget: &Budget) -> Result<Vec<Candidate>, UsbError> {
    budget.check()?;
    let usb = [b'U' as u16, b'S' as u16, b'B' as u16, 0];
    let set = unsafe {
        SetupDiGetClassDevsW(
            ptr::null(),
            usb.as_ptr(),
            ptr::null_mut(),
            DIGCF_PRESENT | DIGCF_ALLCLASSES,
        )
    };
    if set == INVALID_HANDLE_VALUE {
        return Err(enumeration_error("USB SetupDiGetClassDevsW"));
    }
    let set = DeviceInfoSetGuard(set);
    let mut found = Vec::new();
    for index in 0..4096 {
        budget.check()?;
        let mut info = SpDevInfoData::default();
        if unsafe { SetupDiEnumDeviceInfo(set.0, index, &mut info) } == 0 {
            if super::last_error() == ERROR_NO_MORE_ITEMS {
                found.sort();
                return Ok(found);
            }
            return Err(enumeration_error("USB SetupDiEnumDeviceInfo"));
        }
        let instance_id = device_instance_id(set.0, &mut info)?;
        let Some(product_id) = candidate_product(&instance_id) else {
            continue;
        };
        let service = property(set.0, &mut info, SPDRP_SERVICE, 1)?;
        let expected_service = if product_id == 0x350a {
            "Rockusb"
        } else {
            "WINUSB"
        };
        if !utf16_string(&service)
            .map_err(|error| invalid(&format!("PnP service: {error}")))?
            .eq_ignore_ascii_case(expected_service)
        {
            continue;
        }
        let driver_key = utf16_string(&property(set.0, &mut info, SPDRP_DRIVER, 1)?)
            .map_err(|error| invalid(&format!("PnP driver key: {error}")))?;
        let location_paths = property(set.0, &mut info, SPDRP_LOCATION_PATHS, 7)?;
        location_from_property(&location_paths)?;
        let address = property(set.0, &mut info, SPDRP_ADDRESS, 4)?;
        if address.len() != 4 {
            return Err(invalid("PnP port property has the wrong length"));
        }
        let port = read_u32(&address, 0)?;
        if !(1..=255).contains(&port) {
            return Err(invalid("PnP port is outside the USB hub range"));
        }
        let hub_path = parent_hub(info.dev_inst, budget)?;
        found.push(Candidate {
            instance_id,
            product_id,
            driver_key,
            location_paths,
            hub_path,
            port,
        });
        if found.len() > MAX_CANDIDATES {
            return Err(invalid(
                "too many Rockchip candidates for one bounded census",
            ));
        }
    }
    Err(invalid("USB PnP census exceeded its node bound"))
}

fn candidate_product(value: &str) -> Option<u16> {
    let upper = value.to_ascii_uppercase();
    [
        ("USB\\VID_2207&PID_350A", 0x350a),
        ("USB\\VID_2207&PID_5000", 0x5000),
    ]
    .into_iter()
    .find_map(|(prefix, product)| {
        upper
            .strip_prefix(prefix)
            .filter(|rest| rest.starts_with('\\') || rest.starts_with('&'))
            .map(|_| product)
    })
}

/// PnP location paths name the physical port and survive PID/serial changes.
/// An instance ID names a personality; it must never stand in for topology.
pub(super) fn device_location(
    set: DeviceInfoSet,
    info: &mut SpDevInfoData,
) -> Result<u32, UsbError> {
    location_from_property(&property(set, info, SPDRP_LOCATION_PATHS, 7)?)
}

fn location_from_property(bytes: &[u8]) -> Result<u32, UsbError> {
    if bytes.len() < 6 || !bytes.len().is_multiple_of(2) || !bytes.ends_with(&[0, 0, 0, 0]) {
        return Err(invalid("missing or unterminated PnP location paths"));
    }
    let units = bytes[..bytes.len() - 4]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect::<Vec<_>>();
    let decoded =
        String::from_utf16(&units).map_err(|_| invalid("invalid UTF-16 PnP location paths"))?;
    let mut paths = decoded
        .split('\0')
        .map(str::to_ascii_uppercase)
        .collect::<Vec<_>>();
    if paths.iter().any(String::is_empty) {
        return Err(invalid("PnP location paths contain an empty path"));
    }
    paths.sort();
    paths.dedup();
    let mut hash = 0x811c_9dc5u32;
    for path in paths {
        for unit in path.encode_utf16().chain(std::iter::once(0)) {
            hash ^= u32::from(unit);
            hash = hash.wrapping_mul(0x0100_0193);
        }
    }
    Ok(hash)
}

fn property(
    set: DeviceInfoSet,
    info: &mut SpDevInfoData,
    name: u32,
    expected_kind: u32,
) -> Result<Vec<u8>, UsbError> {
    let mut kind = 0;
    let mut required = 0;
    let _ = unsafe {
        SetupDiGetDeviceRegistryPropertyW(
            set,
            info,
            name,
            &mut kind,
            ptr::null_mut(),
            0,
            &mut required,
        )
    };
    if super::last_error() != ERROR_INSUFFICIENT_BUFFER
        || required == 0
        || required as usize > MAX_PROPERTY_BYTES
    {
        return Err(invalid("required PnP property is missing or oversized"));
    }
    let mut value = vec![0; required as usize];
    if unsafe {
        SetupDiGetDeviceRegistryPropertyW(
            set,
            info,
            name,
            &mut kind,
            value.as_mut_ptr(),
            value.len() as u32,
            &mut required,
        )
    } == 0
        || kind != expected_kind
        || required as usize > value.len()
    {
        return Err(invalid("PnP property changed or has the wrong type"));
    }
    value.truncate(required as usize);
    Ok(value)
}

fn parent_hub(child: u32, budget: &Budget) -> Result<Vec<u16>, UsbError> {
    budget.check()?;
    let mut parent = 0;
    if unsafe { CM_Get_Parent(&mut parent, child, 0) } != 0 {
        return Err(invalid("USB parent is unavailable"));
    }
    let mut length = 0;
    if unsafe { CM_Get_Device_ID_Size(&mut length, parent, 0) } != 0 || !(1..4096).contains(&length)
    {
        return Err(invalid("USB parent identity is unavailable"));
    }
    let mut id = vec![0u16; length as usize + 1];
    if unsafe { CM_Get_Device_IDW(parent, id.as_mut_ptr(), id.len() as u32, 0) } != 0 {
        return Err(invalid("USB parent identity changed"));
    }
    // Flag zero means CM_GET_DEVICE_INTERFACE_LIST_PRESENT. Limit hotplug
    // retries rather than looping indefinitely while the tree is changing.
    for _ in 0..3 {
        budget.check()?;
        let mut units = 0;
        if unsafe { CM_Get_Device_Interface_List_SizeW(&mut units, &HUB_GUID, id.as_ptr(), 0) } != 0
            || !(2..=32_768).contains(&units)
        {
            return Err(invalid(
                "USB parent does not expose a bounded active hub interface",
            ));
        }
        let mut paths = vec![0u16; units as usize];
        let code = unsafe {
            CM_Get_Device_Interface_ListW(&HUB_GUID, id.as_ptr(), paths.as_mut_ptr(), units, 0)
        };
        if code == 0x1a {
            // CR_BUFFER_SMALL
            continue;
        }
        if code != 0 {
            return Err(invalid("USB parent hub interface is unavailable"));
        }
        let end = paths
            .iter()
            .position(|v| *v == 0)
            .ok_or_else(|| invalid("unterminated hub path"))?;
        if end == 0 || end + 1 == paths.len() || paths[end + 1..].iter().any(|v| *v != 0) {
            return Err(invalid("USB parent hub interface is missing or ambiguous"));
        }
        paths.truncate(end + 1);
        return Ok(paths);
    }
    Err(invalid("USB parent hub interface kept changing"))
}

/// Closed read-only Windows hub query set. Never expose arbitrary IOCTLs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Query {
    Connection,
    DriverKey,
    Descriptor,
}

impl Query {
    fn code(self) -> u32 {
        // CTL_CODE(FILE_DEVICE_USB=0x22, function, METHOD_BUFFERED, FILE_ANY_ACCESS)
        let function = match self {
            Self::Connection => 274,
            Self::DriverKey => 264,
            Self::Descriptor => 260,
        };
        (0x22 << 16) | (function << 2)
    }
}

trait Hub {
    fn query(&mut self, query: Query, bytes: Vec<u8>, budget: &Budget)
    -> Result<Vec<u8>, UsbError>;
}

struct NativeHub(Handle);

impl NativeHub {
    fn open(path: &[u16], budget: &Budget) -> Result<Self, UsbError> {
        budget.check()?;
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null_mut(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(enumeration_error("open exact Windows USB hub"));
        }
        Ok(Self(handle))
    }
}

impl Drop for NativeHub {
    fn drop(&mut self) {
        let _ = unsafe { super::CloseHandle(self.0) };
    }
}

impl Hub for NativeHub {
    fn query(
        &mut self,
        query: Query,
        mut bytes: Vec<u8>,
        budget: &Budget,
    ) -> Result<Vec<u8>, UsbError> {
        budget.check()?;
        let size = u32::try_from(bytes.len()).map_err(|_| invalid("hub query is oversized"))?;
        let mut returned = 0;
        if unsafe {
            DeviceIoControl(
                self.0,
                query.code(),
                bytes.as_mut_ptr().cast(),
                size,
                bytes.as_mut_ptr().cast(),
                size,
                &mut returned,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(enumeration_error("read-only Windows USB hub query"));
        }
        budget.check()?;
        if returned > size {
            return Err(invalid("hub returned an oversized response"));
        }
        bytes.truncate(returned as usize);
        Ok(bytes)
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Connection {
    device: [u8; 18],
    configuration: u8,
    address: u16,
}

fn connection(
    hub: &mut impl Hub,
    port: u32,
    product_id: u16,
    budget: &Budget,
) -> Result<Connection, UsbError> {
    // Allow the hub to include all USB endpoint pipe records (packed 11 bytes
    // each). Identity consumes only the fixed header; no pipe handle is used.
    let mut request = vec![0; CONNECTION_BYTES + 32 * 11];
    request[..4].copy_from_slice(&port.to_le_bytes());
    let bytes = hub.query(Query::Connection, request, budget)?;
    if bytes.len() < CONNECTION_BYTES
        || read_u32(&bytes, 0)? != port
        || read_u32(&bytes, 31)? != 1
        || bytes[24] != 0
    {
        return Err(invalid(
            "the exact USB port is not a connected non-hub device",
        ));
    }
    let device: [u8; 18] = bytes[4..22].try_into().expect("checked connection length");
    if device[0] != 18
        || device[1] != 1
        || device[17] != 1
        || read_u16(&device, 8)? != 0x2207
        || read_u16(&device, 10)? != product_id
        || bytes[22] == 0
    {
        return Err(invalid(
            "device descriptor does not identify the supported configured Rockchip device",
        ));
    }
    Ok(Connection {
        device,
        configuration: bytes[22],
        address: read_u16(&bytes, 25)?,
    })
}

fn driver_key(hub: &mut impl Hub, port: u32, budget: &Budget) -> Result<String, UsbError> {
    let mut request = vec![0; MAX_PROPERTY_BYTES];
    request[..4].copy_from_slice(&port.to_le_bytes());
    let bytes = hub.query(Query::DriverKey, request, budget)?;
    if bytes.len() < 10 || read_u32(&bytes, 0)? != port {
        return Err(invalid("invalid hub driver-key response"));
    }
    let total = read_u32(&bytes, 4)? as usize;
    if total != bytes.len() {
        return Err(invalid("incomplete hub driver-key response"));
    }
    hub_driver_key(&bytes[8..])
}

fn hub_driver_key(bytes: &[u8]) -> Result<String, UsbError> {
    // DriverKeyName is a terminated WCHAR array inside the returned structure.
    // ActualLength can include zero padding after its terminator; those bytes
    // are not part of the identity. Never accept nonzero data after the first
    // terminator, and still compare the entire decoded key with PnP's key.
    let (units, trailing) = bytes.as_chunks::<2>();
    if !trailing.is_empty() {
        return Err(invalid("hub driver key has an incomplete WCHAR"));
    }
    let end = units
        .iter()
        .position(|unit| *unit == [0, 0])
        .ok_or_else(|| invalid("hub driver key is unterminated"))?;
    if end == 0 || units[end..].iter().any(|unit| *unit != [0, 0]) {
        return Err(invalid(
            "hub driver key is empty or contains data after its terminator",
        ));
    }
    decode_utf16(&bytes[..end * 2]).map_err(|error| invalid(&format!("hub driver key: {error}")))
}

fn descriptor(
    hub: &mut impl Hub,
    port: u32,
    kind: u8,
    index: u8,
    language: u16,
    maximum: u16,
    budget: &Budget,
) -> Result<Vec<u8>, UsbError> {
    let mut request = vec![0; DESCRIPTOR_HEADER_BYTES + maximum as usize];
    request[..4].copy_from_slice(&port.to_le_bytes());
    request[4] = 0x80;
    request[5] = 6;
    request[6] = index;
    request[7] = kind;
    request[8..10].copy_from_slice(&language.to_le_bytes());
    request[10..12].copy_from_slice(&maximum.to_le_bytes());
    let bytes = hub.query(Query::Descriptor, request, budget)?;
    if bytes.len() < DESCRIPTOR_HEADER_BYTES + 2
        || read_u32(&bytes, 0)? != port
        || bytes[6] != index
        || bytes[7] != kind
    {
        return Err(invalid("incomplete or mismatched USB descriptor response"));
    }
    Ok(bytes[DESCRIPTOR_HEADER_BYTES..].to_vec())
}

fn observe_candidate(
    hub: &mut impl Hub,
    candidate: &Candidate,
    budget: &Budget,
) -> Result<Option<UsbInterfaceDescriptor>, UsbError> {
    let before = connection(hub, candidate.port, candidate.product_id, budget)?;
    if !driver_key(hub, candidate.port, budget)?.eq_ignore_ascii_case(&candidate.driver_key) {
        return Err(invalid("hub port and PnP driver key do not match"));
    }
    let device = &before.device;
    let (config, strings) = identity_descriptors(hub, candidate.port, device, budget)?;
    let interface = config_interface(&config, before.configuration)?;
    // Device/address snapshots alone cannot detect a changed serial or active
    // configuration on an otherwise identical device. Read these bytes again.
    if (config, strings.clone()) != identity_descriptors(hub, candidate.port, device, budget)? {
        return Err(invalid(
            "USB identity descriptors changed during observation",
        ));
    }
    let after = connection(hub, candidate.port, candidate.product_id, budget)?;
    if before != after
        || !driver_key(hub, candidate.port, budget)?.eq_ignore_ascii_case(&candidate.driver_key)
    {
        return Err(invalid(
            "device or driver binding changed while reading descriptors",
        ));
    }
    let [vendor_name, product_name, serial] = strings;
    let record = UsbInterfaceDescriptor {
        vendor_id: read_u16(device, 8)?,
        product_id: read_u16(device, 10)?,
        usb_specification: read_u16(device, 2)?,
        device_release: read_u16(device, 12)?,
        location_id: location_from_property(&candidate.location_paths)?,
        interface_number: interface[0],
        interface_class: interface[1],
        interface_subclass: interface[2],
        interface_protocol: interface[3],
        serial,
        product_name,
        vendor_name,
    };
    // The discovery Profile maps this PID to Loader. Never emit a Maskrom
    // observation merely because Rockchip shares that VID/PID between modes.
    Ok(
        (candidate.product_id == 0x5000 || UsbInterfaceSelector::dayu200_loader().matches(&record))
            .then_some(record),
    )
}

type IdentityDescriptors = (Vec<u8>, [Option<String>; 3]);

fn identity_descriptors(
    hub: &mut impl Hub,
    port: u32,
    device: &[u8; 18],
    budget: &Budget,
) -> Result<IdentityDescriptors, UsbError> {
    let config = descriptor(hub, port, 2, 0, 0, u16::MAX, budget)?;
    let strings = if device[14..17].iter().any(|value| *value != 0) {
        let languages = descriptor(hub, port, 3, 0, 0, 255, budget)?;
        let language = string_language(&languages)?;
        let mut result = [None, None, None];
        for (out, index) in result.iter_mut().zip(&device[14..17]) {
            if *index != 0 {
                *out = Some(
                    usb_string(&descriptor(hub, port, 3, *index, language, 255, budget)?)
                        .map_err(|error| invalid(&format!("USB string index {index}: {error}")))?,
                );
            }
        }
        result
    } else {
        [None, None, None]
    };
    Ok((config, strings))
}

fn config_interface(bytes: &[u8], active: u8) -> Result<[u8; 4], UsbError> {
    if bytes.len() < 9
        || bytes[0] != 9
        || bytes[1] != 2
        || read_u16(bytes, 2)? as usize != bytes.len()
        || bytes[4] != 1
        || bytes[5] != active
    {
        return Err(invalid(
            "unsupported or incomplete active USB configuration",
        ));
    }
    let mut offset = 9;
    let mut interface = None;
    let mut endpoints = Vec::new();
    while offset < bytes.len() {
        let length = *bytes
            .get(offset)
            .ok_or_else(|| invalid("truncated USB descriptor"))? as usize;
        if length < 2 || length > bytes.len() - offset {
            return Err(invalid("USB descriptor has an invalid length"));
        }
        let item = &bytes[offset..offset + length];
        match item[1] {
            4 => {
                if length != 9 || interface.is_some() || item[3] != 0 || item[4] != 2 {
                    return Err(invalid(
                        "USB interface is incomplete, alternate or ambiguous",
                    ));
                }
                interface = Some([item[2], item[5], item[6], item[7]]);
            }
            5 => {
                if length < 7
                    || interface.is_none()
                    || item[3] & 3 != 2
                    || item[2] & 0x0f == 0
                    || item[2] & 0x70 != 0
                    || read_u16(item, 4)? == 0
                    || endpoints.contains(&item[2])
                {
                    return Err(invalid("USB bulk endpoint is malformed or duplicated"));
                }
                endpoints.push(item[2]);
            }
            2 => return Err(invalid("nested USB configuration descriptor")),
            _ => {}
        }
        offset += length;
    }
    if endpoints.len() != 2 || endpoints.iter().filter(|id| **id & 0x80 != 0).count() != 1 {
        return Err(invalid(
            "USB interface does not have one bulk pipe in each direction",
        ));
    }
    interface.ok_or_else(|| invalid("USB configuration has no interface"))
}

fn string_language(bytes: &[u8]) -> Result<u16, UsbError> {
    if bytes.len() < 4
        || bytes[0] as usize != bytes.len()
        || bytes[1] != 3
        || !bytes.len().is_multiple_of(2)
    {
        return Err(invalid("invalid USB language descriptor"));
    }
    let language = read_u16(bytes, 2)?;
    if language == 0 {
        return Err(invalid("USB language is zero"));
    }
    Ok(language)
}

fn usb_string(bytes: &[u8]) -> Result<String, UsbError> {
    if bytes.len() < 4
        || bytes[0] as usize != bytes.len()
        || bytes[1] != 3
        || !bytes.len().is_multiple_of(2)
    {
        return Err(invalid("invalid USB string descriptor"));
    }
    decode_utf16(&bytes[2..])
}

fn utf16_string(bytes: &[u8]) -> Result<String, UsbError> {
    if bytes.len() < 4 || !bytes.len().is_multiple_of(2) || bytes[bytes.len() - 2..] != [0, 0] {
        return Err(invalid("unterminated UTF-16 property"));
    }
    decode_utf16(&bytes[..bytes.len() - 2])
}

fn decode_utf16(bytes: &[u8]) -> Result<String, UsbError> {
    let (pairs, trailing) = bytes.as_chunks::<2>();
    if !trailing.is_empty() {
        return Err(invalid("incomplete UTF-16 code unit"));
    }
    let units = pairs
        .iter()
        .map(|v| u16::from_le_bytes(*v))
        .collect::<Vec<_>>();
    if units.contains(&0) {
        return Err(invalid("embedded null in USB identity"));
    }
    String::from_utf16(&units).map_err(|_| invalid("invalid UTF-16 USB identity"))
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, UsbError> {
    let part = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| invalid("truncated 16-bit field"))?;
    Ok(u16::from_le_bytes([part[0], part[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, UsbError> {
    let part = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| invalid("truncated 32-bit field"))?;
    Ok(u32::from_le_bytes([part[0], part[1], part[2], part[3]]))
}

#[cfg(test)]
#[path = "windows_vendor_tests.rs"]
mod tests;
