//! Source component resolution, adapters, and `ComponentSet` management.

pub(crate) mod adapters;
mod component;
mod component_set;
mod error;
mod resolver;

pub use adapters::ResolveMode;
pub use component::{ComponentProps, MetadataMember, SourceComponent, TreeHandle};
pub use component_set::{
    ComponentSet, ComponentSetBuilder, ComponentSetOptions, ManifestOptions, MetadataOptions,
};
pub use error::ResolveError;
pub use resolver::{ForceIgnorePolicy, MetadataResolver};

pub use crate::manifest::{DestructiveChangesKind, ManifestPurpose};
