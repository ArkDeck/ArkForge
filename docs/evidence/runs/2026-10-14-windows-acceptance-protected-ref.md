# AF-W1 protected release source preflight

Date: 2026-10-14. Software regression only; no physical acceptance ran.
Base: `4bef017e9b6c064a283817b5756e2f7c1717082d`.

The physical acceptance workflow required a protected default-branch source in
its error message but checked only the ref name. It could therefore proceed
from an unprotected default branch if release inputs were supplied. The actual
preflight now also requires the GitHub-provided `GITHUB_REF_PROTECTED` value to
be exactly `true`, before package signing or installation.

The software contract suite executes that real preflight block with temporary
non-credential fixture values and restores the caller's environment afterwards.
It accepts a protected default branch with complete inputs, rejects false,
missing and malformed protection signals, rejects protected feature branches,
tags and PR refs, and rejects each missing production input. Fixtures neither
configure GitHub protection nor dispatch an acceptance job.

## Local targeted checks

- `pwsh -NoProfile -File packaging/windows/Test-WindowsAcceptanceContract.ps1`:
  exit 0, 35 checks passed. Log:
  `afw1-protected-ref-contract-native-20261014.log` in the local task tools
  directory. The initial sandbox attempt failed because the existing junction
  fixture was denied; its original log is retained separately as
  `afw1-protected-ref-contract-20261014.log`. The successful native retry used
  the unchanged fixture and assertions.
- `git diff --check`: exit 0.
- No local Rust build or device test was needed for this workflow/PowerShell
  change. The independent Windows long soak remains active.

## CI

Current-head Windows and Rust/Swift workflows are pending the first push at this
commit. The PR records their actual run IDs and results when available.

## Remaining physical prerequisites

The readback found no AF-W1 workflow runs, published releases or GitHub
environments. This repair does not provision a production signer, signed
canonical driver package, protected release environment, second account,
runner or Loader. It establishes no AF-W1 or GJ4 hardware result.
