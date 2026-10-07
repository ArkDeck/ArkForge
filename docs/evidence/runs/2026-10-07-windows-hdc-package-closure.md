# Windows HDC package dependency closure

The official Windows SDK HDC imports `libusb_shared.dll`. The previous Windows
producer copied and signed only `hdc.exe`, so a production package could lack its
required app-local dependency. This increment inspects a bounded AMD64 PE import
closure, copies only that exact sibling DLL when required, retains the complete
redistribution notice, and signs the staged DLL with the same release identity.
The trusted outer manifest binds the final signed images, notice and signed
validation module. Installer and acceptance consumers verify that closed tools
inventory before driver or HDC actions. The original SDK bytes and the nested
four-file `arkforge.release-bundle/v1` inventory are unchanged.

## Local targeted checks

Commands ran from `D:/src/ArkDeck-wt/arkforge-root` using PowerShell 7. Logs and
CREATE_NEW reports are retained under
`D:/src/ArkDeck-wt/tools/gj4-public-materials-20261007`.

| Command | Exit | Result | Log/report |
| --- | --- | --- | --- |
| `pwsh -NoProfile -File packaging/windows/Test-HdcPackage.ps1 -ReportPath <new report>` | 0 | 21 passed, 0 failed, 0 skipped | `hdc-package-fixed.stdout.log`, `hdc-package-fixed.stderr.log`, `hdc-package-tests-fixed.json` |
| Same HDC command after the bounded-read correction | 0 | 22 passed, 0 failed, 0 skipped | `hdc-package-bounded-final.stdout.log`, `hdc-package-bounded-final.json` |
| `pwsh -NoProfile -File packaging/windows/Test-ReleaseBundle.ps1 -ReportPath <new report>` | 0 | 21 passed, 0 failed, 0 skipped | `release-bundle-closure.stdout.log`, `release-bundle-closure.json` |
| `pwsh -NoProfile -File packaging/windows/Test-WindowsAcceptanceContract.ps1` | 0 | 31 software checks passed | `acceptance-closure.stdout.log` |
| `pwsh -NoProfile -File <task-owned copy_sdk_hdc.ps1>` | 0 | Official SDK source and copied whole bytes equal | `sdk-hdc-copy.stdout.log`, `sdk-hdc-copy.json` |
| `pwsh -NoProfile -File <task-owned parse_packaging.ps1>` | 0 | All 8 packaging PowerShell files parse | `parse-packaging.stdout.log`, `parse-packaging.command.json` |
| `git diff --check` | 0 | No whitespace errors | Final source freeze report |

The new cases cover exact dependency copying, system-only closure, missing
notice/DLL, transitive or duplicate imports, invalid PE/RVA/architecture/role,
delayed imports, reparse ancestry, retained-source drift/write/delete denial,
exclusive output preservation, release-signature refusal, and exact manifest
SHA/count/signer/inventory checks. Fixture PE metadata is synthetic and never
executed. Public signed Windows executable bytes are used only to exercise the
cryptographic consumer gate; they are not an HDC release or hardware evidence.
The existing acceptance software guards inspect only their own PowerShell
process and task-owned copies. No HDC, daemon, driver installer or device was
executed in these checks.

Initial HDC test attempts failed at the manifest-member check: regex evaluation
overwrote PowerShell's case-insensitive automatic `$Matches` variable. The local
binding variable was renamed to `$boundFacts`; the complete 21-case suite then
passed. The original failures remain in `hdc-package-2.*` and
`hdc-package-diagnostic.*`. These were a code defect, not an invalid load run.

Final source review found that whole hashing preceded the input length checks.
The producer now validates each image's 64 MiB limit and the notice's 2 MiB limit
before hashing; handles remain tracked for disposal on every refusal. The added
regression intercepts whole hashing and rejects any oversized read, checking
HDC, DLL and notice independently with no output copy. The final distinct
software guard count is 74 (22 HDC, 21 unchanged bundle and 31 acceptance).
Only the changed HDC suite and PowerShell parsing were repeated for this narrow
ordering correction (`parse-packaging-bounded-final.*`, exit 0).

The official installed SDK pre-sign copy accepted these actual whole source
facts, then remeasured the unchanged originals and copies:

| Member | Bytes | SHA-256 |
| --- | ---: | --- |
| `tools/hdc.exe` | 5743104 | `c79518498aaf4e719733961216444e70c3eb53c8ba7006b933e6d7f2e1c6101e` |
| `tools/libusb_shared.dll` | 202240 | `4652cf440870e8d72db17a60dbb6f8a62bb89db30aa6af8b10c35347e5942c29` |
| `tools/NOTICE.txt` | 553689 | `357668622e7bcd5febe86bcfdc51c07d4342200cbd48ca178fd6cb0e69696055` |

These are source pins, not signed release pins. The copy check neither signed nor
launched those files. Full production packaging remains dependent on the release
owner's signing identity and the genuine canonical driver catalog.

## Public driver qualification

The official HiHope DAYU200 archive at `hihope_iot/docs` revision
`99c62c56db5a848b6d259d298e8810594201cd42`, path
`HiHope_DAYU200/烧写工具及指南/windows/DriverAssitant_v5.1.1.zip`, was downloaded
for bounded inspection only. Its whole SHA-256 is
`d9c020c544bf449b2114ace11678e8d2f532f88954239f82a8552ea7252cfd63`
(9813168 bytes); its Git blob identity matches the published
`7a1a34f269c801c11f50d5ae068b8a115c224c87`.

The original Windows x64/Win10 Rockchip catalog passed `SignTool verify /kp /v`;
its original INF passed `/kp /v /c <vendor cat> <vendor inf>`. The same catalog
against ArkForge's unchanged canonical INF failed with exit 1, “File not found
in the specified catalog.” All three outputs and the unchanged canonical INF
digest are retained in `catalog-verification.json` and its named logs. The vendor
INF uses `Rockusb`; ArkForge requires its exact WinUSB/private-interface INF.
The genuine vendor catalog therefore cannot be renamed or substituted for
`arkforge-rockusb.cat`. No driver installation or trust-store change occurred.

The public readiness readback still found zero registered GitHub runners and
HTTP 404 for `windows-production`; no AF-W1 run or delivery artifact exists.
The production runner, release signer and WHDP catalog attesting ArkForge's exact
INF remain owner-provisioned gates described in the packaging README.

## CI

CI is pending publication of this increment. The existing nested producer and
acceptance tests remained green locally. Rust source and wire/contracts were
not changed, so no Rust build or repeated workspace suite was run. None of these
software or public-material checks establishes AF-W1, REAL_DEVICE_PASS, a
HardwareCampaign, or production support.
