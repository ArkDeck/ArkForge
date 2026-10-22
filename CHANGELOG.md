# Changelog

All notable changes to ArkForge are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
The Cargo package version in the workspace manifest is the package version.
Contract identity strings are separate and are described in
[docs/arkdeck-contract.md](docs/arkdeck-contract.md). This repository does not
publish git tags or GitHub Releases. Consumers pin a full git commit.

## [Unreleased]

### Added

- Community documents: this changelog, `CONTRIBUTING.md`, `SECURITY.md`, and
  `CODE_OF_CONDUCT.md`.
- GitHub issue templates for bugs, features, and device reports, a pull
  request template, and `.github/CODEOWNERS`.
- A root `justfile` with `build`, `test`, `lint`, `fmt`, and `ci`.

### Changed

- `README.md` is English. The Chinese readme is `README.zh-CN.md`.
- The stage ledger `TASKS.md` and the OpenSpec change drafts `docs/openspec/`
  now live under `docs/archive/`. Evidence, decisions, and architecture stay
  in place.
- `proto/` is `api/` (`api/arkforge.proto` and `api/versions.yaml`). `scripts/`
  is `hack/`. `transcripts/` is `test/transcripts/`. `spec/` is `docs/spec/`.
  `.gitattributes` keeps those conformance fixtures and transcripts as raw
  bytes on checkout.
- `arkforge` and `arkforged` live under `cmd/`. `arkforge-arkdeck-adapter`
  lives under `crates/`. Crate names are unchanged. `Package.swift` stays at
  the repository root.
