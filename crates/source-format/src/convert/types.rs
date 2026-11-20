//! Public and internal conversion configuration and pipeline types.

use std::path::{Path, PathBuf};

use regex::Regex;

use crate::resolve::{SourceComponent, TreeHandle};

/// Target format for [`MetadataConverter::convert`](crate::convert::MetadataConverter::convert).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TargetFormat {
    /// SFDX source format (decomposed, `-meta.xml` sidecars, under `main/default/`).
    Source,
    /// Metadata API format (recomposed, flat type directories + `package.xml`).
    Metadata,
}

/// Controls whether conversion creates a timestamped `metadataPackage_<ts>` subdirectory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum UniqueDirPolicy {
    /// Generate a unique `metadataPackage_<timestamp>` subdirectory inside `output_dir`.
    #[default]
    Generate,
    /// Write directly into `output_dir` (equivalent to `genUniqueDir: false` in SDR).
    Exact,
}

/// Output destination configuration for [`MetadataConverter::convert`](crate::convert::MetadataConverter::convert).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ConvertOutput {
    /// Write converted files (and `package.xml` when converting to MDAPI) to a directory.
    Directory {
        /// Base output directory path.
        output_dir: PathBuf,
        /// Optional package name subdirectory / manifest `fullName`.
        package_name: Option<String>,
        /// Whether to generate a unique subdirectory when `package_name` is `None`.
        unique_dir: UniqueDirPolicy,
    },
    /// Write converted files into a ZIP archive (to disk if `output_dir` is set, else in memory).
    Zip {
        /// Optional output directory where `<package_name|metadataPackage_ts>.zip` is written.
        output_dir: Option<PathBuf>,
        /// Optional zip base name and manifest `fullName`.
        package_name: Option<String>,
        /// Whether to generate a unique name when `package_name` is `None`.
        unique_dir: UniqueDirPolicy,
    },
    /// Merge converted source components over an existing set of source components.
    Merge {
        /// Existing components in the target project used for in-place merges.
        merge_with: Vec<SourceComponent>,
        /// Default package directory for newly discovered components.
        default_dir: PathBuf,
        /// Project root directory used as the symlink safety boundary.
        project_dir: Option<PathBuf>,
    },
}

/// Controls how [`MetadataConverter::convert_incremental`](crate::convert::MetadataConverter::convert_incremental)
/// validates cached input files on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum CacheVerifyMode {
    /// Reuse cached `blake3` digests when file length and modification timestamp are unchanged;
    /// re-hash with `blake3` when metadata differs (fast default).
    #[default]
    StatThenHash,
    /// Always read and recompute `blake3` digests for every input and cached output file.
    AlwaysHash,
}

/// Controls which output components are re-resolved into [`ConvertResult::converted`] after an
/// incremental directory conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum IncrementalResolvePolicy {
    /// Re-resolve only newly written components from rebuilt units, leaving skipped cached units
    /// out of [`ConvertResult::converted`] (fastest).
    #[default]
    RebuiltOnly,
    /// Re-resolve all current output files so [`ConvertResult::converted`] matches a cold run.
    All,
}

/// Configuration for persistent `blake3` incremental conversions via
/// [`MetadataConverter::convert_incremental`](crate::convert::MetadataConverter::convert_incremental).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvertCache {
    cache_dir: PathBuf,
    verify_mode: CacheVerifyMode,
    resolve_policy: IncrementalResolvePolicy,
}

impl ConvertCache {
    /// Creates an incremental cache configuration storing persistent index/archive artifacts in
    /// `cache_dir` using [`CacheVerifyMode::StatThenHash`] and
    /// [`IncrementalResolvePolicy::RebuiltOnly`].
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            cache_dir: cache_dir.into(),
            verify_mode: CacheVerifyMode::default(),
            resolve_policy: IncrementalResolvePolicy::default(),
        }
    }

    /// Sets how cached files are verified on disk.
    #[must_use]
    pub fn with_verify_mode(mut self, verify_mode: CacheVerifyMode) -> Self {
        self.verify_mode = verify_mode;
        self
    }

    /// Sets which output paths are re-resolved into [`ConvertResult::converted`].
    #[must_use]
    pub fn with_resolve_policy(mut self, resolve_policy: IncrementalResolvePolicy) -> Self {
        self.resolve_policy = resolve_policy;
        self
    }

    /// Returns the persistent cache directory path.
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Returns the configured cache verification mode.
    pub fn verify_mode(&self) -> CacheVerifyMode {
        self.verify_mode
    }

    /// Returns the configured post-write re-resolution policy.
    pub fn resolve_policy(&self) -> IncrementalResolvePolicy {
        self.resolve_policy
    }
}

/// Summary statistics recorded by [`MetadataConverter::convert_incremental`](crate::convert::MetadataConverter::convert_incremental).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IncrementalStats {
    /// Total conversion work units (components or recomposition buckets) evaluated.
    pub total_units: usize,
    /// Work units satisfied from the persistent cache.
    pub cached_units: usize,
    /// Work units rebuilt due to missing, invalidated, or modified inputs/outputs.
    pub rebuilt_units: usize,
    /// Output files or ZIP entries reused without writing or re-compressing.
    pub skipped_writes: usize,
    /// Output files or ZIP entries newly written or compressed.
    pub written_files: usize,
    /// Obsolete output files or ZIP entries removed since the previous cached build.
    pub removed_stale_files: usize,
}

/// Result returned by [`MetadataConverter::convert`](crate::convert::MetadataConverter::convert).
#[derive(Debug, Clone, Default)]
pub struct ConvertResult {
    /// Output directory or `.zip` path written, when applicable.
    pub package_path: Option<PathBuf>,
    /// In-memory ZIP bytes when `ConvertOutput::Zip { output_dir: None, .. }` was used.
    pub zip_bytes: Option<Vec<u8>>,
    /// Total file + directory entries recorded in the generated ZIP archive.
    pub zip_file_count: usize,
    /// Re-resolved source components produced on disk.
    pub converted: Vec<SourceComponent>,
    /// Paths deleted during merge conversion (e.g., removed unaddressable decomposed children).
    pub deleted: Vec<PathBuf>,
    /// Incremental build statistics when invoked via [`MetadataConverter::convert_incremental`](crate::convert::MetadataConverter::convert_incremental).
    pub incremental: Option<IncrementalStats>,
}

/// Compiled string replacement rule attached to a component file path.
#[derive(Debug, Clone)]
pub(crate) struct MarkedReplacement {
    pub to_replace: Regex,
    pub replace_with: String,
}

/// Source payload for a single [`WriteInfo`].
#[derive(Debug, Clone)]
pub(crate) enum WriteSource {
    /// Pre-serialized XML or binary bytes ready to write.
    Bytes(Vec<u8>),
    /// Copy file contents from a tree container, optionally applying replacements.
    TreeFile {
        tree: TreeHandle,
        path: PathBuf,
        replacements: Vec<MarkedReplacement>,
    },
}

/// Action executed by a writer for a single output path.
#[derive(Debug, Clone)]
pub(crate) enum WriteAction {
    /// Write file contents.
    Write(WriteSource),
    /// Delete an obsolete file on disk during merge.
    Delete,
}

/// Single output operation emitted by a transformer or finalizer.
#[derive(Debug, Clone)]
pub(crate) struct WriteInfo {
    pub output: PathBuf,
    pub action: WriteAction,
}

/// Controls how many written paths from a [`WriterFormat`] need post-write re-resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ResolveScope {
    /// Re-resolve every written file (decomposed components, `DecomposedLabels`, finalizers).
    #[default]
    AllFiles,
    /// Re-resolve only the first written file for this component (paired content + meta XML / bundles).
    FirstOnly,
}

/// Grouped write operations associated with a converted component.
#[derive(Debug, Clone)]
pub(crate) struct WriterFormat {
    pub write_infos: Vec<WriteInfo>,
    pub resolve_scope: ResolveScope,
}
