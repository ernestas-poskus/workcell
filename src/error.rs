//! The error type every module returns.

/// What went wrong.
///
/// The crate deliberately has no exit-code notion: a binary maps these variants to its own
/// contract. Every variant carries operator-facing detail; none carries a credential.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A filesystem operation failed, with the path or context it happened on.
    #[error("io: {detail}")]
    Io {
        /// What failed, and where.
        detail: String,
    },
    /// A Git operation failed.
    #[error("{operation} failed: {detail}")]
    Git {
        /// The logical operation that failed, rendered without secrets.
        operation: crate::types::GitOperation,
        /// Its libgit2 message or the underlying failure.
        detail: String,
    },
    /// A child process could not be started or did not behave.
    #[error("process: {detail}")]
    Process {
        /// What happened.
        detail: String,
    },
    /// The caller's configuration cannot mean what it says.
    #[error("configuration: {detail}")]
    Config {
        /// What is wrong.
        detail: String,
    },
    /// A path is not usable for the requested operation.
    #[error("path: {detail}")]
    Path {
        /// What is wrong with the path.
        detail: String,
    },
}

impl Error {
    /// Wrap an [`std::io::Error`] with the context it happened in.
    #[must_use]
    pub fn io(context: &str, error: std::io::Error) -> Self {
        Error::Io {
            detail: format!("{context}: {error}"),
        }
    }

    /// A failed Git command, with its rendered invocation.
    #[must_use]
    pub fn git(
        operation: impl Into<crate::types::GitOperation>,
        detail: impl Into<String>,
    ) -> Self {
        Error::Git {
            operation: operation.into(),
            detail: detail.into(),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error::Io {
            detail: error.to_string(),
        }
    }
}

/// The crate's result alias.
pub type Result<T> = std::result::Result<T, Error>;
