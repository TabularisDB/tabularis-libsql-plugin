//! Plugin-local error type, kept deliberately small (no `anyhow`/`thiserror`).
//!
//! Every error carries a JSON-RPC error code so handlers can bubble failures
//! straight out to the host with `?`.

use std::fmt;

#[derive(Debug)]
pub struct PluginError {
    pub code: i64,
    pub message: String,
}

impl PluginError {
    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: msg.into(),
        }
    }

    pub fn invalid_params(msg: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: msg.into(),
        }
    }

    /// An operation the underlying database genuinely cannot perform. Surfaced
    /// as "method not found" so the host treats it as unsupported rather than a
    /// transient failure.
    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self {
            code: -32601,
            message: msg.into(),
        }
    }
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for PluginError {}

impl From<libsql::Error> for PluginError {
    fn from(err: libsql::Error) -> Self {
        PluginError::internal(format!("sqlite error: {err}"))
    }
}
