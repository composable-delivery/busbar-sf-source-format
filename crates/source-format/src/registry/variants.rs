//! Embedded registry loading, preset resolution, and shallow registry merging.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use super::error::RegistryError;
use super::project::SfdxProjectConfig;
use super::types::{MetadataRegistry, MetadataRegistryPatch, Preset, StandardValueSetRegistry};

const BASE_REGISTRY_JSON: &str = include_str!("../../data/metadataRegistry.json");
const STD_VALUE_SET_JSON: &str = include_str!("../../data/stdValueSetRegistry.json");

const PRESET_DECOMPOSE_CUSTOM_LABELS_BETA: &str =
    include_str!("../../data/presets/decomposeCustomLabelsBeta.json");
const PRESET_DECOMPOSE_CUSTOM_LABELS_BETA2: &str =
    include_str!("../../data/presets/decomposeCustomLabelsBeta2.json");
const PRESET_DECOMPOSE_ESR_BETA: &str =
    include_str!("../../data/presets/decomposeExternalServiceRegistrationBeta.json");
const PRESET_DECOMPOSE_PERMISSION_SET_BETA: &str =
    include_str!("../../data/presets/decomposePermissionSetBeta.json");
const PRESET_DECOMPOSE_PERMISSION_SET_BETA2: &str =
    include_str!("../../data/presets/decomposePermissionSetBeta2.json");
const PRESET_DECOMPOSE_SHARING_RULES_BETA: &str =
    include_str!("../../data/presets/decomposeSharingRulesBeta.json");
const PRESET_DECOMPOSE_WORKFLOW_BETA: &str =
    include_str!("../../data/presets/decomposeWorkflowBeta.json");

static DEFAULT_REGISTRY: OnceLock<Arc<MetadataRegistry>> = OnceLock::new();
static STD_VALUE_SET_REGISTRY: OnceLock<StandardValueSetRegistry> = OnceLock::new();

pub(crate) fn default_registry_arc() -> Arc<MetadataRegistry> {
    Arc::clone(DEFAULT_REGISTRY.get_or_init(|| {
        Arc::new(
            serde_json::from_str(BASE_REGISTRY_JSON)
                .expect("embedded metadataRegistry.json is valid"),
        )
    }))
}

/// Returns a reference to the embedded default [`MetadataRegistry`].
///
/// # Panics
///
/// Panics only if the embedded `metadataRegistry.json` asset is malformed, which is
/// prevented by crate tests.
pub fn default_registry() -> &'static MetadataRegistry {
    DEFAULT_REGISTRY
        .get_or_init(|| {
            Arc::new(
                serde_json::from_str(BASE_REGISTRY_JSON)
                    .expect("embedded metadataRegistry.json is valid"),
            )
        })
        .as_ref()
}

/// Returns a reference to the embedded [`StandardValueSetRegistry`].
///
/// # Panics
///
/// Panics only if the embedded `stdValueSetRegistry.json` asset is malformed, which is
/// prevented by crate tests.
pub fn standard_value_set_registry() -> &'static StandardValueSetRegistry {
    STD_VALUE_SET_REGISTRY.get_or_init(|| {
        serde_json::from_str(STD_VALUE_SET_JSON)
            .expect("embedded stdValueSetRegistry.json is valid")
    })
}

/// Loads one of the built-in SDR presets as a [`MetadataRegistryPatch`].
///
/// # Errors
///
/// Returns [`RegistryError::Json`] if the embedded preset JSON fails to deserialize.
pub fn load_preset(preset: Preset) -> Result<MetadataRegistryPatch, RegistryError> {
    let raw = match preset {
        Preset::DecomposeCustomLabelsBeta => PRESET_DECOMPOSE_CUSTOM_LABELS_BETA,
        Preset::DecomposeCustomLabelsBeta2 => PRESET_DECOMPOSE_CUSTOM_LABELS_BETA2,
        Preset::DecomposeExternalServiceRegistrationBeta => PRESET_DECOMPOSE_ESR_BETA,
        Preset::DecomposePermissionSetBeta => PRESET_DECOMPOSE_PERMISSION_SET_BETA,
        Preset::DecomposePermissionSetBeta2 => PRESET_DECOMPOSE_PERMISSION_SET_BETA2,
        Preset::DecomposeSharingRulesBeta => PRESET_DECOMPOSE_SHARING_RULES_BETA,
        Preset::DecomposeWorkflowBeta => PRESET_DECOMPOSE_WORKFLOW_BETA,
    };
    serde_json::from_str(raw).map_err(|source| RegistryError::Json {
        context: format!("preset {preset}"),
        source,
    })
}

/// Builds the effective [`MetadataRegistry`] by merging presets and `registryCustomizations`
/// discovered from `sfdx-project.json` in (or above) `project_dir`.
///
/// Returns a clone of the base registry when no `sfdx-project.json` exists.
///
/// # Errors
///
/// Returns [`RegistryError`] if `sfdx-project.json` cannot be read or parsed, if any
/// `sourceBehaviorOptions` preset name is unknown, or if `registryCustomizations` contains
/// unsafe `directoryName` traversal sequences.
pub fn effective_registry(project_dir: Option<&Path>) -> Result<MetadataRegistry, RegistryError> {
    Ok((*effective_registry_arc(project_dir)?).clone())
}

pub(crate) fn effective_registry_arc(
    project_dir: Option<&Path>,
) -> Result<Arc<MetadataRegistry>, RegistryError> {
    let Some(dir) = project_dir else {
        return Ok(default_registry_arc());
    };
    let Some((_path, config)) = SfdxProjectConfig::find_and_load(dir)? else {
        return Ok(default_registry_arc());
    };
    effective_registry_arc_from_config(&config)
}

pub(crate) fn effective_registry_arc_from_config(
    config: &SfdxProjectConfig,
) -> Result<Arc<MetadataRegistry>, RegistryError> {
    if config.registry_presets.is_empty()
        && config.source_behavior_options.is_empty()
        && config.registry_customizations.is_none()
    {
        return Ok(default_registry_arc());
    }
    Ok(Arc::new(effective_registry_from_config(config)?))
}

/// Builds the effective [`MetadataRegistry`] from an already-parsed [`SfdxProjectConfig`].
///
/// # Errors
///
/// Returns [`RegistryError`] if any preset name is unknown or if `registryCustomizations`
/// contains unsafe `directoryName` path traversal sequences.
pub fn effective_registry_from_config(
    config: &SfdxProjectConfig,
) -> Result<MetadataRegistry, RegistryError> {
    if let Some(customizations) = &config.registry_customizations {
        validate_customization_directory_names(customizations)?;
    }

    let mut seen = HashSet::new();
    let mut presets = Vec::new();
    for raw_name in config
        .registry_presets
        .iter()
        .chain(config.source_behavior_options.iter())
    {
        if seen.insert(raw_name.as_str()) {
            let preset: Preset = raw_name.parse()?;
            presets.push(load_preset(preset)?);
        }
    }

    let mut merged = default_registry().clone();
    for preset in presets {
        merged = first_level_merge(merged, &preset);
    }
    if let Some(customizations) = &config.registry_customizations {
        merged = first_level_merge(merged, customizations);
    }

    Ok(remove_empty_strings(merged))
}

/// Shallowly merges top-level registry maps (`types`, `suffixes`, `strictDirectoryNames`,
/// `childTypes`) from `overrides` onto `original`.
pub fn first_level_merge(
    mut original: MetadataRegistry,
    overrides: &MetadataRegistryPatch,
) -> MetadataRegistry {
    for (k, v) in &overrides.types {
        original.types.insert(k.clone(), v.clone());
    }
    for (k, v) in &overrides.child_types {
        original.child_types.insert(k.clone(), v.clone());
    }
    for (k, v) in &overrides.suffixes {
        original.suffixes.insert(k.clone(), v.clone());
    }
    for (k, v) in &overrides.strict_directory_names {
        original.strict_directory_names.insert(k.clone(), v.clone());
    }
    original
}

fn remove_empty_strings(mut registry: MetadataRegistry) -> MetadataRegistry {
    registry.child_types.retain(|_, v| !v.is_empty());
    registry.suffixes.retain(|_, v| !v.is_empty());
    registry.strict_directory_names.retain(|_, v| !v.is_empty());
    registry
}

fn validate_customization_directory_names(
    patch: &MetadataRegistryPatch,
) -> Result<(), RegistryError> {
    for (type_id, type_def) in &patch.types {
        if has_parent_dir_segment(&type_def.directory_name) {
            let type_name = if type_def.name.is_empty() {
                type_id.clone()
            } else {
                type_def.name.clone()
            };
            return Err(RegistryError::PathTraversal {
                directory_name: type_def.directory_name.clone(),
                type_name,
            });
        }
    }
    Ok(())
}

fn has_parent_dir_segment(directory_name: &str) -> bool {
    directory_name.split(['/', '\\']).any(|seg| seg == "..")
}

/// Converts a [`MetadataRegistryPatch`] into a standalone [`MetadataRegistry`] with empty-string
/// deletion markers stripped.
pub fn patch_to_registry(patch: &MetadataRegistryPatch) -> MetadataRegistry {
    remove_empty_strings(MetadataRegistry {
        types: patch.types.clone(),
        suffixes: patch.suffixes.clone(),
        strict_directory_names: patch.strict_directory_names.clone(),
        child_types: patch.child_types.clone(),
    })
}
