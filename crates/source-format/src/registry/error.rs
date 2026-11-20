//! Error types for the metadata registry crate.

use std::path::PathBuf;

use thiserror::Error;

/// Errors returned while loading, merging, or querying the metadata registry.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RegistryError {
    /// Failed to read a registry or project configuration file from disk.
    #[error("failed to read {path}: {source}")]
    Io {
        /// Path that failed to read.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// Failed to parse registry JSON or `sfdx-project.json`.
    #[error("failed to parse JSON for {context}: {source}")]
    Json {
        /// Description or file path of the JSON being parsed.
        context: String,
        /// Underlying `serde_json` error.
        #[source]
        source: serde_json::Error,
    },

    /// Requested metadata type does not exist in the registry.
    #[error("missing metadata type definition for '{name}'")]
    MissingTypeDefinition {
        /// Requested type name.
        name: String,
        /// Similar type name suggestions.
        suggestions: Vec<String>,
    },

    /// Requested child metadata type exists in `childTypes`, but is missing under its parent.
    #[error("missing child metadata type definition '{child_id}' under parent '{parent_id}'")]
    MissingChildTypeDefinition {
        /// Parent type identifier.
        parent_id: String,
        /// Child type identifier.
        child_id: String,
    },

    /// Requested `sourceBehaviorOptions` preset name is unknown.
    #[error("unknown source behavior preset '{name}'")]
    InvalidPreset {
        /// Invalid preset name.
        name: String,
    },

    /// A `directoryName` in `registryCustomizations` contains path traversal sequences.
    #[error("unsafe directoryName '{directory_name}' for metadata type '{type_name}'")]
    PathTraversal {
        /// Unsafe `directoryName` value.
        directory_name: String,
        /// Name or ID of the offending type.
        type_name: String,
    },
}
