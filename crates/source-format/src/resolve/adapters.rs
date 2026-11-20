//! Source adapters matching SDR's `src/resolve/adapters/*`.

use std::path::{Component, Path, PathBuf};

use crate::registry::{AdapterStrategy, DecompositionStrategy, MetadataType, RegistryAccess};
use crate::tree::{ForceIgnore, TreeContainer, TreeFileKind};

use crate::path_util::{
    MetadataXml, ext_name, parent_name, parse_metadata_xml, parse_nested_full_name, path_base_name,
};
use crate::resolve::component::{ComponentProps, SourceComponent, TreeHandle};
use crate::resolve::error::ResolveError;

const META_XML_SUFFIX: &str = "-meta.xml";

/// Controls how [`MetadataResolver`](crate::resolve::MetadataResolver) resolves components.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ResolveMode {
    /// Path was passed directly as a source file or directory.
    Source,
    /// Path was discovered while walking a package or MDAPI root.
    NonSource,
}

/// Resolves a [`SourceComponent`] from `trigger_path` using `metadata_type`'s configured
/// [`AdapterStrategy`].
pub(crate) fn resolve_with_adapter(
    metadata_type: &MetadataType,
    trigger_path: &Path,
    mode: ResolveMode,
    registry: &RegistryAccess,
    tree: &TreeHandle,
    force_ignore: &ForceIgnore,
) -> Result<Option<SourceComponent>, ResolveError> {
    let strategy = metadata_type
        .strategies
        .as_ref()
        .map_or(AdapterStrategy::Default, |s| s.adapter);

    match strategy {
        AdapterStrategy::Default => {
            let component = init_base_component(
                metadata_type,
                trigger_path,
                AllowMetadataWithContent::No,
                OwnFolder::No,
                registry,
                tree,
                force_ignore,
                |p| Some(p.to_path_buf()),
            )?;
            Ok(component)
        }
        AdapterStrategy::MatchingContentFile => {
            let component = init_base_component(
                metadata_type,
                trigger_path,
                AllowMetadataWithContent::Yes,
                OwnFolder::No,
                registry,
                tree,
                force_ignore,
                |p| Some(PathBuf::from(format!("{}{META_XML_SUFFIX}", p.display()))),
            )?;
            populate_matching_content(
                metadata_type,
                trigger_path,
                component,
                registry,
                tree,
                force_ignore,
            )
        }
        AdapterStrategy::MixedContent => {
            let component = init_base_component(
                metadata_type,
                trigger_path,
                AllowMetadataWithContent::Yes,
                OwnFolder::No,
                registry,
                tree,
                force_ignore,
                |p| find_metadata_from_content(metadata_type, p, tree),
            )?;
            populate_mixed_content(metadata_type, trigger_path, component, tree, force_ignore)
        }
        AdapterStrategy::Bundle => {
            let component = init_base_component(
                metadata_type,
                trigger_path,
                AllowMetadataWithContent::Yes,
                OwnFolder::Yes,
                registry,
                tree,
                force_ignore,
                |p| find_root_xml_in_own_folder(metadata_type, p, tree),
            )?;
            populate_bundle(metadata_type, trigger_path, component, tree, force_ignore)
        }
        AdapterStrategy::Decomposed => resolve_decomposed(
            metadata_type,
            trigger_path,
            mode,
            registry,
            tree,
            force_ignore,
        ),
        AdapterStrategy::DigitalExperience => {
            resolve_digital_experience(metadata_type, trigger_path, registry, tree, force_ignore)
        }
        AdapterStrategy::UiBundles => {
            resolve_ui_bundle(metadata_type, trigger_path, registry, tree, force_ignore)
        }
        AdapterStrategy::PartiallyDecomposed => {
            resolve_partially_decomposed(metadata_type, trigger_path, registry, tree, force_ignore)
        }
    }
}

/// Returns whether the adapter allows sole content+metadata XML definitions (`Foo.tab`).
pub(crate) fn allows_metadata_with_content(strategy: AdapterStrategy) -> bool {
    !matches!(
        strategy,
        AdapterStrategy::Default
            | AdapterStrategy::Decomposed
            | AdapterStrategy::PartiallyDecomposed
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AllowMetadataWithContent {
    Yes,
    No,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OwnFolder {
    Yes,
    No,
}

#[expect(
    clippy::too_many_arguments,
    reason = "internal adapter initializer shared across 5 strategies"
)]
fn init_base_component<F>(
    metadata_type: &MetadataType,
    trigger_path: &Path,
    allow_content_meta: AllowMetadataWithContent,
    _own_folder: OwnFolder,
    registry: &RegistryAccess,
    tree: &TreeHandle,
    force_ignore: &ForceIgnore,
    find_root_xml: F,
) -> Result<Option<SourceComponent>, ResolveError>
where
    F: FnOnce(&Path) -> Option<PathBuf>,
{
    let mut root_meta = parse_as_root_metadata_xml(metadata_type, trigger_path, allow_content_meta);
    if root_meta.is_none()
        && let Some(root_path) = find_root_xml(trigger_path)
    {
        root_meta = parse_metadata_xml(&root_path);
    }

    if let Some(ref meta) = root_meta
        && force_ignore.denies(&meta.path)
    {
        return Err(ResolveError::ForceIgnoreMetadataXml {
            xml_path: meta.path.clone(),
            trigger_path: trigger_path.to_path_buf(),
        });
    }

    let component = if let Some(meta) = root_meta {
        let name = calculate_name(registry, metadata_type, &meta)?;
        let parent_type = metadata_type
            .folder_type
            .as_deref()
            .and_then(|ft| registry.type_by_name(ft).ok())
            .cloned();
        Some(SourceComponent::new(
            ComponentProps {
                name,
                metadata_type: metadata_type.clone(),
                xml: Some(meta.path),
                content: None,
                parent: None,
                parent_type,
            },
            tree.clone(),
            force_ignore.clone(),
        ))
    } else {
        None
    };

    Ok(component)
}

fn populate_matching_content(
    metadata_type: &MetadataType,
    trigger_path: &Path,
    component: Option<SourceComponent>,
    registry: &RegistryAccess,
    tree: &TreeHandle,
    force_ignore: &ForceIgnore,
) -> Result<Option<SourceComponent>, ResolveError> {
    let Some(mut component) = component else {
        return Err(ResolveError::ExpectedSourceFiles {
            path: trigger_path.to_path_buf(),
            type_name: metadata_type.name.clone(),
        });
    };

    let trigger_str = trigger_path.to_string_lossy();
    let mut source_path: Option<PathBuf> = None;

    if component.xml() == Some(trigger_path) {
        if let Some(stripped) = trigger_str.strip_suffix(META_XML_SUFFIX) {
            let candidate = PathBuf::from(stripped);
            if tree.exists(&candidate) {
                source_path = Some(candidate);
            }
        }
    } else if registry
        .type_by_suffix(ext_name(trigger_path))
        .is_some_and(|t| t == metadata_type)
    {
        source_path = Some(trigger_path.to_path_buf());
    }

    let Some(content_path) = source_path else {
        return Err(ResolveError::ExpectedSourceFiles {
            path: trigger_path.to_path_buf(),
            type_name: metadata_type.name.clone(),
        });
    };

    if force_ignore.denies(&content_path) {
        return Err(ResolveError::ForceIgnoreSource {
            type_name: metadata_type.name.clone(),
            source_path: content_path,
        });
    }

    component.set_content(content_path);
    Ok(Some(component))
}

fn populate_mixed_content(
    metadata_type: &MetadataType,
    trigger_path: &Path,
    component: Option<SourceComponent>,
    tree: &TreeHandle,
    force_ignore: &ForceIgnore,
) -> Result<Option<SourceComponent>, ResolveError> {
    let trimmed = trim_path_to_content(metadata_type, trigger_path);
    let content_path =
        if component.as_ref().and_then(SourceComponent::xml) == Some(trimmed.as_path()) {
            trimmed
                .parent()
                .and_then(|dir| tree.find(TreeFileKind::Content, path_base_name(&trimmed), dir))
        } else {
            Some(trimmed)
        };

    let Some(content_path) = content_path.filter(|p| tree.exists(p)) else {
        return Err(ResolveError::ExpectedSourceFiles {
            path: trigger_path.to_path_buf(),
            type_name: metadata_type.name.clone(),
        });
    };

    if let Some(mut existing) = component {
        existing.set_content(content_path);
        return Ok(Some(existing));
    }

    let xml = metadata_type
        .meta_file_suffix
        .as_deref()
        .map(|s| content_path.join(s));
    Ok(Some(SourceComponent::new(
        ComponentProps {
            name: path_base_name(&content_path).to_owned(),
            metadata_type: metadata_type.clone(),
            xml,
            content: Some(content_path),
            parent: None,
            parent_type: None,
        },
        tree.clone(),
        force_ignore.clone(),
    )))
}

fn populate_bundle(
    metadata_type: &MetadataType,
    trigger_path: &Path,
    component: Option<SourceComponent>,
    tree: &TreeHandle,
    force_ignore: &ForceIgnore,
) -> Result<Option<SourceComponent>, ResolveError> {
    if tree.is_directory(trigger_path)? {
        if tree.read_directory(trigger_path)?.is_empty() {
            return Ok(None);
        }
    } else if component.is_none() {
        let root = trim_path_to_content(metadata_type, trigger_path);
        if !tree.exists(&root) || !tree.is_directory(&root)? {
            return Ok(None);
        }
    }
    populate_mixed_content(metadata_type, trigger_path, component, tree, force_ignore)
}

fn resolve_decomposed(
    metadata_type: &MetadataType,
    trigger_path: &Path,
    mode: ResolveMode,
    registry: &RegistryAccess,
    tree: &TreeHandle,
    force_ignore: &ForceIgnore,
) -> Result<Option<SourceComponent>, ResolveError> {
    let mut root_meta =
        parse_as_root_metadata_xml(metadata_type, trigger_path, AllowMetadataWithContent::No);
    if root_meta.is_none()
        && let Some(root_path) = find_root_xml_in_own_folder(metadata_type, trigger_path, tree)
    {
        root_meta = parse_metadata_xml(&root_path);
    }

    let mut component = if let Some(meta) = root_meta {
        if force_ignore.denies(&meta.path) {
            return Err(ResolveError::ForceIgnoreMetadataXml {
                xml_path: meta.path,
                trigger_path: trigger_path.to_path_buf(),
            });
        }
        let name = if metadata_type.folder_type.is_some() {
            format!("{}/{}", parent_name(&meta.path), meta.full_name)
        } else {
            meta.full_name
        };
        Some(SourceComponent::new(
            ComponentProps {
                name,
                metadata_type: metadata_type.clone(),
                xml: Some(meta.path),
                content: None,
                parent: None,
                parent_type: None,
            },
            tree.clone(),
            force_ignore.clone(),
        ))
    } else {
        let _ = registry;
        None
    };

    let Some(meta_xml) = parse_metadata_xml(trigger_path) else {
        return Ok(component);
    };

    let path_to_content = trim_path_to_content(metadata_type, trigger_path);
    let strategy = metadata_type
        .strategies
        .as_ref()
        .and_then(|s| s.decomposition);

    if let Some(children_cfg) = &metadata_type.children
        && let Some(child_type_id) = children_cfg.suffixes.get(&meta_xml.suffix)
        && let Some(child_type) = children_cfg.types.get(child_type_id)
        && child_type.unaddressable_without_parent != Some(true)
        && child_type.addressable()
    {
        if matches!(
            strategy,
            Some(DecompositionStrategy::FolderPerType | DecompositionStrategy::TopLevel)
        ) || mode == ResolveMode::Source
        {
            let mut parent = component.unwrap_or_else(|| {
                let parent_name_val = if strategy == Some(DecompositionStrategy::FolderPerType) {
                    path_base_name(&path_to_content).to_owned()
                } else {
                    path_to_content.to_string_lossy().into_owned()
                };
                SourceComponent::new(
                    ComponentProps {
                        name: parent_name_val,
                        metadata_type: metadata_type.clone(),
                        xml: None,
                        content: None,
                        parent: None,
                        parent_type: None,
                    },
                    tree.clone(),
                    force_ignore.clone(),
                )
            });
            parent.set_content(path_to_content);
            return Ok(Some(SourceComponent::new(
                ComponentProps {
                    name: meta_xml.full_name,
                    metadata_type: child_type.clone(),
                    xml: Some(trigger_path.to_path_buf()),
                    content: None,
                    parent: Some(Box::new(parent)),
                    parent_type: None,
                },
                tree.clone(),
                force_ignore.clone(),
            )));
        }
    } else if component.is_none() {
        return Err(ResolveError::UnexpectedChildType {
            path: trigger_path.to_path_buf(),
            parent_type: metadata_type.name.clone(),
        });
    }

    if let Some(ref mut comp) = component {
        comp.set_content(path_to_content);
    }
    Ok(component)
}

fn find_root_xml_in_own_folder(
    metadata_type: &MetadataType,
    trigger_path: &Path,
    tree: &TreeHandle,
) -> Option<PathBuf> {
    let component_root = trim_path_to_content(metadata_type, trigger_path);
    if !tree.is_directory(&component_root).ok()? {
        return None;
    }

    let entries = tree.read_directory(&component_root).ok()?;
    for entry in entries {
        let full = component_root.join(&entry);
        if let Some(parsed) = parse_metadata_xml(&full) {
            let matches_root = metadata_type.suffix.as_deref() == Some(parsed.suffix.as_str())
                || metadata_type.legacy_suffix.as_deref() == Some(parsed.suffix.as_str());
            if matches_root {
                return Some(full);
            }
        }
    }

    let root_name = component_root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    tree.find(TreeFileKind::MetadataXml, root_name, &component_root)
}

fn find_metadata_from_content(
    metadata_type: &MetadataType,
    trigger_path: &Path,
    tree: &TreeHandle,
) -> Option<PathBuf> {
    let root_content = trim_path_to_content(metadata_type, trigger_path);
    let root_type_dir = root_content.parent()?;
    let full_name = path_base_name(&root_content);
    tree.find(TreeFileKind::MetadataXml, full_name, root_type_dir)
}

pub(crate) fn trim_path_to_content(metadata_type: &MetadataType, path: &Path) -> PathBuf {
    let segments: Vec<&str> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    let Some(idx) = segments
        .iter()
        .rposition(|s| *s == metadata_type.directory_name)
    else {
        return path.to_path_buf();
    };
    let offset = if metadata_type.stored_in_folder() {
        3
    } else {
        2
    };
    let end = (idx + offset).min(segments.len());
    let mut out = if path.is_absolute() {
        PathBuf::from("/")
    } else {
        PathBuf::new()
    };
    for seg in &segments[..end] {
        out.push(seg);
    }
    out
}

fn parse_as_root_metadata_xml(
    metadata_type: &MetadataType,
    path: &Path,
    allow_content_meta: AllowMetadataWithContent,
) -> Option<MetadataXml> {
    if let Some(meta_xml) = parse_metadata_xml(path) {
        let is_root = if metadata_type.strict_directory() {
            let parent = path.parent();
            let type_dir = if metadata_type.stored_in_folder() {
                parent.and_then(Path::parent)
            } else {
                parent
            };
            let type_dir_name = type_dir
                .and_then(Path::file_name)
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            let name_matches_parent = parent.and_then(Path::file_name).and_then(|s| s.to_str())
                == Some(meta_xml.full_name.as_str());
            let in_type_dir = type_dir_name == metadata_type.directory_name;
            let suffix_matches = metadata_type.suffix.as_deref() == Some(meta_xml.suffix.as_str())
                || metadata_type.legacy_suffix.as_deref() == Some(meta_xml.suffix.as_str());
            suffix_matches && (name_matches_parent || in_type_dir)
        } else {
            true
        };
        return is_root.then_some(meta_xml);
    }

    if let Some(folder_meta) = parse_as_folder_metadata_xml_bare(path) {
        return Some(folder_meta);
    }

    if allow_content_meta == AllowMetadataWithContent::No {
        return parse_as_content_metadata_xml(metadata_type, path);
    }
    None
}

fn parse_as_folder_metadata_xml_bare(path: &Path) -> Option<MetadataXml> {
    let file = path.file_name()?.to_str()?;
    let stem = file.strip_suffix(META_XML_SUFFIX)?;
    if stem.is_empty() || stem.contains('.') {
        return None;
    }
    if path.components().count() > 1 {
        Some(MetadataXml {
            full_name: stem.to_owned(),
            suffix: String::new(),
            path: path.to_path_buf(),
        })
    } else {
        None
    }
}

fn parse_as_content_metadata_xml(metadata_type: &MetadataType, path: &Path) -> Option<MetadataXml> {
    if metadata_type.stored_in_folder()
        && let Some(full_name) = parse_nested_full_name(path, &metadata_type.directory_name)
        && let Some(suffix) = &metadata_type.suffix
    {
        return Some(MetadataXml {
            full_name,
            suffix: suffix.clone(),
            path: path.to_path_buf(),
        });
    }

    let parent_path = path.parent()?;
    let segments: Vec<&str> = parent_path
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    let type_folder_idx = segments
        .iter()
        .rposition(|s| *s == metadata_type.directory_name)?;
    let allowed_idx = if metadata_type.folder_type.as_deref() == Some(metadata_type.id.as_str()) {
        segments.len().checked_sub(2)?
    } else {
        segments.len().checked_sub(1)?
    };
    if type_folder_idx != allowed_idx {
        return None;
    }

    let file_name = path.file_name()?.to_str()?;
    let (name_part, suffix_part) = file_name.rsplit_once('.')?;
    if metadata_type.suffix.as_deref() == Some(suffix_part) {
        return Some(MetadataXml {
            full_name: name_part.to_owned(),
            suffix: suffix_part.to_owned(),
            path: path.to_path_buf(),
        });
    }
    None
}

fn calculate_name(
    registry: &RegistryAccess,
    metadata_type: &MetadataType,
    root_meta: &MetadataXml,
) -> Result<String, ResolveError> {
    if metadata_type.folder_content_type.is_some() || metadata_type.stored_in_folder() {
        return parse_nested_full_name(&root_meta.path, &metadata_type.directory_name).ok_or_else(
            || ResolveError::TypeInference {
                path: root_meta.path.clone(),
            },
        );
    }

    let Some(folder_type_id) = &metadata_type.folder_type else {
        return Ok(root_meta.full_name.clone());
    };

    let grandparent = registry.type_by_name(folder_type_id)?;
    if let Some(gp_folder_type) = &grandparent.folder_type {
        if gp_folder_type == &metadata_type.id {
            return Ok(root_meta.full_name.clone());
        }
        let segments: Vec<&str> = root_meta
            .path
            .components()
            .filter_map(|c| match c {
                Component::Normal(s) => s.to_str(),
                _ => None,
            })
            .collect();
        if let Some(idx) = segments
            .iter()
            .position(|s| *s == grandparent.directory_name)
            && let Some(model_name) = segments.get(idx + 1)
        {
            return Ok(format!("{model_name}.{}", root_meta.full_name));
        }
    }

    Err(ResolveError::TypeInference {
        path: root_meta.path.clone(),
    })
}

fn resolve_digital_experience(
    metadata_type: &MetadataType,
    trigger_path: &Path,
    registry: &RegistryAccess,
    tree: &TreeHandle,
    force_ignore: &ForceIgnore,
) -> Result<Option<SourceComponent>, ResolveError> {
    if metadata_type.id == "digitalexperiencebundle" {
        let Some(bundle_xml) =
            de_bundle_metadata_xml_path(trigger_path, metadata_type, metadata_type)
        else {
            return Ok(None);
        };
        if force_ignore.denies(&bundle_xml) {
            return Err(ResolveError::ForceIgnoreMetadataXml {
                xml_path: bundle_xml,
                trigger_path: trigger_path.to_path_buf(),
            });
        }
        if parse_metadata_xml(&bundle_xml).is_none() || !tree.exists(&bundle_xml) {
            return Ok(None);
        }
        let bundle_name = de_bundle_name_from_xml(&bundle_xml);
        return Ok(Some(SourceComponent::new(
            ComponentProps {
                name: bundle_name,
                metadata_type: metadata_type.clone(),
                xml: Some(bundle_xml),
                content: None,
                parent: None,
                parent_type: None,
            },
            tree.clone(),
            force_ignore.clone(),
        )));
    }

    if tree.is_directory(trigger_path)? {
        if tree.read_directory(trigger_path)?.is_empty() {
            return Ok(None);
        }
    } else if !tree.exists(trigger_path) {
        return Err(ResolveError::ExpectedSourceFiles {
            path: trigger_path.to_path_buf(),
            type_name: metadata_type.name.clone(),
        });
    }

    let content_path = trim_de_content_path(trigger_path, tree)?;
    if !tree.exists(&content_path) {
        return Err(ResolveError::ExpectedSourceFiles {
            path: trigger_path.to_path_buf(),
            type_name: metadata_type.name.clone(),
        });
    }

    let xml_path = metadata_type
        .meta_file_suffix
        .as_deref()
        .map(|s| content_path.join(s));

    let Some(parent_type) = registry.parent_type(&metadata_type.id) else {
        return Err(ResolveError::TypeInference {
            path: trigger_path.to_path_buf(),
        });
    };

    let bundle_xml = de_bundle_metadata_xml_path(&content_path, metadata_type, parent_type)
        .ok_or_else(|| ResolveError::TypeInference {
            path: trigger_path.to_path_buf(),
        })?;
    let bundle_name = de_bundle_name_from_xml(&bundle_xml);
    let parent = SourceComponent::new(
        ComponentProps {
            name: bundle_name,
            metadata_type: parent_type.clone(),
            xml: Some(bundle_xml),
            content: None,
            parent: None,
            parent_type: None,
        },
        tree.clone(),
        force_ignore.clone(),
    );
    let comp_name = format!(
        "{}/{}",
        parent_name(&content_path),
        path_base_name(&content_path)
    );
    Ok(Some(SourceComponent::new(
        ComponentProps {
            name: comp_name,
            metadata_type: metadata_type.clone(),
            xml: xml_path,
            content: Some(content_path),
            parent: Some(Box::new(parent)),
            parent_type: Some(parent_type.clone()),
        },
        tree.clone(),
        force_ignore.clone(),
    )))
}

fn trim_de_content_path(path: &Path, tree: &TreeHandle) -> Result<PathBuf, ResolveError> {
    let base = if tree.is_directory(path)? {
        path.to_path_buf()
    } else {
        path.parent().unwrap_or(path).to_path_buf()
    };
    let segments: Vec<&str> = base
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    let Some(de_idx) = segments.iter().position(|s| *s == "digitalExperiences") else {
        return Ok(base);
    };
    // `digitalExperiences` + `BaseType/SpaceApiName/ContentType/ContentApiName` (4 segments)
    let target_len = de_idx + 1 + 4;
    if segments.len() > target_len {
        let mut out = if base.is_absolute() {
            PathBuf::from("/")
        } else {
            PathBuf::new()
        };
        for seg in &segments[..target_len] {
            out.push(seg);
        }
        return Ok(out);
    }
    Ok(base)
}

fn de_bundle_metadata_xml_path(
    path: &Path,
    self_type: &MetadataType,
    bundle_type: &MetadataType,
) -> Option<PathBuf> {
    if self_type.id == "digitalexperiencebundle"
        && path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|n| n.ends_with(META_XML_SUFFIX))
    {
        return Some(path.to_path_buf());
    }
    let segments: Vec<&str> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    let type_idx = segments
        .iter()
        .rposition(|s| *s == self_type.directory_name)?;
    let bundle_name = segments.get(type_idx + 2)?;
    let suffix = bundle_type.suffix.as_deref()?;
    let mut base = if path.is_absolute() {
        PathBuf::from("/")
    } else {
        PathBuf::new()
    };
    for seg in &segments[..=(type_idx + 2)] {
        base.push(seg);
    }
    Some(base.join(format!("{bundle_name}.{suffix}{META_XML_SUFFIX}")))
}

fn de_bundle_name_from_xml(bundle_xml: &Path) -> String {
    let space_dir = bundle_xml.parent().unwrap_or(bundle_xml);
    let base_type_dir = space_dir.parent().unwrap_or(space_dir);
    let base_type = base_type_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let space_name = space_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    format!("{base_type}/{space_name}")
}

fn resolve_ui_bundle(
    metadata_type: &MetadataType,
    trigger_path: &Path,
    registry: &RegistryAccess,
    tree: &TreeHandle,
    force_ignore: &ForceIgnore,
) -> Result<Option<SourceComponent>, ResolveError> {
    let component = init_base_component(
        metadata_type,
        trigger_path,
        AllowMetadataWithContent::Yes,
        OwnFolder::Yes,
        registry,
        tree,
        force_ignore,
        |p| find_root_xml_in_own_folder(metadata_type, p, tree),
    )?;
    let Some(source) = populate_bundle(metadata_type, trigger_path, component, tree, force_ignore)?
    else {
        return Ok(None);
    };
    let Some(content_path) = source.content() else {
        return Ok(Some(source));
    };
    let app_name = path_base_name(content_path).to_owned();
    let expected_xml = content_path.join(format!("{app_name}.uibundle-meta.xml"));
    if !tree.exists(&expected_xml) {
        return Err(ResolveError::ExpectedSourceFiles {
            path: expected_xml,
            type_name: metadata_type.name.clone(),
        });
    }
    if source.xml() == Some(expected_xml.as_path()) {
        return Ok(Some(source));
    }
    Ok(Some(SourceComponent::new(
        ComponentProps {
            name: app_name,
            metadata_type: source.metadata_type().clone(),
            xml: Some(expected_xml),
            content: source.content().map(Path::to_path_buf),
            parent: source.parent().cloned().map(Box::new),
            parent_type: source.parent_type().cloned(),
        },
        tree.clone(),
        force_ignore.clone(),
    )))
}

fn resolve_partially_decomposed(
    metadata_type: &MetadataType,
    trigger_path: &Path,
    registry: &RegistryAccess,
    tree: &TreeHandle,
    force_ignore: &ForceIgnore,
) -> Result<Option<SourceComponent>, ResolveError> {
    let _base = init_base_component(
        metadata_type,
        trigger_path,
        AllowMetadataWithContent::No,
        OwnFolder::No,
        registry,
        tree,
        force_ignore,
        |p| Some(p.to_path_buf()),
    )?;
    let name = partial_decomposed_name(trigger_path);
    let trigger_str = trigger_path.to_string_lossy();
    let parent_type = registry.parent_type(&metadata_type.id);

    if metadata_type.children.is_none()
        && let Some(pt) = parent_type
        && let Some(children) = &pt.children
        && children
            .suffixes
            .keys()
            .any(|sfx| trigger_str.ends_with(&format!(".{sfx}")))
    {
        let ext = ext_name(trigger_path);
        let target_suffix = pt
            .suffix
            .as_deref()
            .unwrap_or("externalServiceRegistration");
        let xml_path = if !ext.is_empty()
            && let Some(stem) = trigger_str.strip_suffix(ext)
        {
            PathBuf::from(format!("{stem}{target_suffix}{META_XML_SUFFIX}"))
        } else {
            trigger_path.to_path_buf()
        };
        return Ok(Some(SourceComponent::new(
            ComponentProps {
                name,
                metadata_type: pt.clone(),
                xml: Some(xml_path),
                content: None,
                parent: None,
                parent_type: None,
            },
            tree.clone(),
            force_ignore.clone(),
        )));
    }

    Ok(Some(SourceComponent::new(
        ComponentProps {
            name,
            metadata_type: metadata_type.clone(),
            xml: Some(trigger_path.to_path_buf()),
            content: None,
            parent: None,
            parent_type: None,
        },
        tree.clone(),
        force_ignore.clone(),
    )))
}

fn partial_decomposed_name(path: &Path) -> String {
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    file_name.split('.').next().unwrap_or(file_name).to_owned()
}
