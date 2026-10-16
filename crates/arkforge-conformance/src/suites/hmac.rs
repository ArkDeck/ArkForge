//! HMAC-SHA-256 (RFC 2104), vectors from RFC 4231 plus the key handling edge
//! the permit tag depends on (key longer than the block size is hashed first).

use crate::json::{Json, hex, unhex};
use crate::suites::case_id;
use crate::tree::{Case, Tree};
use arkforge_core::digest::hmac_sha256;

const SUITE: &str = "hmac-sha256";

pub fn populate(tree: &mut Tree) {
    // Tags are the published RFC 4231 values (cases 1–4, 6, 7). The two
    // block-size edges are not in that RFC; their tags are pinned here and
    // cross-checked against the `hmac` crate in arkforge-core's dev tests.
    let vectors: Vec<(&str, Vec<u8>, Vec<u8>, &str)> = vec![
        (
            "RFC 4231 case 1",
            vec![0x0b; 20],
            b"Hi There".to_vec(),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
        ),
        (
            "RFC 4231 case 2 (key shorter than block)",
            b"Jefe".to_vec(),
            b"what do ya want for nothing?".to_vec(),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
        ),
        (
            "RFC 4231 case 3",
            vec![0xaa; 20],
            vec![0xdd; 50],
            "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe",
        ),
        (
            "RFC 4231 case 4",
            unhex("0102030405060708090a0b0c0d0e0f10111213141516171819"),
            vec![0xcd; 50],
            "82558a389a443c0ea4cc819899f2083a85f0faa3e578f8077a2e3ff46729665b",
        ),
        (
            "RFC 4231 case 6 (key longer than block size is hashed first)",
            vec![0xaa; 131],
            b"Test Using Larger Than Block-Size Key - Hash Key First".to_vec(),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54",
        ),
        (
            "RFC 4231 case 7 (long key, long data)",
            vec![0xaa; 131],
            b"This is a test using a larger than block-size key and a larger than block-size data. The key needs to be hashed before being used by the HMAC algorithm.".to_vec(),
            "9b09ffa71b942fcb27635fbcd5b0e944bfdc63644f0713938a7f51535c3a35e2",
        ),
        (
            "key of exactly one block (64 bytes) is used as-is",
            vec![0x42; 64],
            b"arkforge".to_vec(),
            "1b9658b4783286b2020a195b227ce6ea557a4ea50ada347e9c6bf347b3ab4019",
        ),
        (
            "empty key and empty message",
            Vec::new(),
            Vec::new(),
            "b613679a0814d9ec772f95d778c35fc5ff1697c493715653c6c712144292c5ad",
        ),
    ];

    for (index, (title, key, data, published)) in vectors.iter().enumerate() {
        let tag = hmac_sha256(key, data);
        assert_eq!(
            tag.to_hex(),
            *published,
            "HMAC-SHA-256 implementation diverges from the published vector {title}"
        );
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
                expected: Json::object(vec![("tag", Json::str(*published))]),
            },
            Vec::new(),
        );
    }
}
