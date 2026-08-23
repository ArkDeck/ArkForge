//! HMAC-SHA-256 (RFC 2104), vectors from RFC 4231 plus the key handling edge
//! the permit tag depends on (key longer than the block size is hashed first).

use crate::json::{Json, hex, unhex};
use crate::suites::case_id;
use crate::tree::{Case, Tree};
use arkforge_core::digest::hmac_sha256;

const SUITE: &str = "hmac-sha256";

pub fn populate(tree: &mut Tree) {
    let vectors: Vec<(&str, Vec<u8>, Vec<u8>)> = vec![
        ("RFC 4231 case 1", vec![0x0b; 20], b"Hi There".to_vec()),
        (
            "RFC 4231 case 2 (key shorter than block)",
            b"Jefe".to_vec(),
            b"what do ya want for nothing?".to_vec(),
        ),
        ("RFC 4231 case 3", vec![0xaa; 20], vec![0xdd; 50]),
        (
            "RFC 4231 case 4",
            unhex("0102030405060708090a0b0c0d0e0f10111213141516171819"),
            vec![0xcd; 50],
        ),
        (
            "RFC 4231 case 6 (key longer than block size is hashed first)",
            vec![0xaa; 131],
            b"Test Using Larger Than Block-Size Key - Hash Key First".to_vec(),
        ),
        (
            "RFC 4231 case 7 (long key, long data)",
            vec![0xaa; 131],
            b"This is a test using a larger than block-size key and a larger than block-size data. The key needs to be hashed before being used by the HMAC algorithm.".to_vec(),
        ),
        (
            "key of exactly one block (64 bytes) is used as-is",
            vec![0x42; 64],
            b"arkforge".to_vec(),
        ),
        ("empty key and empty message", Vec::new(), Vec::new()),
    ];

    for (index, (title, key, data)) in vectors.iter().enumerate() {
        let tag = hmac_sha256(key, data);
        tree.case(
            &Case {
                id: case_id("HMAC", index as u32 + 1),
                suite: SUITE,
                title: title.to_string(),
                requirements: vec!["AF-DIG-002"],
                kind: "digest",
                description: "HMAC-SHA-256(key, data), lowercase hex.".to_string(),
                input: Json::object(vec![
                    ("keyHex", Json::str(hex(key))),
                    ("dataHex", Json::str(hex(data))),
                ]),
                expected: Json::object(vec![("tag", Json::str(tag.to_hex()))]),
            },
            Vec::new(),
        );
    }
}
