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
- PowerShell parse: all 12 release/setup hack/modules, zero errors; local
  `windows-inbox-winusb-20261014/powershell-parse-final.json`. `git diff --check`:
  exit 0. No Rust or Swift runtime/wire source changed; full workspace checks
  run in PR CI, not as an additional local build alongside the ongoing soak.

## CI

Initial implementation: [PR #7](https://github.com/ArkDeck/ArkForge/pull/7),
head `eb292496210a5645be6d251fc9d2ff49b96296d0`, passed native Windows run
[38526202665](https://github.com/ArkDeck/ArkForge/actions/runs/38526202665) and
Rust/Swift run [38526202608](https://github.com/ArkDeck/ArkForge/actions/runs/38526202608).
Both push runs also passed. Main published it as
`d2bcd145d5187ad3573c01c5ec80e15095b18355`.

## Elevated preflight and path-case correction

The first elevated invocation of the published setup tool refused in
preflight, before creating intent or modifying the device. Inspection still
reported the original Rockusb binding and no registered ArkForge interface.
An elevated read-only comparison confirmed identical helper hashes, signed
inbox file hashes, byte counts, signer, driver model and version. Only
SetupAPI's INF spelling differed: `c:\windows\inf\winusb.inf` versus
`C:\WINDOWS\INF\winusb.inf`.

The follow-up compares that absolute INF path using Windows' ordinal
case-insensitive path semantics. It still requires the same path, exact
model/provider/version and independently pinned signed file bytes. The new
regression accepts the observed case difference and rejects another path,
file, provider/version or string-valued version. `Test-InboxWinUsb.ps1`: exit
0, nine groups; local `windows-inbox-winusb-20261014/path-case-checks.log`.
`git diff --check`: exit 0. Original local records remain unchanged in
`published-binding-exit.json` and `elevated-comparison.json`. Follow-up CI is
pending at commit time; its PR records the actual result.

## Qualification

These results establish implementation/software checks and actual read-only
Windows driver discovery only. Completed elevated binding, WinUSB transport discovery,
firmware writing/readback and reboot have not been performed by this change's
checks. They do not establish AF-W1, `REAL_DEVICE_PASS`, production support or
an accepted hardware campaign. The signed production package path remains
unchanged and has its existing separate release/physical acceptance inputs.

References: Microsoft documents
[manual system WinUSB selection and interface registration](https://learn.microsoft.com/en-us/windows-hardware/drivers/usbcon/winusb-installation),
[driver-list inheritance](https://learn.microsoft.com/en-us/windows/win32/api/setupapi/nf-setupapi-setupdiopendeviceinfow),
and [exact-device installation](https://learn.microsoft.com/en-us/windows/win32/api/newdev/nf-newdev-diinstalldevice).
