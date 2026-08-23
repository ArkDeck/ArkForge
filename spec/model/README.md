# `spec/model/`
status: see each file

| file | what | authority |
|---|---|---|
| `digest-bodies.cddl` | every CBOR map that is digested or MAC'ed, by domain | fixtures under `conformance/v1/{permit,admission,journal,plan,action-receipt,reconcile}` are authoritative over the prose; rules marked `[fixture]` are normative, the rest draft |
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
| `arkforge/v1/device-observation\0` | `device-observation` | normative (plan 003) |
| `arkforge/v1/admission-device-facts\0` | `device-facts-for-admission` | normative (admission + plan) |
| `arkforge/v1/usb-topology\0` | 4-byte big-endian location id | draft |
| `arkforge/v1/usb-descriptor\0` | ordered descriptor payload | draft |
| `arkforge/v1/device-serial\0` | raw serial/connect-key bytes | draft |
| `arkforge/v1/rescue-device-facts\0` | `rescue-device-facts` | draft |
| `arkforge/v1/transcript\0` | `transcript.md` | draft (plan 003 pins the digest) |
| `arkforge/v1/possible-effect-set\0` | `possible-effect-set` | normative (reconcile 001) |
| `arkforge/v1/provider-facts\0` | `maturity-key` | draft |
| `arkforge/v1/toolchain-facts\0`, `artifact-facts\0` | `toolchain-identity`, `artifact-identity` | draft |
| `arkforge/v1/recovery-coverage\0` | `device-profile.recovery` | draft |
| `arkforge/v1/transport-session\0` | session facts | draft (gap: body not yet documented) |
| `arkforge/v1/rescue-plan\0`, `rescue-receipt\0` | rescue surface | gap |
| `arkforge/v1/authority-support-key\0` | standalone support key | gap |
| `arkforge/v1/action-receipt\0` | `action-receipt` | normative (action-receipt suite) |

## Offline validation

`cargo run -p arkforge-conformance -- validate` is dependency-free and runs in
the offline build. It parses both Draft 2020-12 schemas, evaluates the exact
keyword/regex subset they use against every published profile/transcript,
also runs the semantic loaders, checks CDDL delimiter structure, and requires
all `Domain::ALL` strings to be declared in `digest-bodies.cddl`. An unknown
regex or unresolved `$ref` fails closed.
