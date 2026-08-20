//! macOS IOUSBLib binding.
//!
//! This is intentionally the only file in the workspace that names raw IOKit
//! interfaces.  The vtable prefix is the SDK's `IOUSBInterfaceInterface182`;
//! that is the first interface revision with bounded synchronous pipe I/O.

use super::{BulkInterface, UsbError, UsbInterfaceDescriptor, UsbInterfaceSelector};
use core::ffi::{c_char, c_void};
use std::ffi::{CStr, CString};
use std::ptr;

type IoObject = u32;
type IoIterator = IoObject;
type IoService = IoObject;
type IoReturn = i32;
type CfTypeRef = *const c_void;
type CfStringRef = *const c_void;
type CfUuidRef = *const c_void;

const IO_SUCCESS: IoReturn = 0;
const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const USB_DIRECTION_OUT: u8 = 0;
const USB_DIRECTION_IN: u8 = 1;
const USB_TRANSFER_BULK: u8 = 2;

// IOCFPlugIn.h: C244E858-109C-11D4-91D4-0050E4C6426F.
const IO_CF_PLUGIN_INTERFACE_ID: CfUuidBytes = CfUuidBytes::new([
    0xc2, 0x44, 0xe8, 0x58, 0x10, 0x9c, 0x11, 0xd4, 0x91, 0xd4, 0x00, 0x50, 0xe4, 0xc6, 0x42, 0x6f,
]);
// IOUSBLib.h: 2D9786C6-9EF3-11D4-AD51-000A27052861.
const IO_USB_INTERFACE_USER_CLIENT_TYPE_ID: CfUuidBytes = CfUuidBytes::new([
    0x2d, 0x97, 0x86, 0xc6, 0x9e, 0xf3, 0x11, 0xd4, 0xad, 0x51, 0x00, 0x0a, 0x27, 0x05, 0x28, 0x61,
]);
// IOUSBLib.h: IOUSBInterfaceInterface182.
const IO_USB_INTERFACE_ID_182: CfUuidBytes = CfUuidBytes::new([
    0x49, 0x23, 0xac, 0x4c, 0x48, 0x96, 0x11, 0xd5, 0x92, 0x08, 0x00, 0x0a, 0x27, 0x80, 0x1e, 0x86,
]);

#[repr(C)]
#[derive(Clone, Copy)]
struct CfUuidBytes {
    bytes: [u8; 16],
}

impl CfUuidBytes {
    const fn new(bytes: [u8; 16]) -> Self {
        Self { bytes }
    }
}

type QueryInterface = unsafe extern "C" fn(*mut c_void, CfUuidBytes, *mut *mut c_void) -> i32;
type AddRef = unsafe extern "C" fn(*mut c_void) -> u32;
type Release = unsafe extern "C" fn(*mut c_void) -> u32;
type InterfaceOpenClose = unsafe extern "C" fn(*mut c_void) -> IoReturn;
type GetPipeProperties =
    unsafe extern "C" fn(*mut c_void, u8, *mut u8, *mut u8, *mut u8, *mut u16, *mut u8) -> IoReturn;
type ReadPipeTo =
    unsafe extern "C" fn(*mut c_void, u8, *mut c_void, *mut u32, u32, u32) -> IoReturn;
type WritePipeTo = unsafe extern "C" fn(*mut c_void, u8, *mut c_void, u32, u32, u32) -> IoReturn;

/// Prefix of `IOCFPlugInInterfaceStruct` through IUnknown.  Only these fields
/// are touched before the plug-in is released.
#[repr(C)]
struct IoCfPlugInInterface {
    reserved: *mut c_void,
    query_interface: QueryInterface,
    add_ref: AddRef,
    release: Release,
}

/// SDK layout of `IOUSBInterfaceInterface182` through `WritePipeTO`.
/// Unused function slots are pointers solely to preserve their ABI offsets.
#[repr(C)]
struct IoUsbInterface182 {
    reserved: *mut c_void,
    query_interface: QueryInterface,
    add_ref: AddRef,
    release: Release,
    create_async_event_source: *const c_void,
    get_async_event_source: *const c_void,
    create_async_port: *const c_void,
    get_async_port: *const c_void,
    usb_interface_open: InterfaceOpenClose,
    usb_interface_close: InterfaceOpenClose,
    get_interface_class: *const c_void,
    get_interface_subclass: *const c_void,
    get_interface_protocol: *const c_void,
    get_device_vendor: *const c_void,
    get_device_product: *const c_void,
    get_device_release: *const c_void,
    get_configuration_value: *const c_void,
    get_interface_number: *const c_void,
    get_alternate_setting: *const c_void,
    get_num_endpoints: unsafe extern "C" fn(*mut c_void, *mut u8) -> IoReturn,
    get_location_id: *const c_void,
    get_device: *const c_void,
    set_alternate_interface: *const c_void,
    get_bus_frame_number: *const c_void,
    control_request: *const c_void,
    control_request_async: *const c_void,
    get_pipe_properties: GetPipeProperties,
    get_pipe_status: *const c_void,
    abort_pipe: *const c_void,
    reset_pipe: *const c_void,
    clear_pipe_stall: *const c_void,
    read_pipe: *const c_void,
    write_pipe: *const c_void,
    read_pipe_async: *const c_void,
    write_pipe_async: *const c_void,
    read_isoch_pipe_async: *const c_void,
    write_isoch_pipe_async: *const c_void,
    control_request_to: *const c_void,
    control_request_async_to: *const c_void,
    read_pipe_to: ReadPipeTo,
    write_pipe_to: WritePipeTo,
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingServices(
        main_port: u32,
        matching: *mut c_void,
        existing: *mut IoIterator,
    ) -> IoReturn;
    fn IOIteratorNext(iterator: IoIterator) -> IoObject;
    fn IOObjectRelease(object: IoObject) -> IoReturn;
    fn IOCreatePlugInInterfaceForService(
        service: IoService,
        plugin_type: CfUuidRef,
        interface_type: CfUuidRef,
        interface: *mut *mut *mut IoCfPlugInInterface,
        score: *mut i32,
    ) -> IoReturn;
    fn IORegistryEntryCreateCFProperty(
        entry: IoService,
        key: CfStringRef,
        allocator: *const c_void,
        options: u32,
    ) -> CfTypeRef;
    fn IORegistryEntryGetParentEntry(
        entry: IoService,
        plane: *const c_char,
        parent: *mut IoService,
    ) -> IoReturn;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFUUIDGetConstantUUIDWithBytes(
        allocator: *const c_void,
        byte0: u8,
        byte1: u8,
        byte2: u8,
        byte3: u8,
        byte4: u8,
        byte5: u8,
        byte6: u8,
        byte7: u8,
        byte8: u8,
        byte9: u8,
        byte10: u8,
        byte11: u8,
        byte12: u8,
        byte13: u8,
        byte14: u8,
        byte15: u8,
    ) -> CfUuidRef;
    fn CFStringCreateWithCString(
        allocator: *const c_void,
        c_string: *const c_char,
        encoding: u32,
    ) -> CfStringRef;
    fn CFStringGetTypeID() -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFGetTypeID(value: CfTypeRef) -> usize;
    fn CFStringGetCString(
        string: CfStringRef,
        buffer: *mut c_char,
        buffer_size: isize,
        encoding: u32,
    ) -> bool;
    fn CFNumberGetValue(number: CfTypeRef, number_type: i32, value: *mut c_void) -> bool;
    fn CFRelease(value: CfTypeRef);
}

pub fn enumerate() -> Result<Vec<UsbInterfaceDescriptor>, UsbError> {
    let services = collect_services()?;
    Ok(services
        .into_iter()
        .map(|service| service.descriptor)
        .collect())
}

pub fn open_unique(
    selector: UsbInterfaceSelector,
    timeout_ms: u32,
) -> Result<Box<dyn BulkInterface>, UsbError> {
    let mut matches: Vec<ServiceRecord> = collect_services()?
        .into_iter()
        .filter(|service| selector.matches(&service.descriptor))
        .collect();
    match matches.len() {
        0 => Err(UsbError::NoMatchingInterface(selector)),
        1 => {
            let service = matches.pop().expect("one matched interface");
            let handle = InterfaceHandle::from_service(service.service.0, service.descriptor)?;
            Ok(Box::new(handle.open(timeout_ms)?))
        }
        _ => Err(UsbError::AmbiguousInterfaces(
            matches
                .iter()
                .map(|service| service.descriptor.clone())
                .collect(),
        )),
    }
}

pub fn open_exact(
    selector: UsbInterfaceSelector,
    expected: &UsbInterfaceDescriptor,
    timeout_ms: u32,
) -> Result<Box<dyn BulkInterface>, UsbError> {
    let mut matches: Vec<ServiceRecord> = collect_services()?
        .into_iter()
        .filter(|service| selector.matches(&service.descriptor) && service.descriptor == *expected)
        .collect();
    match matches.len() {
        0 => Err(UsbError::NoExactInterface(expected.clone())),
        1 => {
            let service = matches.pop().expect("one exact interface");
            let handle = InterfaceHandle::from_service(service.service.0, service.descriptor)?;
            Ok(Box::new(handle.open(timeout_ms)?))
        }
        _ => Err(UsbError::AmbiguousInterfaces(
            matches
                .iter()
                .map(|service| service.descriptor.clone())
                .collect(),
        )),
    }
}

fn collect_services() -> Result<Vec<ServiceRecord>, UsbError> {
    let class_name = CString::new("IOUSBHostInterface").expect("literal has no NUL");
    // IOServiceMatching returns a consumed dictionary.  Passing main port 0 is
    // kIOMainPortDefault on supported macOS releases.
    let matching = unsafe { IOServiceMatching(class_name.as_ptr()) };
    if matching.is_null() {
        return Err(UsbError::Enumeration(
            "IOServiceMatching(IOUSBHostInterface) returned null".into(),
        ));
    }
    let mut iterator = 0;
    let status = unsafe { IOServiceGetMatchingServices(0, matching, &mut iterator) };
    if status != IO_SUCCESS {
        return Err(UsbError::Enumeration(status_message(
            status,
            "IOServiceGetMatchingServices",
        )));
    }
    let iterator = IoObjectGuard(iterator);
    let mut services = Vec::new();
    loop {
        let service = unsafe { IOIteratorNext(iterator.0) };
        if service == 0 {
            break;
        }
        let service = IoObjectGuard(service);
        match registry_descriptor(service.0) {
            Ok(descriptor) => services.push(ServiceRecord {
                service,
                descriptor,
            }),
            // Ignore malformed unrelated interface nodes. Exact matching is
            // performed only after registry observation, and any exact node
            // that lacks required fields therefore cannot be claimed.
            Err(_) => continue,
        }
    }
    Ok(services)
}

#[derive(Debug)]
struct ServiceRecord {
    service: IoObjectGuard,
    descriptor: UsbInterfaceDescriptor,
}

#[derive(Debug)]
struct IoObjectGuard(IoObject);

impl Drop for IoObjectGuard {
    fn drop(&mut self) {
        if self.0 != 0 {
            let _ = unsafe { IOObjectRelease(self.0) };
        }
    }
}

struct InterfaceHandle {
    raw: *mut *mut IoUsbInterface182,
    descriptor: UsbInterfaceDescriptor,
}

impl core::fmt::Debug for InterfaceHandle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("InterfaceHandle")
            .field("descriptor", &self.descriptor)
            .finish_non_exhaustive()
    }
}

impl InterfaceHandle {
    fn from_service(
        service: IoService,
        descriptor: UsbInterfaceDescriptor,
    ) -> Result<Self, UsbError> {
        let plugin_type = uuid_ref(IO_USB_INTERFACE_USER_CLIENT_TYPE_ID);
        let plugin_interface = uuid_ref(IO_CF_PLUGIN_INTERFACE_ID);
        let mut plugin: *mut *mut IoCfPlugInInterface = ptr::null_mut();
        let mut score = 0i32;
        let status = unsafe {
            IOCreatePlugInInterfaceForService(
                service,
                plugin_type,
                plugin_interface,
                &mut plugin,
                &mut score,
            )
        };
        if status != IO_SUCCESS {
            return Err(UsbError::Claim(status_message(
                status,
                "IOCreatePlugInInterfaceForService",
            )));
        }
        if plugin.is_null() {
            return Err(UsbError::Claim(
                "IOCreatePlugInInterfaceForService returned a null plug-in".into(),
            ));
        }

        let mut raw_interface: *mut c_void = ptr::null_mut();
        let query = unsafe { (**plugin).query_interface };
        let query_status = unsafe {
            query(
                plugin.cast::<c_void>(),
                IO_USB_INTERFACE_ID_182,
                &mut raw_interface,
            )
        };
        let release_plugin = unsafe { (**plugin).release };
        let _ = unsafe { release_plugin(plugin.cast::<c_void>()) };
        if query_status != 0 || raw_interface.is_null() {
            return Err(UsbError::Claim(format!(
                "QueryInterface(IOUSBInterfaceInterface182) returned 0x{:08x}",
                query_status as u32
            )));
        }

        let raw = raw_interface.cast::<*mut IoUsbInterface182>();
        Ok(Self { raw, descriptor })
    }

    fn open(self, timeout_ms: u32) -> Result<MacOsBulkInterface, UsbError> {
        let status = unsafe {
            let table = &**self.raw;
            (table.usb_interface_open)(self.raw.cast::<c_void>())
        };
        if status != IO_SUCCESS {
            return Err(UsbError::Claim(status_message(status, "USBInterfaceOpen")));
        }

        let mut count = 0u8;
        let status = unsafe {
            let table = &**self.raw;
            (table.get_num_endpoints)(self.raw.cast::<c_void>(), &mut count)
        };
        if status != IO_SUCCESS {
            close_interface(self.raw);
            return Err(UsbError::Claim(status_message(status, "GetNumEndpoints")));
        }

        let mut bulk_in = None;
        let mut bulk_out = None;
        for pipe in 1..=count {
            let mut direction = 0u8;
            let mut number = 0u8;
            let mut transfer_type = 0u8;
            let mut max_packet_size = 0u16;
            let mut interval = 0u8;
            let status = unsafe {
                let table = &**self.raw;
                (table.get_pipe_properties)(
                    self.raw.cast::<c_void>(),
                    pipe,
                    &mut direction,
                    &mut number,
                    &mut transfer_type,
                    &mut max_packet_size,
                    &mut interval,
                )
            };
            if status != IO_SUCCESS || transfer_type != USB_TRANSFER_BULK {
                continue;
            }
            match direction {
                USB_DIRECTION_IN if bulk_in.is_none() => bulk_in = Some(pipe),
                USB_DIRECTION_OUT if bulk_out.is_none() => bulk_out = Some(pipe),
                _ => {}
            }
        }

        let Some(bulk_in) = bulk_in else {
            close_interface(self.raw);
            return Err(UsbError::MissingBulkPipe { direction: "IN" });
        };
        let Some(bulk_out) = bulk_out else {
            close_interface(self.raw);
            return Err(UsbError::MissingBulkPipe { direction: "OUT" });
        };
        Ok(MacOsBulkInterface {
            handle: Some(self),
            bulk_in,
            bulk_out,
            timeout_ms,
        })
    }
}

impl Drop for InterfaceHandle {
    fn drop(&mut self) {
        release_interface(self.raw);
    }
}

struct MacOsBulkInterface {
    // Option lets Drop close before InterfaceHandle releases the COM object.
    handle: Option<InterfaceHandle>,
    bulk_in: u8,
    bulk_out: u8,
    timeout_ms: u32,
}

impl core::fmt::Debug for MacOsBulkInterface {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MacOsBulkInterface")
            .field(
                "descriptor",
                &self.handle.as_ref().map(|handle| &handle.descriptor),
            )
            .field("bulk_in", &self.bulk_in)
            .field("bulk_out", &self.bulk_out)
            .field("timeout_ms", &self.timeout_ms)
            .finish()
    }
}

impl BulkInterface for MacOsBulkInterface {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), UsbError> {
        let size =
            u32::try_from(bytes.len()).map_err(|_| UsbError::TransferTooLarge(bytes.len()))?;
        let handle = self.handle.as_ref().expect("handle exists until Drop");
        let status = unsafe {
            let table = &**handle.raw;
            (table.write_pipe_to)(
                handle.raw.cast::<c_void>(),
                self.bulk_out,
                bytes.as_ptr().cast_mut().cast::<c_void>(),
                size,
                self.timeout_ms,
                self.timeout_ms,
            )
        };
        check(status, "WritePipeTO")
    }

    fn read_some(&mut self, bytes: &mut [u8]) -> Result<usize, UsbError> {
        let mut size =
            u32::try_from(bytes.len()).map_err(|_| UsbError::TransferTooLarge(bytes.len()))?;
        let handle = self.handle.as_ref().expect("handle exists until Drop");
        let status = unsafe {
            let table = &**handle.raw;
            (table.read_pipe_to)(
                handle.raw.cast::<c_void>(),
                self.bulk_in,
                bytes.as_mut_ptr().cast::<c_void>(),
                &mut size,
                self.timeout_ms,
                self.timeout_ms,
            )
        };
        check(status, "ReadPipeTO")?;
        Ok(size as usize)
    }

    fn read_exact(&mut self, bytes: &mut [u8]) -> Result<(), UsbError> {
        let expected = bytes.len();
        let actual = self.read_some(bytes)?;
        if actual != expected {
            return Err(UsbError::ShortTransfer { expected, actual });
        }
        Ok(())
    }

    fn descriptor(&self) -> &UsbInterfaceDescriptor {
        &self
            .handle
            .as_ref()
            .expect("handle exists until Drop")
            .descriptor
    }
}

impl Drop for MacOsBulkInterface {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            close_interface(handle.raw);
            drop(handle);
        }
    }
}

fn registry_descriptor(service: IoService) -> Result<UsbInterfaceDescriptor, UsbError> {
    let parent = parent_service(service);
    let field = |name| {
        number_property(service, name)
            .or_else(|| {
                parent
                    .as_ref()
                    .and_then(|device| number_property(device.0, name))
            })
            .ok_or_else(|| {
                UsbError::Enumeration(format!(
                    "IOUSBHostInterface and parent are missing numeric property {name}"
                ))
            })
    };
    let text = |name| {
        string_property(service, name).or_else(|| {
            parent
                .as_ref()
                .and_then(|device| string_property(device.0, name))
        })
    };
    Ok(UsbInterfaceDescriptor {
        vendor_id: u16::try_from(field("idVendor")?)
            .map_err(|_| UsbError::Enumeration("idVendor does not fit a USB descriptor".into()))?,
        product_id: u16::try_from(field("idProduct")?)
            .map_err(|_| UsbError::Enumeration("idProduct does not fit a USB descriptor".into()))?,
        usb_specification: u16::try_from(field("bcdUSB")?)
            .map_err(|_| UsbError::Enumeration("bcdUSB does not fit a USB descriptor".into()))?,
        device_release: u16::try_from(field("bcdDevice")?)
            .map_err(|_| UsbError::Enumeration("bcdDevice does not fit a USB descriptor".into()))?,
        location_id: field("locationID")?,
        interface_class: u8::try_from(field("bInterfaceClass")?).map_err(|_| {
            UsbError::Enumeration("bInterfaceClass does not fit a USB descriptor".into())
        })?,
        interface_subclass: u8::try_from(field("bInterfaceSubClass")?).map_err(|_| {
            UsbError::Enumeration("bInterfaceSubClass does not fit a USB descriptor".into())
        })?,
        interface_protocol: u8::try_from(field("bInterfaceProtocol")?).map_err(|_| {
            UsbError::Enumeration("bInterfaceProtocol does not fit a USB descriptor".into())
        })?,
        interface_number: u8::try_from(field("bInterfaceNumber")?).map_err(|_| {
            UsbError::Enumeration("bInterfaceNumber does not fit a USB descriptor".into())
        })?,
        // IOUSBHost normally mirrors device descriptor strings onto the
        // interface node, but that is not an ABI guarantee. Loader firmware
        // variants may expose them only on the parent IOUSBHostDevice.
        serial: text("USB Serial Number"),
        product_name: text("USB Product Name"),
        vendor_name: text("USB Vendor Name"),
    })
}

fn parent_service(service: IoService) -> Option<IoObjectGuard> {
    let plane = CString::new("IOService").expect("literal has no NUL");
    let mut parent = 0;
    let status = unsafe { IORegistryEntryGetParentEntry(service, plane.as_ptr(), &mut parent) };
    (status == IO_SUCCESS && parent != 0).then_some(IoObjectGuard(parent))
}

fn number_property(service: IoService, name: &str) -> Option<u32> {
    let name = CString::new(name).ok()?;
    let key =
        unsafe { CFStringCreateWithCString(ptr::null(), name.as_ptr(), CF_STRING_ENCODING_UTF8) };
    if key.is_null() {
        return None;
    }
    let value = unsafe { IORegistryEntryCreateCFProperty(service, key, ptr::null(), 0) };
    unsafe { CFRelease(key) };
    if value.is_null() {
        return None;
    }
    let is_number = unsafe { CFGetTypeID(value) == CFNumberGetTypeID() };
    if !is_number {
        unsafe { CFRelease(value) };
        return None;
    }
    let mut parsed = 0i64;
    // kCFNumberSInt64Type = 4. Asking CF to widen here avoids depending on
    // the concrete number width used by one macOS registry producer.
    let copied = unsafe { CFNumberGetValue(value, 4, (&mut parsed as *mut i64).cast()) };
    unsafe { CFRelease(value) };
    if !copied || parsed < 0 {
        return None;
    }
    u32::try_from(parsed).ok()
}

fn string_property(service: IoService, name: &str) -> Option<String> {
    let name = CString::new(name).ok()?;
    let key =
        unsafe { CFStringCreateWithCString(ptr::null(), name.as_ptr(), CF_STRING_ENCODING_UTF8) };
    if key.is_null() {
        return None;
    }
    let value = unsafe { IORegistryEntryCreateCFProperty(service, key, ptr::null(), 0) };
    unsafe { CFRelease(key) };
    if value.is_null() {
        return None;
    }
    let is_string = unsafe { CFGetTypeID(value) == CFStringGetTypeID() };
    if !is_string {
        unsafe { CFRelease(value) };
        return None;
    }
    let mut buffer = [0i8; 1024];
    let copied = unsafe {
        CFStringGetCString(
            value,
            buffer.as_mut_ptr(),
            buffer.len() as isize,
            CF_STRING_ENCODING_UTF8,
        )
    };
    unsafe { CFRelease(value) };
    if !copied {
        return None;
    }
    Some(
        unsafe { CStr::from_ptr(buffer.as_ptr()) }
            .to_string_lossy()
            .into_owned(),
    )
}

fn uuid_ref(bytes: CfUuidBytes) -> CfUuidRef {
    let b = bytes.bytes;
    unsafe {
        CFUUIDGetConstantUUIDWithBytes(
            ptr::null(),
            b[0],
            b[1],
            b[2],
            b[3],
            b[4],
            b[5],
            b[6],
            b[7],
            b[8],
            b[9],
            b[10],
            b[11],
            b[12],
            b[13],
            b[14],
            b[15],
        )
    }
}

fn close_interface(raw: *mut *mut IoUsbInterface182) {
    if raw.is_null() {
        return;
    }
    let close = unsafe { (**raw).usb_interface_close };
    let _ = unsafe { close(raw.cast::<c_void>()) };
}

fn release_interface(raw: *mut *mut IoUsbInterface182) {
    if raw.is_null() {
        return;
    }
    let release = unsafe { (**raw).release };
    let _ = unsafe { release(raw.cast::<c_void>()) };
}

fn check(status: IoReturn, operation: &str) -> Result<(), UsbError> {
    if status == IO_SUCCESS {
        Ok(())
    } else {
        Err(UsbError::Transfer(status_message(status, operation)))
    }
}

fn status_message(status: IoReturn, operation: &str) -> String {
    format!("{operation} returned 0x{:08x}", status as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iokit_enumeration_returns_descriptor_shaped_facts() {
        // Host contents are not an acceptance fixture.  If USB interfaces are
        // present, every returned record must still carry descriptor-shaped
        // facts obtained from its IOUSBHostInterface registry node.
        for record in enumerate().expect("IOKit enumeration works") {
            assert_ne!(record.vendor_id, 0);
            assert_ne!(record.location_id, 0, "USB topology is absent: {record:?}");
            if record.vendor_id == 0x2207 && matches!(record.product_id, 0x350a | 0x5000) {
                assert!(
                    matches!(record.serial.as_deref(), Some(value) if !value.is_empty()),
                    "known DAYU200 modes carry the serial used by NRU-AC-1: {record:?}"
                );
            }
        }
    }
}
