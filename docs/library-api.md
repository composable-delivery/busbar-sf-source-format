---
title: Library API Guide
description: Public Rust API overview across sf-registry, sf-tree, sf-xml, and sf-source-format.
---

# Library API Guide

The workspace publishes two crates: the core library [`busbar-sf-source-format`](../crates/source-format/Cargo.toml) (which bundles the `registry`, `tree`, `xml`, `resolve`, `manifest`, and `convert` modules) and [`busbar-sf-source-format-cli`](../crates/cli/Cargo.toml).

## `registry`: Metadata Types & Presets

[`RegistryAccess`](../crates/source-format/src/registry/access.rs) provides cheap, `Arc`-backed lookups over the 543+ embedded Salesforce metadata definitions and source-behavior presets.

- `RegistryAccess::new()` returns a cached handle to the default embedded registry in nanoseconds.
- `RegistryAccess::from_project_dir(path)` locates `sfdx-project.json` in or above `path` and layers any `sourceBehaviorOptions` presets and `registryCustomizations`.
- `RegistryAccess::from_project_config(&config)` builds a registry from an in-memory `SfdxProjectConfig`.
- `type_by_name`, `type_by_suffix`, `guess_type_by_suffix`, `strict_folder_types`, and `parent_type` expose typed `MetadataType` metadata.

All 7 upstream presets are modeled via the `Preset` enum:

- `DecomposeCustomLabelsBeta` / `DecomposeCustomLabelsBeta2`
- `DecomposeExternalServiceRegistrationBeta`
- `DecomposePermissionSetBeta` / `DecomposePermissionSetBeta2`
- `DecomposeSharingRulesBeta`
- `DecomposeWorkflowBeta`

## `tree`: Tree Containers, `.forceignore`, and Path Safety

[`TreeContainer`](../crates/source-format/src/tree/tree.rs) abstracts file access across:

- `FsTree` — local filesystem rooted at a working directory
- `ZipTree::from_bytes(&bytes)` — in-memory read-only ZIP archive container
- `MemoryTree` — mutable in-memory tree useful for tests and virtual transforms

Supporting utilities:

- `ForceIgnore::find_and_create(seed)` and `ForceIgnore::from_file(path)` evaluate `.forceignore` patterns together with SDR's `DEFAULT_IGNORE_RULES`.
- `busbar_sf_source_format::tree::safety` (re-exported under `tree`) provides `normalize_lexical`, `ensure_within_root`, `ensure_no_symlink_on_path`, and `ensure_safe_directory_name`.

## `xml`: Lossless Ordered XML DOM

[`busbar_sf_source_format::xml`](../crates/source-format/src/xml/mod.rs) models XML documents as ordered node trees (`XmlDocument`, `XmlElement`, `XmlNode`) instead of key-sorted maps:

- `parse_xml(input)` parses XML into an `XmlDocument` via `quick-xml`, preserving declaration, element order, comments, CDATA, and attributes.
- `XmlDocument::to_xml_string()` serializes with 4-space indentation, explicit closing tags (`<tag></tag>`), and SDR entity/comment parity (`correct_comments`, `handle_special_entities`).
- `semantic_eq(&left, &right)` compares two documents semantically while ignoring sibling ordering differences.

## `resolve`, `manifest`, and `convert`

### Component Resolution (`busbar_sf_source_format::resolve`)

`ComponentSetBuilder::build(&ComponentSetOptions { .. })` resolves components from any combination of:

- `sourcepath` (`Vec<PathBuf>`)
- `manifest` (`ManifestOptions` for `package.xml`, `destructiveChangesPre.xml`, `destructiveChangesPost.xml`)
- `metadata` (`MetadataOptions` for `Type:FullName` expressions and exclusions)
- `project_dir` (`Option<PathBuf>`) for `sfdx-project.json` and `.forceignore`

Lower-level entry points include `ComponentSet::from_source`, `ComponentSet::from_source_with_tree`, `ComponentSet::from_manifest`, and `MetadataResolver`.

### Manifests (`busbar_sf_source_format::manifest`)

- `parse_manifest(contents, source_label)` parses `package.xml` and `destructiveChanges*.xml` into `PackageManifest`.
- `render_manifest_xml(&manifest)` renders a `PackageManifest` with SDR-compatible formatting.
- `ManifestPurpose` (`Standard`, `Deploy`, `Retrieve`) controls how empty decomposed parents are represented.

### Format Conversion (`busbar_sf_source_format::convert`)

`MetadataConverter::new(registry).convert(&components, target_format, output)` converts a `ComponentSet`:

- `TargetFormat::Source` — MDAPI to SFDX source format
- `TargetFormat::Metadata` — SFDX source format to MDAPI
- `ConvertOutput::Directory { output_dir, package_name, unique_dir }` — write loose files to disk
- `ConvertOutput::Zip { output_dir, package_name, unique_dir }` — write a `.zip` archive to disk or return in-memory `zip_bytes` when `output_dir` is `None`
- `ConvertOutput::Merge { merge_with, default_dir, project_dir }` — merge SFDX output into existing source trees

When generating full Rustdoc HTML (`cargo doc --workspace --no-deps --all-features`), complete symbol documentation is published under `/api/`.

