//! Core component representation (`SourceComponent`, `MetadataMember`, `TreeHandle`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::registry::MetadataType;
use crate::tree::{
    DirEntryInfo, ForceIgnore, FsTree, MemoryTree, PathKindHint, TreeContainer, TreeError,
    TreeFileKind, ZipTree,
};
use crate::xml::{XmlDocument, parse_xml};

use crate::convert::{MarkedReplacement, TargetFormat, apply_replacements};
use crate::manifest::DestructiveChangesKind;
use crate::path_util::{
    base_without_suffixes, calculate_relative_path, parse_metadata_xml, path_base_name,
};
use crate::resolve::error::ResolveError;

/// Cheaply cloneable tree container handle hiding `Arc` sharing from public signatures.
#[derive(Debug, Clone)]
pub struct TreeHandle {
    inner: TreeKind,
}

#[derive(Debug, Clone)]
enum TreeKind {
    Fs(Arc<FsTree>),
    Zip(Arc<ZipTree>),
    Memory(Arc<MemoryTree>),
}

impl TreeHandle {
    /// Creates a [`TreeHandle`] backed by the local filesystem.
    pub fn fs() -> Self {
        Self {
            inner: TreeKind::Fs(Arc::new(FsTree::new())),
        }
    }

    /// Creates a [`TreeHandle`] backed by the local filesystem with a read-through directory cache.
    ///
    /// Intended for single-pass read-only resolution where the filesystem tree is not modified
    /// while the handle is in use.
    pub fn cached_fs() -> Self {
        Self {
            inner: TreeKind::Fs(Arc::new(FsTree::new_cached())),
        }
    }

    /// Creates a [`TreeHandle`] backed by the local filesystem with a custom working directory.
    pub fn fs_with_cwd(cwd: impl Into<PathBuf>) -> Self {
        Self {
            inner: TreeKind::Fs(Arc::new(FsTree::with_cwd(cwd))),
        }
    }

    /// Creates a [`TreeHandle`] backed by an in-memory [`ZipTree`].
    pub fn zip(tree: ZipTree) -> Self {
        Self {
            inner: TreeKind::Zip(Arc::new(tree)),
        }
    }

    /// Creates a [`TreeHandle`] backed by an in-memory [`MemoryTree`].
    pub fn memory(tree: MemoryTree) -> Self {
        Self {
            inner: TreeKind::Memory(Arc::new(tree)),
        }
    }

    pub(crate) fn is_fs(&self) -> bool {
        matches!(self.inner, TreeKind::Fs(_))
    }

    pub(crate) fn fs_path(&self, path: &Path) -> Option<PathBuf> {
        match &self.inner {
            TreeKind::Fs(t) => Some(t.resolve_path(path)),
            TreeKind::Zip(_) | TreeKind::Memory(_) => None,
        }
    }
}

impl Default for TreeHandle {
    fn default() -> Self {
        Self::fs()
    }
}

impl TreeContainer for TreeHandle {
    fn exists(&self, path: &Path) -> bool {
        match &self.inner {
            TreeKind::Fs(t) => t.exists(path),
            TreeKind::Zip(t) => t.exists(path),
            TreeKind::Memory(t) => t.exists(path),
        }
    }

    fn is_directory(&self, path: &Path) -> Result<bool, TreeError> {
        match &self.inner {
            TreeKind::Fs(t) => t.is_directory(path),
            TreeKind::Zip(t) => t.is_directory(path),
            TreeKind::Memory(t) => t.is_directory(path),
        }
    }

    fn read_directory(&self, path: &Path) -> Result<Vec<String>, TreeError> {
        match &self.inner {
            TreeKind::Fs(t) => t.read_directory(path),
            TreeKind::Zip(t) => t.read_directory(path),
            TreeKind::Memory(t) => t.read_directory(path),
        }
    }

    fn read_directory_entries(&self, path: &Path) -> Result<Vec<DirEntryInfo>, TreeError> {
        match &self.inner {
            TreeKind::Fs(t) => t.read_directory_entries(path),
            TreeKind::Zip(t) => t.read_directory_entries(path),
            TreeKind::Memory(t) => t.read_directory_entries(path),
        }
    }

    fn read_file(&self, path: &Path) -> Result<Vec<u8>, TreeError> {
        match &self.inner {
            TreeKind::Fs(t) => t.read_file(path),
            TreeKind::Zip(t) => t.read_file(path),
            TreeKind::Memory(t) => t.read_file(path),
        }
    }

    fn find(&self, kind: TreeFileKind, name: &str, directory: &Path) -> Option<PathBuf> {
        match &self.inner {
            TreeKind::Fs(t) => t.find(kind, name, directory),
            TreeKind::Zip(t) => t.find(kind, name, directory),
            TreeKind::Memory(t) => t.find(kind, name, directory),
        }
    }
}

/// Lightweight metadata reference consisting of a type name and member full name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MetadataMember {
    /// Metadata type API name (e.g., `"CustomField"`).
    pub type_name: String,
    /// Full metadata member name (e.g., `"Account.Industry"`).
    pub full_name: String,
}

/// Construction properties for a [`SourceComponent`].
#[derive(Debug, Clone)]
pub struct ComponentProps {
    /// Local component name (without parent prefix).
    pub name: String,
    /// Registry metadata type definition.
    pub metadata_type: MetadataType,
    /// Path to the `-meta.xml` or MDAPI XML file, if any.
    pub xml: Option<PathBuf>,
    /// Path to the source content file or bundle/component directory, if any.
    pub content: Option<PathBuf>,
    /// Optional parent component (for decomposed or non-decomposed child components).
    pub parent: Option<Box<SourceComponent>>,
    /// Optional parent folder metadata type (for `folderType`-linked types).
    pub parent_type: Option<MetadataType>,
}

/// A resolved Salesforce metadata component backed by a [`TreeHandle`].
#[derive(Debug, Clone)]
pub struct SourceComponent {
    name: String,
    metadata_type: MetadataType,
    xml: Option<PathBuf>,
    content: Option<PathBuf>,
    parent: Option<Box<SourceComponent>>,
    parent_type: Option<MetadataType>,
    tree: TreeHandle,
    force_ignore: ForceIgnore,
    destructive_changes_kind: Option<DestructiveChangesKind>,
    pub(crate) replacements: BTreeMap<PathBuf, Vec<MarkedReplacement>>,
}

impl SourceComponent {
    /// Creates a new [`SourceComponent`].
    pub fn new(props: ComponentProps, tree: TreeHandle, force_ignore: ForceIgnore) -> Self {
        Self {
            name: props.name,
            metadata_type: props.metadata_type,
            xml: props.xml,
            content: props.content,
            parent: props.parent,
            parent_type: props.parent_type,
            tree,
            force_ignore,
            destructive_changes_kind: None,
            replacements: BTreeMap::new(),
        }
    }

    /// Returns the component's local name without parent prefix.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Computes the Metadata API `fullName` of this component.
    pub fn full_name(&self) -> String {
        if self.metadata_type.ignore_parsed_full_name == Some(true) {
            return self.metadata_type.name.clone();
        }
        if let Some(parent) = &self.parent {
            if self.metadata_type.ignore_parent_name == Some(true) {
                return self.name.clone();
            }
            return format!("{}.{}", parent.full_name(), self.name);
        }
        self.name.clone()
    }

    /// Returns the [`MetadataType`] definition for this component.
    pub fn metadata_type(&self) -> &MetadataType {
        &self.metadata_type
    }

    /// Returns the XML file path if this component has one.
    pub fn xml(&self) -> Option<&Path> {
        self.xml.as_deref()
    }

    /// Returns the content file or bundle directory path if present.
    pub fn content(&self) -> Option<&Path> {
        self.content.as_deref()
    }

    /// Sets the content path on this component.
    pub(crate) fn set_content(&mut self, content: PathBuf) {
        self.content = Some(content);
    }

    /// Returns the parent [`SourceComponent`] if this is a child component.
    pub fn parent(&self) -> Option<&SourceComponent> {
        self.parent.as_deref()
    }

    /// Returns the folder/container [`MetadataType`] if configured via `folderType`.
    pub fn parent_type(&self) -> Option<&MetadataType> {
        self.parent_type.as_ref()
    }

    /// Returns the backing [`TreeHandle`].
    pub fn tree(&self) -> &TreeHandle {
        &self.tree
    }

    /// Returns the associated [`ForceIgnore`].
    pub fn force_ignore(&self) -> &ForceIgnore {
        &self.force_ignore
    }

    /// Returns whether this component is addressable in a Metadata API manifest.
    pub fn addressable(&self) -> bool {
        self.metadata_type.addressable()
    }

    /// Returns whether this component is marked for deletion.
    pub fn is_marked_for_delete(&self) -> bool {
        self.destructive_changes_kind.is_some()
    }

    /// Returns the destructive change phase if marked for deletion.
    pub fn destructive_changes_kind(&self) -> Option<DestructiveChangesKind> {
        self.destructive_changes_kind
    }

    /// Marks or unmarks this component for destructive changes.
    pub(crate) fn set_destructive_changes_kind(&mut self, kind: Option<DestructiveChangesKind>) {
        self.destructive_changes_kind = kind;
    }

    /// Reads and parses this component's XML file into an [`XmlDocument`].
    ///
    /// If this component is a non-decomposed child sharing its parent's XML file, extracts
    /// the matching child element and wraps it as the document root.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError`] if no XML path is present or reading/parsing fails.
    pub fn parse_xml(&self) -> Result<XmlDocument, ResolveError> {
        let Some(xml_path) = &self.xml else {
            return Err(ResolveError::ExpectedSourceFiles {
                path: self
                    .content
                    .clone()
                    .unwrap_or_else(|| PathBuf::from(&self.name)),
                type_name: self.metadata_type.name.clone(),
            });
        };
        self.parse_xml_at(xml_path)
    }

    /// Reads and parses the XML file at `xml_path`, applying any marked replacements.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError`] if reading or parsing fails.
    pub fn parse_xml_at(&self, xml_path: &Path) -> Result<XmlDocument, ResolveError> {
        let mut raw = self.tree.read_to_string(xml_path)?;
        let active_replacements = self.replacements.get(xml_path).or_else(|| {
            self.parent
                .as_deref()
                .and_then(|p| p.replacements.get(xml_path))
        });
        if let Some(rules) = active_replacements {
            raw = apply_replacements(&raw, rules);
        }

        let doc = parse_xml(&raw)?;
        if doc.root.name == self.metadata_type.name {
            return Ok(doc);
        }
        if let Some(parent) = &self.parent
            && doc.root.name == parent.metadata_type.name
        {
            return self.extract_child_from_parent_doc(&doc, xml_path);
        }
        Ok(doc)
    }

    /// Extracts the XML element for `self` from an already-parsed parent [`XmlDocument`].
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError::MissingUniqueElement`] if no matching child element is found.
    pub fn extract_child_from_parent_doc(
        &self,
        parent_doc: &XmlDocument,
        xml_path: &Path,
    ) -> Result<XmlDocument, ResolveError> {
        let tag_name = self.metadata_type.effective_xml_element();
        let unique_id = self.metadata_type.unique_id_element.as_deref();
        let matched = parent_doc
            .root
            .child_elements()
            .filter(|el| el.name == tag_name)
            .find(|el| {
                unique_id
                    .and_then(|id| el.extract_unique_element(Some(id)))
                    .is_some_and(|val| val == self.name)
            })
            .cloned();

        let Some(mut el) = matched else {
            return Err(ResolveError::MissingUniqueElement {
                element: unique_id.unwrap_or("fullName").to_owned(),
                child_type: self.metadata_type.name.clone(),
                xml_path: xml_path.to_path_buf(),
            });
        };
        el.name.clone_from(&self.metadata_type.name);
        Ok(XmlDocument {
            declaration: parent_doc.declaration,
            root: el,
        })
    }

    /// Walks all non-XML content files under `self.content`, respecting `.forceignore`.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError::Tree`] if directory traversal fails.
    pub fn walk_content(&self) -> Result<Vec<PathBuf>, ResolveError> {
        let mut out = Vec::new();
        if let Some(content) = &self.content {
            self.walk_path(content, &mut out)?;
        }
        Ok(out)
    }

    fn walk_path(&self, current: &Path, out: &mut Vec<PathBuf>) -> Result<(), ResolveError> {
        if !self.tree.is_directory(current)? {
            if self.xml.as_deref() != Some(current) {
                out.push(current.to_path_buf());
            }
            return Ok(());
        }
        for entry in self.tree.read_directory_entries(current)? {
            let child = current.join(&entry.name);
            let hint = if entry.is_dir {
                PathKindHint::DirectoryOrUnwritten
            } else {
                PathKindHint::File
            };
            if self.force_ignore.denies_with_kind(&child, hint) {
                continue;
            }
            if entry.is_dir {
                self.walk_path(&child, out)?;
            } else if self.xml.as_deref() != Some(child.as_path()) {
                out.push(child);
            }
        }
        Ok(())
    }

    /// Resolves child [`SourceComponent`]s for composite/decomposed parent types.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError`] if child XML files cannot be read or contain invalid/unexpected
    /// child types.
    pub fn children(&self) -> Result<Vec<SourceComponent>, ResolveError> {
        if self.parent.is_some() {
            return Ok(Vec::new());
        }
        let Some(children_cfg) = &self.metadata_type.children else {
            return Ok(Vec::new());
        };

        if let Some(content_dir) = &self.content {
            return self.decomposed_children(content_dir, children_cfg);
        }
        self.non_decomposed_children(children_cfg)
    }

    fn decomposed_children(
        &self,
        content_dir: &Path,
        children_cfg: &crate::registry::ChildrenConfig,
    ) -> Result<Vec<SourceComponent>, ResolveError> {
        let mut files = Vec::new();
        self.walk_all_files(content_dir, &mut files)?;

        let mut children = Vec::new();
        for fs_path in files {
            let Some(child_xml) = parse_metadata_xml(&fs_path) else {
                continue;
            };
            let is_root = self.metadata_type.suffix.as_deref() == Some(child_xml.suffix.as_str())
                || self.metadata_type.legacy_suffix.as_deref() == Some(child_xml.suffix.as_str());
            if is_root {
                continue;
            }
            let Some(child_type_id) = children_cfg.suffixes.get(&child_xml.suffix) else {
                return Err(ResolveError::UnexpectedChildType {
                    path: fs_path,
                    parent_type: self.metadata_type.name.clone(),
                });
            };
            let Some(child_type) = children_cfg.types.get(child_type_id) else {
                return Err(ResolveError::UnexpectedChildType {
                    path: fs_path,
                    parent_type: self.metadata_type.name.clone(),
                });
            };
            let name = if child_type.suffix.is_some() {
                base_without_suffixes(&fs_path, child_type)
            } else {
                path_base_name(&fs_path).to_owned()
            };
            children.push(SourceComponent::new(
                ComponentProps {
                    name,
                    metadata_type: child_type.clone(),
                    xml: Some(fs_path),
                    content: None,
                    parent: Some(Box::new(self.clone())),
                    parent_type: None,
                },
                self.tree.clone(),
                self.force_ignore.clone(),
            ));
        }
        Ok(children)
    }

    fn non_decomposed_children(
        &self,
        children_cfg: &crate::registry::ChildrenConfig,
    ) -> Result<Vec<SourceComponent>, ResolveError> {
        let Some(xml_path) = &self.xml else {
            return Ok(Vec::new());
        };
        let doc = self.parse_xml_at(xml_path)?;
        let mut children = Vec::new();
        for child_type in children_cfg.types.values() {
            let (Some(unique_id), Some(xml_elem_name)) = (
                child_type.unique_id_element.as_deref(),
                child_type.xml_element_name.as_deref(),
            ) else {
                continue;
            };
            for el in doc
                .root
                .child_elements()
                .filter(|e| e.name == xml_elem_name)
            {
                let Some(name) = el.extract_unique_element(Some(unique_id)) else {
                    return Err(ResolveError::MissingUniqueElement {
                        element: unique_id.to_owned(),
                        child_type: child_type.name.clone(),
                        xml_path: xml_path.clone(),
                    });
                };
                children.push(SourceComponent::new(
                    ComponentProps {
                        name: name.to_owned(),
                        metadata_type: child_type.clone(),
                        xml: Some(xml_path.clone()),
                        content: None,
                        parent: Some(Box::new(self.clone())),
                        parent_type: None,
                    },
                    self.tree.clone(),
                    self.force_ignore.clone(),
                ));
            }
        }
        Ok(children)
    }

    fn walk_all_files(&self, current: &Path, out: &mut Vec<PathBuf>) -> Result<(), ResolveError> {
        if !self.tree.is_directory(current)? {
            out.push(current.to_path_buf());
            return Ok(());
        }
        for entry in self.tree.read_directory_entries(current)? {
            let child = current.join(&entry.name);
            let hint = if entry.is_dir {
                PathKindHint::DirectoryOrUnwritten
            } else {
                PathKindHint::File
            };
            if self.force_ignore.denies_with_kind(&child, hint) {
                continue;
            }
            if entry.is_dir {
                self.walk_all_files(&child, out)?;
            } else {
                out.push(child);
            }
        }
        Ok(())
    }

    /// Computes the package-relative destination path for `fs_path` in `format`.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError::Tree`] if a registry `directoryName` fails path-traversal checks.
    pub fn package_relative_path(
        &self,
        fs_path: &Path,
        format: TargetFormat,
    ) -> Result<PathBuf, ResolveError> {
        Ok(calculate_relative_path(
            format,
            &self.metadata_type,
            self.parent_type.as_ref(),
            &self.full_name(),
            fs_path,
        )?)
    }
}
