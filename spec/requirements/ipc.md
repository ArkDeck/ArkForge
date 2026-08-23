# Local IPC: framing, sessions, Protobuf wire subset
status: normative
area: IPC
rationale: architecture.md §15, §23, proto/arkforge.proto (normative schema)
conformance: protobuf

### AF-IPC-001 — frame
status: normative
tests: [AF-CONF-PB-031]

A frame is a 4-byte big-endian unsigned length followed by that many bytes of
one Protobuf message. A clean end of stream between frames is not an error; a
cut inside a frame is (`PB_TRUNCATED`).

### AF-IPC-002 — frame bound
status: normative
tests: [AF-CONF-PB-031]

A declared length greater than 16 777 216 MUST be refused before allocation
(`PB_FRAME_TOO_LARGE`).

### AF-IPC-003 — handshake
status: normative
tests: [AF-CONF-PB-001..004, AF-CONF-PB-031]

The first frame on a connection is `Hello{protocol_major, protocol_minor,
session_kind}`; the daemon answers `HelloAck`. A peer whose major differs from
the daemon's (1) is refused with `HelloAck.refusal`; minor differences are
accepted in both directions (unknown fields skip). `HelloAck` also carries the
standing execution facts (`execution_ready`, `execution_blockers`,
`toolchain_id`, `toolchain_sha256`).

### AF-IPC-004 — two session kinds
status: normative
tests: [AF-CONF-PB-018]

`public` may call `inspectArtifact`, `discoverDevices`, `probeDevice`,
`materializePlan` (assessment only), `watchJob`, `getRecoveryGuide`, `getJob`,
`listJobs`. Only `controller` may call `importArtifact`, `startExecution`,
`cancelJob`, `reconcileJob`, `planSupersedingRecovery`, `submitStepPermit`,
`submitManagedControlReceipt`. A public call to a controller API is refused
(`SESSION_NOT_PERMITTED`); `startExecution` is never reachable from the public
endpoint.

### AF-IPC-005 — request/response correlation
status: normative
tests: [AF-CONF-PB-005, AF-CONF-PB-006]

Every `Request` carries a `request_id`; every `Response` echoes it. Streams
number responses with `stream_sequence` (0 for a single reply) and end with
`stream_end = true`. `watchJob` accepts `from_sequence` to resume.

### AF-IPC-006 — the controller secret
status: draft
source: crates/arkforged/src/main.rs, crates/arkforge-standalone/src/supervisor.rs
tests: []

The pairing secret is handed to the daemon by the process that starts it
(inherited stdin/handle), lives only in memory, and defines the pairing epoch.
Each daemon start rotates the epoch. The reference daemon exposes the epoch on
`daemon status` and in `SubmitStepPermitRequest.pairing_epoch`.

## Protobuf wire subset (AF-CONF-PB-001..030)

### AF-IPC-010 — proto3 encoding
status: normative
tests: [AF-CONF-PB-001..017]

Messages are encoded per `proto/arkforge.proto` with: varint for integers,
bools and enums; length-delimited for strings, bytes and messages; fields
written in ascending field-number order; proto3 defaults (0, false, empty)
omitted; repeated message fields as one tag per element (no packing).

### AF-IPC-011 — round trip
status: normative
tests: [AF-CONF-PB-001..017]

Decoding an encoded message and re-encoding it MUST reproduce the bytes.

### AF-IPC-012 — enums
status: normative
tests: [AF-CONF-PB-018, AF-CONF-PB-020..022]

Zero is `UNSPECIFIED` for every enum. A required enum field that is absent or
explicitly zero MUST be refused (`PB_MISSING_FIELD`); any value the build does
not know MUST be refused (`PB_UNKNOWN_ENUM_VALUE`), never defaulted. The
`Api`, `Status`, `SessionKind`, `JobEventKind` and `ManagedControlAction`
values and text spellings are in AF-CONF-PB-018.

### AF-IPC-013 — unknown fields are skipped
status: normative
tests: [AF-CONF-PB-019]

A field number the decoder does not know is skipped (forward compatibility);
field numbers are never reused.

### AF-IPC-014 — required enum absent
status: normative
tests: [AF-CONF-PB-020..022]

### AF-IPC-015 — decoder limits
status: normative
tests: [AF-CONF-PB-023..028, AF-CONF-PB-030]

The decoder MUST refuse: field number 0; wire types other than 0, 1, 2, 5
(groups are not in the subset); varints longer than 10 bytes; a length that
overruns the message; a message that ends mid-field; nesting deeper than 16
messages; a wire type that does not match the field's declared type.

### AF-IPC-016 — strings are UTF-8
status: normative
tests: [AF-CONF-PB-029]

A `string` field with invalid UTF-8 MUST be refused naming the field.

### AF-IPC-017 — the permit is opaque on the wire
status: normative
tests: [AF-CONF-PB-009]

`SubmitStepPermitRequest.permit_cbor` carries the exact signed bytes
(AF-AUTH-012); the IPC layer MUST NOT parse or re-encode them.

### AF-IPC-018 — endpoint
status: draft
source: crates/arkforge-platform/src/platform.rs
tests: [crates/arkforge-platform/tests/local_roundtrip.rs]

Unix: a Unix domain socket in the runtime directory. Windows: a byte-mode
local-only Named Pipe (`PIPE_REJECT_REMOTE_CLIENTS`, `FILE_FLAG_FIRST_PIPE_INSTANCE`,
an explicit DACL containing only the current logon SID; clients connect with
identification-level SQOS). Both carry the same framing and schema; a port MUST
NOT introduce a second business API per platform. See `ports/ipc-framing.md`.
