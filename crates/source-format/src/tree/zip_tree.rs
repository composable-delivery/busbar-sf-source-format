//! In-memory zip archive [`TreeContainer`] implementation.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read as _};
use std::path::Path;

use zip::ZipArchive;

use super::error::TreeError;
use super::safety::{ensure_safe_directory_name, normalize_lexical};
use super::tree::TreeContainer;

/// [`TreeContainer`] backed by an in-memory ZIP archive (equivalent to SDR's `ZipTreeContainer`).
#[derive(Debug, Clone)]
pub struct ZipTree {
    files: BTreeMap<String, Vec<u8>>,
    dir_children: BTreeMap<String, BTreeSet<String>>,
}

impl ZipTree {
    /// Parses an in-memory ZIP archive and builds directory indices for all entries.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError::Zip`] if the ZIP archive is invalid, [`TreeError::PathTraversal`]
    /// if any entry contains `..` segments, or [`TreeError::Io`] on decompression failure.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TreeError> {
        let cursor = Cursor::new(bytes);
        let mut archive = ZipArchive::new(cursor)?;
        let mut files = BTreeMap::new();
        let mut dir_children: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        dir_children.entry(".".to_owned()).or_default();

        for idx in 0..archive.len() {
            let mut entry = archive.by_index(idx)?;
            let raw_name = entry.name()?.replace('\\', "/");
            ensure_safe_directory_name(&raw_name, "zip entry")?;
            let is_dir = raw_name.ends_with('/') || entry.is_dir();
            let trimmed = raw_name.trim_matches('/');
            if trimmed.is_empty() {
                continue;
            }

            if is_dir {
                register_ancestors(&mut dir_children, trimmed);
                dir_children.entry(trimmed.to_owned()).or_default();
            } else {
                let mut data = Vec::new();
                entry
                    .read_to_end(&mut data)
                    .map_err(|source| TreeError::Io {
                        path: Path::new(trimmed).to_path_buf(),
                        source,
                    })?;
                register_ancestors(&mut dir_children, trimmed);
                files.insert(trimmed.to_owned(), data);
            }
        }

        Ok(Self {
            files,
            dir_children,
        })
    }

    fn normalize_key(path: &Path) -> String {
        let posix = path.to_string_lossy().replace('\\', "/");
        let norm = normalize_lexical(Path::new(&posix));
        let s = norm.to_string_lossy().replace('\\', "/");
        if s.is_empty() || s == "." || s == "./" {
            ".".to_owned()
        } else {
            s.trim_start_matches("./").trim_end_matches('/').to_owned()
        }
    }
}

fn register_ancestors(dir_children: &mut BTreeMap<String, BTreeSet<String>>, entry: &str) {
    let parts: Vec<&str> = entry.split('/').filter(|p| !p.is_empty()).collect();
    for i in 0..parts.len() {
        let parent = if i == 0 {
            ".".to_owned()
        } else {
            parts[..i].join("/")
        };
        dir_children
            .entry(parent)
            .or_default()
            .insert(parts[i].to_owned());
    }
}

impl TreeContainer for ZipTree {
    fn exists(&self, path: &Path) -> bool {
        let key = Self::normalize_key(path);
        self.dir_children.contains_key(&key) || self.files.contains_key(&key)
    }

    fn is_directory(&self, path: &Path) -> Result<bool, TreeError> {
        let key = Self::normalize_key(path);
        if self.dir_children.contains_key(&key) {
            Ok(true)
        } else if self.files.contains_key(&key) {
            Ok(false)
        } else {
            Err(TreeError::PathNotFound {
                path: path.to_path_buf(),
            })
        }
    }

    fn read_directory(&self, path: &Path) -> Result<Vec<String>, TreeError> {
        let key = Self::normalize_key(path);
        if let Some(children) = self.dir_children.get(&key) {
            Ok(children.iter().cloned().collect())
        } else if self.files.contains_key(&key) {
            Err(TreeError::ExpectedDirectory {
                path: path.to_path_buf(),
            })
        } else {
            Err(TreeError::PathNotFound {
                path: path.to_path_buf(),
            })
        }
    }

    fn read_file(&self, path: &Path) -> Result<Vec<u8>, TreeError> {
        let key = Self::normalize_key(path);
        if let Some(bytes) = self.files.get(&key) {
            Ok(bytes.clone())
        } else if self.dir_children.contains_key(&key) {
            Err(TreeError::ExpectedFile {
                path: path.to_path_buf(),
            })
        } else {
            Err(TreeError::PathNotFound {
                path: path.to_path_buf(),
            })
        }
    }
}
