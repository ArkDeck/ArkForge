//! The daemon's request handler, independent of any socket.
//!
//! architecture.md 15.3. Keeping dispatch transport-agnostic is what lets the
//! API-surface tests assert "the public socket cannot start execution" and
//! "startExecution is unavailable" without opening a socket — the properties
//! belong to the service, not to the plumbing.

include!("parts/preamble.rs");
include!("parts/lifecycle.rs");
include!("parts/requests.rs");
include!("parts/recovery.rs");
include!("parts/permits.rs");
include!("parts/materialize.rs");
include!("parts/helpers.rs");
include!("parts/tests.rs");
