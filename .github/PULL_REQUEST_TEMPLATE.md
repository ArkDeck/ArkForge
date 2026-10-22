## Summary

<!-- What changed, and why. Name the crates, spec sections, or workflows. -->

## Spec, wire, and contract

<!-- Delete the lines that do not apply. -->

- [ ] No spec, digest, Protobuf, or ArkDeck contract change
- [ ] Spec or conformance fixtures changed, and `docs/spec/manifest.yaml` / `docs/spec/ISSUES.md` match the diff
- [ ] IPC or adapter contract constants changed, and `docs/arkdeck-contract.md` matches

This pull request does not create a git tag or a GitHub Release.

## Hardware

<!-- "None" is the usual answer. A green test run is not device acceptance. -->

- [ ] No device was flashed, reset, or rescued for this change
- [ ] A named hardware campaign was already authorized, and the evidence record is linked below

## Test plan

<!-- Commands you ran, and what you could not run. -->

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- [ ] `cargo test --workspace --all-targets`
- [ ] `swift test` (required when the Swift SDK or the Rust/Swift wire changes)
