//! Error types for metadata format conversion, writing, and replacements.

use std::path::PathBuf;

use thiserror::Error;

use crate::resolve::ResolveError;

/// Errors returned while converting metadata between MDAPI and SFDX source formats.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ConvertError {
    /// Underlying component resolution error.
    #[error(transparent)]
    Resolve(#[from] ResolveError),

    /// Underlying registry query or preset error.
    #[error(transparent)]
    Registry(#[from] crate::registry::RegistryError),

    /// Underlying tree container, path safety, or `.forceignore` error.
    #[error(transparent)]
    Tree(#[from] crate::tree::TreeError),

    /// Underlying XML parsing error.
    #[error(transparent)]
    Xml(#[from] crate::xml::XmlError),

    /// Underlying filesystem I/O error.
    #[error("I/O error on {path}: {source}")]
    Io {
        /// Path associated with the I/O failure.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// ZIP archive creation or extraction error.
    #[error("zip archive error: {0}")]
    Zip(#[from] zip::result::ZipError),

    /// Invalid regular expression in `sfdx-project.json` replacements.
    #[error("invalid replacement regex '{pattern}': {source}")]
    InvalidRegex {
        /// Regex pattern string.
        pattern: String,
        /// Underlying `regex` compilation error.
        #[source]
        source: regex::Error,
    },

    /// Invalid glob pattern in `sfdx-project.json` replacements.
    #[error("invalid replacement glob '{pattern}': {source}")]
    InvalidGlob {
        /// Glob pattern string.
        pattern: String,
        /// Underlying `globset` compilation error.
        #[source]
        source: globset::Error,
    },

    /// Unset required environment variable referenced by a replacement rule.
    #[error("missing environment variable '{var_name}' required by replacement rule")]
    MissingEnvVar {
        /// Environment variable name.
        var_name: String,
    },

    /// Output target `Merge` was requested with `TargetFormat::Metadata`.
    #[error("merge output mode is only supported when converting to source format")]
    MergeMetadataTargetUnsupported,

    /// Transformer strategy is not supported or not yet implemented.
    #[error("transformer strategy '{strategy}' is not supported for '{type_name}'")]
    UnsupportedTransformer {
        /// Transformer strategy name.
        strategy: String,
        /// Metadata type name.
        type_name: String,
    },

    /// Static resource directory content used a non-archive `contentType`.
    #[error(
        "static resource '{name}' has directory content but non-archive contentType '{content_type}'"
    )]
    StaticResourceExpectedArchive {
        /// Static resource component name.
        name: String,
        /// Declared MIME contentType.
        content_type: String,
    },

    /// Static resource XML is missing a `<contentType>` element.
    #[error("static resource '{name}' is missing <contentType> in its metadata XML")]
    StaticResourceMissingContentType {
        /// Static resource identifier or path.
        name: String,
    },

    /// Static resource archive file could not be opened as a valid zip.
    #[error("unable to open zip archive '{path}' for static resource '{name}': {reason}")]
    BadZipFile {
        /// Archive path.
        path: PathBuf,
        /// Static resource name.
        name: String,
        /// Failure details.
        reason: String,
    },

    /// Profile fragments across package directories declared conflicting singleton values.
    #[error(
        "conflicting singleton <{tag}> in Profile '{profile_name}' ({first_path} vs {second_path})"
    )]
    ProfileSingletonConflict {
        /// Profile fullName.
        profile_name: String,
        /// Conflicting singleton tag name.
        tag: String,
        /// First fragment file path.
        first_path: PathBuf,
        /// Conflicting fragment file path.
        second_path: PathBuf,
    },

    /// Recomposition encountered child components without a parent component.
    #[error("missing parent component in recomposition state for children: {children}")]
    MissingRecompositionParent {
        /// Comma-separated child names.
        children: String,
    },

    /// `UIBundle` deploy-time descriptor validation failed (`ui-bundle.json` / `mcp-resources.json`).
    #[error("invalid UIBundle descriptor: {message}")]
    UiBundleValidation {
        /// Validation failure message.
        message: String,
    },

    /// JSON/YAML conversion failure for `ExternalServiceRegistration` schema decomposition.
    #[error("invalid schema format for '{name}' ({path}): {reason}")]
    InvalidSchema {
        /// Component name.
        name: String,
        /// Related XML or schema file path.
        path: PathBuf,
        /// Parser or serializer failure reason.
        reason: String,
    },
}
