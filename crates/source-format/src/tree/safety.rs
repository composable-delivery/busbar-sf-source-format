//! Path normalization, root-containment, and symlink-traversal safety checks.

use std::fs;
use std::path::{Component, Path, PathBuf};

use super::error::TreeError;

/// Lexically normalizes a path by resolving `.` and `..` components without touching disk.
pub fn normalize_lexical(path: &Path) -> PathBuf {
    let mut stack: Vec<Component<'_>> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => match stack.last() {
                Some(Component::Normal(_)) => {
                    stack.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => stack.push(comp),
            },
            other => stack.push(other),
        }
    }
    if stack.is_empty() {
        PathBuf::from(".")
    } else {
        stack.iter().collect()
    }
}

/// Ensures `candidate` resolves inside `root` (or equals `root`) after lexical normalization.
///
/// Relative `candidate` paths are joined onto `root` first.
///
/// # Errors
///
/// Returns [`TreeError::PathOutsideRoot`] if `candidate` escapes `root`.
pub fn ensure_within_root(root: &Path, candidate: &Path) -> Result<PathBuf, TreeError> {
    let abs_root = absolutize_lexical(root);
    let joined = if candidate.is_absolute() {
        normalize_lexical(candidate)
    } else {
        normalize_lexical(&abs_root.join(candidate))
    };

    if joined == abs_root || joined.starts_with(&abs_root) {
        Ok(joined)
    } else {
        Err(TreeError::PathOutsideRoot {
            path: candidate.to_path_buf(),
            root: root.to_path_buf(),
        })
    }
}

/// Walks every path segment between `root` (exclusive) and `destination` (inclusive) and
/// rejects any existing symbolic link, matching SDR's `findSymlinkOnPath`.
///
/// # Errors
///
/// Returns [`TreeError::PathOutsideRoot`] if `destination` is outside `root`,
/// [`TreeError::SymlinkOnPath`] if any segment is a symlink, or [`TreeError::Io`] on
/// unexpected metadata errors.
pub fn ensure_no_symlink_on_path(root: &Path, destination: &Path) -> Result<(), TreeError> {
    let norm_root = absolutize_lexical(root);
    let norm_dest = ensure_within_root(&norm_root, destination)?;
    let Ok(rel) = norm_dest.strip_prefix(&norm_root) else {
        return Err(TreeError::PathOutsideRoot {
            path: destination.to_path_buf(),
            root: root.to_path_buf(),
        });
    };

    let mut current = norm_root;
    for comp in rel.components() {
        if let Component::Normal(seg) = comp {
            current.push(seg);
            match fs::symlink_metadata(&current) {
                Ok(meta) => {
                    if meta.file_type().is_symlink() {
                        return Err(TreeError::SymlinkOnPath {
                            symlink: current,
                            destination: destination.to_path_buf(),
                            root: root.to_path_buf(),
                        });
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(TreeError::Io {
                        path: current,
                        source,
                    });
                }
            }
        }
    }
    Ok(())
}

/// Validates that a registry `directoryName` or archive entry contains no `..` traversal segments.
///
/// # Errors
///
/// Returns [`TreeError::PathTraversal`] if any path segment is `..`.
pub fn ensure_safe_directory_name(directory_name: &str, context: &str) -> Result<(), TreeError> {
    if directory_name.split(['/', '\\']).any(|seg| seg == "..") {
        return Err(TreeError::PathTraversal {
            path: directory_name.to_owned(),
            context: context.to_owned(),
        });
    }
    Ok(())
}

fn absolutize_lexical(path: &Path) -> PathBuf {
    if path.is_absolute() {
        normalize_lexical(path)
    } else if let Ok(cwd) = std::env::current_dir() {
        normalize_lexical(&cwd.join(path))
    } else {
        normalize_lexical(path)
    }
}
