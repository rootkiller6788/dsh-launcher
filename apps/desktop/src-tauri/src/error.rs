use launcher_core::{CodedError, ErrorCode};
use serde::Serializer;

/// Command-layer error. Serializes as a [`CodedError`] object so the frontend
/// sees `{ code, category, title, message, nextAction }` on an `invoke`
/// rejection — one shape for the banner, the Activity log, and the diagnostic
/// package. Untagged errors fall back to `E9001` internal.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    Coded(CodedError),
    #[error("{0}")]
    Core(#[from] anyhow::Error),
    #[error("{0}")]
    Message(String),
}

impl AppError {
    pub fn msg(text: impl Into<String>) -> Self {
        Self::Message(text.into())
    }

    /// Tag a failure with a stable error code. `message` is the dynamic
    /// detail; `next_action` comes from the code's table entry.
    pub fn coded(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::Coded(CodedError::new(code, message))
    }

    /// The coded form, folding untagged variants into `E9001` internal.
    pub fn coded_error(&self) -> CodedError {
        match self {
            Self::Coded(c) => c.clone(),
            Self::Core(e) => CodedError::internal(e.to_string()),
            Self::Message(m) => CodedError::internal(m.clone()),
        }
    }
}

impl From<tauri::Error> for AppError {
    fn from(e: tauri::Error) -> Self {
        Self::Message(e.to_string())
    }
}

impl serde::Serialize for AppError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.coded_error().serialize(serializer)
    }
}
