//! Abstract tree container trait and filename classification utilities.

use std::path::{Path, PathBuf};

use super::error::TreeError;

const META_XML_SUFFIX: &str = "-meta.xml";

/// Kind of file searched by [`TreeContainer::find`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TreeFileKind {
    /// A non-meta content file (not ending in `<name>.<suffix>-meta.xml`).
    Content,
    /// A metadata XML sidecar file (`<name>.<suffix>-meta.xml`).
    MetadataXml,
}

/// Directory entry metadata returned by [`TreeContainer::read_directory_entries`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntryInfo {
    /// Child entry basename within the directory.
    pub name: String,
    /// Whether this entry is a directory (following symlinks where supported).
    pub is_dir: bool,
}

/// Abstract read interface over a filesystem directory, zip archive, or virtual tree.
pub trait TreeContainer {
    /// Returns `true` if `path` exists in the container.
    fn exists(&self, path: &Path) -> bool;

    /// Returns `true` if `path` exists and is a directory.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError::PathNotFound`] if `path` does not exist or [`TreeError::Io`] on
    /// filesystem errors.
    fn is_directory(&self, path: &Path) -> Result<bool, TreeError>;

    /// Lists entry names (file and directory basenames) inside `path`.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError`] if `path` cannot be read.
    fn read_directory(&self, path: &Path) -> Result<Vec<String>, TreeError>;

    /// Lists entry names along with directory status inside `path`.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError`] if `path` cannot be read.
    fn read_directory_entries(&self, path: &Path) -> Result<Vec<DirEntryInfo>, TreeError> {
        let names = self.read_directory(path)?;
        let mut entries = Vec::with_capacity(names.len());
        for name in names {
            let full = path.join(&name);
            let is_dir = self.is_directory(&full).unwrap_or(false);
            entries.push(DirEntryInfo { name, is_dir });
        }
        Ok(entries)
    }

    /// Reads the raw bytes of the file at `path`.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError`] if `path` does not exist or is a directory.
    fn read_file(&self, path: &Path) -> Result<Vec<u8>, TreeError>;

    /// Reads the file at `path` as UTF-8 text.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError`] if reading fails or bytes are not valid UTF-8.
    fn read_to_string(&self, path: &Path) -> Result<String, TreeError> {
        let bytes = self.read_file(path)?;
        String::from_utf8(bytes).map_err(|source| TreeError::InvalidUtf8 {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Searches `directory` for the first entry matching `name` (before the first `.`) and
    /// [`TreeFileKind`].
    fn find(&self, kind: TreeFileKind, name: &str, directory: &Path) -> Option<PathBuf> {
        let entries = self.read_directory(directory).ok()?;
        let found = entries.into_iter().find(|entry| {
            let is_meta = is_metadata_xml_filename(entry);
            let kind_matches = match kind {
                TreeFileKind::MetadataXml => is_meta,
                TreeFileKind::Content => !is_meta,
            };
            kind_matches && base_name(Path::new(entry)) == name
        })?;
        Some(directory.join(found))
    }
}

/// Returns the portion of the path's filename before the first `.`, matching SDR's `baseName`.
pub fn base_name(path: &Path) -> &str {
    let file = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    file.split('.').next().unwrap_or_default()
}

/// Returns `true` if `filename` matches SDR's `(.+)\.(.+)-meta\.xml` pattern.
pub fn is_metadata_xml_filename(filename: &str) -> bool {
    let Some(stem) = filename.strip_suffix(META_XML_SUFFIX) else {
        return false;
    };
    let Some((prefix, suffix)) = stem.rsplit_once('.') else {
        return false;
    };
    !prefix.is_empty() && !suffix.is_empty()
}
