//! `ComponentSet` and `ComponentSetBuilder` matching SDR's `src/collections/`.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use crate::registry::{
    MetadataType, RecompositionStrategy, RegistryAccess, SfdxProjectConfig, TransformerStrategy,
};
use crate::tree::TreeContainer;
use globset::Glob;
use indexmap::IndexMap;
use rayon::prelude::*;

use crate::manifest::{
    DestructiveChangesKind, ManifestPurpose, ManifestTypeMembers, PackageManifest, parse_manifest,
    render_manifest_xml,
};
use crate::resolve::component::{ComponentProps, MetadataMember, SourceComponent, TreeHandle};
use crate::resolve::error::ResolveError;
use crate::resolve::resolver::{ForceIgnorePolicy, MetadataResolver};

const KEY_DELIMITER: char = '#';
const WILDCARD: &str = "*";
const FALLBACK_API_VERSION: &str = "58.0";

/// Options for resolving components from a manifest file.
#[derive(Debug, Clone, Default)]
pub struct ManifestOptions {
    /// Path to `package.xml`.
    pub manifest_path: PathBuf,
    /// Local source directories to search for matching components.
    pub directory_paths: Vec<PathBuf>,
    /// Optional `destructiveChangesPre.xml` path.
    pub destructive_changes_pre: Option<PathBuf>,
    /// Optional `destructiveChangesPost.xml` path.
    pub destructive_changes_post: Option<PathBuf>,
}

/// Options for resolving components from `Type:Name` metadata expressions.
#[derive(Debug, Clone, Default)]
pub struct MetadataOptions {
    /// Metadata entries (`Type:FullName` or `Type`).
    pub metadata_entries: Vec<String>,
    /// Local directories to search.
    pub directory_paths: Vec<PathBuf>,
    /// Metadata entries to exclude.
    pub excluded_entries: Vec<String>,
    /// Destructive entries executed before deploy (`Type:FullName`, no wildcards).
    pub destructive_entries_pre: Vec<String>,
    /// Destructive entries executed after deploy (`Type:FullName`, no wildcards).
    pub destructive_entries_post: Vec<String>,
}

/// Unified options for [`ComponentSetBuilder::build`].
#[derive(Debug, Clone, Default)]
pub struct ComponentSetOptions {
    /// Filesystem source or MDAPI paths to scan.
    pub sourcepath: Vec<PathBuf>,
    /// Manifest-driven resolution options.
    pub manifest: Option<ManifestOptions>,
    /// Metadata-entry-driven resolution options.
    pub metadata: Option<MetadataOptions>,
    /// Override Metadata API version.
    pub api_version: Option<String>,
    /// Override source API version.
    pub source_api_version: Option<String>,
    /// Project directory containing `sfdx-project.json`.
    pub project_dir: Option<PathBuf>,
}

/// Collection of metadata components (source-backed and/or manifest-only) with deterministic
/// ordering, destructive change tracking, and `package.xml` generation.
#[derive(Debug, Clone)]
pub struct ComponentSet {
    registry: RegistryAccess,
    components: IndexMap<String, IndexMap<String, Option<SourceComponent>>>,
    manifest_components: IndexMap<String, IndexMap<String, SourceComponent>>,
    destructive_pre: IndexMap<String, IndexMap<String, SourceComponent>>,
    destructive_post: IndexMap<String, IndexMap<String, SourceComponent>>,
    api_version: Option<String>,
    source_api_version: Option<String>,
    project_directory: Option<PathBuf>,
    full_name: Option<String>,
    manifest_purpose: ManifestPurpose,
    force_ignored_paths: HashSet<PathBuf>,
}

impl ComponentSet {
    /// Creates an empty [`ComponentSet`] using `registry`.
    pub fn new(registry: RegistryAccess) -> Self {
        Self {
            registry,
            components: IndexMap::new(),
            manifest_components: IndexMap::new(),
            destructive_pre: IndexMap::new(),
            destructive_post: IndexMap::new(),
            api_version: None,
            source_api_version: None,
            project_directory: None,
            full_name: None,
            manifest_purpose: ManifestPurpose::Standard,
            force_ignored_paths: HashSet::new(),
        }
    }

    /// Resolves metadata components from filesystem paths using default `.forceignore` discovery.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError`] if any path is missing or cannot be inferred.
    pub fn from_source(
        fs_paths: &[PathBuf],
        registry: RegistryAccess,
        inclusive_filter: Option<&Self>,
    ) -> Result<Self, ResolveError> {
        Self::from_source_with_tree(fs_paths, &TreeHandle::fs(), registry, inclusive_filter)
    }

    /// Resolves metadata components from paths in a custom [`TreeHandle`] (e.g., [`crate::tree::ZipTree`]).
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError`] if any path is missing or cannot be inferred.
    pub fn from_source_with_tree(
        fs_paths: &[PathBuf],
        tree: &TreeHandle,
        registry: RegistryAccess,
        inclusive_filter: Option<&Self>,
    ) -> Result<Self, ResolveError> {
        let mut resolver = MetadataResolver::new(
            registry.clone(),
            tree.clone(),
            ForceIgnorePolicy::FromProject,
        );
        let mut set = Self::new(registry);
        for path in fs_paths {
            for component in resolver.components_from_path(path, inclusive_filter)? {
                set.add_source_component(component, None);
            }
        }
        set.force_ignored_paths
            .clone_from(resolver.force_ignored_paths());
        Ok(set)
    }

    /// Resolves a [`ComponentSet`] from a `package.xml` manifest and optional local source directories.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError`] if the manifest is invalid or source directories fail to resolve.
    pub fn from_manifest(
        manifest_path: &Path,
        resolve_source_paths: Option<&[PathBuf]>,
        destructive_pre: Option<&Path>,
        destructive_post: Option<&Path>,
        registry: RegistryAccess,
        tree: &TreeHandle,
    ) -> Result<Self, ResolveError> {
        let manifest_xml = tree.read_to_string(manifest_path)?;
        let parsed = parse_manifest(&manifest_xml, manifest_path)?;

        let mut result = Self::new(registry.clone());
        if !parsed.version.is_empty() {
            result.source_api_version = Some(parsed.version.clone());
        }
        result.full_name.clone_from(&parsed.full_name);

        let mut include_set = resolve_source_paths.map(|_| Self::new(registry.clone()));

        if let Some(pre_path) = destructive_pre {
            let xml = tree.read_to_string(pre_path)?;
            let pre = parse_manifest(&xml, pre_path)?;
            for (ty, full_name) in expand_manifest_members(&pre, &registry)? {
                let comp = SourceComponent::new(
                    ComponentProps {
                        name: full_name,
                        metadata_type: ty,
                        xml: None,
                        content: None,
                        parent: None,
                        parent_type: None,
                    },
                    tree.clone(),
                    crate::tree::ForceIgnore::empty(),
                );
                result.add_source_component(comp, Some(DestructiveChangesKind::Pre));
            }
        }
        if let Some(post_path) = destructive_post {
            let xml = tree.read_to_string(post_path)?;
            let post = parse_manifest(&xml, post_path)?;
            for (ty, full_name) in expand_manifest_members(&post, &registry)? {
                let comp = SourceComponent::new(
                    ComponentProps {
                        name: full_name,
                        metadata_type: ty,
                        xml: None,
                        content: None,
                        parent: None,
                        parent_type: None,
                    },
                    tree.clone(),
                    crate::tree::ForceIgnore::empty(),
                );
                result.add_source_component(comp, Some(DestructiveChangesKind::Post));
            }
        }

        for (ty, full_name) in expand_manifest_members(&parsed, &registry)? {
            let member = MetadataMember {
                type_name: ty.name,
                full_name,
            };
            if let Some(ref mut inc) = include_set {
                inc.add_member(&member)?;
            }
            result.add_member(&member)?;
        }

        if let Some(paths) = resolve_source_paths {
            let resolved =
                Self::from_source_with_tree(paths, tree, registry, include_set.as_ref())?;
            result
                .force_ignored_paths
                .clone_from(&resolved.force_ignored_paths);
            for comp in resolved.source_components() {
                result.add_source_component(comp, None);
            }
        }

        Ok(result)
    }

    /// Returns the associated [`RegistryAccess`].
    pub fn registry(&self) -> &RegistryAccess {
        &self.registry
    }

    /// Returns the number of entries in the set.
    pub fn size(&self) -> usize {
        self.components.values().map(|m| m.len().max(1)).sum()
    }

    /// Sets the [`ManifestPurpose`] (`Deploy` vs `Retrieve`) controlling empty parent inclusion.
    pub fn set_manifest_purpose(&mut self, purpose: ManifestPurpose) {
        self.manifest_purpose = purpose;
    }

    pub(crate) fn manifest_purpose(&self) -> ManifestPurpose {
        self.manifest_purpose
    }

    pub(crate) fn full_name(&self) -> Option<&str> {
        self.full_name.as_deref()
    }

    pub(crate) fn destructive_member_keys(&self) -> Vec<(DestructiveChangesKind, String)> {
        let mut keys = Vec::with_capacity(self.destructive_pre.len() + self.destructive_post.len());
        for (k, bucket) in &self.destructive_pre {
            for sk in bucket.keys() {
                keys.push((DestructiveChangesKind::Pre, format!("{k}:{sk}")));
            }
        }
        for (k, bucket) in &self.destructive_post {
            for sk in bucket.keys() {
                keys.push((DestructiveChangesKind::Post, format!("{k}:{sk}")));
            }
        }
        keys.sort_by(|a, b| a.1.cmp(&b.1));
        keys
    }

    /// Sets the `apiVersion` override.
    pub fn set_api_version(&mut self, version: Option<String>) {
        self.api_version = version;
    }

    /// Sets the `sourceApiVersion` override.
    pub fn set_source_api_version(&mut self, version: Option<String>) {
        self.source_api_version = version;
    }

    /// Sets the project root directory.
    pub fn set_project_directory(&mut self, dir: Option<PathBuf>) {
        self.project_directory = dir;
    }

    /// Returns the configured project root directory, if any.
    pub fn project_directory(&self) -> Option<&Path> {
        self.project_directory.as_deref()
    }

    /// Sets the package `fullName` used in `package.xml`.
    pub fn set_full_name(&mut self, full_name: Option<String>) {
        self.full_name = full_name;
    }

    /// Returns paths skipped by `.forceignore`.
    pub fn force_ignored_paths(&self) -> &HashSet<PathBuf> {
        &self.force_ignored_paths
    }

    /// Adds a non-source [`MetadataMember`] to the set.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError::Registry`] if the member's type is unknown.
    pub fn add_member(&mut self, member: &MetadataMember) -> Result<(), ResolveError> {
        let ty = self.registry.type_by_name(&member.type_name)?.clone();
        let key = simple_key(&ty.id, &member.full_name);
        self.components.entry(key.clone()).or_default();

        let stub = SourceComponent::new(
            ComponentProps {
                name: member.full_name.clone(),
                metadata_type: ty,
                xml: None,
                content: None,
                parent: None,
                parent_type: None,
            },
            TreeHandle::fs(),
            crate::tree::ForceIgnore::empty(),
        );
        let sk = source_key(&stub);
        self.manifest_components
            .entry(key)
            .or_default()
            .insert(sk, stub);
        Ok(())
    }

    /// Adds a [`SourceComponent`] to the set, optionally marking it for destructive changes.
    pub fn add_source_component(
        &mut self,
        mut component: SourceComponent,
        deletion_kind: Option<DestructiveChangesKind>,
    ) {
        let key = simple_key(&component.metadata_type().id, &component.full_name());
        let src_key = source_key(&component);

        if let Some(kind) = deletion_kind {
            component.set_destructive_changes_kind(Some(kind));
            self.components
                .entry(key.clone())
                .or_default()
                .insert(src_key.clone(), Some(component.clone()));
            let target = match kind {
                DestructiveChangesKind::Pre => &mut self.destructive_pre,
                DestructiveChangesKind::Post => &mut self.destructive_post,
            };
            target.entry(key).or_default().insert(src_key, component);
        } else {
            self.components
                .entry(key.clone())
                .or_default()
                .insert(src_key.clone(), Some(component.clone()));
            self.manifest_components
                .entry(key)
                .or_default()
                .insert(src_key, component);
        }
    }

    /// Returns all source-backed [`SourceComponent`] instances in insertion order.
    pub fn source_components(&self) -> Vec<SourceComponent> {
        self.components
            .values()
            .flat_map(|bucket| bucket.values().flatten().cloned())
            .collect()
    }

    /// Returns source-backed components matching the given type ID/name and `fullName`.
    pub fn source_components_for(&self, type_id: &str, full_name: &str) -> Vec<SourceComponent> {
        let key = simple_key(type_id, full_name);
        self.components
            .get(&key)
            .map(|bucket| bucket.values().flatten().cloned().collect())
            .unwrap_or_default()
    }

    /// Returns filenames associated with `member` (falling back to parent children if needed).
    pub fn filenames_by_name_and_type(&self, member: &MetadataMember) -> Vec<PathBuf> {
        let Ok(ty) = self.registry.type_by_name(&member.type_name) else {
            return Vec::new();
        };
        let key = simple_key(&ty.id, &member.full_name);
        if let Some(bucket) = self.components.get(&key) {
            let files = collect_filenames(bucket.values().flatten());
            if !files.is_empty() {
                return files;
            }
        }
        if let Some(parent_type) = self.registry.parent_type(&ty.id)
            && let Some((parent_full_name, _)) = member.full_name.rsplit_once('.')
        {
            let parent_key = simple_key(&parent_type.id, parent_full_name);
            if let Some(bucket) = self.components.get(&parent_key) {
                let matching: Vec<SourceComponent> = bucket
                    .values()
                    .flatten()
                    .flat_map(|p| p.children().unwrap_or_default())
                    .filter(|c| c.full_name() == member.full_name)
                    .collect();
                return collect_filenames(matching.iter());
            }
        }
        Vec::new()
    }

    /// Tests whether `component` (or a wildcard / parent entry covering it) is in the set.
    pub fn has_component(&self, component: &SourceComponent) -> bool {
        let full_name = component.full_name();
        let ty = component.metadata_type();
        let key = simple_key(&ty.id, &full_name);
        if self.components.contains_key(&key) {
            return true;
        }
        if self.components.contains_key(&simple_key(&ty.id, WILDCARD)) {
            return true;
        }
        if let Some(parent) = component.parent() {
            let parent_full_name = parent.full_name();
            let parent_id = &parent.metadata_type().id;
            if self
                .components
                .contains_key(&simple_key(parent_id, &parent_full_name))
                || self
                    .components
                    .contains_key(&simple_key(parent_id, WILDCARD))
                || self.components.contains_key(&simple_key(
                    &ty.id,
                    &format!("{parent_full_name}.{WILDCARD}"),
                ))
            {
                return true;
            }
        }
        false
    }

    /// Returns which destructive change phases exist in this set.
    pub fn destructive_changes_kinds(&self) -> Vec<DestructiveChangesKind> {
        let mut kinds = Vec::new();
        if !self.destructive_pre.is_empty() {
            kinds.push(DestructiveChangesKind::Pre);
        }
        if !self.destructive_post.is_empty() {
            kinds.push(DestructiveChangesKind::Post);
        }
        kinds
    }

    /// Resolves the API version for manifest generation matching SDR's fallback chain.
    pub fn resolved_api_version(&self) -> String {
        if let Some(v) = &self.source_api_version {
            return v.clone();
        }
        if let Some(v) = &self.api_version {
            return v.clone();
        }
        if let Some(dir) = &self.project_directory
            && let Ok(Some((_, cfg))) = SfdxProjectConfig::find_and_load(dir)
            && let Some(v) = cfg.source_api_version
        {
            return v;
        }
        FALLBACK_API_VERSION.to_owned()
    }

    /// Builds a [`PackageManifest`] for either the standard package or a destructive phase.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError`] if child lookup or XML inspection fails.
    pub fn to_manifest(
        &self,
        destructive: Option<DestructiveChangesKind>,
    ) -> Result<PackageManifest, ResolveError> {
        let has_destructive = !self.destructive_pre.is_empty() || !self.destructive_post.is_empty();
        let mut type_map: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

        match destructive {
            Some(DestructiveChangesKind::Pre) => {
                self.populate_type_map_from_source(
                    &self.destructive_pre,
                    Some(DestructiveChangesKind::Pre),
                    &mut type_map,
                )?;
            }
            Some(DestructiveChangesKind::Post) => {
                self.populate_type_map_from_source(
                    &self.destructive_post,
                    Some(DestructiveChangesKind::Post),
                    &mut type_map,
                )?;
            }
            None if has_destructive => {
                self.populate_type_map_from_source(&self.manifest_components, None, &mut type_map)?;
            }
            None => {
                let entries: Vec<(&String, &IndexMap<String, Option<SourceComponent>>)> =
                    self.components.iter().collect();
                let resolved = if entries.len() >= 8 {
                    entries
                        .into_par_iter()
                        .map(|(key, bucket)| {
                            let comps: Vec<&SourceComponent> = bucket.values().flatten().collect();
                            self.collect_entry_manifest_members(key, &comps)
                        })
                        .collect::<Result<Vec<_>, _>>()?
                } else {
                    entries
                        .into_iter()
                        .map(|(key, bucket)| {
                            let comps: Vec<&SourceComponent> = bucket.values().flatten().collect();
                            self.collect_entry_manifest_members(key, &comps)
                        })
                        .collect::<Result<Vec<_>, _>>()?
                };
                for members in resolved {
                    for (ty, member_name) in members {
                        add_to_type_map(&mut type_map, &ty, &member_name, None);
                    }
                }
            }
        }

        let types = type_map
            .into_iter()
            .map(|(name, members)| ManifestTypeMembers {
                name,
                members: members.into_iter().collect(),
            })
            .collect();

        Ok(PackageManifest {
            types,
            version: self.resolved_api_version(),
            full_name: self.full_name.clone(),
        })
    }

    /// Renders `package.xml` or `destructiveChanges*.xml` as a formatted XML string.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError`] if manifest construction fails.
    pub fn render_package_xml(
        &self,
        destructive: Option<DestructiveChangesKind>,
    ) -> Result<String, ResolveError> {
        let manifest = self.to_manifest(destructive)?;
        Ok(render_manifest_xml(&manifest))
    }

    fn populate_type_map_from_source(
        &self,
        source: &IndexMap<String, IndexMap<String, SourceComponent>>,
        destructive: Option<DestructiveChangesKind>,
        type_map: &mut BTreeMap<String, BTreeSet<String>>,
    ) -> Result<(), ResolveError> {
        let entries: Vec<(&String, &IndexMap<String, SourceComponent>)> = source.iter().collect();
        let resolved = if entries.len() >= 8 {
            entries
                .into_par_iter()
                .map(|(key, bucket)| {
                    let comps: Vec<&SourceComponent> = bucket.values().collect();
                    self.collect_entry_manifest_members(key, &comps)
                })
                .collect::<Result<Vec<_>, _>>()?
        } else {
            entries
                .into_iter()
                .map(|(key, bucket)| {
                    let comps: Vec<&SourceComponent> = bucket.values().collect();
                    self.collect_entry_manifest_members(key, &comps)
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        for members in resolved {
            for (ty, member_name) in members {
                add_to_type_map(type_map, &ty, &member_name, destructive);
            }
        }
        Ok(())
    }

    fn collect_entry_manifest_members(
        &self,
        key: &str,
        comps: &[&SourceComponent],
    ) -> Result<Vec<(MetadataType, String)>, ResolveError> {
        let (type_id, full_name) = split_on_first_delimiter(key);
        let ty = self.registry.type_by_name(type_id)?;
        let mut out = Vec::new();
        for comp in comps {
            for child in comp.children()? {
                out.push((child.metadata_type().clone(), child.full_name()));
            }
        }

        // Skip empty decomposed parents on non-retrieve manifests when children are independently addressable.
        let is_decomposed_parent = ty.strategies.as_ref().and_then(|s| s.transformer)
            == Some(TransformerStrategy::Decomposed);
        let has_independently_addressable_child = ty.children.as_ref().is_some_and(|c| {
            c.types
                .values()
                .any(|ct| ct.unaddressable_without_parent != Some(true))
                && c.types.values().any(MetadataType::addressable)
        });
        if self.manifest_purpose != ManifestPurpose::Retrieve
            && is_decomposed_parent
            && has_independently_addressable_child
            && let Some(parent_comp) = comps.iter().find(|c| c.full_name() == full_name)
            && parent_comp.xml().is_some()
            && let Ok(doc) = parent_comp.parse_xml()
            && !doc.root.has_real_values()
        {
            return Ok(out);
        }

        let manifest_type = match &ty.folder_content_type {
            Some(content_id) => self.registry.type_by_name(content_id)?,
            None => ty,
        };
        let member_name = construct_full_name(&self.registry, ty, full_name);
        out.push((manifest_type.clone(), member_name));
        Ok(out)
    }
}

/// Fluent entry point matching SDR's `ComponentSetBuilder.build(...)`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ComponentSetBuilder;

impl ComponentSetBuilder {
    /// Builds a [`ComponentSet`] using the local filesystem [`TreeHandle`].
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError`] if options are empty or any path/manifest/entry fails resolution.
    pub fn build(options: &ComponentSetOptions) -> Result<ComponentSet, ResolveError> {
        Self::build_with_tree(options, &TreeHandle::cached_fs())
    }

    /// Builds a [`ComponentSet`] using an explicit [`TreeHandle`].
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError`] if options are empty or any path/manifest/entry fails resolution.
    pub fn build_with_tree(
        options: &ComponentSetOptions,
        tree: &TreeHandle,
    ) -> Result<ComponentSet, ResolveError> {
        let registry = RegistryAccess::from_project_dir(
            options
                .project_dir
                .as_deref()
                .unwrap_or_else(|| Path::new(".")),
        )?;

        let mut component_set = if options.sourcepath.is_empty() {
            None
        } else {
            Some(build_from_sourcepath(
                &options.sourcepath,
                options.metadata.as_ref(),
                &registry,
                tree,
            )?)
        };

        if let Some(manifest) = &options.manifest {
            let resolve_paths = (!manifest.directory_paths.is_empty())
                .then_some(manifest.directory_paths.as_slice());
            component_set = Some(ComponentSet::from_manifest(
                &manifest.manifest_path,
                resolve_paths,
                manifest.destructive_changes_pre.as_deref(),
                manifest.destructive_changes_post.as_deref(),
                registry.clone(),
                tree,
            )?);
        }

        if let Some(md) = &options.metadata
            && options.sourcepath.is_empty()
        {
            let base = component_set.unwrap_or_else(|| ComponentSet::new(registry.clone()));
            component_set = Some(apply_metadata_options(base, md, &registry, tree)?);
        }

        let mut cs = component_set.ok_or(ResolveError::EmptyBuilderInput)?;
        if cs.api_version.is_none() {
            cs.api_version.clone_from(&options.api_version);
        }
        if cs.source_api_version.is_none() {
            cs.source_api_version
                .clone_from(&options.source_api_version);
        }
        cs.project_directory.clone_from(&options.project_dir);
        Ok(cs)
    }
}

fn build_from_sourcepath(
    sourcepath: &[PathBuf],
    metadata: Option<&MetadataOptions>,
    registry: &RegistryAccess,
    tree: &TreeHandle,
) -> Result<ComponentSet, ResolveError> {
    let mut cs = ComponentSet::from_source_with_tree(sourcepath, tree, registry.clone(), None)?;
    let Some(md) = metadata else {
        return Ok(cs);
    };

    if !md.excluded_entries.is_empty() {
        let excluded = expand_entries(&md.excluded_entries, sourcepath, registry, tree)?;
        let filtered: Vec<SourceComponent> = cs
            .source_components()
            .into_iter()
            .filter(|comp| {
                !excluded.iter().any(|ex| {
                    ex.type_name == comp.metadata_type().name
                        && (ex.full_name == comp.full_name() || ex.full_name == WILDCARD)
                })
            })
            .collect();
        let mut next = ComponentSet::new(registry.clone());
        for comp in filtered {
            next.add_source_component(comp, None);
        }
        cs = next;
    }

    if !md.metadata_entries.is_empty() {
        let kept = expand_entries(&md.metadata_entries, sourcepath, registry, tree)?;
        let filtered: Vec<SourceComponent> = cs
            .source_components()
            .into_iter()
            .filter(|comp| {
                kept.iter().any(|k| {
                    k.type_name == comp.metadata_type().name
                        && (k.full_name == comp.full_name() || k.full_name == WILDCARD)
                })
            })
            .collect();
        let mut next = ComponentSet::new(registry.clone());
        for comp in filtered {
            next.add_source_component(comp, None);
        }
        cs = next;
    }

    Ok(cs)
}

fn apply_metadata_options(
    mut cs: ComponentSet,
    md: &MetadataOptions,
    registry: &RegistryAccess,
    tree: &TreeHandle,
) -> Result<ComponentSet, ResolveError> {
    let mut filter = ComponentSet::new(registry.clone());

    for member in expand_entries(&md.metadata_entries, &md.directory_paths, registry, tree)? {
        cs.add_member(&member)?;
        filter.add_member(&member)?;
    }

    for (raw_list, kind) in [
        (&md.destructive_entries_pre, DestructiveChangesKind::Pre),
        (&md.destructive_entries_post, DestructiveChangesKind::Post),
    ] {
        for raw in raw_list {
            let (ty, name) = parse_entry_to_type_and_name(raw, registry)?;
            if name.contains(WILDCARD) {
                return Err(ResolveError::WildcardInDestructiveEntries);
            }
            for member in expand_single_entry(&ty, &name, &md.directory_paths, registry, tree)? {
                let mty = registry.type_by_name(&member.type_name)?.clone();
                let stub = SourceComponent::new(
                    ComponentProps {
                        name: member.full_name,
                        metadata_type: mty,
                        xml: None,
                        content: None,
                        parent: None,
                        parent_type: None,
                    },
                    tree.clone(),
                    crate::tree::ForceIgnore::empty(),
                );
                cs.add_source_component(stub, Some(kind));
            }
        }
    }

    let resolved = ComponentSet::from_source_with_tree(
        &md.directory_paths,
        tree,
        registry.clone(),
        Some(&filter),
    )?;

    if !resolved.force_ignored_paths().is_empty() {
        let mut ignore_resolver = MetadataResolver::new(
            registry.clone(),
            tree.clone(),
            ForceIgnorePolicy::Permissive,
        );
        let mut ignored_keys: HashSet<(String, String)> = HashSet::new();
        for ignored_path in resolved.force_ignored_paths() {
            if let Ok(comps) = ignore_resolver.components_from_path(ignored_path, None) {
                for comp in comps {
                    ignored_keys
                        .insert((comp.metadata_type().name.clone(), comp.name().to_owned()));
                }
            }
        }
        if !ignored_keys.is_empty() {
            cs.remove_matching(&ignored_keys);
        }
        cs.force_ignored_paths
            .clone_from(resolved.force_ignored_paths());
    }

    for comp in resolved.source_components() {
        cs.add_source_component(comp, None);
    }
    Ok(cs)
}

impl ComponentSet {
    fn remove_matching(&mut self, targets: &HashSet<(String, String)>) {
        let mut keys_to_remove = Vec::new();
        for key in self.components.keys() {
            let (type_id, full_name) = split_on_first_delimiter(key);
            if let Ok(ty) = self.registry.type_by_name(type_id)
                && targets.contains(&(ty.name.clone(), full_name.to_owned()))
            {
                keys_to_remove.push(key.clone());
            }
        }
        for key in keys_to_remove {
            self.components.shift_remove(&key);
            self.manifest_components.shift_remove(&key);
        }
    }
}

fn expand_manifest_members(
    manifest: &PackageManifest,
    registry: &RegistryAccess,
) -> Result<Vec<(MetadataType, String)>, ResolveError> {
    let mut out = Vec::new();
    for entry in &manifest.types {
        let ty = registry.type_by_name(&entry.name)?;
        let parent_type = ty
            .folder_type
            .as_deref()
            .and_then(|ft| registry.type_by_name(ft).ok());
        for raw_member in &entry.members {
            let full_name = if parent_type.is_some_and(|pt| pt.folder_content_type.is_some()) {
                raw_member
                    .strip_suffix('/')
                    .unwrap_or(raw_member)
                    .to_owned()
            } else {
                raw_member.clone()
            };
            let resolved_type = if let Some(pt) = parent_type
                && ty.folder_type.is_some()
                && ((pt.folder_content_type.is_some() && raw_member.ends_with('/'))
                    || is_member_nested_in_folder(raw_member, ty, pt, &entry.members))
            {
                pt.clone()
            } else {
                ty.clone()
            };
            out.push((resolved_type, full_name));
        }
    }
    Ok(out)
}

fn is_member_nested_in_folder(
    full_name: &str,
    ty: &MetadataType,
    parent_type: &MetadataType,
    members: &[String],
) -> bool {
    if ty.stored_in_folder() {
        !full_name.contains('/') || members.iter().any(|m| m.contains(&format!("{full_name}/")))
    } else {
        parent_type.folder_type.as_deref() != Some(parent_type.id.as_str())
    }
}

fn expand_entries(
    raw_entries: &[String],
    directory_paths: &[PathBuf],
    registry: &RegistryAccess,
    tree: &TreeHandle,
) -> Result<Vec<MetadataMember>, ResolveError> {
    let mut out = Vec::new();
    for raw in raw_entries {
        let (ty, name) = parse_entry_to_type_and_name(raw, registry)?;
        out.extend(expand_single_entry(
            &ty,
            &name,
            directory_paths,
            registry,
            tree,
        )?);
    }
    Ok(out)
}

fn parse_entry_to_type_and_name(
    raw: &str,
    registry: &RegistryAccess,
) -> Result<(MetadataType, String), ResolveError> {
    let (raw_type, raw_name) = raw.split_once(':').map_or((raw, WILDCARD), |(t, n)| (t, n));
    let ty = registry.type_by_name(raw_type.trim())?;
    if ty.name == "CustomLabels"
        && ty.strategies.as_ref().and_then(|s| s.transformer)
            == Some(TransformerStrategy::DecomposedLabels)
    {
        return Err(ResolveError::UseCustomLabelForDecomposed);
    }
    let trimmed_name = raw_name.trim();
    let metadata_name = if trimmed_name.is_empty() {
        WILDCARD.to_owned()
    } else {
        trimmed_name.to_owned()
    };
    Ok((ty.clone(), metadata_name))
}

fn expand_single_entry(
    ty: &MetadataType,
    metadata_name: &str,
    directory_paths: &[PathBuf],
    registry: &RegistryAccess,
    tree: &TreeHandle,
) -> Result<Vec<MetadataMember>, ResolveError> {
    if metadata_name.contains('*') && metadata_name.len() > 1 && !metadata_name.contains(".*") {
        let mut filter = ComponentSet::new(registry.clone());
        filter.add_member(&MetadataMember {
            type_name: ty.name.clone(),
            full_name: WILDCARD.to_owned(),
        })?;
        let matcher = Glob::new(metadata_name).map_or(None, |g| Some(g.compile_matcher()));
        let resolved = ComponentSet::from_source_with_tree(
            directory_paths,
            tree,
            registry.clone(),
            Some(&filter),
        )?;
        return Ok(resolved
            .source_components()
            .into_iter()
            .filter(|c| matcher.as_ref().is_some_and(|m| m.is_match(c.full_name())))
            .map(|c| MetadataMember {
                type_name: c.metadata_type().name.clone(),
                full_name: c.full_name(),
            })
            .collect());
    }
    Ok(vec![MetadataMember {
        type_name: ty.name.clone(),
        full_name: metadata_name.to_owned(),
    }])
}

fn simple_key(type_id: &str, full_name: &str) -> String {
    format!(
        "{}{KEY_DELIMITER}{full_name}",
        type_id.trim().to_ascii_lowercase()
    )
}

fn split_on_first_delimiter(input: &str) -> (&str, &str) {
    input.split_once(KEY_DELIMITER).unwrap_or((input, ""))
}

fn source_key(component: &SourceComponent) -> String {
    format!(
        "{}{}{}{}",
        component.metadata_type().name,
        component.full_name(),
        component
            .xml()
            .map(|p| p.to_string_lossy())
            .unwrap_or_default(),
        component
            .content()
            .map(|p| p.to_string_lossy())
            .unwrap_or_default()
    )
}

fn collect_filenames<'a>(components: impl Iterator<Item = &'a SourceComponent>) -> Vec<PathBuf> {
    let mut set = BTreeSet::new();
    for comp in components {
        if let Ok(walked) = comp.walk_content() {
            for p in walked {
                set.insert(p);
            }
        }
        if let Some(content) = comp.content() {
            set.insert(content.to_path_buf());
        }
        if let Some(xml) = comp.xml() {
            set.insert(xml.to_path_buf());
        }
    }
    set.into_iter().collect()
}

fn construct_full_name(registry: &RegistryAccess, ty: &MetadataType, full_name: &str) -> String {
    if matches!(
        ty.name.as_str(),
        "DashboardFolder" | "ReportFolder" | "EmailTemplateFolder"
    ) && !full_name.ends_with('/')
    {
        return format!("{full_name}/");
    }
    if registry
        .parent_type(&ty.name)
        .and_then(|pt| pt.strategies.as_ref())
        .and_then(|s| s.recomposition)
        == Some(RecompositionStrategy::StartEmpty)
        && let Some((_, child)) = full_name.split_once('.')
    {
        return child.to_owned();
    }
    full_name.to_owned()
}

fn add_to_type_map(
    type_map: &mut BTreeMap<String, BTreeSet<String>>,
    ty: &MetadataType,
    full_name: &str,
    destructive: Option<DestructiveChangesKind>,
) {
    if !ty.addressable() {
        return;
    }
    if full_name == WILDCARD && ty.supports_wildcard_and_name != Some(true) && destructive.is_none()
    {
        let mut set = BTreeSet::new();
        set.insert(WILDCARD.to_owned());
        type_map.insert(ty.name.clone(), set);
        return;
    }
    let existing = type_map.entry(ty.name.clone()).or_default();
    if !existing.contains(WILDCARD) || ty.supports_wildcard_and_name == Some(true) {
        existing.insert(full_name.to_owned());
    }
}
