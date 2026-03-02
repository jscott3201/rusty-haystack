//! HBF codec error type.

use std::fmt;

/// Errors that can occur during HBF encoding or decoding.
#[derive(Debug)]
pub enum HbfError {
    /// A descriptive error message.
    Message(String),
    /// An I/O error.
    Io(std::io::Error),
    /// Unexpected end of input.
    Eof,
    /// Encountered an unknown type tag byte.
    InvalidTag(u8),
    /// The HBF magic header was missing or incorrect.
    InvalidMagic,
    /// UTF-8 decoding failed.
    Utf8(std::str::Utf8Error),
}

impl fmt::Display for HbfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HbfError::Message(msg) => write!(f, "{msg}"),
            HbfError::Io(e) => write!(f, "I/O error: {e}"),
            HbfError::Eof => write!(f, "unexpected end of input"),
            HbfError::InvalidTag(tag) => write!(f, "invalid type tag: 0x{tag:02x}"),
            HbfError::InvalidMagic => write!(f, "invalid HBF magic header"),
            HbfError::Utf8(e) => write!(f, "UTF-8 error: {e}"),
        }
    }
}

impl std::error::Error for HbfError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            HbfError::Io(e) => Some(e),
            HbfError::Utf8(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for HbfError {
    fn from(e: std::io::Error) -> Self {
        HbfError::Io(e)
    }
}

impl From<std::str::Utf8Error> for HbfError {
    fn from(e: std::str::Utf8Error) -> Self {
        HbfError::Utf8(e)
    }
}
