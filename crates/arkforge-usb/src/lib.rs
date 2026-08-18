//! Safe USB bulk-I/O boundary for ArkForge.
//!
//! CHG-2026-063 deliberately confines every IOKit declaration, raw pointer,
//! and unsafe operation to this crate.  Callers see descriptors and exact
//! read/write methods; they never receive an IOKit service or interface.

#![deny(unsafe_op_in_unsafe_fn)]

use core::fmt;

/// The DAYU200 RockUSB Loader interface descriptor.
pub const ROCKCHIP_VENDOR_ID: u16 = 0x2207;
pub const DAYU200_LOADER_PRODUCT_ID: u16 = 0x350a;
pub const ROCKUSB_INTERFACE_CLASS: u8 = 0xff;
pub const ROCKUSB_INTERFACE_SUBCLASS: u8 = 6;
pub const ROCKUSB_INTERFACE_PROTOCOL: u8 = 5;

/// One USB interface and the identity facts the OS reported for its device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbInterfaceDescriptor {
    pub vendor_id: u16,
    pub product_id: u16,
    /// USB specification (`bcdUSB`). Rockchip uses bit zero to distinguish
    /// Loader (1) from Maskrom (0) even though both use PID 0x350a.
    pub usb_specification: u16,
    pub device_release: u16,
    pub location_id: u32,
    pub interface_class: u8,
    pub interface_subclass: u8,
    pub interface_protocol: u8,
    pub interface_number: u8,
    pub serial: Option<String>,
    pub product_name: Option<String>,
    pub vendor_name: Option<String>,
}

/// Exact descriptor match for the interface ArkForge is allowed to claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbInterfaceSelector {
    pub vendor_id: u16,
    pub product_id: u16,
    pub interface_class: u8,
    pub interface_subclass: u8,
    pub interface_protocol: u8,
    pub require_rockchip_loader: bool,
}

impl UsbInterfaceSelector {
    pub const fn dayu200_loader() -> Self {
        Self {
            vendor_id: ROCKCHIP_VENDOR_ID,
            product_id: DAYU200_LOADER_PRODUCT_ID,
            interface_class: ROCKUSB_INTERFACE_CLASS,
            interface_subclass: ROCKUSB_INTERFACE_SUBCLASS,
            interface_protocol: ROCKUSB_INTERFACE_PROTOCOL,
            require_rockchip_loader: true,
        }
    }

    pub fn matches(self, descriptor: &UsbInterfaceDescriptor) -> bool {
        descriptor.vendor_id == self.vendor_id
            && descriptor.product_id == self.product_id
            && descriptor.interface_class == self.interface_class
            && descriptor.interface_subclass == self.interface_subclass
            && descriptor.interface_protocol == self.interface_protocol
            && (!self.require_rockchip_loader || descriptor.usb_specification & 1 == 1)
    }
}

/// An exclusively claimed pair of bulk pipes.
pub trait BulkInterface: fmt::Debug {
    /// Writes the entire buffer or returns an error; short writes never count.
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), UsbError>;

    /// Reads one bounded bulk transfer and returns its actual byte count.
    /// This exists because READ_FLASH_INFO is declared as 11 bytes on the
    /// wire, while measured Rockchip loaders may pad the data stage up to one
    /// 512-byte transfer just as the pinned vendor implementation anticipates.
    fn read_some(&mut self, bytes: &mut [u8]) -> Result<usize, UsbError>;

    /// Reads exactly the requested number of bytes or returns an error.
    fn read_exact(&mut self, bytes: &mut [u8]) -> Result<(), UsbError>;

    fn descriptor(&self) -> &UsbInterfaceDescriptor;
}

/// USB substrate failures.  The text includes the IOKit result where one was
/// returned, without projecting it into a protocol verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsbError {
    UnsupportedPlatform,
    Enumeration(String),
    NoMatchingInterface(UsbInterfaceSelector),
    AmbiguousInterfaces(Vec<UsbInterfaceDescriptor>),
    Claim(String),
    MissingBulkPipe { direction: &'static str },
    Transfer(String),
    TransferTooLarge(usize),
    ShortTransfer { expected: usize, actual: usize },
}

impl fmt::Display for UsbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UsbError::UnsupportedPlatform => {
                f.write_str("native IOKit USB is available on macOS only")
            }
            UsbError::Enumeration(detail) => write!(f, "USB enumeration: {detail}"),
            UsbError::NoMatchingInterface(selector) => write!(
                f,
                "no USB interface matches {:04x}:{:04x} class {:02x}/{:02x}/{:02x}{}",
                selector.vendor_id,
                selector.product_id,
                selector.interface_class,
                selector.interface_subclass,
                selector.interface_protocol,
                if selector.require_rockchip_loader {
                    " with Rockchip Loader bcdUSB bit"
                } else {
                    ""
                }
            ),
            UsbError::AmbiguousInterfaces(records) => write!(
                f,
                "{} USB interfaces match the exact selector; refusing an ambiguous claim",
                records.len()
            ),
            UsbError::Claim(detail) => write!(f, "USB claim: {detail}"),
            UsbError::MissingBulkPipe { direction } => {
                write!(f, "claimed interface has no bulk {direction} pipe")
            }
            UsbError::Transfer(detail) => write!(f, "USB transfer: {detail}"),
            UsbError::TransferTooLarge(size) => {
                write!(f, "USB transfer size {size} exceeds the UInt32 ABI")
            }
            UsbError::ShortTransfer { expected, actual } => write!(
                f,
                "USB transfer returned {actual} bytes; exactly {expected} were required"
            ),
        }
    }
}

impl std::error::Error for UsbError {}

/// Native macOS transport.  It enumerates `IOUSBHostInterface` services,
/// claims exactly one matched interface, and exposes only its bulk pipes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeUsb {
    timeout_ms: u32,
}

impl NativeUsb {
    pub const fn new(timeout_ms: u32) -> Self {
        Self { timeout_ms }
    }

    pub fn enumerate(&self) -> Result<Vec<UsbInterfaceDescriptor>, UsbError> {
        platform::enumerate()
    }

    pub fn open_unique(
        &self,
        selector: UsbInterfaceSelector,
    ) -> Result<Box<dyn BulkInterface>, UsbError> {
        platform::open_unique(selector, self.timeout_ms)
    }
}

#[cfg(target_os = "macos")]
mod platform;

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;

    pub fn enumerate() -> Result<Vec<UsbInterfaceDescriptor>, UsbError> {
        Err(UsbError::UnsupportedPlatform)
    }

    pub fn open_unique(
        _selector: UsbInterfaceSelector,
        _timeout_ms: u32,
    ) -> Result<Box<dyn BulkInterface>, UsbError> {
        Err(UsbError::UnsupportedPlatform)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_loader_selector_is_exact_not_vid_only() {
        let mut descriptor = UsbInterfaceDescriptor {
            vendor_id: ROCKCHIP_VENDOR_ID,
            product_id: DAYU200_LOADER_PRODUCT_ID,
            usb_specification: 0x0201,
            device_release: 0x0200,
            location_id: 0x0110_0000,
            interface_class: ROCKUSB_INTERFACE_CLASS,
            interface_subclass: ROCKUSB_INTERFACE_SUBCLASS,
            interface_protocol: ROCKUSB_INTERFACE_PROTOCOL,
            interface_number: 0,
            serial: Some("redacted-in-export".into()),
            product_name: Some("USB download gadget".into()),
            vendor_name: Some("Rockchip".into()),
        };
        let selector = UsbInterfaceSelector::dayu200_loader();
        assert!(selector.matches(&descriptor));
        descriptor.usb_specification = 0x0200;
        assert!(!selector.matches(&descriptor));
        descriptor.usb_specification = 0x0201;
        descriptor.product_id = 0x5000;
        assert!(!selector.matches(&descriptor));
    }
}
