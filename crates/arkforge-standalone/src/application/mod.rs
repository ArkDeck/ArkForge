//! Stable application boundary for ArkFlash and other presentation frontends.

mod dto;
mod mock;
mod service;

pub use dto::*;
pub use mock::{ApplicationCall, ApplicationMethod, MockApplicationService};
pub use service::{ApplicationService, StandaloneApplicationService};
