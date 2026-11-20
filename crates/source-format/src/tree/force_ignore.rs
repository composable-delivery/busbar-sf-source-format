//! `.forceignore` discovery and path matching matching SDR's `ForceIgnore`.

use std::borrow::Cow;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use super::error::TreeError;
use super::safety::normalize_lexical;

const FORCE_IGNORE_FILE: &str = ".forceignore";

const DEFAULT_IGNORE_PATTERNS: &[&str] = &[
    "**/*.dup",
    "**/.*",
    "!.sf",
    "**/.sf/**",
    "!**/.sf/orgs",
    "!**/.sf/orgs/**",
    "**/.sf/orgs/*/**",
    "!**/.sf/orgs/*/remoteMetadata",
    "!**/.sf/orgs/*/remoteMetadata/**",
    "**/package2-descriptor.json",
    "**/package2-manifest.json",
];

/// Path-kind hint passed to [`ForceIgnore::denies_with_kind`] to avoid redundant `stat` calls
/// when the caller already knows whether a path is a file or directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PathKindHint {
    /// Existing regular file on disk (`is_dir = false`).
    File,
    /// Existing directory or unwritten destination path (`is_dir = true`).
    DirectoryOrUnwritten,
}

/// Evaluates `.forceignore` exclusion rules using gitignore syntax and SDR default ignores.
#[derive(Debug, Clone)]
pub struct ForceIgnore {
    inner: Option<Arc<ForceIgnoreInner>>,
}

#[derive(Debug)]
struct ForceIgnoreInner {
    matcher: Gitignore,
    root_dir: PathBuf,
}

impl Default for ForceIgnore {
    fn default() -> Self {
        Self::empty()
    }
}

impl ForceIgnore {
    /// Creates a permissive `ForceIgnore` that accepts every path (used when no `.forceignore`
    /// file exists).
    pub fn empty() -> Self {
        Self { inner: None }
    }

    /// Returns `true` when no `.forceignore` matcher is loaded (every path is accepted).
    pub fn is_empty(&self) -> bool {
        self.inner.is_none()
    }

    /// Searches upward from `seed` for a `.forceignore` file without reading or compiling it.
    pub fn find_path(seed: &Path) -> Option<PathBuf> {
        search_up(seed, FORCE_IGNORE_FILE)
    }

    /// Loads a `.forceignore` file from `path`.
    ///
    /// Returns [`Self::empty()`] if `path` does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError::Io`] if an existing `.forceignore` cannot be read or
    /// [`TreeError::ForceIgnoreParse`] if pattern compilation fails.
    pub fn from_file(path: &Path) -> Result<Self, TreeError> {
        if !path.is_file() {
            return Ok(Self::empty());
        }
        let contents = fs::read_to_string(path).map_err(|source| TreeError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let root_dir = path.parent().map_or_else(|| PathBuf::from("."), absolutize);
        let mut builder = GitignoreBuilder::new(&root_dir);
        for pattern in DEFAULT_IGNORE_PATTERNS {
            builder
                .add_line(None, pattern)
                .map_err(|err| TreeError::ForceIgnoreParse {
                    path: path.to_path_buf(),
                    message: err.to_string(),
                })?;
        }
        for line in contents.lines() {
            builder
                .add_line(None, line)
                .map_err(|err| TreeError::ForceIgnoreParse {
                    path: path.to_path_buf(),
                    message: err.to_string(),
                })?;
        }
        let matcher = builder.build().map_err(|err| TreeError::ForceIgnoreParse {
            path: path.to_path_buf(),
            message: err.to_string(),
        })?;
        Ok(Self {
            inner: Some(Arc::new(ForceIgnoreInner { matcher, root_dir })),
        })
    }

    /// Searches upward from `seed` for `.forceignore` and builds a `ForceIgnore` if found.
    ///
    /// Returns [`Self::empty()`] when no `.forceignore` exists in `seed` or any ancestor.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError`] if a discovered `.forceignore` cannot be read or parsed.
    pub fn find_and_create(seed: &Path) -> Result<Self, TreeError> {
        let Some(found) = search_up(seed, FORCE_IGNORE_FILE) else {
            return Ok(Self::empty());
        };
        Self::from_file(&found)
    }

    /// Returns `true` if `fs_path` matches a `.forceignore` rule and is excluded.
    pub fn denies(&self, fs_path: &Path) -> bool {
        let Some(inner) = &self.inner else {
            return false;
        };
        let abs_path = resolve_against_root(&inner.root_dir, fs_path);
        // Known files on disk pass `DirectoryOrUnwritten` only when not a regular file.
        let kind = match fs::metadata(abs_path.as_ref()) {
            Ok(meta) if meta.is_file() => PathKindHint::File,
            _ => PathKindHint::DirectoryOrUnwritten,
        };
        Self::denies_resolved(inner, abs_path.as_ref(), kind)
    }

    /// Returns `true` if `fs_path` matches a `.forceignore` rule using a caller-supplied
    /// [`PathKindHint`], avoiding extra filesystem metadata queries.
    pub fn denies_with_kind(&self, fs_path: &Path, kind: PathKindHint) -> bool {
        let Some(inner) = &self.inner else {
            return false;
        };
        let abs_path = resolve_against_root(&inner.root_dir, fs_path);
        Self::denies_resolved(inner, abs_path.as_ref(), kind)
    }

    fn denies_resolved(inner: &ForceIgnoreInner, abs_path: &Path, kind: PathKindHint) -> bool {
        let Ok(rel) = abs_path.strip_prefix(&inner.root_dir) else {
            return false;
        };
        if rel.as_os_str().is_empty() {
            return false;
        }
        let is_dir = matches!(kind, PathKindHint::DirectoryOrUnwritten);
        inner
            .matcher
            .matched_path_or_any_parents(rel, is_dir)
            .is_ignore()
    }

    /// Returns `true` if `fs_path` is not excluded by `.forceignore`.
    pub fn accepts(&self, fs_path: &Path) -> bool {
        !self.denies(fs_path)
    }
}

fn resolve_against_root<'a>(root_dir: &Path, fs_path: &'a Path) -> Cow<'a, Path> {
    if fs_path.is_absolute() && !has_dot_segments(fs_path) {
        Cow::Borrowed(fs_path)
    } else if fs_path.is_absolute() {
        Cow::Owned(normalize_lexical(fs_path))
    } else {
        Cow::Owned(normalize_lexical(&root_dir.join(fs_path)))
    }
}

fn has_dot_segments(path: &Path) -> bool {
    path.components()
        .any(|c| matches!(c, Component::CurDir | Component::ParentDir))
}

fn search_up(start: &Path, filename: &str) -> Option<PathBuf> {
    let abs = absolutize(start);
    let mut current = abs.as_path();
    loop {
        let candidate = current.join(filename);
        if candidate.is_file() {
            return Some(candidate);
        }
        current = current.parent()?;
    }
}

fn absolutize(path: &Path) -> PathBuf {
    if path.is_absolute() {
        normalize_lexical(path)
    } else if let Ok(cwd) = std::env::current_dir() {
        normalize_lexical(&cwd.join(path))
    } else {
        normalize_lexical(path)
    }
}
