//! The strict YAML subset (spec/model/strict-yaml.md): what the profile reader
//! accepts, as a value tree, and what it refuses.

use crate::json::Json;
use crate::suites::case_id;
use crate::tree::{Case, Tree};
use arkforge_core::yaml::{YamlValue, parse};

const SUITE: &str = "strict-yaml";

fn value_json(value: &YamlValue) -> Json {
    match value {
        YamlValue::Scalar(text) => Json::object(vec![("scalar", Json::str(text.clone()))]),
        YamlValue::Null => Json::str("null"),
        YamlValue::Sequence(items) => Json::object(vec![(
            "sequence",
            Json::Array(items.iter().map(value_json).collect()),
        )]),
        YamlValue::Mapping(entries) => Json::object(vec![(
            "mapping",
            Json::Array(
                entries
                    .iter()
                    .map(|(k, v)| Json::Array(vec![Json::str(k.clone()), value_json(v)]))
                    .collect(),
            ),
        )]),
    }
}

pub fn populate(tree: &mut Tree) {
    let cases: Vec<(&str, &str, &str)> = vec![
        (
            "block mapping with nested block",
            "AF-YAML-001",
            "a: 1\nb:\n  c: x\n  d: y\n",
        ),
        (
            "block sequence of scalars",
            "AF-YAML-001",
            "items:\n  - one\n  - two\n",
        ),
        (
            "sequence items that are mappings",
            "AF-YAML-001",
            "providers:\n  - id: a\n    backend: b\n  - id: c\n    backend: d\n",
        ),
        (
            "flow sequence with plain and quoted items",
            "AF-YAML-002",
            "a: [x, 'y', \"z\"]\n",
        ),
        ("empty flow sequence", "AF-YAML-002", "a: []\n"),
        ("explicit empty value is null", "AF-YAML-003", "a:\nb: 1\n"),
        ("null and tilde are null", "AF-YAML-003", "a: null\nb: ~\n"),
        ("document start marker", "AF-YAML-001", "---\na: 1\n"),
        ("comment after a value", "AF-YAML-004", "a: x # comment\n"),
        (
            "hash inside quotes is not a comment",
            "AF-YAML-004",
            "a: \"x # not a comment\"\n",
        ),
        (
            "full-line comment and blank lines",
            "AF-YAML-004",
            "# heading\n\na: 1\n\n# trailing\n",
        ),
        (
            "quotes are stripped, escapes are not processed",
            "AF-YAML-005",
            "a: 'it''s'\nb: \"back\\\\slash\"\n",
        ),
        (
            "hex and underscore integers stay text for the parser",
            "AF-YAML-006",
            "vendorId: 0x2207\nfiller: 0xCC\nbig: 1_000\n",
        ),
        (
            "a scalar containing a colon without a following space",
            "AF-YAML-001",
            "url: a:b:c\n",
        ),
        (
            "deeper nesting",
            "AF-YAML-001",
            "a:\n  b:\n    c:\n      - d: 1\n        e: 2\n",
        ),
        (
            "sequence item with an indented child block",
            "AF-YAML-001",
            "a:\n  -\n    k: v\n",
        ),
        (
            "anchor at line start is rejected",
            "AF-YAML-012",
            "&x a: 1\n",
        ),
        ("alias at line start is rejected", "AF-YAML-012", "*x\n"),
        // rejections
        ("duplicate key", "AF-YAML-010", "a: 1\na: 2\n"),
        (
            "duplicate key inside a sequence item mapping",
            "AF-YAML-010",
            "items:\n  - id: a\n    id: b\n",
        ),
        ("tab indentation", "AF-YAML-011", "a:\n\t- x\n"),
        (
            "anchor marker inside a value is rejected",
            "AF-YAML-012",
            "a: &x 1\n",
        ),
        (
            "alias marker inside a value is rejected",
            "AF-YAML-012",
            "a: *x\n",
        ),
        (
            "tag inside a value is rejected",
            "AF-YAML-012",
            "a: !!str 1\n",
        ),
        ("flow mapping", "AF-YAML-013", "a: {b: 1}\n"),
        ("literal block scalar", "AF-YAML-014", "a: |\n  text\n"),
        ("folded block scalar", "AF-YAML-014", "a: >\n  text\n"),
        (
            "flow sequence not closed on one line",
            "AF-YAML-013",
            "a: [x,\n  y]\n",
        ),
        ("nested flow collection", "AF-YAML-013", "a: [x, [y]]\n"),
        (
            "empty item in a flow sequence",
            "AF-YAML-013",
            "a: [x,,y]\n",
        ),
        (
            "inconsistent indentation inside a mapping",
            "AF-YAML-015",
            "a: 1\n  b: 2\n",
        ),
        (
            "unexpected indentation inside a sequence",
            "AF-YAML-015",
            "a:\n  - x\n    - y\n",
        ),
        (
            "a line that is not key: value",
            "AF-YAML-015",
            "a: 1\njust text\n",
        ),
    ];

    for (index, (title, requirement, source)) in cases.iter().enumerate() {
        let outcome = parse(source);
        let expected = match &outcome {
            Ok(value) => Json::object(vec![
                ("result", Json::str("accept")),
                ("value", value_json(value)),
            ]),
            Err(error) => Json::object(vec![
                ("result", Json::str("reject")),
                ("message", Json::str(error.to_string())),
            ]),
        };
        tree.case(
            &Case {
                id: case_id("YAML", index as u32 + 1),
                suite: SUITE,
                title: title.to_string(),
                requirements: vec![requirement],
                kind: "decode",
                description: "Parse `input.yaml` with the strict subset reader. The value tree \
                              has only scalars (text), sequences, mappings and null; typing \
                              happens in the consumer. `message` on a rejection is informative."
                    .to_string(),
                input: Json::object(vec![("yaml", Json::str(*source))]),
                expected,
            },
            vec![("input.yaml", source.as_bytes().to_vec())],
        );
    }
}
