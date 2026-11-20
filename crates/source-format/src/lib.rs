//! Rust port of Salesforce CLI (`@salesforce/source-deploy-retrieve`) metadata format
//! resolution, decomposition (MDAPI -> SFDX), recomposition (SFDX -> MDAPI), and manifest
//! tooling, bundling the metadata registry, tree containers, and lossless XML DOM.

pub mod convert;
pub(crate) mod io_pool;
pub mod manifest;
pub(crate) mod path_util;
pub mod registry;
pub mod resolve;
pub mod tree;
pub mod xml;
