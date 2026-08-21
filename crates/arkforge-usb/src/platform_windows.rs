//! Windows WinUSB binding for the ArkForge RockUSB interface.
//!
//! The companion INF registers `ARKFORGE_ROCKUSB_INTERFACE_GUID`; SetupAPI
//! enumerates only that class, then WinUSB descriptors are checked against the
//! same exact selector used by macOS. No broad VID-only handle is claimed.

use super::{BulkInterface, UsbError, UsbInterfaceDescriptor, UsbInterfaceSelector};
use core::ffi::c_void;
use std::ptr;

type Handle = *mut c_void;
type DeviceInfoSet = Handle;
type WinUsbHandle = Handle;
type Bool = i32;
type Dword = u32;

const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;
const ERROR_NO_MORE_ITEMS: Dword = 259;
const ERROR_INSUFFICIENT_BUFFER: Dword = 122;
const DIGCF_PRESENT: Dword = 0x0000_0002;
const DIGCF_DEVICEINTERFACE: Dword = 0x0000_0010;
const GENERIC_READ: Dword = 0x8000_0000;
const GENERIC_WRITE: Dword = 0x4000_0000;
const FILE_SHARE_READ: Dword = 0x0000_0001;
const FILE_SHARE_WRITE: Dword = 0x0000_0002;
const OPEN_EXISTING: Dword = 3;
const FILE_ATTRIBUTE_NORMAL: Dword = 0x0000_0080;
const FILE_FLAG_OVERLAPPED: Dword = 0x4000_0000;
const USB_DEVICE_DESCRIPTOR_TYPE: u8 = 1;
const USB_STRING_DESCRIPTOR_TYPE: u8 = 3;
const USBD_PIPE_TYPE_BULK: Dword = 2;
const PIPE_TRANSFER_TIMEOUT: Dword = 3;

/// `{6A4E21F0-50A4-4D7A-B71B-9E945B3F6B7B}`; also declared by the INF.
const ARKFORGE_ROCKUSB_INTERFACE_GUID: Guid = Guid {
    data1: 0x6a4e_21f0,
    data2: 0x50a4,
    data3: 0x4d7a,
    data4: [0xb7, 0x1b, 0x9e, 0x94, 0x5b, 0x3f, 0x6b, 0x7b],
};

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[repr(C)]
struct SpDeviceInterfaceData {
    size: Dword,
    interface_class_guid: Guid,
    flags: Dword,
    reserved: usize,
}

impl Default for SpDeviceInterfaceData {
    fn default() -> Self {
        Self {
            size: std::mem::size_of::<Self>() as Dword,
            interface_class_guid: Guid::default(),
            flags: 0,
            reserved: 0,
        }
    }
}

#[repr(C)]
struct SpDevInfoData {
    size: Dword,
    class_guid: Guid,
    dev_inst: Dword,
    reserved: usize,
}

impl Default for SpDevInfoData {
    fn default() -> Self {
        Self {
            size: std::mem::size_of::<Self>() as Dword,
            class_guid: Guid::default(),
            dev_inst: 0,
            reserved: 0,
        }
    }
}

#[repr(C)]
struct SpDeviceInterfaceDetailDataW {
    size: Dword,
    device_path: [u16; 1],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct UsbDeviceDescriptor {
    length: u8,
    descriptor_type: u8,
    usb_specification: u16,
    device_class: u8,
    device_subclass: u8,
    device_protocol: u8,
    max_packet_size_0: u8,
    vendor_id: u16,
    product_id: u16,
    device_release: u16,
    manufacturer_index: u8,
    product_index: u8,
    serial_index: u8,
    num_configurations: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct WinUsbInterfaceDescriptor {
    length: u8,
    descriptor_type: u8,
    interface_number: u8,
    alternate_setting: u8,
    num_endpoints: u8,
    interface_class: u8,
    interface_subclass: u8,
    interface_protocol: u8,
    interface_index: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct WinUsbPipeInformation {
    pipe_type: Dword,
    pipe_id: u8,
    maximum_packet_size: u16,
    interval: u8,
}

#[link(name = "setupapi")]
unsafe extern "system" {
    fn SetupDiGetClassDevsW(
        class_guid: *const Guid,
        enumerator: *const u16,
        parent: Handle,
        flags: Dword,
    ) -> DeviceInfoSet;
    fn SetupDiEnumDeviceInterfaces(
        device_info_set: DeviceInfoSet,
        device_info_data: *mut SpDevInfoData,
        interface_class_guid: *const Guid,
        member_index: Dword,
        device_interface_data: *mut SpDeviceInterfaceData,
    ) -> Bool;
    fn SetupDiGetDeviceInterfaceDetailW(
        device_info_set: DeviceInfoSet,
        device_interface_data: *mut SpDeviceInterfaceData,
        detail: *mut SpDeviceInterfaceDetailDataW,
        detail_size: Dword,
        required_size: *mut Dword,
        device_info_data: *mut SpDevInfoData,
    ) -> Bool;
    fn SetupDiGetDeviceInstanceIdW(
        device_info_set: DeviceInfoSet,
        device_info_data: *mut SpDevInfoData,
        instance_id: *mut u16,
        instance_id_size: Dword,
        required_size: *mut Dword,
    ) -> Bool;
    fn SetupDiDestroyDeviceInfoList(device_info_set: DeviceInfoSet) -> Bool;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(
        file_name: *const u16,
        desired_access: Dword,
        share_mode: Dword,
        security_attributes: *mut c_void,
        creation_disposition: Dword,
        flags_and_attributes: Dword,
        template: Handle,
    ) -> Handle;
    fn CloseHandle(handle: Handle) -> Bool;
    fn GetLastError() -> Dword;
}

#[link(name = "winusb")]
unsafe extern "system" {
    fn WinUsb_Initialize(device: Handle, interface_handle: *mut WinUsbHandle) -> Bool;
    fn WinUsb_Free(interface_handle: WinUsbHandle) -> Bool;
    fn WinUsb_QueryInterfaceSettings(
        interface_handle: WinUsbHandle,
        alternate_setting: u8,
        descriptor: *mut WinUsbInterfaceDescriptor,
    ) -> Bool;
    fn WinUsb_QueryPipe(
        interface_handle: WinUsbHandle,
        alternate_setting: u8,
        pipe_index: u8,
        pipe_information: *mut WinUsbPipeInformation,
    ) -> Bool;
    fn WinUsb_GetDescriptor(
        interface_handle: WinUsbHandle,
        descriptor_type: u8,
        index: u8,
        language_id: u16,
        buffer: *mut u8,
        buffer_length: Dword,
        transferred: *mut Dword,
    ) -> Bool;
    fn WinUsb_SetPipePolicy(
        interface_handle: WinUsbHandle,
        pipe_id: u8,
        policy_type: Dword,
        value_length: Dword,
        value: *mut c_void,
    ) -> Bool;
    fn WinUsb_ReadPipe(
        interface_handle: WinUsbHandle,
        pipe_id: u8,
        buffer: *mut u8,
        buffer_length: Dword,
        transferred: *mut Dword,
        overlapped: *mut c_void,
    ) -> Bool;
    fn WinUsb_WritePipe(
        interface_handle: WinUsbHandle,
        pipe_id: u8,
        buffer: *mut u8,
        buffer_length: Dword,
        transferred: *mut Dword,
        overlapped: *mut c_void,
    ) -> Bool;
}

pub fn enumerate() -> Result<Vec<UsbInterfaceDescriptor>, UsbError> {
    Ok(collect_services()?
        .into_iter()
        .map(|service| service.descriptor)
        .collect())
}

pub fn open_unique(
    selector: UsbInterfaceSelector,
    timeout_ms: u32,
) -> Result<Box<dyn BulkInterface>, UsbError> {
    let mut matches = collect_services()?
        .into_iter()
        .filter(|service| selector.matches(&service.descriptor))
        .collect::<Vec<_>>();
    match matches.len() {
        0 => Err(UsbError::NoMatchingInterface(selector)),
        1 => open_service(matches.pop().expect("one match"), timeout_ms),
        _ => Err(UsbError::AmbiguousInterfaces(
            matches.into_iter().map(|item| item.descriptor).collect(),
        )),
    }
}

pub fn open_exact(
    selector: UsbInterfaceSelector,
    expected: &UsbInterfaceDescriptor,
    timeout_ms: u32,
) -> Result<Box<dyn BulkInterface>, UsbError> {
    let mut matches = collect_services()?
        .into_iter()
        .filter(|service| selector.matches(&service.descriptor) && service.descriptor == *expected)
        .collect::<Vec<_>>();
    match matches.len() {
        0 => Err(UsbError::NoExactInterface(expected.clone())),
        1 => open_service(matches.pop().expect("one exact match"), timeout_ms),
        _ => Err(UsbError::AmbiguousInterfaces(
            matches.into_iter().map(|item| item.descriptor).collect(),
        )),
    }
}

#[derive(Debug)]
struct ServiceRecord {
    path: Vec<u16>,
    descriptor: UsbInterfaceDescriptor,
}

fn collect_services() -> Result<Vec<ServiceRecord>, UsbError> {
    let set = unsafe {
        SetupDiGetClassDevsW(
            &ARKFORGE_ROCKUSB_INTERFACE_GUID,
            ptr::null(),
            ptr::null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    };
    if set == INVALID_HANDLE_VALUE {
        return Err(enumeration_error("SetupDiGetClassDevsW"));
    }
    let set = DeviceInfoSetGuard(set);
    let mut records = Vec::new();
    let mut first_failure = None;
    let mut index = 0;
    loop {
        let mut interface = SpDeviceInterfaceData::default();
        if unsafe {
            SetupDiEnumDeviceInterfaces(
                set.0,
                ptr::null_mut(),
                &ARKFORGE_ROCKUSB_INTERFACE_GUID,
                index,
                &mut interface,
            )
        } == 0
        {
            let error = unsafe { GetLastError() };
            if error == ERROR_NO_MORE_ITEMS {
                break;
            }
            return Err(UsbError::Enumeration(format!(
                "SetupDiEnumDeviceInterfaces({index}) returned Windows error {error}"
            )));
        }
        let (path, instance_id) = interface_path(set.0, &mut interface)?;
        match WinUsbDevice::open(&path).and_then(|handle| handle.descriptor(&instance_id)) {
            Ok(descriptor) => records.push(ServiceRecord { path, descriptor }),
            Err(error) if first_failure.is_none() => first_failure = Some(error),
            Err(_) => {}
        }
        index += 1;
    }
    if records.is_empty()
        && let Some(error) = first_failure
    {
        Err(error)
    } else {
        Ok(records)
    }
}

fn interface_path(
    set: DeviceInfoSet,
    interface: &mut SpDeviceInterfaceData,
) -> Result<(Vec<u16>, String), UsbError> {
    let mut required = 0;
    let mut device_info = SpDevInfoData::default();
    let _ = unsafe {
        SetupDiGetDeviceInterfaceDetailW(
            set,
            interface,
            ptr::null_mut(),
            0,
            &mut required,
            &mut device_info,
        )
    };
    if unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER
        || required < std::mem::size_of::<SpDeviceInterfaceDetailDataW>() as Dword
    {
        return Err(enumeration_error("size SetupDiGetDeviceInterfaceDetailW"));
    }
    let mut storage = vec![0u8; required as usize];
    let detail = storage.as_mut_ptr().cast::<SpDeviceInterfaceDetailDataW>();
    unsafe { (*detail).size = std::mem::size_of::<SpDeviceInterfaceDetailDataW>() as Dword };
    if unsafe {
        SetupDiGetDeviceInterfaceDetailW(
            set,
            interface,
            detail,
            required,
            &mut required,
            &mut device_info,
        )
    } == 0
    {
        return Err(enumeration_error("SetupDiGetDeviceInterfaceDetailW"));
    }
    let path_start = unsafe { (*detail).device_path.as_ptr() };
    let capacity = (storage.len() - 4) / 2;
    let path_length = (0..capacity)
        .find(|index| unsafe { *path_start.add(*index) } == 0)
        .ok_or_else(|| UsbError::Enumeration("WinUSB device path is not terminated".into()))?;
    let mut path = unsafe { std::slice::from_raw_parts(path_start, path_length) }.to_vec();
    path.push(0);
    let instance_id = device_instance_id(set, &mut device_info)?;
    Ok((path, instance_id))
}

fn device_instance_id(
    set: DeviceInfoSet,
    device_info: &mut SpDevInfoData,
) -> Result<String, UsbError> {
    let mut required = 0;
    let _ =
        unsafe { SetupDiGetDeviceInstanceIdW(set, device_info, ptr::null_mut(), 0, &mut required) };
    if required == 0 {
        return Err(enumeration_error("size SetupDiGetDeviceInstanceIdW"));
    }
    let mut text = vec![0u16; required as usize];
    if unsafe {
        SetupDiGetDeviceInstanceIdW(set, device_info, text.as_mut_ptr(), required, &mut required)
    } == 0
    {
        return Err(enumeration_error("SetupDiGetDeviceInstanceIdW"));
    }
    let end = text
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(text.len());
    String::from_utf16(&text[..end])
        .map_err(|_| UsbError::Enumeration("device instance id is invalid UTF-16".into()))
}

fn open_service(
    service: ServiceRecord,
    timeout_ms: u32,
) -> Result<Box<dyn BulkInterface>, UsbError> {
    let device = WinUsbDevice::open(&service.path)?;
    let (bulk_in, bulk_out) = device.bulk_pipes()?;
    device.set_timeout(bulk_in, timeout_ms)?;
    device.set_timeout(bulk_out, timeout_ms)?;
    Ok(Box::new(WindowsBulkInterface {
        device,
        descriptor: service.descriptor,
        bulk_in,
        bulk_out,
    }))
}

struct WinUsbDevice {
    file: Handle,
    interface: WinUsbHandle,
}

unsafe impl Send for WinUsbDevice {}

impl WinUsbDevice {
    fn open(path: &[u16]) -> Result<Self, UsbError> {
        let file = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null_mut(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
                ptr::null_mut(),
            )
        };
        if file == INVALID_HANDLE_VALUE {
            return Err(claim_error("CreateFileW"));
        }
        let mut interface = ptr::null_mut();
        if unsafe { WinUsb_Initialize(file, &mut interface) } == 0 {
            let error = claim_error("WinUsb_Initialize");
            let _ = unsafe { CloseHandle(file) };
            return Err(error);
        }
        Ok(Self { file, interface })
    }

    fn descriptor(&self, instance_id: &str) -> Result<UsbInterfaceDescriptor, UsbError> {
        let device: UsbDeviceDescriptor = self.get_descriptor(USB_DEVICE_DESCRIPTOR_TYPE, 0, 0)?;
        let mut interface = WinUsbInterfaceDescriptor::default();
        if unsafe { WinUsb_QueryInterfaceSettings(self.interface, 0, &mut interface) } == 0 {
            return Err(claim_error("WinUsb_QueryInterfaceSettings"));
        }
        Ok(UsbInterfaceDescriptor {
            vendor_id: device.vendor_id,
            product_id: device.product_id,
            usb_specification: device.usb_specification,
            device_release: device.device_release,
            location_id: instance_location(instance_id),
            interface_class: interface.interface_class,
            interface_subclass: interface.interface_subclass,
            interface_protocol: interface.interface_protocol,
            interface_number: interface.interface_number,
            serial: self.string_descriptor(device.serial_index),
            product_name: self.string_descriptor(device.product_index),
            vendor_name: self.string_descriptor(device.manufacturer_index),
        })
    }

    fn get_descriptor<T: Default>(
        &self,
        descriptor_type: u8,
        index: u8,
        language_id: u16,
    ) -> Result<T, UsbError> {
        let mut value = T::default();
        let mut transferred = 0;
        let size = std::mem::size_of::<T>() as Dword;
        if unsafe {
            WinUsb_GetDescriptor(
                self.interface,
                descriptor_type,
                index,
                language_id,
                (&mut value as *mut T).cast(),
                size,
                &mut transferred,
            )
        } == 0
            || transferred != size
        {
            return Err(claim_error("WinUsb_GetDescriptor"));
        }
        Ok(value)
    }

    fn string_descriptor(&self, index: u8) -> Option<String> {
        if index == 0 {
            return None;
        }
        let mut language = [0u8; 256];
        let mut transferred = 0;
        let language_ok = unsafe {
            WinUsb_GetDescriptor(
                self.interface,
                USB_STRING_DESCRIPTOR_TYPE,
                0,
                0,
                language.as_mut_ptr(),
                language.len() as Dword,
                &mut transferred,
            )
        } != 0;
        let language_id = if language_ok && transferred >= 4 {
            u16::from_le_bytes([language[2], language[3]])
        } else {
            0x0409
        };
        let mut buffer = [0u8; 256];
        if unsafe {
            WinUsb_GetDescriptor(
                self.interface,
                USB_STRING_DESCRIPTOR_TYPE,
                index,
                language_id,
                buffer.as_mut_ptr(),
                buffer.len() as Dword,
                &mut transferred,
            )
        } == 0
            || transferred < 2
        {
            return None;
        }
        let declared = usize::from(buffer[0]).min(transferred as usize);
        if declared < 2 || declared % 2 != 0 {
            return None;
        }
        let units = buffer[2..declared]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        String::from_utf16(&units)
            .ok()
            .filter(|value| !value.is_empty())
    }

    fn bulk_pipes(&self) -> Result<(u8, u8), UsbError> {
        let mut descriptor = WinUsbInterfaceDescriptor::default();
        if unsafe { WinUsb_QueryInterfaceSettings(self.interface, 0, &mut descriptor) } == 0 {
            return Err(claim_error("WinUsb_QueryInterfaceSettings"));
        }
        let mut bulk_in = None;
        let mut bulk_out = None;
        for index in 0..descriptor.num_endpoints {
            let mut pipe = WinUsbPipeInformation::default();
            if unsafe { WinUsb_QueryPipe(self.interface, 0, index, &mut pipe) } == 0 {
                return Err(claim_error("WinUsb_QueryPipe"));
            }
            if pipe.pipe_type == USBD_PIPE_TYPE_BULK {
                if pipe.pipe_id & 0x80 != 0 {
                    bulk_in = Some(pipe.pipe_id);
                } else {
                    bulk_out = Some(pipe.pipe_id);
                }
            }
        }
        Ok((
            bulk_in.ok_or(UsbError::MissingBulkPipe { direction: "in" })?,
            bulk_out.ok_or(UsbError::MissingBulkPipe { direction: "out" })?,
        ))
    }

    fn set_timeout(&self, pipe: u8, timeout_ms: u32) -> Result<(), UsbError> {
        let mut value = timeout_ms;
        if unsafe {
            WinUsb_SetPipePolicy(
                self.interface,
                pipe,
                PIPE_TRANSFER_TIMEOUT,
                std::mem::size_of::<Dword>() as Dword,
                (&mut value as *mut Dword).cast(),
            )
        } == 0
        {
            Err(claim_error("WinUsb_SetPipePolicy(PIPE_TRANSFER_TIMEOUT)"))
        } else {
            Ok(())
        }
    }
}

impl Drop for WinUsbDevice {
    fn drop(&mut self) {
        let _ = unsafe { WinUsb_Free(self.interface) };
        let _ = unsafe { CloseHandle(self.file) };
    }
}

struct WindowsBulkInterface {
    device: WinUsbDevice,
    descriptor: UsbInterfaceDescriptor,
    bulk_in: u8,
    bulk_out: u8,
}

impl std::fmt::Debug for WindowsBulkInterface {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WindowsBulkInterface")
            .field("descriptor", &self.descriptor)
            .field("bulk_in", &self.bulk_in)
            .field("bulk_out", &self.bulk_out)
            .finish()
    }
}

impl BulkInterface for WindowsBulkInterface {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), UsbError> {
        let size =
            Dword::try_from(bytes.len()).map_err(|_| UsbError::TransferTooLarge(bytes.len()))?;
        let mut transferred = 0;
        if unsafe {
            WinUsb_WritePipe(
                self.device.interface,
                self.bulk_out,
                bytes.as_ptr().cast_mut(),
                size,
                &mut transferred,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(transfer_error("WinUsb_WritePipe"));
        }
        if transferred != size {
            return Err(UsbError::ShortTransfer {
                expected: size as usize,
                actual: transferred as usize,
            });
        }
        Ok(())
    }

    fn read_some(&mut self, bytes: &mut [u8]) -> Result<usize, UsbError> {
        let size =
            Dword::try_from(bytes.len()).map_err(|_| UsbError::TransferTooLarge(bytes.len()))?;
        let mut transferred = 0;
        if unsafe {
            WinUsb_ReadPipe(
                self.device.interface,
                self.bulk_in,
                bytes.as_mut_ptr(),
                size,
                &mut transferred,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(transfer_error("WinUsb_ReadPipe"));
        }
        Ok(transferred as usize)
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
        &self.descriptor
    }
}

struct DeviceInfoSetGuard(DeviceInfoSet);

impl Drop for DeviceInfoSetGuard {
    fn drop(&mut self) {
        let _ = unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

fn instance_location(instance_id: &str) -> u32 {
    let mut hash = 0x811c_9dc5u32;
    for unit in instance_id.encode_utf16() {
        hash ^= u32::from(unit);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

fn last_error() -> Dword {
    unsafe { GetLastError() }
}

fn enumeration_error(operation: &str) -> UsbError {
    UsbError::Enumeration(format!(
        "{operation} returned Windows error {}",
        last_error()
    ))
}

fn claim_error(operation: &str) -> UsbError {
    UsbError::Claim(format!(
        "{operation} returned Windows error {}",
        last_error()
    ))
}

fn transfer_error(operation: &str) -> UsbError {
    UsbError::Transfer(format!(
        "{operation} returned Windows error {}",
        last_error()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_guid_matches_the_packaged_inf() {
        assert_eq!(ARKFORGE_ROCKUSB_INTERFACE_GUID.data1, 0x6a4e_21f0);
        assert_eq!(
            instance_location("USB\\VID_2207&PID_350A\\ONE"),
            0x6fd8_9561
        );
    }
}
