//! Error types for tree containers, path safety checks, and `.forceignore`.

use std::path::PathBuf;

use thiserror::Error;

/// Errors returned by [`super::TreeContainer`], path safety checks, or `.forceignore`.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TreeError {
    /// Underlying filesystem I/O error.
    #[error("I/O error on {path}: {source}")]
    Io {
        /// Path associated with the failure.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// Failed to read or index a zip archive.
    #[error("zip archive error: {0}")]
    Zip(#[from] zip::result::ZipError),

    /// Target path does not exist in the tree container.
    #[error("path not found: {path}")]
    PathNotFound {
        /// Missing path.
        path: PathBuf,
    },

    /// Expected a directory at `path`, but found a file.
    #[error("expected directory path, found file: {path}")]
    ExpectedDirectory {
        /// File path where a directory was expected.
        path: PathBuf,
    },

    /// Expected a file at `path`, but found a directory.
    #[error("expected file path, found directory: {path}")]
    ExpectedFile {
        /// Directory path where a file was expected.
        path: PathBuf,
    },

    /// Destination path resolves outside the allowed output/project root.
    #[error("path '{path}' resolves outside root '{root}'")]
    PathOutsideRoot {
        /// Candidate path.
        path: PathBuf,
        /// Permitted root directory.
        root: PathBuf,
    },

    /// A symbolic link was encountered along a write destination path.
    #[error("refusing to write through symbolic link '{symlink}' under root '{root}'")]
    SymlinkOnPath {
        /// Symlink segment encountered.
        symlink: PathBuf,
        /// Destination path attempted.
        destination: PathBuf,
        /// Trusted root directory.
        root: PathBuf,
    },

    /// A registry `directoryName` or archive entry contains `..` traversal segments.
    #[error("unsafe path traversal in '{path}' for '{context}'")]
    PathTraversal {
        /// Unsafe path string.
        path: String,
        /// Metadata type or archive entry context.
        context: String,
    },

    /// File contents were not valid UTF-8 when text was requested.
    #[error("invalid UTF-8 in {path}: {source}")]
    InvalidUtf8 {
        /// Path that failed UTF-8 decoding.
        path: PathBuf,
        /// Underlying UTF-8 error.
        #[source]
        source: std::string::FromUtf8Error,
    },

    /// Failed to parse a `.forceignore` file.
    #[error("failed to parse .forceignore at {path}: {message}")]
    ForceIgnoreParse {
        /// Path to the `.forceignore` file.
        path: PathBuf,
        /// Parse error message.
        message: String,
    },
}
