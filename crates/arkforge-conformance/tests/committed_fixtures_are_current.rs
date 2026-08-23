//! The committed fixtures under `spec/conformance/v1` must be exactly what the
//! reference implementation generates today. Any drift is a spec change that
//! has to be reviewed, not a test to be updated in passing.

#[test]
fn committed_fixtures_match_the_reference_implementation() {
    let tree = arkforge_conformance::generate();
    let root = arkforge_conformance::committed_root();
    let problems = tree.diff_against(&root);
    assert!(
        problems.is_empty(),
        "\n{}\n\nspec/conformance/v1 is out of date with the reference implementation. \
         If the change is intended, regenerate with\n\n    cargo run -p arkforge-conformance -- generate\n\n\
         and review the diff as a spec revision (bump spec/manifest.yaml).\n",
        problems.join("\n")
    );
}

#[test]
fn generation_is_deterministic() {
    let first = arkforge_conformance::generate();
    let second = arkforge_conformance::generate();
    assert_eq!(first.files(), second.files());
}
