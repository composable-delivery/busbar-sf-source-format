//! Filesystem-backed [`TreeContainer`] implementation.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};

use super::error::TreeError;
use super::tree::{DirEntryInfo, TreeContainer, TreeFileKind, base_name, is_metadata_xml_filename};

type DirCache = Arc<RwLock<HashMap<PathBuf, Arc<[DirEntryInfo]>>>>;

enum CachedEntry {
    Uncached,
    Missing,
    Present { is_dir: bool },
}

/// [`TreeContainer`] backed by the host OS filesystem (`std::fs`).
#[derive(Debug, Default)]
pub struct FsTree {
    cwd: Option<PathBuf>,
    dir_cache: Option<DirCache>,
}

impl Clone for FsTree {
    fn clone(&self) -> Self {
        Self {
            cwd: self.cwd.clone(),
            dir_cache: self.dir_cache.as_ref().map(Arc::clone),
        }
    }
}

impl FsTree {
    /// Creates a new [`FsTree`] resolving paths directly (or relative to process CWD).
    pub fn new() -> Self {
        Self {
            cwd: None,
            dir_cache: None,
        }
    }

    /// Creates a new [`FsTree`] with a shared read-through directory listing cache.
    ///
    /// Intended for read-only resolution passes where the underlying directory tree is not
    /// mutated while the tree handle is in use.
    pub fn new_cached() -> Self {
        Self {
            cwd: None,
            dir_cache: Some(Arc::new(RwLock::new(HashMap::new()))),
        }
    }

    /// Creates a new [`FsTree`] resolving relative paths against `cwd`.
    pub fn with_cwd(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: Some(cwd.into()),
            dir_cache: None,
        }
    }

    /// Resolves `path` against `self.cwd` if set.
    pub fn resolve_path(&self, path: &Path) -> PathBuf {
        self.resolve(path)
    }

    fn resolve(&self, path: &Path) -> PathBuf {
        match &self.cwd {
            Some(cwd) if !path.is_absolute() && !path.starts_with(cwd) => cwd.join(path),
            _ => path.to_path_buf(),
        }
    }

    fn cached_lookup(&self, resolved: &Path) -> CachedEntry {
        let Some(cache) = self.dir_cache.as_ref() else {
            return CachedEntry::Uncached;
        };
        let guard = cache.read().unwrap_or_else(PoisonError::into_inner);
        if guard.contains_key(resolved) {
            return CachedEntry::Present { is_dir: true };
        }
        let Some(parent) = resolved.parent() else {
            return CachedEntry::Uncached;
        };
        let Some(parent_entries) = guard.get(parent) else {
            return CachedEntry::Uncached;
        };
        let Some(file_name) = resolved.file_name().and_then(|s| s.to_str()) else {
            return CachedEntry::Uncached;
        };
        match parent_entries.binary_search_by(|entry| entry.name.as_str().cmp(file_name)) {
            Ok(idx) => CachedEntry::Present {
                is_dir: parent_entries[idx].is_dir,
            },
            Err(_) => CachedEntry::Missing,
        }
    }

    fn read_dir_shared(&self, path: &Path) -> Result<Arc<[DirEntryInfo]>, TreeError> {
        let resolved = self.resolve(path);
        if let Some(cache) = &self.dir_cache {
            let guard = cache.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(cached) = guard.get(&resolved) {
                return Ok(Arc::clone(cached));
            }
        }

        match fs::read_dir(&resolved) {
            Ok(entries) => {
                let mut parsed = Vec::new();
                for entry in entries {
                    let entry = entry.map_err(|source| TreeError::Io {
                        path: resolved.clone(),
                        source,
                    })?;
                    let is_dir = match entry.file_type() {
                        Ok(ft) if ft.is_dir() => true,
                        Ok(ft) if ft.is_file() => false,
                        _ => fs::metadata(entry.path()).is_ok_and(|meta| meta.is_dir()),
                    };
                    parsed.push(DirEntryInfo {
                        name: entry.file_name().to_string_lossy().into_owned(),
                        is_dir,
                    });
                }
                parsed.sort_by(|a, b| a.name.cmp(&b.name));
                let shared: Arc<[DirEntryInfo]> = Arc::from(parsed);
                if let Some(cache) = &self.dir_cache {
                    let mut guard = cache.write().unwrap_or_else(PoisonError::into_inner);
                    guard.entry(resolved).or_insert_with(|| Arc::clone(&shared));
                }
                Ok(shared)
            }
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                // Do not cache missing or non-directory paths as valid directories.
                Ok(Arc::from([]))
            }
            Err(source) => Err(TreeError::Io {
                path: resolved,
                source,
            }),
        }
    }
}

impl TreeContainer for FsTree {
    fn exists(&self, path: &Path) -> bool {
        let resolved = self.resolve(path);
        match self.cached_lookup(&resolved) {
            CachedEntry::Present { .. } => true,
            CachedEntry::Missing => false,
            CachedEntry::Uncached => resolved.exists(),
        }
    }

    fn is_directory(&self, path: &Path) -> Result<bool, TreeError> {
        let resolved = self.resolve(path);
        match self.cached_lookup(&resolved) {
            CachedEntry::Present { is_dir } => return Ok(is_dir),
            CachedEntry::Missing => return Err(TreeError::PathNotFound { path: resolved }),
            CachedEntry::Uncached => {}
        }
        match fs::metadata(&resolved) {
            Ok(meta) => Ok(meta.is_dir()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                Err(TreeError::PathNotFound { path: resolved })
            }
            Err(source) => Err(TreeError::Io {
                path: resolved,
                source,
            }),
        }
    }

    fn read_directory(&self, path: &Path) -> Result<Vec<String>, TreeError> {
        let entries = self.read_dir_shared(path)?;
        Ok(entries.iter().map(|entry| entry.name.clone()).collect())
    }

    fn read_directory_entries(&self, path: &Path) -> Result<Vec<DirEntryInfo>, TreeError> {
        let entries = self.read_dir_shared(path)?;
        Ok(entries.to_vec())
    }

    fn read_file(&self, path: &Path) -> Result<Vec<u8>, TreeError> {
        let resolved = self.resolve(path);
        fs::read(&resolved).map_err(|source| {
            if source.kind() == std::io::ErrorKind::IsADirectory || resolved.is_dir() {
                TreeError::ExpectedFile { path: resolved }
            } else {
                TreeError::Io {
                    path: resolved,
                    source,
                }
            }
        })
    }

    fn find(&self, kind: TreeFileKind, name: &str, directory: &Path) -> Option<PathBuf> {
        let entries = self.read_dir_shared(directory).ok()?;
        let found = entries.iter().find(|entry| {
            let is_meta = is_metadata_xml_filename(&entry.name);
            let kind_matches = match kind {
                TreeFileKind::MetadataXml => is_meta,
                TreeFileKind::Content => !is_meta,
            };
            kind_matches && base_name(Path::new(&entry.name)) == name
        })?;
        Some(directory.join(&found.name))
    }
}
