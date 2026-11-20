//! Metadata component resolution from filesystem, zip, or memory trees.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use crate::registry::{AdapterStrategy, MetadataType, RegistryAccess};
use crate::tree::{ForceIgnore, PathKindHint, TreeContainer};
use rayon::prelude::*;

use crate::io_pool::io_install;
use crate::path_util::{ext_name, parent_name, parse_metadata_xml};
use crate::resolve::adapters::{ResolveMode, allows_metadata_with_content, resolve_with_adapter};
use crate::resolve::component::{SourceComponent, TreeHandle};
use crate::resolve::component_set::ComponentSet;
use crate::resolve::error::ResolveError;

const META_XML_SUFFIX: &str = "-meta.xml";

/// Resolves files and directories into [`SourceComponent`] instances using a [`RegistryAccess`].
#[derive(Debug, Clone)]
pub struct MetadataResolver {
    registry: RegistryAccess,
    tree: TreeHandle,
    use_fs_force_ignore: ForceIgnorePolicy,
    force_ignored_paths: HashSet<PathBuf>,
    force_ignore_cache: HashMap<PathBuf, ForceIgnore>,
}

/// Controls whether [`MetadataResolver`] searches for a `.forceignore` file on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ForceIgnorePolicy {
    /// Search upward from the target path for `.forceignore`.
    FromProject,
    /// Use an empty permissive [`ForceIgnore`].
    Permissive,
}

impl MetadataResolver {
    /// Creates a new [`MetadataResolver`].
    pub fn new(
        registry: RegistryAccess,
        tree: TreeHandle,
        use_fs_force_ignore: ForceIgnorePolicy,
    ) -> Self {
        Self {
            registry,
            tree,
            use_fs_force_ignore,
            force_ignored_paths: HashSet::new(),
            force_ignore_cache: HashMap::new(),
        }
    }

    /// Returns paths skipped due to `.forceignore` rules during resolution.
    pub fn force_ignored_paths(&self) -> &HashSet<PathBuf> {
        &self.force_ignored_paths
    }

    /// Resolves metadata components from `fs_path` using the supplied `force_ignore` without
    /// mutating `self`, allowing parallel post-write resolution across paths.
    pub(crate) fn resolve_single_path_with_ignore(
        &self,
        fs_path: &Path,
        force_ignore: &ForceIgnore,
    ) -> Result<(Vec<SourceComponent>, HashSet<PathBuf>), ResolveError> {
        self.walk_from_path_with_ignore(fs_path, force_ignore, None)
    }

    /// Resolves metadata components from `fs_path`, optionally filtering by `inclusive_filter`.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError`] if `fs_path` does not exist, a file's metadata type cannot be
    /// inferred, or reading/parsing fails.
    pub fn components_from_path(
        &mut self,
        fs_path: &Path,
        inclusive_filter: Option<&ComponentSet>,
    ) -> Result<Vec<SourceComponent>, ResolveError> {
        if !self.tree.exists(fs_path) {
            return Err(ResolveError::PathNotFound {
                path: fs_path.to_path_buf(),
            });
        }

        let force_ignore = self.force_ignore_for_path(fs_path)?;
        let (components, ignored) =
            self.walk_from_path_with_ignore(fs_path, &force_ignore, inclusive_filter)?;
        self.force_ignored_paths.extend(ignored);
        Ok(components)
    }

    fn force_ignore_for_path(&mut self, fs_path: &Path) -> Result<ForceIgnore, ResolveError> {
        match self.use_fs_force_ignore {
            ForceIgnorePolicy::Permissive => Ok(ForceIgnore::empty()),
            ForceIgnorePolicy::FromProject => {
                let is_dir = self.tree.is_directory(fs_path).unwrap_or(false);
                let cache_dir = if is_dir {
                    fs_path
                } else {
                    fs_path.parent().unwrap_or(fs_path)
                };
                if let Some(cached) = self.force_ignore_cache.get(cache_dir) {
                    return Ok(cached.clone());
                }
                let loaded = ForceIgnore::find_and_create(fs_path)?;
                self.force_ignore_cache
                    .insert(cache_dir.to_path_buf(), loaded.clone());
                Ok(loaded)
            }
        }
    }

    fn walk_from_path_with_ignore(
        &self,
        fs_path: &Path,
        force_ignore: &ForceIgnore,
        inclusive_filter: Option<&ComponentSet>,
    ) -> Result<(Vec<SourceComponent>, HashSet<PathBuf>), ResolveError> {
        let is_dir = self.tree.is_directory(fs_path)?;
        if is_dir && !self.resolve_directory_as_component(fs_path)? {
            return self.components_from_path_recursive(fs_path, force_ignore, inclusive_filter);
        }

        let mut ignored = HashSet::new();
        let kind_hint = if is_dir {
            PathKindHint::DirectoryOrUnwritten
        } else {
            PathKindHint::File
        };
        let Some(component) = self.resolve_component(
            fs_path,
            ResolveMode::Source,
            force_ignore,
            kind_hint,
            &mut ignored,
        )?
        else {
            return Ok((Vec::new(), ignored));
        };
        if let Some(filter) = inclusive_filter
            && !filter.has_component(&component)
        {
            return Ok((Vec::new(), ignored));
        }
        Ok((vec![component], ignored))
    }

    fn components_from_path_recursive(
        &self,
        dir: &Path,
        force_ignore: &ForceIgnore,
        inclusive_filter: Option<&ComponentSet>,
    ) -> Result<(Vec<SourceComponent>, HashSet<PathBuf>), ResolveError> {
        if !self.tree.is_directory(dir)? && force_ignore.denies_with_kind(dir, PathKindHint::File) {
            return Ok((Vec::new(), HashSet::new()));
        }

        let raw_entries = self.tree.read_directory_entries(dir)?;
        let mut entries: Vec<(PathBuf, bool)> = Vec::with_capacity(raw_entries.len());
        for entry in raw_entries {
            entries.push((dir.join(entry.name), entry.is_dir));
        }
        // Directories first, preserving deterministic lexicographical order within each group.
        entries.sort_by(|(pa, da), (pb, db)| db.cmp(da).then_with(|| pa.cmp(pb)));

        let mut dir_queue: Vec<PathBuf> = Vec::new();
        let mut components: Vec<SourceComponent> = Vec::new();
        let mut ignored = HashSet::new();
        let mut ignore: HashSet<PathBuf> = HashSet::new();

        for (fs_path, is_dir) in entries {
            if ignore.contains(&fs_path) {
                continue;
            }
            if is_dir {
                if self.resolve_directory_as_component(&fs_path)? {
                    if self.tree.read_directory(&fs_path)?.is_empty() {
                        continue;
                    }
                    if let Some(component) = self.resolve_component(
                        &fs_path,
                        ResolveMode::Source,
                        force_ignore,
                        PathKindHint::DirectoryOrUnwritten,
                        &mut ignored,
                    )? {
                        let included =
                            inclusive_filter.is_none_or(|filter| filter.has_component(&component));
                        if included {
                            if let Some(xml) = component.xml() {
                                ignore.insert(xml.to_path_buf());
                            }
                            components.push(component);
                        }
                    }
                } else {
                    let skip_node_modules = force_ignore
                        .denies_with_kind(&fs_path, PathKindHint::DirectoryOrUnwritten)
                        && fs_path
                            .components()
                            .any(|c| matches!(c, Component::Normal(s) if s == "node_modules"));
                    if !skip_node_modules {
                        dir_queue.push(fs_path);
                    }
                }
            } else if self.is_metadata(&fs_path)
                && let Some(component) = self.resolve_component(
                    &fs_path,
                    ResolveMode::NonSource,
                    force_ignore,
                    PathKindHint::File,
                    &mut ignored,
                )?
            {
                let included =
                    inclusive_filter.is_none_or(|filter| filter.has_component(&component));
                let strict_early_exit = {
                    let ty = component.metadata_type();
                    let type_dir = if ty.stored_in_folder() {
                        fs_path.parent().and_then(Path::parent)
                    } else {
                        fs_path.parent()
                    };
                    let type_dir_name = type_dir
                        .and_then(Path::file_name)
                        .and_then(|s| s.to_str())
                        .unwrap_or_default();
                    ty.strict_directory() && type_dir_name != ty.directory_name
                };

                if included {
                    if let Some(content) = component.content() {
                        ignore.insert(content.to_path_buf());
                    }
                    components.push(component);
                } else if let Some(filter) = inclusive_filter {
                    for child in component.children()? {
                        if filter.has_component(&child) {
                            components.push(child);
                        }
                    }
                }

                if strict_early_exit {
                    return Ok((components, ignored));
                }
            }
        }

        self.expand_queued_dirs(
            &dir_queue,
            force_ignore,
            inclusive_filter,
            &mut components,
            &mut ignored,
        )?;
        Ok((components, ignored))
    }

    fn expand_queued_dirs(
        &self,
        dir_queue: &[PathBuf],
        force_ignore: &ForceIgnore,
        inclusive_filter: Option<&ComponentSet>,
        components: &mut Vec<SourceComponent>,
        ignored: &mut HashSet<PathBuf>,
    ) -> Result<(), ResolveError> {
        if dir_queue.len() >= 4 {
            let nested_results: Vec<(Vec<SourceComponent>, HashSet<PathBuf>)> = io_install(|| {
                dir_queue
                    .par_iter()
                    .map(|queued_dir| {
                        self.components_from_path_recursive(
                            queued_dir,
                            force_ignore,
                            inclusive_filter,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()
            })?;
            for (mut nested_comps, nested_ignored) in nested_results {
                components.append(&mut nested_comps);
                ignored.extend(nested_ignored);
            }
        } else {
            for queued_dir in dir_queue {
                let (mut nested_comps, nested_ignored) = self.components_from_path_recursive(
                    queued_dir,
                    force_ignore,
                    inclusive_filter,
                )?;
                components.append(&mut nested_comps);
                ignored.extend(nested_ignored);
            }
        }
        Ok(())
    }

    fn resolve_component(
        &self,
        fs_path: &Path,
        mode: ResolveMode,
        force_ignore: &ForceIgnore,
        kind_hint: PathKindHint,
        ignored_paths: &mut HashSet<PathBuf>,
    ) -> Result<Option<SourceComponent>, ResolveError> {
        if force_ignore.denies_with_kind(fs_path, kind_hint) {
            ignored_paths.insert(fs_path.to_path_buf());
            return Ok(None);
        }

        if let Some(ty) = self.resolve_type(fs_path) {
            let strategy = ty
                .strategies
                .as_ref()
                .map_or(AdapterStrategy::Default, |s| s.adapter);
            let should_resolve = mode == ResolveMode::Source
                || parse_metadata_xml(fs_path).is_some()
                || !self.parse_as_content_metadata_xml(fs_path)
                || !allows_metadata_with_content(strategy);
            if should_resolve {
                return resolve_with_adapter(
                    &ty,
                    fs_path,
                    mode,
                    &self.registry,
                    &self.tree,
                    force_ignore,
                );
            }
            return Ok(None);
        }

        if self.is_probably_package_manifest(fs_path) {
            return Ok(None);
        }

        Err(ResolveError::TypeInference {
            path: fs_path.to_path_buf(),
        })
    }

    fn resolve_type(&self, fs_path: &Path) -> Option<MetadataType> {
        // Attempt 1: strict folder match
        if let Some(ty) = self.resolve_type_from_strict_folder(fs_path) {
            return Some(ty);
        }

        // Attempt 2: `-meta.xml` suffix match
        if let Some(parsed) = parse_metadata_xml(fs_path)
            && let Some(ty) = self.registry.type_by_suffix(&parsed.suffix)
        {
            return Some(ty.clone());
        }

        // Attempt 2.5: folder-level `-meta.xml` (e.g. `reports/MyFolder-meta.xml`)
        if let Some(folder_dir) = self.parse_as_folder_metadata_xml(fs_path)
            && let Some(ty) = self
                .registry
                .find_type(|t| t.directory_name == folder_dir && !t.stored_in_folder())
        {
            return Some(ty.clone());
        }

        // Attempt 3: plain extension suffix match
        if let Some(ty) = self.registry.type_by_suffix(ext_name(fs_path))
            && (!ty.strict_directory() || path_has_segment(fs_path, &ty.directory_name))
        {
            return Some(ty.clone());
        }

        // Attempt 4: path-based bundle content types
        if let Some(ty) = self.parse_as_metadata(fs_path) {
            return Some(ty);
        }

        None
    }

    fn resolve_type_from_strict_folder(&self, fs_path: &Path) -> Option<MetadataType> {
        let path_str = fs_path.to_string_lossy();
        let parent = parent_name(fs_path);
        let mut best: Option<(usize, &MetadataType)> = None;
        for comp in fs_path.components() {
            let Component::Normal(os_seg) = comp else {
                continue;
            };
            let Some(seg) = os_seg.to_str() else {
                continue;
            };
            for (idx, ty) in self.registry.strict_folder_types_for_dir(seg) {
                if best.is_some_and(|(best_idx, _)| idx >= best_idx) {
                    continue;
                }
                if ty.stored_in_folder() && parent == ty.directory_name {
                    continue;
                }
                if is_mixed_content_or_bundle(ty)
                    || suffix_matches(ty, &path_str)
                    || child_suffix_matches(ty, &path_str)
                    || legacy_suffix_matches(ty, &path_str)
                {
                    best = Some((idx, ty));
                }
            }
        }
        best.map(|(_, ty)| ty.clone())
    }

    fn resolve_directory_as_component(&self, dir_path: &Path) -> Result<bool, ResolveError> {
        let Some(ty) = self.resolve_type(dir_path) else {
            return Ok(false);
        };
        let segments: Vec<&str> = dir_path
            .components()
            .filter_map(|c| match c {
                Component::Normal(s) => s.to_str(),
                _ => None,
            })
            .collect();
        let Some(type_idx) = segments.iter().rposition(|s| *s == ty.directory_name) else {
            return Ok(false);
        };
        let folder_offset = if ty.stored_in_folder() { 2 } else { 1 };
        if segments.len().saturating_sub(folder_offset) <= type_idx {
            return Ok(false);
        }
        if self
            .tree
            .read_directory(dir_path)?
            .iter()
            .any(|entry| entry == &ty.directory_name)
        {
            return Ok(false);
        }
        if ty.children.is_some() {
            return Ok(false);
        }
        Ok(true)
    }

    fn is_metadata(&self, fs_path: &Path) -> bool {
        parse_metadata_xml(fs_path).is_some()
            || self.parse_as_content_metadata_xml(fs_path)
            || self.parse_as_folder_metadata_xml(fs_path).is_some()
            || self.parse_as_metadata(fs_path).is_some()
    }

    fn parse_as_content_metadata_xml(&self, fs_path: &Path) -> bool {
        if let Some(suffix_type) = self.registry.type_by_suffix(ext_name(fs_path))
            && path_has_segment(fs_path, &suffix_type.directory_name)
        {
            return true;
        }
        self.resolve_type_from_strict_folder(fs_path).is_some()
    }

    fn parse_as_folder_metadata_xml(&self, fs_path: &Path) -> Option<String> {
        let file = fs_path.file_name()?.to_str()?;
        let stem = file.strip_suffix(META_XML_SUFFIX)?;
        if stem.is_empty() || stem.contains('.') {
            return None;
        }
        let parent = fs_path.parent()?;
        for folder_type in self.registry.folder_content_types() {
            if path_has_segment(parent, &folder_type.directory_name) {
                return Some(folder_type.directory_name.clone());
            }
        }
        None
    }

    fn parse_as_metadata(&self, fs_path: &Path) -> Option<MetadataType> {
        if self.tree.is_directory(fs_path).unwrap_or(false) {
            return None;
        }
        for name in [
            "DigitalExperience",
            "ExperiencePropertyTypeBundle",
            "LightningTypeBundle",
            "ContentTypeBundle",
            "UiWidgetBundle",
        ] {
            if let Ok(ty) = self.registry.type_by_name(name)
                && path_has_segment(fs_path, &ty.directory_name)
            {
                return Some(ty.clone());
            }
        }
        None
    }

    fn is_probably_package_manifest(&self, fs_path: &Path) -> bool {
        let name = fs_path.to_string_lossy();
        if !name.ends_with(".xml") || name.ends_with(META_XML_SUFFIX) {
            return false;
        }
        if fs_path.file_name().and_then(|s| s.to_str()) == Some("package.xml") {
            return true;
        }
        self.tree
            .read_to_string(fs_path)
            .is_ok_and(|s| s.contains("<Package xmlns"))
    }
}

fn path_has_segment(path: &Path, segment: &str) -> bool {
    path.components()
        .any(|c| matches!(c, Component::Normal(s) if s == segment))
}

fn is_mixed_content_or_bundle(ty: &MetadataType) -> bool {
    ty.strategies.as_ref().is_some_and(|s| {
        matches!(
            s.adapter,
            AdapterStrategy::MixedContent | AdapterStrategy::Bundle | AdapterStrategy::UiBundles
        )
    })
}

fn child_suffix_matches(ty: &MetadataType, path_str: &str) -> bool {
    ty.children.as_ref().is_some_and(|children| {
        children
            .types
            .values()
            .any(|ct| suffix_matches(ct, path_str) || legacy_suffix_matches(ct, path_str))
    })
}

fn suffix_matches(ty: &MetadataType, path_str: &str) -> bool {
    ty.suffix.as_deref().is_some_and(|suffix| {
        path_str.ends_with(suffix) || path_str.ends_with(&format!("{suffix}{META_XML_SUFFIX}"))
    })
}

fn legacy_suffix_matches(ty: &MetadataType, path_str: &str) -> bool {
    ty.legacy_suffix.as_deref().is_some_and(|legacy| {
        path_str.ends_with(legacy) || path_str.ends_with(&format!("{legacy}{META_XML_SUFFIX}"))
    })
}
