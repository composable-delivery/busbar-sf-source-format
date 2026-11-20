//! Virtual in-memory [`TreeContainer`] implementation for tests and synthetic trees.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::error::TreeError;
use super::safety::normalize_lexical;
use super::tree::TreeContainer;

/// Virtual in-memory [`TreeContainer`] equivalent to SDR's `VirtualTreeContainer`.
#[derive(Debug, Clone, Default)]
pub struct MemoryTree {
    files: BTreeMap<String, Vec<u8>>,
    dir_children: BTreeMap<String, BTreeSet<String>>,
}

impl MemoryTree {
    /// Creates an empty [`MemoryTree`].
    pub fn new() -> Self {
        let mut dir_children = BTreeMap::new();
        dir_children.insert(".".to_owned(), BTreeSet::new());
        Self {
            files: BTreeMap::new(),
            dir_children,
        }
    }

    /// Inserts a virtual file and automatically registers all parent directories.
    pub fn insert_file(&mut self, path: impl AsRef<Path>, contents: impl Into<Vec<u8>>) {
        let key = normalize_key(path.as_ref());
        if key == "." {
            return;
        }
        let parts: Vec<&str> = key.split('/').filter(|p| !p.is_empty()).collect();
        for i in 0..parts.len() {
            let parent = if i == 0 {
                if key.starts_with('/') {
                    "/".to_owned()
                } else {
                    ".".to_owned()
                }
            } else if key.starts_with('/') {
                format!("/{}", parts[..i].join("/"))
            } else {
                parts[..i].join("/")
            };
            self.dir_children
                .entry(parent)
                .or_default()
                .insert(parts[i].to_owned());
        }
        self.files.insert(key, contents.into());
    }
}

fn normalize_key(path: &Path) -> String {
    let posix = path.to_string_lossy().replace('\\', "/");
    let norm = normalize_lexical(Path::new(&posix));
    let s = norm.to_string_lossy().replace('\\', "/");
    if s.is_empty() || s == "." {
        ".".to_owned()
    } else if s == "/" {
        "/".to_owned()
    } else {
        s.trim_start_matches("./").trim_end_matches('/').to_owned()
    }
}

impl TreeContainer for MemoryTree {
    fn exists(&self, path: &Path) -> bool {
        let key = normalize_key(path);
        self.dir_children.contains_key(&key) || self.files.contains_key(&key)
    }

    fn is_directory(&self, path: &Path) -> Result<bool, TreeError> {
        let key = normalize_key(path);
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
        let key = normalize_key(path);
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
        let key = normalize_key(path);
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
