//! SHA-256 (FIPS 180-4). Public vectors, re-emitted in the fixture shape so a
//! port's runner treats them like every other suite.

use crate::json::Json;
use crate::suites::case_id;
use crate::tree::{Case, Tree};
use arkforge_core::digest::Sha256;

const SUITE: &str = "sha256";

struct Vector {
    title: &'static str,
    input: Input,
}

enum Input {
    Literal(&'static [u8]),
    Repeat(u8, usize),
}

pub fn populate(tree: &mut Tree) {
    let vectors = [
        Vector {
            title: "empty input",
            input: Input::Literal(b""),
        },
        Vector {
            title: "FIPS 180-4 'abc'",
            input: Input::Literal(b"abc"),
        },
        Vector {
            title: "FIPS 180-4 two-block message (56 bytes)",
            input: Input::Literal(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        },
        Vector {
            title: "FIPS 180-4 four-block message (112 bytes)",
            input: Input::Literal(
                b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu",
            ),
        },
        Vector {
            title: "exactly one block (64 bytes)",
            input: Input::Repeat(b'a', 64),
        },
        Vector {
            title: "one byte short of the padding boundary (55 bytes)",
            input: Input::Repeat(b'a', 55),
        },
        Vector {
            title: "NIST one million 'a'",
            input: Input::Repeat(b'a', 1_000_000),
        },
    ];

    for (index, vector) in vectors.iter().enumerate() {
        let bytes: Vec<u8> = match vector.input {
            Input::Literal(bytes) => bytes.to_vec(),
            Input::Repeat(byte, count) => vec![byte; count],
        };
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let digest = hasher.finalize();

        let (input_json, files) = match vector.input {
            Input::Literal(literal) => (
                Json::object(vec![
                    ("ascii", Json::str(String::from_utf8_lossy(literal))),
                    ("length", Json::Unsigned(literal.len() as u64)),
                ]),
                vec![("input.bin", bytes)],
            ),
            Input::Repeat(byte, count) => (
                Json::object(vec![
                    ("repeatByte", Json::Unsigned(byte as u64)),
                    ("count", Json::Unsigned(count as u64)),
                    (
                        "note",
                        Json::str("input is generated, not stored: `count` copies of `repeatByte`"),
                    ),
                ]),
                Vec::new(),
            ),
        };

        tree.case(
            &Case {
                id: case_id("SHA256", index as u32 + 1),
                suite: SUITE,
                title: vector.title.to_string(),
                requirements: vec!["AF-DIG-001"],
                kind: "digest",
                description: "SHA-256 of the input bytes, lowercase hex.".to_string(),
                input: input_json,
                expected: Json::object(vec![("sha256", Json::str(digest.to_hex()))]),
            },
            files,
        );
    }
}
