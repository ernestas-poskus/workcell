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
    /// A Git command failed.
    #[error("{command} failed: {detail}")]
    Git {
        /// The command that failed, rendered without secrets.
        command: String,
        /// Its stderr tail or the underlying failure.
        detail: String,
    },
    /// A child process could not be started or did not behave.
    #[error("process: {detail}")]
    Process {
        /// What happened.
        detail: String,
    },
    /// The sandbox is missing, unsupported on this host, or refused to start.
    #[error("sandbox: {detail}")]
    Sandbox {
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
    pub fn git(command: impl Into<String>, detail: impl Into<String>) -> Self {
        Error::Git {
            command: command.into(),
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
