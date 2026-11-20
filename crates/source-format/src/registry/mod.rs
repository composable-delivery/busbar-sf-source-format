//! Salesforce metadata type registry, source behavior presets, and project variants.
//!
//! This crate embeds the upstream Salesforce CLI (`@salesforce/source-deploy-retrieve`)
//! metadata registry, standard value set list, and source behavior presets, and supports
//! layering project-level overrides from `sfdx-project.json`.

mod access;
mod error;
mod project;
mod types;
mod variants;

pub use access::RegistryAccess;
pub use error::RegistryError;
pub use project::{
    EnvCondition, EnvConditionValue, PackageDirectory, ReplacementConfig, ReplacementSource,
    ReplacementTarget, SfdxProjectConfig,
};
pub use types::{
    AdapterStrategy, ChildrenConfig, DecompositionStrategy, MetadataRegistry,
    MetadataRegistryPatch, MetadataType, Preset, RecompositionStrategy, StandardValueSetRegistry,
    TransformerStrategy, TypeStrategies,
};
pub use variants::{
    default_registry, effective_registry, effective_registry_from_config, first_level_merge,
    load_preset, patch_to_registry, standard_value_set_registry,
};
