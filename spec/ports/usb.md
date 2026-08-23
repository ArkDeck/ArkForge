# Port: bulk USB interface
status: draft
source: crates/arkforge-usb/src/lib.rs (safe surface), platform.rs (IOKit), platform_windows.rs (WinUSB); crates/arkforge-transport/src/usb.rs

## Purpose
Enumerate USB interfaces, claim exactly one, move bytes over its bulk pipes,
and release it — with no protocol knowledge. It is the only place `unsafe`
(or a C ABI) is permitted in a port (README §3).

## Operations
| op | input | output |
|---|---|---|
| `enumerate()` | — | list of `InterfaceDescriptor{vendorId, productId, usbSpecification, deviceRelease, locationId, interfaceClass, interfaceSubclass, interfaceProtocol, interfaceNumber, serial?, productName?, vendorName?}` |
| `open_unique(selector, timeoutMs)` | `Selector{vendorId, productId, class, subclass, protocol, requireRockchipLoader}` | a claimed `BulkInterface`, or `NoMatchingInterface` / `AmbiguousInterfaces{descriptors}` |
| `open_exact(selector, expected, timeoutMs)` | selector + the exact descriptor previously observed | the claimed interface, or `NoExactInterface{expected}` |
| `write_all(bytes)` | up to 2^32−1 bytes | all bytes written or error |
| `read_some(buffer)` | buffer | count read (may be short) |
| `read_exact(buffer)` | buffer | fills it or `ShortTransfer{expected, actual}` |
| `descriptor()` | — | the claimed interface's descriptor |

`requireRockchipLoader` means `usbSpecification & 1 == 1` (the Loader personality's
bcdUSB bit); the selector never matches on VID alone.

## Ownership and lifetime
A claimed interface is exclusively owned by its handle; dropping/closing the
handle releases the claim. The reference transport claims per semantic call
and releases afterwards, so a crashed daemon never leaves a stale claim.

## Thread-safety
A handle is used from one thread at a time. Enumeration may run concurrently
with a claimed handle.

## Deadlines
`timeoutMs` bounds each bulk transfer; it is a wall-clock budget set by the
caller per transfer size, not a global constant. A timeout is `Transfer`.

## Short reads/writes
`write_all` never returns short. `read_some` may; `read_exact` converts a short
read into `ShortTransfer`. A transfer larger than the platform ABI allows is
`TransferTooLarge` before any I/O.

## Idempotency and external effects
`enumerate`, `open_*`, `descriptor` have no device effect. `write_all` may have
any effect the protocol above gives the bytes; this port never interprets
them. The port MUST NOT retry a write on its own.

## Crash / retry
After a crash the caller re-enumerates and re-opens exactly; the port keeps no
state. Whether the device was touched is the engine's question (journal), not
the port's.

## Error classes
`USB_UNSUPPORTED_PLATFORM`, `USB_ENUMERATION`, `USB_NO_MATCHING_INTERFACE`,
`USB_NO_EXACT_INTERFACE`, `USB_AMBIGUOUS_INTERFACES`, `USB_CLAIM`,
`USB_MISSING_BULK_PIPE{direction}`, `USB_TRANSFER`, `USB_TRANSFER_TOO_LARGE`,
`USB_SHORT_TRANSFER{expected, actual}`. Platform error text is carried as
detail, never as the class.

## Identity facts this port feeds
`locationId` → topology digest (`SHA-256("arkforge/v1/usb-topology\0" ||
locationId as 4-byte big-endian)`); `serial` bytes →
`"arkforge/v1/device-serial\0"`; descriptor bytes →
`"arkforge/v1/usb-descriptor\0"`; `productName`/`vendorName` →
protocol-identity facts `usb.productName` / `usb.vendorName` (AF-TRN-002).

## Conformance hooks
A mock implementing `BulkInterface` over scripted reads/writes is enough for
the protocol engine's tests; the replay transport does not need this port at
all. Real-device behaviour is evidence (`docs/evidence/`), not a fixture.

---

## Appendix (informative): RockUSB protocol constants
source: crates/arkforge-provider/src/rockusb_protocol.rs, pinned to
`rockchip-linux/rkdeveloptool@304f073752fd25c854e1bcf05d8e7f925b1f4e14`
(`RKComm.h`, `RKComm.cpp`), evidence RK-001.

| item | value |
|---|---|
| Rockchip vendor id | `0x2207` |
| DAYU200 normal (HDC) product id | `0x5000` (AD-008) |
| DAYU200 Loader product id | `0x350a` (AD-008) |
| RockUSB interface class/subclass/protocol | `0xff` / `6` / `5` |
| logical block | 512 bytes |
| transfer chunk | 16384 sectors (8 MiB) per command |
| CBW | 31 bytes: `"USBC"`, tag u32 LE, transfer length u32 LE, flags (`0x80` = IN), LUN 0, command length, opcode at byte 15, address u32 **BE** at 17..21, sector count u16 **BE** at 22..24 |
| CSW | 13 bytes: `"USBS"`, tag u32 LE (must echo the CBW tag), residue, status |
| opcodes | `TEST_UNIT_READY 0x00`, `READ_LBA 0x14`, `WRITE_LBA 0x15`, `READ_FLASH_INFO 0x1a`, `DEVICE_RESET 0xff` |
| partition table device string | `rk29xxnand` |
| read window | measured per session; 2026-08-04: blind from sector 65536 (32 MiB), reads beyond return uniform `0xCC` (AD-006) |
| erased medium filler | `0xCC` (profile `readDomain.erasedMediumFiller`) |

These are *facts the reference provider pins*, not requirements of the port:
a port for another SoC family carries its own appendix.
