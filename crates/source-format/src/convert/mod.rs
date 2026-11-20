//! Metadata format conversion pipeline (`MetadataConverter`), transformers, finalizers, and
//! `sfdx-project.json` replacements.

mod cache;
mod converter;
mod error;
mod finalizers;
mod git_index;
mod replacements;
mod transformers;
mod types;
mod ui_bundle;
mod writer;

pub use converter::MetadataConverter;
pub use error::ConvertError;
pub(crate) use replacements::apply_replacements;
pub(crate) use types::MarkedReplacement;
pub use types::{
    CacheVerifyMode, ConvertCache, ConvertOutput, ConvertResult, IncrementalResolvePolicy,
    IncrementalStats, TargetFormat, UniqueDirPolicy,
};
