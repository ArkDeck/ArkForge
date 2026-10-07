# Windows release-bundle producer — 2026-10-07

The Windows packager previously emitted an installer tree without the release
unit ArkDeck's current Windows reader accepts. The genuine packaging pipeline
now produces `ArkForge.release-bundle` after signing/verifying its native CLI
and daemon, before signing the outer all-payload manifest. The nested unit has
exactly the two signed AMD64 executables, the unchanged published DAYU200 YAML
and the BOM-free `arkforge.release-bundle/v1` manifest. The three closed roles,
Windows paths, profile ID, full byte counts and lowercase SHA-256 match ArkDeck's
`rust/crates/arkdeck-contract/src/arkforge_bundle.rs` consumer. The signed outer
package manifest binds the nested manifest as well as every member.

The producer retains read-only source/copy handles while copying, verifies the
same trusted Authenticode signer before/after copy, remeasures all final bytes,
and refuses reparse points, undeclared/missing members and overwrites. The build
uses a static MSVC CRT and a bounded independent PE dependency inspection:
only the explicit Windows system closure is accepted; vendor/host CRT DLLs and
delayed imports fail closed. The exact synchronization API set used by the real
Rust images is a documented Windows library ([Microsoft WaitOnAddress](https://learn.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-waitonaddress));
there is no wildcard `api-ms-*` allowance. Existing executable `/pa`, WHDP driver
catalog `/kp`/catalog-membership, canonical INF and installer-signature gates
remain in place. No profile, maturity registry, permit, campaign or hardware
evidence state is changed.

## Local targeted checks

Source parent: `03c6cb65d89507f2f35eaa840df7f2d6129477cd`, branch
`codex/windows-release-bundle`, worktree `D:/src/ArkDeck-wt/arkforge-root`.
The single ArkForge build lane was explicitly granted and released after the
one build drained. Live ArkDeck/ArkForge/HDC opt-ins were cleared for that child;
no built image was executed. Logs/results are CREATE_NEW under
`D:/src/ArkDeck-wt/tools/logs/arkforge-release-bundle-20261007/`.

| Check | Actual result | Local record |
| --- | --- | --- |
| `pwsh -NoProfile -File packaging/windows/Test-ReleaseBundle.ps1 -ReportPath <fresh-task-report>` | Exit 0, 2.630 s; 21 passed, 0 failed/skipped | `software-4.log`, `software-4.json` |
| `cargo build --locked --offline --release --target x86_64-pc-windows-msvc --target-dir <fixed-owner-target> -j 2 -p arkforge-cli --bin arkforge -p arkforged --bin arkforged`, `RUSTFLAGS=-C target-feature=+crt-static` | Exit 0, 40.439 s; both genuine images built, never launched | `build-1.log`, `build-1.json` |
| Retained read-only PE/whole-file inspection of those images using the producer's dependency decoder | Exit 0; AMD64/PE32+, explicit system-only imports, actual signatures `NotSigned` | `unsigned-build-inspection-2.json` |

The 21 cases cover exact closed inventory/schema/byte identity, BOM-free JSON,
unchanged existing output, signer/unsigned/missing-image refusal, exact and
non-duplicate profile identity/version/case, undeclared/missing/drifted members,
root/nested reparse refusal, existing packaging/signing order and driver checks,
system/CRT/vendor/unknown-API closure, source parsing, and malformed dependency
RVA, delayed imports and incomplete whole PE bytes. The signed public Windows
`where.exe` used in positive copy fixtures is labelled software fixture data;
it is never represented as a genuine ArkForge release or launched.

Unsigned genuine build facts (signing will necessarily change these digests):

| Image | Bytes | SHA-256 | Imported Windows libraries |
| --- | --- | --- | --- |
| `arkforge.exe` | 1,726,976 | `73835aa88db3d5e65f43d773b8202498f14261e3f175682b6d52ecee511caa30` | `api-ms-win-core-synch-l1-2-0.dll`, `SETUPAPI.dll`, `KERNEL32.dll`, `WINUSB.DLL`, `ADVAPI32.dll`, `bcrypt.dll`, `WINTRUST.dll`, `ntdll.dll` |
| `arkforged.exe` | 1,708,032 | `dc94d64e4b8f69e0200f315265c3d44c18bd7f7d09e35b48b24b8f6f606de85b` | `api-ms-win-core-synch-l1-2-0.dll`, `SETUPAPI.dll`, `KERNEL32.dll`, `WINUSB.DLL`, `ADVAPI32.dll`, `ntdll.dll` |

Earlier software checks are preserved: `software-1.log` passed the initial 16
cases; `software-2.log` and `software-3.log` passed 20 cases before the final
ordinal profile regression. The first unsigned-image inspection refused the
documented Windows synchronization API set omitted from the initial closed
list. After checking the primary Microsoft definition, only that exact name
was added and the unknown-API refusal remains covered; the same unchanged
unsigned images then passed. This is an initial producer-list defect/fix, not
an invalid load run. An earlier inline parser wrapper failed shell quoting
before parsing any source; the final script's actual parser case passed.

No Rust runtime source or contract/fixture bytes changed, so no unrelated
workspace test/clippy rerun was performed. The genuine release build is the
targeted native closure check. A complete production-signed package invocation,
driver installation, HDC/runtime launch, cross-account probe and physical AF-W1
were **not performed locally**. They require the actual protected release
certificate, redistributable HDC, WHDP-signed canonical catalog and qualified
Windows Loader/second-account acceptance environment. No hardware pass or
production-support claim follows from these checks.

## CI

CI is pending in the delivery PR. The Windows software workflow runs both the
producer and acceptance guard suites. Current-head CI results will be recorded
in that PR without replacing a reviewed or green source head.

## AF-W1 acceptance and matched delivery guards

The same delivery change repairs the actual production acceptance predicates:
exact Runtime epoch and selected HDC digest, passive status with no device Job,
exactly one registered DAYU200 Loader observation, and the published denied
account `daemon stop` route on the identical native Named Pipe path. Its result
must be the native supervisor-connect Win32 error 5. An uncertain denied-stop
probe retains the installation and never retries the stop.

The successful owner stop acknowledgement precedes the supervisor's native
exit in the current implementation. Acceptance therefore retains the original
supervisor and daemon process handles, proves PID/birth/image file identity and
the accepted whole SHA-256, and requires bounded exit of both original processes
before cleanup or a full receipt. There is no process kill or PID reacquisition.

The protected physical workflow exports the exact accepted outer package ZIP,
nested bundle ZIP, AF-W1 JSON and delivery hash/count receipt. Every ZIP member
is read whole against a unique canonical expected inventory; count-preserving
duplicate path substitution is rejected. The second account's password is
provided only to its credential step and removed before the probe, and is not
written to evidence.

Local targeted checks: `packaging/windows/Test-WindowsAcceptanceContract.ps1`
passed 31/31 software checks, zero failed/skipped, exit 0 in 2.2 seconds. Logs
are retained under `D:/src/ArkDeck-wt/tools/logs/arkforge-acceptance-20261007/`
(`acceptance-uncertainty-final.log`, `acceptance-static-uncertainty-final.log`,
`source-checks-uncertainty-final.json`). The source parser, workflow YAML and
diff check passed. Independent producer/acceptance cross-reviews found no
remaining actionable issue after the native drain and unique archive fixes.
These checks used software/native-file witnesses only; no ArkForge/HDC image,
Runtime, board or real second-account acceptance was executed.

A read-only GitHub check found zero self-hosted runners and no
`windows-production` environment (HTTP 404, including variables/secret-name
endpoints; no secret values were read). The Windows README now supplies the
precise qualified runner labels, protected environment, five variable names,
one second-account secret, genuine signing/WHDP/HDC prerequisites, default-branch
workflow entry and matched artifact retrieval. AF-W1 covers packaging, ACL, HDC
self-test and Loader discovery; destructive flash and evidence-backed support
promotion remain separate HardwareCampaign/Runtime gates.
