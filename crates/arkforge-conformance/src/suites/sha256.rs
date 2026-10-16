//! SHA-256 (FIPS 180-4) and a reviewed subset of the NIST CAVP byte vectors.
//!
//! Expected digests are the published values. The generator checks the
//! reference implementation against them and refuses to emit a fixture whose
//! digest was computed only by this repository.

use crate::json::{Json, unhex};
use crate::suites::case_id;
use crate::tree::{Case, Tree};
use arkforge_core::digest::sha256;

const SUITE: &str = "sha256";
const CAVP: &str = include_str!("../../../arkforge-core/testdata/nist-sha256-cavp.rsp");

struct Vector {
    title: String,
    input: Input,
    /// Published lowercase hex digest. Not derived from `sha256`.
    expected: String,
}

enum Input {
    Literal(&'static [u8]),
    Repeat(u8, usize),
    Cavp {
        file: String,
        len_bits: u64,
        bytes: Vec<u8>,
    },
}

pub fn populate(tree: &mut Tree) {
    let mut vectors = fips_vectors();
    vectors.extend(cavp_vectors());

    for (index, vector) in vectors.iter().enumerate() {
        let bytes = match &vector.input {
            Input::Literal(literal) => literal.to_vec(),
            Input::Repeat(byte, count) => vec![*byte; *count],
            Input::Cavp { bytes, .. } => bytes.clone(),
        };
        let digest = sha256(&bytes).to_hex();
        assert_eq!(
            digest, vector.expected,
            "SHA-256 implementation diverges from the published vector {}",
            vector.title
        );

        let (input_json, files) = match &vector.input {
            Input::Literal(literal) => (
                Json::object(vec![
                    ("ascii", Json::str(String::from_utf8_lossy(literal))),
                    ("length", Json::Unsigned(literal.len() as u64)),
                ]),
                vec![("input.bin", bytes)],
            ),
            Input::Repeat(byte, count) => (
                Json::object(vec![
                    ("repeatByte", Json::Unsigned(*byte as u64)),
                    ("count", Json::Unsigned(*count as u64)),
                    (
                        "note",
                        Json::str("input is generated, not stored: `count` copies of `repeatByte`"),
                    ),
                ]),
                Vec::new(),
            ),
            Input::Cavp {
                file,
                len_bits,
                bytes,
            } => (
                Json::object(vec![
                    (
                        "source",
                        Json::str("NIST CAVP CAVS 11.0 byte-oriented SHA-256"),
                    ),
                    ("file", Json::str(file.clone())),
                    ("lenBits", Json::Unsigned(*len_bits)),
                    ("length", Json::Unsigned(bytes.len() as u64)),
                ]),
                vec![("input.bin", bytes.clone())],
            ),
        };

        tree.case(
            &Case {
                id: case_id("SHA256", index as u32 + 1),
                suite: SUITE,
                title: vector.title.clone(),
                requirements: vec!["AF-DIG-001"],
                kind: "digest",
                description: "SHA-256 of the input bytes, lowercase hex.".to_string(),
                input: input_json,
                expected: Json::object(vec![("sha256", Json::str(vector.expected.clone()))]),
            },
            files,
        );
    }
}

fn fips_vectors() -> Vec<Vector> {
    vec![
        Vector {
            title: "empty input".into(),
            input: Input::Literal(b""),
            expected: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into(),
        },
        Vector {
            title: "FIPS 180-4 'abc'".into(),
            input: Input::Literal(b"abc"),
            expected: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into(),
        },
        Vector {
            title: "FIPS 180-4 two-block message (56 bytes)".into(),
            input: Input::Literal(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            expected: "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1".into(),
        },
        Vector {
            title: "FIPS 180-4 four-block message (112 bytes)".into(),
            input: Input::Literal(
                b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu",
            ),
            expected: "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1".into(),
        },
        Vector {
            title: "exactly one block (64 bytes)".into(),
            input: Input::Repeat(b'a', 64),
            expected: "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb".into(),
        },
        Vector {
            title: "one byte short of the padding boundary (55 bytes)".into(),
            input: Input::Repeat(b'a', 55),
            expected: "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318".into(),
        },
        Vector {
            title: "NIST one million 'a'".into(),
            input: Input::Repeat(b'a', 1_000_000),
            expected: "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0".into(),
        },
    ]
}

fn cavp_vectors() -> Vec<Vector> {
    let mut file = String::new();
    let mut len_bits = None;
    let mut msg = None;
    let mut out = Vec::new();
    for line in CAVP.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix("# file:") {
            file = name.trim().to_string();
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "Len" => len_bits = Some(value.parse::<u64>().expect("CAVP Len")),
            "Msg" => msg = Some(value.to_string()),
            "MD" => {
                let len_bits = len_bits.take().expect("CAVP record is missing Len");
                let msg = msg.take().expect("CAVP record is missing Msg");
                let bytes = if len_bits == 0 {
                    Vec::new()
                } else {
                    let bytes = unhex(&msg);
                    assert_eq!(
                        bytes.len() as u64 * 8,
                        len_bits,
                        "CAVP Msg length does not match Len {len_bits}"
                    );
                    bytes
                };
                out.push((file.clone(), len_bits, bytes, value.to_string()));
            }
            _ => {}
        }
    }
    assert_eq!(
        out.len(),
        8,
        "the reviewed CAVP subset is eight byte vectors"
    );
    out.into_iter()
        .map(|(file, len_bits, bytes, expected)| {
            let noun = if bytes.len() == 1 { "byte" } else { "bytes" };
            let title = format!("CAVP {file} Len={len_bits} bits ({} {noun})", bytes.len());
            Vector {
                title,
                input: Input::Cavp {
                    file,
                    len_bits,
                    bytes,
                },
                expected,
            }
        })
        .collect()
}
