use serde::Serialize;
use serde_json::Value;
use std::error::Error;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolErrorKind {
    InvalidParams,
    Denied,
    NotFound,
    Conflict,
    Timeout,
    Retryable,
    Internal,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolError {
    pub kind: ToolErrorKind,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    pub retryable: bool,
}

impl ToolError {
    pub fn new(kind: ToolErrorKind, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind,
            code: code.into(),
            message: message.into(),
            hint: None,
            details: None,
            retryable: matches!(kind, ToolErrorKind::Timeout | ToolErrorKind::Retryable),
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::InvalidParams, "INVALID_PARAMS", message)
    }

    pub fn denied(message: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::Denied, "DENIED", message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::NotFound, "NOT_FOUND", message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::Conflict, "CONFLICT", message)
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::Timeout, "TIMEOUT", message)
    }

    pub fn retryable(message: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::Retryable, "RETRYABLE", message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::Internal, "INTERNAL", message)
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl Error for ToolError {}

impl From<std::io::Error> for ToolError {
    fn from(err: std::io::Error) -> Self {
        ToolError::internal(err.to_string())
    }
}
