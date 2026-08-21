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
$denied = Get-Credential -Message 'Enter a different Windows account for ACL rejection'
.\Test-ArkForgePackage.ps1 -DeniedCredential $denied `
  -EvidencePath .\arkforge-windows-acceptance.json
```

The full acceptance requires exactly one DAYU200 already in Loader mode and
credentials for a second Windows account whose Named Pipe connection must fail
specifically with Win32 `ERROR_ACCESS_DENIED` (error 5). Any other failure is an
acceptance failure, not ACL evidence. Use `-SkipDevice
-SkipCrossAccount` only for software/package CI; that result is neither USB
hardware nor ACL isolation acceptance. The test runs the signed HDC self-test,
starts the signed runtime, validates the exact HDC binding, exercises same-user
Named Pipe status, rejects a different account, checks the owner-only runtime
ACL, and requires the device to be bound to WinUSB. `-EvidencePath` records the
exact package, executable, HDC, driver, ACL and redacted device identity facts.
Destructive flashing remains a separate, explicitly acknowledged hardware
campaign.

Every push and pull request runs the Rust workspace natively on
`windows-latest`. The `Windows production acceptance` workflow is manual,
requires the protected `windows-production` environment, and runs only on a
self-hosted runner labelled `arkforge-dayu200`. It will not downgrade missing
certificate, Microsoft-signed catalog, HDC, second-account or physical-device
inputs into a software pass.

Uninstall uses the exact published driver name recorded by installation and
preserves per-user runtime journals:

```powershell
.\Uninstall-ArkForge.ps1 -Confirm
```
