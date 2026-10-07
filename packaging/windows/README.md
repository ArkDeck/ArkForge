# ArkForge Windows x64 release

This package keeps three trust decisions explicit:

- `arkforge.exe`, sibling `arkforged.exe`, and the selected `hdc.exe` are
  individually Authenticode-signed and timestamped;
- the DAYU200 Loader (`USB\VID_2207&PID_350A`) binds only through the signed
  `arkforge-rockusb.cat` WinUSB package and the private ArkForge interface GUID;
- runtime startup supplies the exact HDC SHA-256 and enables
  `--require-release-signing`; no PATH lookup or unsigned fallback exists.

Build the release from an x64 Native Tools PowerShell 7 session:

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

The same producer also emits the ArkDeck-consumable release unit inside that
package, with no undeclared files:

```text
ArkForge.release-bundle/
  bin/arkforge.exe
  bin/arkforged.exe
  Contents/Resources/arkforge-bundle.json
  Contents/Resources/profiles/dayu200.yaml
```

`arkforge-bundle.json` uses the existing `arkforge.release-bundle/v1` schema:
one `cli`, one `daemon` and the published `org.openharmony.dayu200` profile.
Each member has its exact signed/copy byte count and lowercase whole SHA-256.
The manifest is UTF-8 without a BOM and excludes itself from the member list.
`ReleaseBundle.psm1` copies only the already signed native images and the current
published profile, retains source and copied file handles against writes/deletes,
remeasures every member, and refuses wrong signers, non-AMD64 images, unsafe
links, extra/missing members and overwrites. This is part of the genuine build
pipeline, not a separately assembled fixture bundle.

The native build links the MSVC CRT statically. A bounded PE import inspection
independently refuses delayed imports and dependencies outside the closed
Windows system-library set; the bundle cannot borrow a vendor/VC runtime DLL
from the build host. `CARGO_TARGET_DIR` selects an existing reusable target, and
the build uses two jobs. The packager restores the caller's Rust flag variables
after its static-CRT build.

Bundle construction follows executable signing and verification and precedes
the existing signed `ArkForge.PackageManifest.ps1`, which binds the nested
manifest and every bundle member. The informational package receipt names the
bundle and its manifest digest. Keep that outer signed package with any exported
bundle ZIP. ArkDeck independently checks its closed manifest/inventory, native
file identities and byte hashes when loading the bundle. Those checks do not
perform Authenticode verification or establish AF-W1; retain the accepted
signed outer package and its matching physical receipt as the delivery proof.
The outer installer signature is not a substitute for the inventory checks.
Its bundle path is the nested
`ArkForge.release-bundle` directory, not the installer/package root.

Software producer regressions run without launching any image or device:

```powershell
.\packaging\windows\Test-ReleaseBundle.ps1
```

The test uses signed public Windows executable bytes as explicitly labelled
fixture data. A successful fixture check or bundle build does not establish
AF-W1, a hardware campaign, or production device support. The protected physical
acceptance workflow must separately accept the exact immutable package/bundle
and produce the corresponding real acceptance evidence before release export.

Install and accept from an elevated PowerShell:

```powershell
.\Install-ArkForge.ps1
$denied = Get-Credential -Message 'Enter a different Windows account for ACL rejection'
.\Test-ArkForgePackage.ps1 -DeniedCredential $denied `
  -EvidencePath .\arkforge-windows-acceptance.json
```

The full acceptance requires exactly one DAYU200 already in Loader mode and
credentials for a second Windows account whose Named Pipe connection must fail
specifically with Win32 `ERROR_ACCESS_DENIED` (error 5). The negative probe uses
the published `daemon stop` leaf against only the fresh acceptance-owned runtime,
with its exact native path spelling; its supervisor-connect error must prove
error 5. `daemon status` is not a published leaf. A wrongly open ACL that permits
the other account to stop this isolated runtime fails acceptance. Any other
failure is an acceptance failure, not ACL evidence. Use `-SkipDevice
-SkipCrossAccount` only for software/package CI; that result is neither USB
hardware nor ACL isolation acceptance. The test runs the signed HDC self-test,
starts the signed runtime, validates the exact HDC binding, exercises same-user
Named Pipe status, rejects a different account, checks the owner-only runtime
ACL, and requires the device to be bound to WinUSB. `-EvidencePath` records the
exact package, executable, HDC, driver, ACL and redacted device identity facts.
The evidence filename must be new, with an already existing ordinary parent.
The script creates it exclusively. An uncertain lifecycle or unverified stop
retains the original runtime/diagnostic files instead of deleting or replaying
them. Destructive flashing remains a separate, explicitly acknowledged hardware
campaign.

Every push and pull request runs the Rust workspace natively on
`windows-latest`. The `Windows production acceptance` workflow is manual,
requires the protected `windows-production` environment, and runs only on a
self-hosted runner labelled `arkforge-dayu200`. It will not downgrade missing
certificate, Microsoft-signed catalog, HDC, second-account or physical-device
inputs into a software pass.

The repository readiness readback on 2026-10-07 found zero registered
self-hosted runners (`total_count: 0`), and the `windows-production` environment
request returned HTTP 404. AF-W1 therefore still needs these deployment inputs;
neither the producer tests nor this documentation establishes a physical pass.
The release owner must provision:

- an elevated Windows x64 runner with all four labels `self-hosted`, `Windows`,
  `X64`, `arkforge-dayu200`, PowerShell 7, Rust/Cargo, x64 MSVC Native Tools and
  Windows SDK SignTool; one DAYU200 must already be in Loader mode, attached to
  that runner, with no other matching Loader device;
- the protected `windows-production` environment and its review/access rules,
  with exactly the required variables `ARKFORGE_CERT_THUMBPRINT`,
  `ARKFORGE_TIMESTAMP_URL`, `ARKFORGE_HDC_PATH`, `ARKFORGE_DRIVER_PACKAGE`,
  `ARKFORGE_DENIED_ACCOUNT`, and secret `ARKFORGE_DENIED_PASSWORD`; the second
  account must resolve to a SID different from the runner owner. Keep the
  password in the environment secret, not a variable, command argument or log;
- the trusted release code-signing certificate with usable private key and
  timestamp service, a redistributable HDC input, and the canonical
  `arkforge-rockusb.inf` with its genuine WHDP-signed catalog. The configured
  input paths are runner-local files; an application certificate cannot replace
  the driver catalog's kernel-policy trust.

Once those prerequisites exist, a release operator selects **Actions → Windows
production acceptance → Run workflow** on the protected default branch, or uses
the equivalent explicit entry:

```powershell
gh workflow run windows-acceptance.yml --repo ArkDeck/ArkForge --ref <protected-default-branch>
```

That manual run installs the signed driver/package and tests the already
attached Loader. It does not enter Loader, flash firmware or open a campaign.
After the full run succeeds, retrieve
`arkforge-windows-production-acceptance-<source-sha>-<run-id>-<run-attempt>` from
that run's artifacts, for example with `gh run download <run-id> --repo
ArkDeck/ArkForge --name <exact-artifact-name> --dir <new-local-directory>`.
Keep all four delivery files together and verify the `delivery.json` archive
hashes before handing the nested bundle to ArkDeck. A missing runner,
environment, release input or failed acceptance remains an explicit blocker.

After that full no-skip AF-W1 gate, the workflow exports one matched delivery:
the unchanged signed outer package ZIP, `ArkForge.release-bundle.zip`, the actual
`arkforge-windows-acceptance.json` and `delivery.json`. The delivery binds source
revision/run, exact bundle/profile/image hashes, and every archive's whole SHA-256
and byte count. Each ZIP's complete expanded member inventory is verified before
upload. This packaging/ACL/Loader acceptance does not itself authorize a flash,
create a HardwareCampaign, or promote production support.

Uninstall uses the exact published driver name recorded by installation and
preserves per-user runtime journals:

```powershell
.\Uninstall-ArkForge.ps1 -Confirm
```
