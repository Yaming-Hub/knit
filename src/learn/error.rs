//! Error types for the knit-learn crate.

use std::fmt;

/// Errors that can occur during data ingestion, profiling, or type inference.
#[derive(Debug)]
pub enum LearnError {
    /// An I/O error occurred while reading data.
    Io(std::io::Error),
    /// An error from the Arrow library.
    Arrow(arrow::error::ArrowError),
    /// An error from the Parquet library.
    Parquet(parquet::errors::ParquetError),
    /// The file format is not supported.
    UnsupportedFormat(String),
    /// A generic error with a message.
    Other(String),
}

impl fmt::Display for LearnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LearnError::Io(e) => write!(f, "I/O error: {e}"),
            LearnError::Arrow(e) => write!(f, "Arrow error: {e}"),
            LearnError::Parquet(e) => write!(f, "Parquet error: {e}"),
            LearnError::UnsupportedFormat(ext) => {
                write!(f, "Unsupported file format: {ext}")
            }
            LearnError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for LearnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LearnError::Io(e) => Some(e),
            LearnError::Arrow(e) => Some(e),
            LearnError::Parquet(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for LearnError {
    fn from(e: std::io::Error) -> Self {
        LearnError::Io(e)
    }
}

impl From<arrow::error::ArrowError> for LearnError {
    fn from(e: arrow::error::ArrowError) -> Self {
        LearnError::Arrow(e)
    }
}

impl From<parquet::errors::ParquetError> for LearnError {
    fn from(e: parquet::errors::ParquetError) -> Self {
        LearnError::Parquet(e)
    }
}

/// Result type alias for knit-learn operations.
pub type LearnResult<T> = Result<T, LearnError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display() {
        let e = LearnError::UnsupportedFormat("xlsx".into());
        assert!(e.to_string().contains("xlsx"));
    }

    #[test]
    fn error_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        let e: LearnError = io_err.into();
        assert!(matches!(e, LearnError::Io(_)));
    }
}