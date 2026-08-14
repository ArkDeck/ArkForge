//! # arkforged
//!
//! The ArkForge mechanics daemon.
//!
//! The library half holds the request handler so the API surface can be tested
//! without opening a socket; the binary half binds the sockets and frames.
//! architecture.md 15.1–15.3.

#![forbid(unsafe_code)]

pub mod service;

pub use service::Service;
