//! Tree containers (`FsTree`, `ZipTree`, `MemoryTree`), path safety guards, and `.forceignore`.

mod error;
mod force_ignore;
mod fs_tree;
mod memory_tree;
mod safety;
mod tree;
mod zip_tree;

pub use error::TreeError;
pub use force_ignore::{ForceIgnore, PathKindHint};
pub use fs_tree::FsTree;
pub use memory_tree::MemoryTree;
pub use safety::{
    ensure_no_symlink_on_path, ensure_safe_directory_name, ensure_within_root, normalize_lexical,
};
pub use tree::{DirEntryInfo, TreeContainer, TreeFileKind, base_name, is_metadata_xml_filename};
pub use zip_tree::ZipTree;
