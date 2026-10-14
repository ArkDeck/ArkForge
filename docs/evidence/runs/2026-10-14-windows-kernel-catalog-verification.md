# Windows kernel catalog verification — 2026-10-14

The release producer passed `/kp /all` to Windows SDK SignTool. SDK
10.0.26100.0 rejects that combination before checking the catalog, so even a
genuine Microsoft-signed input could not reach the rest of packaging.
The producer now uses the kernel policy `/kp` and separately checks that the
exact INF belongs to that catalog. Its canonical ArkForge INF hash requirement,
release signer checks and physical acceptance requirements are unchanged.

`Test-DriverCatalog.ps1` invokes the production helpers with real SignTool and
existing signed catalog/INF bytes. It verifies positive membership and rejects
an unsigned catalog, changed INF and missing member. It preserves original
fixture hashes and resets the workflow exit status only after all expected
negative cases have passed. The native Windows CI lane runs this regression.

## Local targeted checks

Source base: `88214a5884b1e21cdc7f5f4e5ec06b3a30f3741f`. Checks used PowerShell 7
and x64 SignTool from Windows SDK 10.0.26100.0. Logs are retained outside the
repository at `D:/src/ArkDeck-wt/tools/afw1-driver-investigation-20261014`.

| Command/check | Exit | Result | Log |
| --- | --- | --- | --- |
| Original `signtool verify /kp /all /v <catalog>` | 1 | Reproduced incompatible options | `original-kp-all-failure.log` |
| `pwsh -NoProfile -File packaging/windows/Test-DriverCatalog.ps1` | 0 | Four checks; installed catalog/INF fixture | `driver-catalog-default.log` |
| Same test with explicit original Rockchip catalog and INF | 0 | Four checks; unchanged Microsoft-signed vendor bytes | `driver-catalog-rockusb.log` |
| Invoke test in the workflow shell and return `$LASTEXITCODE` | 0 | Expected negative cases do not fail the workflow shell | `driver-catalog-ci-shell.log` |
| `pwsh -NoProfile -File packaging/windows/Test-WindowsAcceptanceContract.ps1` on host | 0 | 31 software guards | `acceptance-contract-host.log` |
| Full Windows packaging software step on host after guard corrections | 0 | Acceptance, bundle, HDC and native catalog checks | `packaging-workflow-final.log` |
| Parse all packaging PowerShell sources | 0 | Nine files, zero parse errors | `powershell-parse.json` |
| `cargo fmt --all -- --check` | 0 | Updated Rust packaging guard is formatted | Console check |
| `git diff --check` | 0 | No whitespace errors | Git check |

The first acceptance-contract attempt could not create its task-owned junction
inside the sandbox (`acceptance-contract.log`, exit 1). The identical command
passed on the host; no assertion or runtime behavior changed. The test inspects
its own PowerShell process and fixture paths, not an ArkForge runtime/device.

The explicit Rockchip cryptographic fixture has these original whole hashes:

| File | Bytes | SHA-256 |
| --- | ---: | --- |
| `rockusb.cat` | 15330 | `a9504dd82965cc442b0c07ce20ee455d69879672a679164d57768cf242142d9f` |
| `rockusb.inf` | 10639 | `456c70637d1b7df01268cce1c5471c08c4a0113a30c4e59fdf6a5390802ceffc` |
| `rockusb.sys` | 46832 | `5d695e3a041ff55198999d6edaefcb0671ae8bf134c6d8b11a170ecd170e1f5f` |

Using this existing vendor package as a signature fixture does not authorize
substituting it for ArkForge's canonical WinUSB package. The vendor INF binds
`rockusb.sys`; the released ArkForge transport uses WinUSB. This increment does
not install either driver, open a USB device, establish vendor-backend support,
sign a release, or produce AF-W1/GJ-4 hardware evidence.

Rust runtime, Swift and wire sources did not change. CI identified existing
Rust and PowerShell packaging guards that literally required the invalid
`/kp /all` command. The guards now check both kernel-policy verification and exact INF membership,
the producer call site, rejection of the incompatible option combination, and
inclusion of the native regression in Windows CI. Local compilation was not
repeated during the ongoing host soak. Existing bundle and HDC closure
implementations were unchanged. Full verification runs on the PR's CI lanes.

## CI

PR #4, initial head `6f35e38d79c6ffb8592e800b0420e680a125c6c9`: Rust run
`38502638529` failed at the stale Rust packaging guard described above.
At head `d91ffe33b8a0f9c1a81209d5c2280bdfe88dcb5f`, Rust/Swift run
`38503153149` passed; Windows run `38503153058` passed the native Rust suite
but failed the second stale guard in `Test-ReleaseBundle.ps1`. Both guards
were corrected and the full Windows packaging software step passed locally.
These are source-test corrections, not invalid load runs. Updated-head CI is pending.
A green software run will not replace the exact signed production inputs and
full physical AF-W1 acceptance.
