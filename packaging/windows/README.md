# ArkForge Windows x64 release

This package keeps three trust decisions explicit:

- `arkforge.exe`, sibling `arkforged.exe`, and the selected `hdc.exe` are
  individually Authenticode-signed and timestamped;
- the DAYU200 Loader (`USB\VID_2207&PID_350A`) binds only through the signed
  `arkforge-rockusb.cat` WinUSB package and the private ArkForge interface GUID;
- runtime startup supplies the exact HDC SHA-256 and enables
  `--require-release-signing`; no PATH lookup or unsigned fallback exists.

Build the release from an x64 Native Tools PowerShell:

```powershell
.\packaging\windows\package-arkforge.ps1 `
  -CertificateThumbprint <sha1-thumbprint> `
  -TimestampUrl https://timestamp.example.invalid `
  -HdcPath C:\controlled-tools\hdc.exe `
  -DriverPackageDirectory C:\controlled-tools\arkforge-driver-signed
```

The HDC input must be a redistributable build selected by the release owner.
The driver directory must contain the canonical INF and its production-signed
catalog returned by the Windows Hardware Developer Program; an application
code-signing certificate is not accepted as a substitute. The packager verifies
the catalog with the kernel policy (`SignTool /kp`) before it signs the exact
CLI, daemon, HDC, PowerShell installer bytes, and a payload-hash manifest.
Installation trusts that signed manifest rather than the mutable ZIP container
or informational JSON receipt. The packager emits both a directory and ZIP and
never flashes a device.

Install and accept from an elevated PowerShell:

```powershell
.\Install-ArkForge.ps1
.\Test-ArkForgePackage.ps1
```

The full acceptance requires one DAYU200 already in Loader mode. Use
`-SkipDevice` only for software/package CI; that result is not USB hardware
acceptance. The test starts the signed runtime, validates the HDC binding,
exercises same-user Named Pipe status, checks the owner-only runtime ACL, and
requires the device to be bound to WinUSB. Destructive flashing remains a
separate, explicitly acknowledged hardware campaign.

Uninstall uses the exact published driver name recorded by installation and
preserves per-user runtime journals:

```powershell
.\Uninstall-ArkForge.ps1 -Confirm
```
