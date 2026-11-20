//! Tests ported from SDR's `test/registry/registryValidation.test.ts`,
//! `test/registry/presetTesting.ts`, and `sfdx-project.json` snapshot fixtures.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use busbar_sf_source_format::registry::{
    AdapterStrategy, DecompositionStrategy, MetadataRegistry, MetadataType, Preset, RegistryAccess,
    RegistryError, SfdxProjectConfig, TransformerStrategy, default_registry, effective_registry,
    first_level_merge, load_preset, patch_to_registry, standard_value_set_registry,
};

const UNSUPPORTED_TYPES: &[&str] = &["PicklistValue", "CustomValue", "StandardValue"];

const KNOWN_SUFFIX_EXCEPTIONS: &[&str] = &[
    "IndustriesManufacturingSettings",
    "IntegrationHubSettings",
    "ActionableEventOrchDef",
    "ActionableEventTypeDef",
    "ObjectHierarchyRelationship",
    "DataSource",
    "CustomLabel",
    "CustomLabels",
    "MatchingRules",
    "MatchingRule",
    "DigitalExperienceBundle",
];

fn validation_registries() -> Vec<(String, MetadataRegistry)> {
    let base = default_registry().clone();

    // Matches SDR `test/registry/presetTesting.ts`: excludes CustomLabelsBeta (conflicts with Beta2)
    // and decomposePermissionSetBeta2 (multiple children share .objectSettings suffix).
    let presets: Vec<(Preset, MetadataRegistry)> = Preset::ALL
        .iter()
        .copied()
        .filter(|p| {
            !matches!(
                p,
                Preset::DecomposeCustomLabelsBeta | Preset::DecomposePermissionSetBeta2
            )
        })
        .map(|p| {
            let patch = load_preset(p).expect("preset loads");
            (p, patch_to_registry(&patch))
        })
        .collect();

    let mut out = vec![("default".to_owned(), base.clone())];
    for (preset, reg) in &presets {
        out.push((preset.to_string(), reg.clone()));
    }
    for (preset, _) in &presets {
        let patch = load_preset(*preset).expect("preset loads");
        out.push((
            format!("default merged with {preset}"),
            first_level_merge(base.clone(), &patch),
        ));
    }

    let mut all_merged = base;
    for (preset, _) in &presets {
        let patch = load_preset(*preset).expect("preset loads");
        all_merged = first_level_merge(all_merged, &patch);
    }
    out.push(("all presets combined".to_owned(), all_merged));
    out
}

fn validate_registry(label: &str, registry: &MetadataRegistry) {
    validate_unsupported_types(label, registry);
    validate_children(label, registry);
    validate_aliases_and_suffixes(label, registry);
    validate_strict_dirs(label, registry);
    validate_ids_and_strategies(label, registry);
    validate_folders_and_xml_suffix(label, registry);
}

fn validate_unsupported_types(label: &str, registry: &MetadataRegistry) {
    for ty in registry.types.values() {
        assert!(
            !UNSUPPORTED_TYPES.contains(&ty.name.as_str()),
            "[{label}] {} should not be in UNSUPPORTED_TYPES",
            ty.name
        );
        if let Some(children) = &ty.children {
            for child in children.types.values() {
                assert!(
                    !UNSUPPORTED_TYPES.contains(&child.name.as_str()),
                    "[{label}] child {} should not be in UNSUPPORTED_TYPES",
                    child.name
                );
            }
        }
    }
}

fn validate_children(label: &str, registry: &MetadataRegistry) {
    let mut expected_child_map = HashMap::new();
    for parent in registry.types.values() {
        let Some(children) = &parent.children else {
            continue;
        };
        let is_folder_per_type = parent.strategies.as_ref().and_then(|s| s.decomposition)
            == Some(DecompositionStrategy::FolderPerType);
        if is_folder_per_type {
            assert!(
                children.directories.is_some(),
                "[{label}] {} uses folderPerType and must define children.directories",
                parent.name
            );
        }
        for child in children.types.values() {
            assert!(
                !registry.types.contains_key(&child.id),
                "[{label}] child {} should not be a top-level type",
                child.name
            );
            let suffix = child
                .suffix
                .as_deref()
                .unwrap_or_else(|| panic!("[{label}] child {} missing suffix", child.name));
            if is_folder_per_type {
                let dirs = children.directories.as_ref().expect("checked above");
                assert_eq!(
                    dirs.get(&child.directory_name).map(String::as_str),
                    Some(child.id.as_str()),
                    "[{label}] child directory mismatch for {}",
                    child.id
                );
            }
            assert_eq!(
                children.suffixes.get(suffix).map(String::as_str),
                Some(child.id.as_str()),
                "[{label}] child suffix mismatch for {}",
                child.id
            );
            expected_child_map.insert(child.id.clone(), parent.id.clone());
        }
    }

    for (child_id, parent_id) in &expected_child_map {
        assert_eq!(
            registry.child_types.get(child_id),
            Some(parent_id),
            "[{label}] childTypes missing {child_id} -> {parent_id}"
        );
    }

    for (child_id, parent_id) in &registry.child_types {
        if parent_id.is_empty() {
            continue;
        }
        let parent = registry
            .types
            .get(parent_id)
            .unwrap_or_else(|| panic!("[{label}] childTypes parent {parent_id} missing"));
        let children = parent
            .children
            .as_ref()
            .unwrap_or_else(|| panic!("[{label}] parent {parent_id} missing children"));
        assert!(
            children.types.contains_key(child_id),
            "[{label}] parent {parent_id} missing child {child_id}"
        );
    }
}

fn validate_aliases_and_suffixes(label: &str, registry: &MetadataRegistry) {
    for ty in registry.types.values() {
        if let Some(target) = &ty.alias_for {
            assert!(
                registry.types.contains_key(target),
                "[{label}] {} alias target {target} missing",
                ty.name
            );
        }
    }

    for (suffix, type_id) in &registry.suffixes {
        if let Some(parent_id) = registry.child_types.get(type_id).filter(|p| !p.is_empty()) {
            let child = &registry.types[parent_id]
                .children
                .as_ref()
                .expect("children")
                .types[type_id];
            assert_eq!(
                child.suffix.as_deref(),
                Some(suffix.as_str()),
                "[{label}] suffix {suffix} mismatch for child {type_id}"
            );
        } else {
            let ty = &registry.types[type_id];
            if let Some(legacy) = &ty.legacy_suffix {
                assert!(
                    legacy == suffix || ty.suffix.as_deref() == Some(suffix.as_str()),
                    "[{label}] suffix {suffix} not in legacy/suffix of {type_id}"
                );
            } else {
                assert_eq!(
                    ty.suffix.as_deref(),
                    Some(suffix.as_str()),
                    "[{label}] suffix {suffix} mismatch for {type_id}"
                );
            }
        }
    }

    for ty in registry.types.values() {
        if let Some(suffix) = &ty.suffix
            && ty.alias_for.is_none()
            && !ty.strict_directory()
            && !KNOWN_SUFFIX_EXCEPTIONS.contains(&ty.name.as_str())
        {
            assert_eq!(
                registry.suffixes.get(suffix).map(String::as_str),
                Some(ty.id.as_str()),
                "[{label}] missing suffix entry {suffix} -> {}",
                ty.id
            );
            if let Some(children) = &ty.children {
                for child in children.types.values() {
                    let child_suffix = child.suffix.as_deref().expect("child suffix");
                    assert_eq!(
                        registry.suffixes.get(child_suffix).map(String::as_str),
                        Some(child.id.as_str()),
                        "[{label}] missing child suffix entry {child_suffix} -> {}",
                        child.id
                    );
                }
            }
        }
    }

    let mut by_suffix: HashMap<&str, Vec<&MetadataType>> = HashMap::new();
    for ty in registry.types.values() {
        if let Some(suffix) = &ty.suffix {
            by_suffix.entry(suffix.as_str()).or_default().push(ty);
        }
    }
    for (suffix, types) in by_suffix {
        if types.len() > 1 {
            let non_strict: Vec<_> = types.iter().filter(|t| !t.strict_directory()).collect();
            assert!(
                non_strict.len() < 2,
                "[{label}] suffix {suffix} has multiple non-strict types: {:?}",
                non_strict.iter().map(|t| &t.name).collect::<Vec<_>>()
            );
        }
    }
}

fn validate_strict_dirs(label: &str, registry: &MetadataRegistry) {
    for ty in registry.types.values() {
        if ty.strict_directory() {
            assert_eq!(
                registry
                    .strict_directory_names
                    .get(&ty.directory_name)
                    .map(String::as_str),
                Some(ty.id.as_str()),
                "[{label}] missing strictDirectoryNames for {}",
                ty.directory_name
            );
        }
    }
    for (dir_name, type_id) in &registry.strict_directory_names {
        let ty = &registry.types[type_id];
        assert_eq!(
            &ty.directory_name, dir_name,
            "[{label}] strictDirectoryNames {dir_name} mismatch"
        );
        assert_eq!(
            ty.strict_directory_name,
            Some(true),
            "[{label}] strictDirectoryNames {dir_name} type not marked strict"
        );
    }
}

fn validate_ids_and_strategies(label: &str, registry: &MetadataRegistry) {
    for (key, ty) in &registry.types {
        assert_eq!(&ty.id, key, "[{label}] id != map key");
        assert_eq!(
            ty.id,
            ty.name.to_ascii_lowercase(),
            "[{label}] id != lowercased name"
        );
        assert!(!ty.name.is_empty(), "[{label}] empty name on {}", ty.id);
        assert!(
            !ty.directory_name.is_empty(),
            "[{label}] empty directoryName on {}",
            ty.id
        );
        if let Some(children) = &ty.children {
            for (child_key, child) in &children.types {
                assert_eq!(&child.id, child_key, "[{label}] child id != map key");
            }
        }
    }

    for ty in registry.types.values() {
        let Some(strategies) = &ty.strategies else {
            continue;
        };
        assert_ne!(
            strategies.transformer,
            Some(TransformerStrategy::Standard),
            "[{label}] {} explicitly specifies standard transformer",
            ty.id
        );
        match strategies.adapter {
            AdapterStrategy::MatchingContentFile | AdapterStrategy::Bundle => {
                assert!(strategies.decomposition.is_none(), "[{label}] {}", ty.id);
                assert!(strategies.recomposition.is_none(), "[{label}] {}", ty.id);
                assert!(strategies.transformer.is_none(), "[{label}] {}", ty.id);
            }
            AdapterStrategy::Decomposed => {
                assert!(strategies.decomposition.is_some(), "[{label}] {}", ty.id);
                assert!(strategies.transformer.is_some(), "[{label}] {}", ty.id);
                assert!(strategies.recomposition.is_none(), "[{label}] {}", ty.id);
            }
            AdapterStrategy::MixedContent => {
                assert!(strategies.decomposition.is_none(), "[{label}] {}", ty.id);
                assert!(strategies.recomposition.is_none(), "[{label}] {}", ty.id);
            }
            _ => {}
        }
        if strategies.transformer == Some(TransformerStrategy::NonDecomposed) {
            let count = ty.children.as_ref().map_or(0, |c| c.types.len());
            assert_eq!(count, 1, "[{label}] nonDecomposed {} child count", ty.id);
        }
    }
}

fn validate_folders_and_xml_suffix(label: &str, registry: &MetadataRegistry) {
    for ty in registry.types.values() {
        if ty.stored_in_folder() {
            let folder_type = ty
                .folder_type
                .as_deref()
                .unwrap_or_else(|| panic!("[{label}] {} inFolder missing folderType", ty.name));
            assert!(
                registry.types.contains_key(folder_type),
                "[{label}] {} folderType {folder_type} missing",
                ty.name
            );
        }
        if ty.suffix.as_deref() == Some(".xml") {
            assert_eq!(
                ty.strict_directory_name,
                Some(true),
                "[{label}] {} has .xml suffix without strictDirectoryName",
                ty.name
            );
        }
    }
}

#[test]
fn registry_validation_base_and_presets() {
    for (label, registry) in validation_registries() {
        validate_registry(&label, &registry);
    }
}

#[test]
fn all_seven_presets_load_and_merge() {
    for preset in Preset::ALL {
        let patch = load_preset(*preset).expect("preset deserializes");
        let base = default_registry().clone();
        let merged = first_level_merge(base, &patch);
        assert!(!merged.types.is_empty());
    }
    let sv = standard_value_set_registry();
    assert!(!sv.fullnames.is_empty());
}

#[test]
fn all_snapshot_sfdx_project_configs_load() {
    let snapshots_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/sdr-snapshots");
    let mut count = 0usize;
    for entry in fs::read_dir(&snapshots_dir).expect("read snapshots dir") {
        let entry = entry.expect("dir entry");
        let path: PathBuf = entry.path();
        if path.is_dir() && path.join("sfdx-project.json").is_file() {
            let loaded = SfdxProjectConfig::find_and_load(&path).expect("parse sfdx-project.json");
            assert!(loaded.is_some(), "missing config in {}", path.display());
            let effective = effective_registry(Some(&path)).expect("build effective registry");
            assert!(effective.types.len() >= 540);
            count += 1;
        }
    }
    assert!(
        count >= 15,
        "expected snapshot projects with sfdx-project.json, found {count}"
    );
}

#[test]
fn registry_access_queries_and_rejects_traversal() {
    let access = RegistryAccess::new();
    let custom_obj = access
        .type_by_name("CustomObject")
        .expect("CustomObject exists");
    assert_eq!(custom_obj.directory_name, "objects");

    let custom_field = access
        .type_by_name("CustomField")
        .expect("CustomField exists");
    assert_eq!(custom_field.directory_name, "fields");
    assert_eq!(
        access.parent_type("CustomField").map(|t| t.id.as_str()),
        Some("customobject")
    );

    let email_folder = access
        .type_by_name("EmailTemplateFolder")
        .expect("EmailTemplateFolder alias exists");
    assert_eq!(email_folder.id, "emailfolder");

    let by_suffix = access.type_by_suffix("cls").expect("cls suffix exists");
    assert_eq!(by_suffix.name, "ApexClass");

    let err = access
        .type_by_name("ApexClas")
        .expect_err("typo returns error");
    match err {
        RegistryError::MissingTypeDefinition { suggestions, .. } => {
            assert!(suggestions.iter().any(|s| s == "ApexClass"));
        }
        other => panic!("unexpected error: {other:?}"),
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    fs::write(
        tmp.path().join("sfdx-project.json"),
        r#"{
            "packageDirectories": [{"path": "force-app", "default": true}],
            "registryCustomizations": {
                "types": {
                    "evil": {
                        "id": "evil",
                        "name": "Evil",
                        "directoryName": "../escape"
                    }
                }
            }
        }"#,
    )
    .expect("write sfdx-project.json");

    let err = effective_registry(Some(tmp.path())).expect_err("path traversal rejected");
    assert!(matches!(err, RegistryError::PathTraversal { .. }));
}
