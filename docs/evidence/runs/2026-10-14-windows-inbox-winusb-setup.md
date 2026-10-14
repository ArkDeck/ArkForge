# Windows system WinUSB setup: implementation and read-only checks

This change adds a host setup path for the existing native Windows transport.
It selects Microsoft's unchanged inbox `winusb.inf` for one exact present
DAYU200 Loader and registers the existing ArkForge interface GUID. It does not
implement vendor-driver transfers, change the shared RockUSB protocol, install
a custom driver package or alter Windows trust/signature policy.

The setup script defaults to inspection. Its separate plan binds the current
device instance/location, prior driver and interface state, signed system file
hashes and installer helper bytes. Elevated apply requires that exact digest
within ten minutes, persists intent before modification, then verifies the
observed binding. A failed/incomplete install stays unknown with its records
retained. There is no automatic retry, rollback or restart. The original OEM
driver package remains installed for an explicitly assessed restoration.

## Local targeted checks

- `packaging/windows/Test-InboxWinUsb.ps1`: exit 0; eight groups cover the exact
  plan round trip, changed identity/location/driver/GUID/filter state, expired
  and foreign plans, changed implementation/system files, malformed snapshot
  types, exact resulting binding, exclusive durable records and unsafe paths.
  The C# interop helper is compiled, but these tests do not call a device or
  driver installation API. Log: local
  `windows-inbox-winusb-20261014/software-checks-final.log`.
- `Bind-InboxWinUsb.ps1 -Action Inspect`: exit 0 on one present Loader. It read
  the existing Rockusb service and OEM driver metadata, verified trusted
  Microsoft Windows signatures on inbox INF/SYS bytes, and associated the
  system `WINUSB` model with that same device through documented driver-list
  inheritance. No install, selection mutation, GUID write or USB bulk I/O ran.
  Redacted log: local `windows-inbox-winusb-20261014/readonly-inspection-final.json`.
- Actual read-only plan generation and guard round trip: exit 0. An attempted
  unelevated apply refused before intent creation or modification. The local
  plan contains private device identifiers and is not committed or exported.
- PowerShell parse: all 12 release/setup scripts/modules, zero errors; local
  `windows-inbox-winusb-20261014/powershell-parse-final.json`. `git diff --check`:
  exit 0. No Rust or Swift runtime/wire source changed; full workspace checks
  run in PR CI, not as an additional local build alongside the ongoing soak.

## CI

Pending submission of this change. Native Windows CI now runs the host-only
binding checks alongside existing package guards. Its result must be read
from the exact PR head; this record does not claim a future CI pass.

## Qualification

These results establish implementation/software checks and actual read-only
Windows driver discovery only. Elevated binding, WinUSB transport discovery,
firmware writing/readback and reboot have not been performed by this change's
checks. They do not establish AF-W1, `REAL_DEVICE_PASS`, production support or
an accepted hardware campaign. The signed production package path remains
unchanged and has its existing separate release/physical acceptance inputs.

References: Microsoft documents
[manual system WinUSB selection and interface registration](https://learn.microsoft.com/en-us/windows-hardware/drivers/usbcon/winusb-installation),
[driver-list inheritance](https://learn.microsoft.com/en-us/windows/win32/api/setupapi/nf-setupapi-setupdiopendeviceinfow),
and [exact-device installation](https://learn.microsoft.com/en-us/windows/win32/api/newdev/nf-newdev-diinstalldevice).
