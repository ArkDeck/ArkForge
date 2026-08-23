//! # arkforge-conformance
//!
//! Generates the language-neutral conformance fixtures under
//! `spec/conformance/v1` from the Rust reference implementation, and checks
//! that the committed fixtures are current.
//!
//! The Rust implementation is the *oracle* that produced these bytes; once
//! committed and reviewed, the bytes are the authority. A second
//! implementation (Zig, C++, Swift, …) passes the suite by reproducing them,
//! never by calling into Rust. The guard test in `tests/` fails whenever the
//! reference implementation's behaviour drifts from the committed fixtures,
//! which turns "we changed the encoding" into a reviewed spec revision
//! instead of a silent break for every other implementation.
//!
//! Generation is deterministic: no clocks, no randomness, no host paths.

#![forbid(unsafe_code)]

pub mod cbor_repr;
pub mod json;
pub mod schema;
pub mod suites;
pub mod tree;

pub use tree::{Case, Tree};

/// The spec version these fixtures belong to. Bumped with `spec/manifest.yaml`.
pub const SPEC_VERSION: &str = "1.0.0-draft.3";

/// Every fixture, in memory.
pub fn generate() -> Tree {
    let mut tree = Tree::new();
    suites::sha256::populate(&mut tree);
    suites::hmac::populate(&mut tree);
    suites::cbor::populate(&mut tree);
    suites::cli::populate(&mut tree);
    suites::permit::populate(&mut tree);
    suites::admission::populate(&mut tree);
    suites::journal::populate(&mut tree);
    suites::crash::populate(&mut tree);
    suites::state_machine::populate(&mut tree);
    suites::protobuf::populate(&mut tree);
    suites::rebind::populate(&mut tree);
    suites::reconcile::populate(&mut tree);
    suites::receipt::populate(&mut tree);
    suites::transcript_dispatch::populate(&mut tree);
    suites::yaml::populate(&mut tree);
    suites::plan::populate(&mut tree);
    let manifest = tree.manifest(SPEC_VERSION).to_pretty();
    tree.put_text("manifest.json", manifest);
    tree
}

/// Where the committed fixtures live, relative to this crate.
pub fn committed_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("spec")
        .join("conformance")
        .join("v1")
}
