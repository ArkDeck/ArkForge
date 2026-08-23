//! One module per conformance suite. Each exposes `populate(&mut Tree)`.

pub mod admission;
pub mod cbor;
pub mod crash;
pub mod hmac;
pub mod journal;
pub mod permit;
pub mod plan;
pub mod protobuf;
pub mod rebind;
pub mod sha256;
pub mod state_machine;
pub mod yaml;

/// Case IDs are `AF-CONF-<SUITE>-<NNN>`.
pub(crate) fn case_id(suite: &str, number: u32) -> String {
    format!("AF-CONF-{suite}-{number:03}")
}
