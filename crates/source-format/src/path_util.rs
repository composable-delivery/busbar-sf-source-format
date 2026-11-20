//! Path manipulation utilities matching SDR's `src/utils/path.ts`.

use std::path::{Component, Path, PathBuf};

use crate::registry::MetadataType;
use crate::tree::{TreeError, base_name, ensure_safe_directory_name};

use crate::convert::TargetFormat;

const DEFAULT_PACKAGE_ROOT_SFDX: &str = "main/default";
const META_XML_SUFFIX: &str = "-meta.xml";

/// Which matching path segment to start trimming from in [`trim_until`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrimOccurrence {
    First,
    Last,
}

/// Parsed `<fullName>.<suffix>-meta.xml` filename components.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MetadataXml {
    pub full_name: String,
    pub suffix: String,
    pub path: PathBuf,
}

/// Strips `-meta.xml` and any segment matching `md_type`'s `suffix` or `legacy_suffix`.
pub(crate) fn base_without_suffixes(fs_path: &Path, md_type: &MetadataType) -> String {
    let name = fs_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let without_meta = name.strip_suffix(META_XML_SUFFIX).unwrap_or(name);
    without_meta
        .split('.')
        .filter(|part| {
            let is_suffix = md_type.suffix.as_deref() == Some(*part);
            let is_legacy = md_type.legacy_suffix.as_deref() == Some(*part);
            !is_suffix && !is_legacy
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// Returns the extension without a leading `.`, or `""` if none exists.
pub(crate) fn ext_name(fs_path: &Path) -> &str {
    fs_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
}

/// Returns the immediate parent directory's name, or `""` if absent.
pub(crate) fn parent_name(fs_path: &Path) -> &str {
    fs_path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or_default()
}

/// Trims `fs_path` up until and including `part`. Returns `fs_path` unchanged if `part` is absent.
pub(crate) fn trim_until(fs_path: &Path, part: &str, occurrence: TrimOccurrence) -> PathBuf {
    let segments: Vec<&str> = fs_path
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    let idx = match occurrence {
        TrimOccurrence::First => segments.iter().position(|s| *s == part),
        TrimOccurrence::Last => segments.iter().rposition(|s| *s == part),
    };
    let Some(start) = idx else {
        return fs_path.to_path_buf();
    };
    segments[start..].iter().collect()
}

/// Parses `<fullName>.<suffix>-meta.xml` filenames, matching SDR's `parseMetadataXml`.
pub(crate) fn parse_metadata_xml(fs_path: &Path) -> Option<MetadataXml> {
    let name = fs_path.file_name()?.to_str()?;
    let stem = name.strip_suffix(META_XML_SUFFIX)?;
    let (full_name, suffix) = stem.rsplit_once('.')?;
    if full_name.is_empty() || suffix.is_empty() {
        return None;
    }
    Some(MetadataXml {
        full_name: full_name.to_owned(),
        suffix: suffix.to_owned(),
        path: fs_path.to_path_buf(),
    })
}

/// Parses folder-nested full names such as `foo/bar/My_Report` relative to `directory_name`.
pub(crate) fn parse_nested_full_name(fs_path: &Path, directory_name: &str) -> Option<String> {
    let segments: Vec<&str> = fs_path
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    let dir_idx = segments.iter().rposition(|s| *s == directory_name)?;
    let tail = &segments[(dir_idx + 1)..];
    if tail.is_empty() {
        return None;
    }
    let mut parts: Vec<&str> = tail.to_vec();
    let last_raw = parts.pop()?;
    let without_meta = last_raw.strip_suffix(META_XML_SUFFIX).unwrap_or(last_raw);
    let file_name = without_meta.split('.').next().unwrap_or(without_meta);
    parts.push(file_name);
    Some(parts.join("/"))
}

/// Computes the package-relative output path for a component file in either format.
pub(crate) fn calculate_relative_path(
    format: TargetFormat,
    self_type: &MetadataType,
    parent_type: Option<&MetadataType>,
    full_name: &str,
    fs_path: &Path,
) -> Result<PathBuf, TreeError> {
    let base = match format {
        TargetFormat::Source => PathBuf::from(DEFAULT_PACKAGE_ROOT_SFDX),
        TargetFormat::Metadata => PathBuf::new(),
    };

    ensure_safe_directory_name(&self_type.directory_name, &self_type.name)?;
    if let Some(pt) = parent_type {
        ensure_safe_directory_name(&pt.directory_name, &pt.name)?;
    }

    if self_type.suffix.is_none()
        || self_type.stored_in_folder()
        || self_type.folder_content_type.is_some()
        || matches!(
            self_type.id.as_str(),
            "digitalexperiencebundle" | "digitalexperience" | "uibundle"
        )
    {
        return Ok(base.join(trim_until(
            fs_path,
            &self_type.directory_name,
            TrimOccurrence::Last,
        )));
    }

    let file_name = fs_path.file_name().unwrap_or_default();
    if self_type.folder_type.is_some() {
        if let Some(pt) = parent_type
            && pt.folder_type.is_some()
            && pt.folder_type.as_deref() != Some(self_type.id.as_str())
        {
            return Ok(base.join(trim_until(
                fs_path,
                &pt.directory_name,
                TrimOccurrence::First,
            )));
        }
        let folder = full_name.split('/').next().unwrap_or(full_name);
        return Ok(base
            .join(&self_type.directory_name)
            .join(folder)
            .join(file_name));
    }

    Ok(base.join(&self_type.directory_name).join(file_name))
}

/// Convenience alias forwarding to [`crate::tree::base_name`].
pub(crate) fn path_base_name(path: &Path) -> &str {
    base_name(path)
}
