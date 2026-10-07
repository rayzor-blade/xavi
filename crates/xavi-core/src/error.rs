use std::fmt;

/// Adapters translate these errors into their runtime's exception mechanism.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    InvalidArgument,
    InvalidState,
    NotSupported,
    ResourceExhausted,
    WouldBlock,
    Cancelled,
    Io,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
}

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidArgument, message)
    }

    pub fn closed() -> Self {
        Self::new(
            ErrorKind::InvalidState,
            "media handle is closed, invalid or of the wrong kind",
        )
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotSupported, message)
    }

    pub fn exhausted() -> Self {
        Self::new(
            ErrorKind::ResourceExhausted,
            "media resource capacity exhausted",
        )
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
