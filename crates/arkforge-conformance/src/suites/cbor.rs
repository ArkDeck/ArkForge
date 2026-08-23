//! RFC 8949 §4.2 deterministic CBOR — the digest model.
//!
//! Two halves: `encode` cases give a value and the one byte string it MUST
//! encode to (and MUST decode back from); `decode` cases give bytes a
//! canonical decoder MUST refuse. The rejection *class* recorded is the one
//! the reference implementation reports; the requirement on a port is to
//! reject, and to report a class from `spec/errors/registry.yaml`'s
//! `cbor` family.

use crate::cbor_repr::{diag, typed_json};
use crate::json::{Json, hex, unhex};
use crate::suites::case_id;
use crate::tree::{Case, Tree};
use arkforge_core::digest::{CborError, CborValue, decode_canonical};

const SUITE: &str = "canonical-cbor";

fn text(value: &str) -> CborValue {
    CborValue::text(value)
}

fn encode_cases() -> Vec<(&'static str, CborValue)> {
    use CborValue::*;
    let mut cases: Vec<(&'static str, CborValue)> = vec![
        ("unsigned 0 (single byte head)", Unsigned(0)),
        ("unsigned 23 (largest immediate)", Unsigned(23)),
        ("unsigned 24 (first 1-byte argument)", Unsigned(24)),
        ("unsigned 255 (largest 1-byte argument)", Unsigned(255)),
        ("unsigned 256 (first 2-byte argument)", Unsigned(256)),
        ("unsigned 65535 (largest 2-byte argument)", Unsigned(65_535)),
        ("unsigned 65536 (first 4-byte argument)", Unsigned(65_536)),
        (
            "unsigned 2^32-1 (largest 4-byte argument)",
            Unsigned(u32::MAX as u64),
        ),
        (
            "unsigned 2^32 (first 8-byte argument)",
            Unsigned(u32::MAX as u64 + 1),
        ),
        ("unsigned u64 max", Unsigned(u64::MAX)),
        (
            "epoch milliseconds (1770000000000)",
            Unsigned(1_770_000_000_000),
        ),
        ("negative -1 (encodes as major 1, argument 0)", Negative(-1)),
        ("negative -24 (largest immediate)", Negative(-24)),
        ("negative -25 (first 1-byte argument)", Negative(-25)),
        ("negative -256", Negative(-256)),
        ("negative -257 (first 2-byte argument)", Negative(-257)),
        ("negative i64 min", Negative(i64::MIN)),
        ("empty byte string", Bytes(vec![])),
        (
            "32-byte digest-shaped byte string",
            Bytes((0u8..32).collect()),
        ),
        (
            "byte string of length 24 (head switches to 1-byte argument)",
            Bytes(vec![0xab; 24]),
        ),
        ("empty text", Text(String::new())),
        ("ascii text", text("arkforge")),
        ("text with 2-byte UTF-8 (ü)", text("\u{00fc}")),
        ("text with 3-byte UTF-8 (中文)", text("中文")),
        ("text with 4-byte UTF-8 (U+1F600)", text("\u{1F600}")),
        ("text with quote and backslash", text("\"\\")),
        (
            "text of length 23 (largest immediate length)",
            text(&"x".repeat(23)),
        ),
        (
            "text of length 24 (first 1-byte length)",
            text(&"x".repeat(24)),
        ),
        ("false", Bool(false)),
        ("true", Bool(true)),
        ("null", Null),
        ("empty array", Array(vec![])),
        (
            "array of three unsigned",
            Array(vec![Unsigned(1), Unsigned(2), Unsigned(3)]),
        ),
        (
            "array of 24 elements (head switches to 1-byte argument)",
            Array((0..24).map(Unsigned).collect()),
        ),
        ("empty map", Map(vec![])),
        (
            "map: shorter key sorts first regardless of lexical order ({\"b\":2,\"aa\":1})",
            Map(vec![(text("aa"), Unsigned(1)), (text("b"), Unsigned(2))]),
        ),
        (
            "map: keys of equal length sort bytewise ({\"a\":1,\"b\":2})",
            Map(vec![(text("b"), Unsigned(2)), (text("a"), Unsigned(1))]),
        ),
        (
            "map: integer key 10 sorts before text key \"a\" (major type order)",
            Map(vec![(text("a"), Unsigned(1)), (Unsigned(10), Unsigned(2))]),
        ),
        (
            "map: unsigned 256 (3-byte head) sorts after unsigned 255 (2-byte head)",
            Map(vec![(Unsigned(256), Null), (Unsigned(255), Null)]),
        ),
        (
            "map: multibyte UTF-8 key sorts after ASCII key of same char count",
            Map(vec![(text("é"), Unsigned(1)), (text("e"), Unsigned(2))]),
        ),
        (
            "nested: map with array and map values, written out of order",
            Map(vec![
                (
                    text("z"),
                    Array(vec![Unsigned(1), Map(vec![(text("k"), Null)])]),
                ),
                (text("a"), Bytes(vec![0xde, 0xad])),
                (
                    text("m"),
                    Map(vec![(text("b"), Bool(true)), (text("a"), Bool(false))]),
                ),
            ]),
        ),
        (
            "permit-shaped map (all value kinds a permit uses)",
            Map(vec![
                (text("permitId"), text("PERMIT-STEP-ENSURE-MODE")),
                (text("singleUse"), Bool(true)),
                (text("issuedAtEpochMs"), Unsigned(1_770_000_000_000)),
                (text("planDigest"), Bytes(vec![0x11; 32])),
                (
                    text("authorityBinding"),
                    Map(vec![
                        (text("bindingRevision"), Unsigned(3)),
                        (text("bindingId"), text("BINDING-VECTOR")),
                    ]),
                ),
            ]),
        ),
    ];
    // Depth exactly at the limit is accepted; one deeper is rejected (see the
    // decode half). MAX_DEPTH is 64 and depth counts from 0 at the root.
    let mut nested = Unsigned(1);
    for _ in 0..64 {
        nested = Array(vec![nested]);
    }
    cases.push(("array nested 64 deep (the maximum accepted depth)", nested));
    cases
}

fn decode_reject_cases() -> Vec<(&'static str, Vec<u8>)> {
    let mut cases: Vec<(&'static str, Vec<u8>)> = vec![
        ("non-shortest head: 0x18 0x05 for 5", unhex("1805")),
        ("non-shortest head: 0x19 0x00ff for 255", unhex("1900ff")),
        (
            "non-shortest head: 0x1a 0000ffff for 65535",
            unhex("1a0000ffff"),
        ),
        (
            "non-shortest head: 0x1b 00000000ffffffff for 2^32-1",
            unhex("1b00000000ffffffff"),
        ),
        ("reserved additional info 28", unhex("1c")),
        ("indefinite-length array", unhex("9f01ff")),
        ("indefinite-length byte string", unhex("5f4101ff")),
        ("indefinite-length map", unhex("bf616101ff")),
        ("half-precision float 0.0", unhex("f90000")),
        ("single-precision float 1.0", unhex("fa3f800000")),
        ("double-precision float 1.0", unhex("fb3ff0000000000000")),
        (
            "tag 0 (date-time)",
            unhex("c074323031332d30332d32315432303a30343a30305a"),
        ),
        ("simple value undefined (23)", unhex("f7")),
        ("simple value 32 via 1-byte argument", unhex("f820")),
        (
            "map keys unsorted: {\"b\":1,\"a\":2}",
            unhex("a2616201616102"),
        ),
        (
            "map keys unsorted: longer key before shorter ({\"aa\":1,\"b\":2})",
            unhex("a262616101616202"),
        ),
        ("duplicate map key", unhex("a2616101616102")),
        ("truncated: array declares 3 items, has 2", unhex("830102")),
        (
            "truncated: byte string declares 4 bytes, has 3",
            unhex("44010203"),
        ),
        (
            "truncated: head promises 8-byte argument, has 4",
            unhex("1b00000000"),
        ),
        ("trailing bytes after a complete value", unhex("0102")),
        ("invalid UTF-8 in text string", unhex("62c328")),
        (
            "negative below i64 min (-2^64)",
            unhex("3bffffffffffffffff"),
        ),
        ("empty input", Vec::new()),
    ];
    let mut too_deep = vec![0x81u8; 65];
    too_deep.push(0x01);
    cases.push(("array nested 65 deep (one past the depth limit)", too_deep));
    cases
}

fn error_class(error: &CborError) -> &'static str {
    match error {
        CborError::NegativeNotNegative(_) => "CBOR_NEGATIVE_NOT_NEGATIVE",
        CborError::DuplicateMapKey(_) => "CBOR_DUPLICATE_MAP_KEY",
        CborError::Truncated => "CBOR_TRUNCATED",
        CborError::NonCanonicalHead => "CBOR_NON_CANONICAL_HEAD",
        CborError::UnsupportedMajorType(_) => "CBOR_UNSUPPORTED_MAJOR_TYPE",
        CborError::UnsupportedSimpleValue(_) => "CBOR_UNSUPPORTED_SIMPLE_VALUE",
        CborError::IndefiniteLength => "CBOR_INDEFINITE_LENGTH",
        CborError::UnsortedMapKeys => "CBOR_UNSORTED_MAP_KEYS",
        CborError::InvalidUtf8 => "CBOR_INVALID_UTF8",
        CborError::TrailingBytes(_) => "CBOR_TRAILING_BYTES",
        CborError::DepthLimit => "CBOR_DEPTH_LIMIT",
    }
}

pub fn populate(tree: &mut Tree) {
    let mut number = 0u32;
    for (title, value) in encode_cases() {
        number += 1;
        let encoded = value.to_canonical_bytes().expect("encodable");
        // The decoder yields map entries in canonical order, which may differ
        // from the order the value was written in; equality is on the bytes.
        let decoded = decode_canonical(&encoded).expect("round trip");
        assert_eq!(
            decoded.to_canonical_bytes().unwrap(),
            encoded,
            "round trip for {title}"
        );
        tree.case(
            &Case {
                id: case_id("CBOR", number),
                suite: SUITE,
                title: title.to_string(),
                requirements: vec!["AF-DIG-003", "AF-DIG-004", "AF-DIG-005"],
                kind: "encode",
                description: "The value encodes to exactly `expected.hex` and decodes back \
                              to the same value. `input.diag` is RFC 8949 §8 notation; \
                              `input.value` is the typed JSON form."
                    .to_string(),
                input: Json::object(vec![
                    ("diag", Json::str(diag(&value))),
                    ("value", typed_json(&value)),
                ]),
                expected: Json::object(vec![
                    ("hex", Json::str(hex(&encoded))),
                    ("roundTrip", Json::Bool(true)),
                ]),
            },
            vec![("encoded.cbor", encoded)],
        );
    }

    // An encoder-side rejection: a value the canonical model refuses to encode.
    number += 1;
    let duplicate = CborValue::Map(vec![
        (text("a"), CborValue::Unsigned(1)),
        (text("a"), CborValue::Unsigned(2)),
    ]);
    let error = duplicate.to_canonical_bytes().unwrap_err();
    tree.case(
        &Case {
            id: case_id("CBOR", number),
            suite: SUITE,
            title: "encoder refuses a map with duplicate keys".to_string(),
            requirements: vec!["AF-DIG-006"],
            kind: "encode",
            description: "A value with two equal keys has no canonical encoding; the \
                          encoder MUST refuse rather than keep either entry."
                .to_string(),
            input: Json::object(vec![("value", typed_json(&duplicate))]),
            expected: Json::object(vec![
                ("result", Json::str("reject")),
                ("errorClass", Json::str(error_class(&error))),
            ]),
        },
        Vec::new(),
    );

    for (title, bytes) in decode_reject_cases() {
        number += 1;
        let error = decode_canonical(&bytes)
            .err()
            .unwrap_or_else(|| panic!("expected rejection for {title}"));
        tree.case(
            &Case {
                id: case_id("CBOR", number),
                suite: SUITE,
                title: title.to_string(),
                requirements: vec!["AF-DIG-007"],
                kind: "decode",
                description: "A canonical decoder MUST reject these bytes. `errorClass` is \
                              the class the reference implementation reports; a port \
                              MUST reject and SHOULD report the same class."
                    .to_string(),
                input: Json::object(vec![("hex", Json::str(hex(&bytes)))]),
                expected: Json::object(vec![
                    ("result", Json::str("reject")),
                    ("errorClass", Json::str(error_class(&error))),
                ]),
            },
            Vec::new(),
        );
    }
}
