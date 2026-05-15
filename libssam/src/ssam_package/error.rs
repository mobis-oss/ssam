// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

//! Error types for SSAM package parsing.

use std::path::PathBuf;

/// Error type for SSAM package parsing operations.
///
/// This enum represents errors that can occur during package file
/// parsing and verification. Error messages are intentionally kept
/// minimal to avoid exposing internal package structure details.
#[derive(Debug)]
pub enum PackageParseError {
    /// Failed to open the package file.
    FileOpen {
        path: PathBuf,
        source: std::io::Error,
    },

    /// The package file has an invalid magic bytes.
    InvalidMagic,

    /// The package file has an unsupported format version.
    InvalidFormatVersion { expected: String, actual: String },

    /// Signature verification failed.
    SignatureVerificationFailed,

    /// Failed to parse package data (e.g., metadata, runtime config, seccomp policy).
    ParseFailed { source: anyhow::Error },

    /// An I/O error occurred while reading the package.
    Io {
        message: String,
        source: anyhow::Error,
    },
}

impl std::error::Error for PackageParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PackageParseError::FileOpen { source, .. } => Some(source),
            PackageParseError::Io { source, .. } | PackageParseError::ParseFailed { source } => {
                Some(source.as_ref())
            }
            _ => None,
        }
    }
}

impl std::fmt::Display for PackageParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if f.alternate() {
            self.alt_display_fmt(f)
        } else {
            self.display_fmt(f)
        }
    }
}

impl PackageParseError {
    fn display_fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackageParseError::FileOpen { .. } => write!(f, "Cannot open package file."),
            PackageParseError::InvalidMagic => write!(f, "Invalid package magic."),
            PackageParseError::InvalidFormatVersion { .. } => {
                write!(f, "Invalid package format version.")
            }
            PackageParseError::SignatureVerificationFailed => {
                write!(f, "Signature verification failed.")
            }
            PackageParseError::ParseFailed { .. } => {
                write!(f, "ParseError: Package data is corrupted.")
            }
            PackageParseError::Io { message, .. } => write!(f, "{message}"),
        }
    }

    fn alt_display_fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackageParseError::FileOpen { path, source } => {
                write!(
                    f,
                    "Cannot open package file at {}: {source}",
                    path.display(),
                )
            }
            PackageParseError::InvalidMagic => write!(f, "Invalid package magic."),
            PackageParseError::InvalidFormatVersion { expected, actual } => {
                write!(
                    f,
                    "Invalid package format version. Expected: {expected}, Actual: {actual}",
                )
            }
            PackageParseError::SignatureVerificationFailed => {
                write!(f, "Signature verification failed.")
            }
            PackageParseError::ParseFailed { source } => {
                write!(
                    f,
                    "ParseError: Package data is corrupted: \nReason: {source:?}"
                )
            }
            PackageParseError::Io { message, source } => {
                write!(f, "{message}: {source}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        assert_eq!(
            PackageParseError::InvalidMagic.to_string(),
            "Invalid package magic."
        );
        assert_eq!(
            PackageParseError::InvalidFormatVersion {
                expected: "0.1.0".to_string(),
                actual: "9.9.9".to_string(),
            }
            .to_string(),
            "Invalid package format version."
        );
        assert_eq!(
            PackageParseError::SignatureVerificationFailed.to_string(),
            "Signature verification failed."
        );
    }

    #[test]
    fn test_file_open_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err = PackageParseError::FileOpen {
            path: PathBuf::from("/test/package.pkg"),
            source: io_err,
        };
        // Verify the variant structure
        assert!(matches!(err, PackageParseError::FileOpen { .. }));

        // Verify the path is preserved
        if let PackageParseError::FileOpen { path, .. } = &err {
            assert_eq!(path.to_string_lossy(), "/test/package.pkg");
        } else {
            panic!("Expected FileOpen variant");
        }
    }

    #[test]
    fn test_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "unexpected eof");
        let err = PackageParseError::Io {
            message: "Failed to seek package file.".to_string(),
            source: io_err.into(),
        };
        // Verify the variant structure and message
        assert!(matches!(err, PackageParseError::Io { .. }));
        if let PackageParseError::Io { message, .. } = &err {
            assert_eq!(message, "Failed to seek package file.");
        } else {
            panic!("Expected Io variant");
        }
        // Verify source error is preserved
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn test_parse_failed_with_anyhow() {
        let inner_err: anyhow::Error =
            std::io::Error::new(std::io::ErrorKind::InvalidData, "bad data").into();
        let err_with_context = inner_err.context("Failed to decode metadata");
        let err = PackageParseError::ParseFailed {
            source: err_with_context,
        };
        // Verify the variant structure
        assert!(matches!(err, PackageParseError::ParseFailed { .. }));
        // Verify source error chain contains context
        assert!(std::error::Error::source(&err).is_some());
        let debug_str = format!("{err:?}");
        assert!(debug_str.contains("Failed to decode metadata"));
    }
}
