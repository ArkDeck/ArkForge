# Digests: SHA-256, HMAC, deterministic CBOR, domain separation
status: normative
area: DIG
rationale: architecture.md §6.2, §15.4, AFD-0001
conformance: sha256, hmac-sha256, canonical-cbor, permit, admission, plan, journal, action-receipt

Every security digest in ArkForge is `SHA-256(domain || payload)`. Protobuf
carries wire compatibility; deterministic CBOR carries meaning that must hash
identically on both sides of the authority boundary. Nothing that can make two
honest encoders disagree (floats, indefinite lengths, tags, unsorted keys,
host paths, locale text) is admitted into the digest model.

### AF-DIG-001 — SHA-256 is FIPS 180-4
status: normative
tests: [AF-CONF-SHA256-001..007]

An implementation MUST compute SHA-256 exactly as FIPS 180-4 specifies,
including for the empty message, single-block, multi-block and one-million-byte
inputs. Digest text form is lowercase hex, 64 characters, no prefix
(`model/vocabularies.yaml#digest-text-forms`).

### AF-DIG-002 — HMAC-SHA-256 is RFC 2104 with a 64-byte block
status: normative
tests: [AF-CONF-HMAC-001..008]

`HMAC(key, msg)` MUST hash a key longer than 64 bytes with SHA-256 first, zero-pad
a shorter key to 64 bytes, and use ipad `0x36` / opad `0x5c`. The permit
integrity tag (AF-AUTH-011) is this function; nothing else derives keys.

### AF-DIG-003 — the digest model is the canonical CBOR subset
status: normative
tests: [AF-CONF-CBOR-001..043]

Values that enter a digest are drawn from exactly: unsigned integers (major 0),
negative integers (major 1, down to −2^63), byte strings, UTF-8 text strings,
arrays, maps, `false`, `true`, `null`. Floats, tags, simple values other than
20/21/22, and indefinite-length items MUST NOT be produced.

### AF-DIG-004 — encoding is RFC 8949 §4.2.1 deterministic
status: normative
tests: [AF-CONF-CBOR-001..043]

Every head MUST use the shortest form that fits its argument (0–23 immediate;
then 1, 2, 4, 8 bytes). Map keys MUST be sorted by the bytewise lexicographic
order of their *encoded* key bytes — so a shorter key always precedes a longer
one, an integer key precedes a text key, and `"b"` precedes `"aa"`.

### AF-DIG-005 — decoding a digest body re-encodes identically
status: normative
tests: [AF-CONF-CBOR-001..043, AF-CONF-PERMIT-027]

For any bytes accepted by the canonical decoder, re-encoding the decoded value
MUST reproduce the input byte for byte. A consumer that verifies a tag or digest
over received bytes MUST verify over the received bytes, never over a
re-encoding.

### AF-DIG-006 — duplicate map keys have no canonical encoding
status: normative
tests: [AF-CONF-CBOR-044]

An encoder MUST refuse a map containing two equal keys rather than keep either.

### AF-DIG-007 — the canonical decoder fails closed
status: normative
tests: [AF-CONF-CBOR-045..069]

The decoder MUST reject: non-shortest heads; additional-information values
28–30; indefinite lengths; floats; tags; simple values other than 20/21/22;
unsorted or duplicate map keys; truncated items; trailing bytes after the root
value; invalid UTF-8 in a text string; negative integers below −2^63; nesting
deeper than 64 levels (depth counted from 0 at the root; 64 nested arrays are
accepted, 65 are not); and empty input. It MUST report a class from the `cbor`
family of `errors/registry.yaml`; the exact class per input is informative
(`expected.errorClass`).

### AF-DIG-008 — domain separation prefixes
status: normative
tests: [AF-CONF-PERMIT-001, AF-CONF-ADMISSION-001, AF-CONF-JOURNAL-001, AF-CONF-PLAN-001..009]

A digest is `SHA-256(domainBytes || payload)` where `domainBytes` is the exact
ASCII string from `model/digest-bodies.cddl` **including its trailing NUL
byte**. Domain strings are part of the wire contract: changing one is a
schema-version event. Two different domains MUST yield different digests for
the same payload.

### AF-DIG-009 — digests of ordered digest lists
status: normative
tests: [AF-CONF-PLAN-007]

Where the spec says a digest is over an ordered list of digests
(`providerExecutionPlanDigest`), the payload is the plain concatenation of the
32-byte digests in order, with no length prefixes and no CBOR framing.

### AF-DIG-010 — secret comparison is constant-time
status: draft
tests: []
gap: not observable from outside; verified by review.

A tag comparison MUST NOT short-circuit on the first differing byte.

### AF-DIG-011 — what may not enter a digest body
status: normative
tests: [AF-CONF-PLAN-009]

A digest body MUST NOT contain floats, host file-system paths, localized text,
or identifiers outside the grammar of AF-ID-001. Free text is permitted only in
fields the model explicitly types as text (`blocker`, `campaign`, `reason`,
`summary`, `rationale`, fact values).

### AF-DIG-012 — ActionReceipt has a canonical semantic identity
status: normative
tests: [AF-CONF-RECEIPT-001..005]

`receiptDigest = SHA-256("arkforge/v1/action-receipt\0" ||
deterministic_cbor(action-receipt))`, using the exact body in
`model/digest-bodies.cddl`. It covers identity, disposition, evidence,
verification fields and the facts map; it is never the evidence digest alone
and never the protobuf bytes. Required IDs and fact keys use OpaqueId,
`evidenceSha256` is exactly 32 bytes, duplicate fact keys reject the receipt,
and verification strength is present only for `verified`.
