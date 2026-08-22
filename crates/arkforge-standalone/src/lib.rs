//! Reusable standalone ArkForge product authority.
//!
//! The canonical CLI and future desktop products share this layer. It owns
//! daemon lifecycle, pairing, exact HDC control, permit issuance and the
//! authority-side durable records. Presentation frontends remain outside.

#![forbid(unsafe_code)]

pub mod application;
mod authority_support;
pub mod config;
mod hdc_control;
pub mod supervisor;

/// Stable application/service failure carried across CLI and UI frontends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandaloneError {
    pub code: String,
    pub message: String,
    pub exit_code: i32,
    pub retryable: bool,
    pub required_acknowledgements: Vec<String>,
}

impl StandaloneError {
    pub fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        exit_code: i32,
        retryable: bool,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            exit_code,
            retryable,
            required_acknowledgements: Vec::new(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("INVALID_ARGUMENT", message, 2, false)
    }

    pub fn with_required_acknowledgements(mut self, tokens: Vec<String>) -> Self {
        if !tokens.is_empty() {
            self.retryable = true;
        }
        self.required_acknowledgements = tokens;
        self
    }
}

impl From<arkforge_client::ClientError> for StandaloneError {
    fn from(error: arkforge_client::ClientError) -> Self {
        Self {
            code: error.code,
            message: error.message,
            exit_code: error.exit_code,
            retryable: error.retryable,
            required_acknowledgements: Vec::new(),
        }
    }
}

impl From<arkforged::rescue::RescueError> for StandaloneError {
    fn from(error: arkforged::rescue::RescueError) -> Self {
        Self {
            code: error.code.to_string(),
            message: error.message,
            exit_code: error.exit_code,
            retryable: error.retryable,
            required_acknowledgements: Vec::new(),
        }
    }
}
