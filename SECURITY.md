# Security policy

ArkForge seals firmware plans and can write devices. Report vulnerabilities
privately so a fix can land before the details are public.

## Reporting

Use a GitHub private vulnerability report:

<https://github.com/ArkDeck/ArkForge/security/advisories/new>

Include what you can:

- affected commit or binary, and the host OS
- the component (`arkforge`, `arkforged`, a crate, the Swift SDK, packaging, or a script)
- a description of the impact and a minimal reproduction that does not require a maintainer to flash an unsuspecting device
- whether the issue is a digest, permit, journal, USB, signing, path, or privilege boundary

Do not open a public GitHub issue for a vulnerability. Do not include pairing
secrets, private keys, credentials, or firmware images you do not have rights
to share.

If you cannot use GitHub advisories, contact the maintainer listed in
[`.github/CODEOWNERS`](.github/CODEOWNERS) through a private channel and point
them at this file.

## Supported revisions

Development consumers pin a full git commit on `main`. The Cargo package
version is `0.1.0`. This repository does not publish git tags or GitHub
Releases; there is no tagged security branch.

| Revision | Security fixes |
| --- | --- |
| `main` (package `0.1.0`) | Accepted through the private report above |
| Git tags and GitHub Releases | Not published |

Contract identity strings such as `arkforge-contract-v1.0.0` are in-process
labels, not release tags. See [docs/arkdeck-contract.md](docs/arkdeck-contract.md).

## What we treat as a security issue

- Plan, permit, journal, or digest forgery, including a change that makes an
  unauthorized write look acknowledged
- Replay of an outcome-unknown write, or a rescue path that silently falls
  back to a normal flash
- Authentication or signing bypass in the daemon, IPC handshake, or Windows
  package
- Path traversal, symlink escape, or another flaw that reads or writes outside
  the intended store
- A dependency or build change that breaks the zero third-party runtime
  dependency rule in a way that hides code from review

Device damage caused by an explicitly authorized plan against the firmware the
operator supplied is a product bug when the plan was wrong, and a security
issue when the plan was accepted without the consent or identity the code
claims to require. Hardware-qualification gaps that are already documented as
unsupported (an empty production registry, DAYU600 execution gates) are not
vulnerabilities by themselves.

## Hardening notes for reporters

- Unknown outcomes are not success. Do not treat a crash during a write as a
  clean failure.
- `--hardware-campaign` does not skip safety gates.
- There is no broad `--yes` or `--force` for apply.
