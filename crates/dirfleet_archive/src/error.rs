use std::path::Path;

/// Errors surfaced to the app. Backends map their own errors onto these, so
/// the UI can react to a missing password without parsing message text.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("This archive is password protected.")]
    PasswordRequired,
    #[error("The password is incorrect.")]
    BadPassword,
    #[error("Volume “{name}” of this multi-part archive is missing.")]
    MissingVolume { name: String },
    #[error("The archive is damaged: {message}")]
    Corrupt { message: String },
    #[error("Unsupported archive: {message}")]
    Unsupported { message: String },
    #[error("{message} ({path})")]
    Io { path: String, message: String },
    #[error("Extraction was cancelled.")]
    Cancelled,
    #[error("The archive changed on disk. Open it again.")]
    ArchiveChanged,
    #[error("{message}")]
    InvalidInput { message: String },
}

impl CoreError {
    pub(crate) fn io(path: &Path, error: std::io::Error) -> Self {
        Self::Io {
            path: path.display().to_string(),
            message: error.to_string(),
        }
    }
}

pub type Result<T, E = CoreError> = std::result::Result<T, E>;
