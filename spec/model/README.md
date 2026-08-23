# `spec/model/`
status: see each file

| file | what | authority |
|---|---|---|
| `digest-bodies.cddl` | every CBOR map that is digested or MAC'ed, by domain | fixtures under `conformance/v1/{permit,admission,journal,plan}` are authoritative over the prose; rules marked `[fixture]` are normative, the rest draft |
| `vocabularies.yaml` | every closed string set with its exact wire spelling, and the identifier grammar | draft; entries with `fixture:` are normative |
| `strict-yaml.md` | the YAML subset every profile/transcript/spec YAML is read with | draft; `conformance/v1/strict-yaml` is authoritative |
| `profile.schema.json` | JSON Schema (2020-12) of the DeviceProfile document as loaded | draft |
| `profile` invariants | `requirements/profile.md` AF-PROF-010..023 | |
| `transcript.schema.json`, `transcript.md` | transcript document and its digest/replay semantics | draft |

Outside this directory but part of the model: `proto/arkforge.proto` (IPC wire
schema, normative) and `profiles/*.yaml` (published data).

## Domain coverage

| domain | rule | status |
|---|---|---|
| (HMAC, no domain) permit | `permit-signing-body` | normative (permit suite) |
| `arkforge/v1/admission-snapshot\0` | `admission-snapshot` | normative (admission suite) |
| `arkforge/v1/journal-record\0` | `journal-record-body` | normative (journal suite) |
| `arkforge/v1/plan\0` | `plan-body` | normative (plan 009) |
| `arkforge/v1/public-step\0` | `public-step` | normative (plan 006) |
| `arkforge/v1/private-action\0` | `private-action` | normative (plan 005) |
| `arkforge/v1/provider-execution-plan\0` | ordered digests | normative (plan 007) |
| `arkforge/v1/public-projection\0` | `[action-digest-binding]` | normative (plan 007) |
| `arkforge/v1/effect-set\0` | `effect-set` | normative (plan 008) |
| `arkforge/v1/artifact-manifest\0` | `artifact-manifest` | normative (plan 001) |
| `arkforge/v1/device-profile\0` | `device-profile` | normative (plan 002) |
| `arkforge/v1/device-facts\0` | `device-observation` and four raw preimages | draft (plan 003 pins one) — ISSUES SI-008 |
| `arkforge/v1/transcript\0` | `transcript.md` | draft (plan 003 pins the digest) |
| `arkforge/v1/possible-effect-set\0` | `possible-effect-set` | draft |
| `arkforge/v1/provider-facts\0` | `maturity-key` | draft |
| `arkforge/v1/toolchain-facts\0`, `artifact-facts\0` | `toolchain-identity`, `artifact-identity` | draft |
| `arkforge/v1/recovery-coverage\0` | `device-profile.recovery` | draft |
| `arkforge/v1/transport-session\0` | session facts | draft (gap: body not yet documented) |
| `arkforge/v1/rescue-plan\0`, `rescue-receipt\0` | rescue surface | gap |
| `arkforge/v1/authority-support-key\0` | standalone support key | gap |
| `arkforge/v1/action-receipt\0` | reserved; no body digested today | ISSUES SI-009 |

## Tooling gap

No JSON Schema or CDDL validator is vendored in this repository (AFD-0001,
offline environment). The schemas were checked by reading the loaders; the
profiles and transcripts are validated by the Rust loaders in CI. Adding an
offline validator to the conformance crate is tracked in `conformance/README.md`.
