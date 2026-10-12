//! Error type for the whole tool.
//!
//! Exit codes follow the spec (section 3): `2` means "the tool or its
//! configuration failed", which is deliberately distinct from `1` meaning
//! "your code has diagnostics". Every error in this enum maps to exit code 2.

use std::fmt;

#[derive(Debug)]
pub enum Error {
    /// I/O failure, with the path that caused it.
    Io { path: String, source: std::io::Error },
    /// Malformed or contradictory configuration.
    Config(String),
    /// A catalog file could not be read or parsed.
    Catalog(String),
    /// The `fpc` binary could not be found or executed.
    FpcNotFound(String),
    /// Bad command line usage that clap did not catch itself.
    Usage(String),
    /// A failure inside the analyzer that is not attributable to user code.
    Internal(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn io(path: impl Into<String>, source: std::io::Error) -> Self {
        Error::Io { path: path.into(), source }
    }

    pub fn config(msg: impl Into<String>) -> Self {
        Error::Config(msg.into())
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Error::Internal(msg.into())
    }

    /// Process exit code for this error. Always `2` per the spec.
    pub fn exit_code(&self) -> i32 {
        2
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { path, source } => write!(f, "{}: {}", path, source),
            Error::Config(m) => write!(f, "configuration error: {}", m),
            Error::Catalog(m) => write!(f, "catalog error: {}", m),
            Error::FpcNotFound(m) => write!(f, "could not run fpc: {}", m),
            Error::Usage(m) => write!(f, "{}", m),
            Error::Internal(m) => write!(f, "internal error: {}", m),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
