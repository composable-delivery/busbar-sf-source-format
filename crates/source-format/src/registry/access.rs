//! Query container for Salesforce metadata registry definitions.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use super::error::RegistryError;
use super::project::SfdxProjectConfig;
use super::types::{MetadataRegistry, MetadataType};
use super::variants::{
    default_registry_arc, effective_registry_arc, effective_registry_arc_from_config,
};

type StrictFolderDirIndex = HashMap<String, Vec<(usize, String)>>;

static DEFAULT_STRICT_INDEX: OnceLock<Arc<StrictFolderDirIndex>> = OnceLock::new();

fn build_strict_dir_index(registry: &MetadataRegistry) -> StrictFolderDirIndex {
    let mut map: StrictFolderDirIndex =
        HashMap::with_capacity(registry.strict_directory_names.len());
    for (idx, type_id) in registry.strict_directory_names.values().enumerate() {
        if let Some(ty) = registry.types.get(type_id) {
            map.entry(ty.directory_name.clone())
                .or_default()
                .push((idx, type_id.clone()));
        }
    }
    map
}

fn default_strict_dir_index() -> Arc<StrictFolderDirIndex> {
    Arc::clone(
        DEFAULT_STRICT_INDEX
            .get_or_init(|| Arc::new(build_strict_dir_index(default_registry_arc().as_ref()))),
    )
}

/// Query interface over a [`MetadataRegistry`].
#[derive(Debug, Clone)]
pub struct RegistryAccess {
    registry: Arc<MetadataRegistry>,
    strict_dir_index: Arc<StrictFolderDirIndex>,
}

impl Default for RegistryAccess {
    fn default() -> Self {
        Self::new()
    }
}

impl RegistryAccess {
    /// Creates a `RegistryAccess` backed by the default embedded metadata registry.
    pub fn new() -> Self {
        Self {
            registry: default_registry_arc(),
            strict_dir_index: default_strict_dir_index(),
        }
    }

    /// Creates a `RegistryAccess` with presets and `registryCustomizations` resolved from
    /// `sfdx-project.json` in (or above) `project_dir`.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError`] if `sfdx-project.json` cannot be read/parsed or specifies an
    /// invalid preset or unsafe customization directory.
    pub fn from_project_dir(project_dir: &Path) -> Result<Self, RegistryError> {
        let default_arc = default_registry_arc();
        let registry = effective_registry_arc(Some(project_dir))?;
        let strict_dir_index = if Arc::ptr_eq(&registry, &default_arc) {
            default_strict_dir_index()
        } else {
            Arc::new(build_strict_dir_index(&registry))
        };
        Ok(Self {
            registry,
            strict_dir_index,
        })
    }

    /// Creates a `RegistryAccess` using an already-parsed [`SfdxProjectConfig`].
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError`] if a preset is unknown or a customization directory is unsafe.
    pub fn from_project_config(config: &SfdxProjectConfig) -> Result<Self, RegistryError> {
        let default_arc = default_registry_arc();
        let registry = effective_registry_arc_from_config(config)?;
        let strict_dir_index = if Arc::ptr_eq(&registry, &default_arc) {
            default_strict_dir_index()
        } else {
            Arc::new(build_strict_dir_index(&registry))
        };
        Ok(Self {
            registry,
            strict_dir_index,
        })
    }

    /// Creates a `RegistryAccess` wrapping an explicit [`MetadataRegistry`].
    pub fn from_registry(registry: MetadataRegistry) -> Self {
        let strict_dir_index = Arc::new(build_strict_dir_index(&registry));
        Self {
            registry: Arc::new(registry),
            strict_dir_index,
        }
    }

    /// Returns the underlying [`MetadataRegistry`].
    pub fn registry(&self) -> &MetadataRegistry {
        &self.registry
    }

    /// Returns `true` when this instance uses the unmodified default embedded registry.
    pub fn is_default(&self) -> bool {
        Arc::ptr_eq(&self.registry, &default_registry_arc())
    }

    /// Looks up a metadata type (or child type) by case-insensitive name, following `aliasFor`.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::MissingTypeDefinition`] when no matching type exists, or
    /// [`RegistryError::MissingChildTypeDefinition`] when a `childTypes` entry points to a
    /// missing child definition.
    pub fn type_by_name(&self, name: &str) -> Result<&MetadataType, RegistryError> {
        let lower = name.trim().to_ascii_lowercase();
        if let Some(parent_id) = self
            .registry
            .child_types
            .get(&lower)
            .filter(|p| !p.is_empty())
        {
            if let Some(child_type) = self
                .registry
                .types
                .get(parent_id)
                .and_then(|parent| parent.children.as_ref())
                .and_then(|children| children.types.get(&lower))
            {
                return Ok(child_type);
            }
            return Err(RegistryError::MissingChildTypeDefinition {
                parent_id: parent_id.clone(),
                child_id: lower,
            });
        }

        let Some(ty) = self.registry.types.get(&lower) else {
            return Err(RegistryError::MissingTypeDefinition {
                name: name.to_owned(),
                suggestions: suggest_types(&self.registry, &lower),
            });
        };

        if let Some(alias) = &ty.alias_for
            && let Some(target) = self.registry.types.get(alias)
        {
            return Ok(target);
        }

        Ok(ty)
    }

    /// Looks up a metadata type by file suffix (exact match as keyed in `registry.suffixes`).
    pub fn type_by_suffix(&self, suffix: &str) -> Option<&MetadataType> {
        let type_id = self.registry.suffixes.get(suffix)?;
        self.type_by_name(type_id).ok()
    }

    /// Returns similar suffix guesses and their metadata types using Levenshtein distance <= 2.
    pub fn guess_type_by_suffix(&self, suffix: &str) -> Vec<(&str, &MetadataType)> {
        let mut guesses = self
            .registry
            .suffixes
            .iter()
            .filter_map(|(candidate, type_id)| {
                let dist = levenshtein(candidate, suffix);
                (dist <= 2)
                    .then(|| {
                        self.type_by_name(type_id)
                            .ok()
                            .map(|ty| (dist, candidate.as_str(), ty))
                    })
                    .flatten()
            })
            .collect::<Vec<_>>();
        guesses.sort_by_key(|(dist, candidate, _)| (*dist, *candidate));
        guesses
            .into_iter()
            .map(|(_, candidate, ty)| (candidate, ty))
            .collect()
    }

    /// Returns the first top-level metadata type matching `predicate`, following `aliasFor`.
    pub fn find_type<F>(&self, mut predicate: F) -> Option<&MetadataType>
    where
        F: FnMut(&MetadataType) -> bool,
    {
        let matched = self.registry.types.values().find(|ty| predicate(ty))?;
        if let Some(alias) = &matched.alias_for
            && let Some(target) = self.registry.types.get(alias)
        {
            return Some(target);
        }
        Some(matched)
    }

    /// Returns all metadata types registered under `strictDirectoryNames`.
    pub fn strict_folder_types(&self) -> Vec<&MetadataType> {
        self.registry
            .strict_directory_names
            .values()
            .filter_map(|type_id| self.registry.types.get(type_id))
            .collect()
    }

    /// Iterates all strict-folder metadata types matching `ty.directory_name == dir_name` in
    /// `strictDirectoryNames` order, yielding `(index, &MetadataType)` pairs without allocating.
    pub fn strict_folder_types_for_dir(
        &self,
        dir_name: &str,
    ) -> impl Iterator<Item = (usize, &MetadataType)> {
        self.strict_dir_index
            .get(dir_name)
            .into_iter()
            .flat_map(|entries| {
                entries.iter().filter_map(|(idx, type_id)| {
                    self.registry.types.get(type_id).map(|ty| (*idx, ty))
                })
            })
    }

    /// Returns all folder container types (`folderContentType` is set and `aliasFor` is unset).
    pub fn folder_content_types(&self) -> Vec<&MetadataType> {
        self.registry
            .types
            .values()
            .filter(|ty| ty.folder_content_type.is_some() && ty.alias_for.is_none())
            .collect()
    }

    /// Returns all metadata types that are aliases (`aliasFor` is set).
    pub fn alias_types(&self) -> Vec<&MetadataType> {
        self.registry
            .types
            .values()
            .filter(|ty| ty.alias_for.is_some())
            .collect()
    }

    /// Returns the parent metadata type for a given child type name, if any.
    pub fn parent_type(&self, child_name: &str) -> Option<&MetadataType> {
        let lower = child_name.trim().to_ascii_lowercase();
        let parent_id = self.registry.child_types.get(&lower)?;
        self.registry.types.get(parent_id)
    }
}

fn suggest_types(registry: &MetadataRegistry, lower: &str) -> Vec<String> {
    let mut matches: Vec<(usize, &str)> = registry
        .types
        .values()
        .filter_map(|ty| {
            let dist = levenshtein(&ty.id, lower);
            (dist <= 3).then_some((dist, ty.name.as_str()))
        })
        .collect();
    matches.sort_unstable_by_key(|(dist, name)| (*dist, *name));
    matches
        .into_iter()
        .take(5)
        .map(|(_, name)| name.to_owned())
        .collect()
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b_chars.len()).collect();
    let mut curr = vec![0; b_chars.len() + 1];

    for (i, ca) in a.chars().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b_chars.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (curr[j] + 1).min(prev[j + 1] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b_chars.len()]
}
