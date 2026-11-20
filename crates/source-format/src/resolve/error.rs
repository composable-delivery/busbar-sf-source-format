//! Error types for metadata source resolution and `ComponentSet` construction.

use std::path::PathBuf;

use thiserror::Error;

use crate::manifest::ManifestError;

/// Errors returned while resolving metadata components, manifests, or `ComponentSet`s.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ResolveError {
    /// Underlying registry query or variant error.
    #[error(transparent)]
    Registry(#[from] crate::registry::RegistryError),

    /// Underlying tree container or `.forceignore` error.
    #[error(transparent)]
    Tree(#[from] crate::tree::TreeError),

    /// Underlying XML parsing error.
    #[error(transparent)]
    Xml(#[from] crate::xml::XmlError),

    /// Underlying manifest parsing or validation error.
    #[error(transparent)]
    Manifest(#[from] ManifestError),

    /// Specified source path or directory does not exist.
    #[error("path not found: {path}")]
    PathNotFound {
        /// Missing path.
        path: PathBuf,
    },

    /// Metadata type could not be inferred for a file.
    #[error("could not infer metadata type for path '{path}'")]
    TypeInference {
        /// Unrecognized file path.
        path: PathBuf,
    },

    /// Unexpected child metadata file inside a decomposed component directory.
    #[error("unexpected child metadata '{path}' inside '{parent_type}'")]
    UnexpectedChildType {
        /// Unexpected file path.
        path: PathBuf,
        /// Parent metadata type name.
        parent_type: String,
    },

    /// Expected content or metadata sidecar file is missing.
    #[error("expected source files at '{path}' for '{type_name}'")]
    ExpectedSourceFiles {
        /// Trigger path missing its companion file.
        path: PathBuf,
        /// Metadata type name.
        type_name: String,
    },

    /// Root metadata `-meta.xml` file is excluded by `.forceignore`.
    #[error(
        "metadata XML file '{xml_path}' is ignored by .forceignore (triggered by '{trigger_path}')"
    )]
    ForceIgnoreMetadataXml {
        /// Ignored `-meta.xml` path.
        xml_path: PathBuf,
        /// Trigger path that referenced it.
        trigger_path: PathBuf,
    },

    /// Source content file is excluded by `.forceignore`.
    #[error("source file '{source_path}' for '{type_name}' is ignored by .forceignore")]
    ForceIgnoreSource {
        /// Metadata type name.
        type_name: String,
        /// Ignored source file path.
        source_path: PathBuf,
    },

    /// Missing `uniqueIdElement` in a non-decomposed child XML element.
    #[error("missing <{element}> on '{child_type}' in '{xml_path}'")]
    MissingUniqueElement {
        /// Expected XML element tag name.
        element: String,
        /// Child metadata type name.
        child_type: String,
        /// Parent XML path.
        xml_path: PathBuf,
    },

    /// Wildcards (`*`) were passed in destructive changes entries.
    #[error("Wildcards are not supported when providing destructive metadata entries")]
    WildcardInDestructiveEntries,

    /// Neither `sourcepath`, `manifest`, nor `metadata` was provided to `ComponentSetBuilder`.
    #[error("cannot build ComponentSet without sourcepath, manifest, or metadata options")]
    EmptyBuilderInput,

    /// Decomposed label type was specified as `CustomLabels` instead of `CustomLabel`.
    #[error("use CustomLabel instead of CustomLabels when decomposeCustomLabelsBeta2 is enabled")]
    UseCustomLabelForDecomposed,

    /// Adapter strategy is not yet implemented in this build.
    #[error("adapter strategy '{strategy}' is not supported for '{type_name}'")]
    UnsupportedAdapter {
        /// Strategy identifier.
        strategy: String,
        /// Metadata type name.
        type_name: String,
    },
}
