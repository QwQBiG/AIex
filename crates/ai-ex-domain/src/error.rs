use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Stable protocol message for a rewrite that would alter saved memory entities.
pub const MEMORY_SAVED_ENTITIES_CHANGED: &str = "memory rewrite would change saved entities; preserve a complete original copy before resolving this data";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Configuration,
    Connectivity,
    Protocol,
    InvalidTransition,
    Safety,
    Unavailable,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
#[error("{kind:?}: {message}")]
pub struct AppError {
    pub kind: ErrorKind,
    pub message: String,
}

impl AppError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn configuration(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Configuration, message)
    }

    pub fn connectivity(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Connectivity, message)
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Protocol, message)
    }

    pub fn invalid_transition(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidTransition, message)
    }

    pub fn safety(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Safety, message)
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unavailable, message)
    }
}
