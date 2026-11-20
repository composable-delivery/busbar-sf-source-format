---
title: Architecture
description: Workspace architecture, resolution adapters, conversion pipeline, collation, and safety model.
---

# Architecture

`busbar-sf-source-format` separates registry metadata, virtual filesystem/safety primitives, lossless XML serialization, and conversion logic into focused internal modules, paired with the `busbar-sf-source-format-cli` binary crate.

## Module & Crate Layout

```text
busbar-sf-source-format-cli (`sf-source-format` binary)
  └── busbar-sf-source-format
        ├── registry (543+ types, 7 presets, sfdx-project.json)
        ├── tree (FsTree, ZipTree, MemoryTree, .forceignore, safety)
        ├── xml (ordered XmlDocument DOM & parity serializer)
        ├── resolve (MetadataResolver, ComponentSetBuilder)
        ├── manifest (package.xml & destructiveChanges*.xml)
        └── convert (MetadataConverter, transformers, finalizers, writers)
```

- `registry`, `tree`, and `xml` have no dependencies on `resolve` or `convert` and are exported directly from `busbar_sf_source_format`.
- `resolve` and `convert` orchestrate resolution, manifest generation, transformation, finalization, and output writing across them.

## Resolution Pipeline (`busbar_sf_source_format::resolve`)

1. **Registry & `.forceignore` Loading**: `RegistryAccess::from_project_dir` merges any `sfdx-project.json` presets and `registryCustomizations` with the embedded `metadataRegistry.json`. `ForceIgnore::find_and_create` loads `.forceignore` alongside SDR's default ignore rules.
2. **Adapter Dispatch**: `MetadataResolver` inspects file paths, strict folder directories (indexed by `directoryName` in `RegistryAccess`), and suffixes to dispatch across eight `AdapterStrategy` modes:
   - `Default`
   - `MatchingContentFile`
   - `MixedContent`
   - `Bundle`
   - `Decomposed`
   - `DigitalExperience`
   - `UiBundles`
   - `PartiallyDecomposed`
3. **`ComponentSet` Indexing**: `ComponentSet` stores resolved `SourceComponent` entries and manifest-only entries in deterministic `IndexMap` structures, tracking destructive pre/post members and API versions.

## Conversion Pipeline (`busbar_sf_source_format::convert`)

1. **String Replacements**: When converting to `TargetFormat::Metadata` for ZIP deploys (or when `SF_APPLY_REPLACEMENTS_ON_CONVERT=true`), `mark_replacements` attaches compiled `regex`/`globset` rules from `sfdx-project.json`.
2. **Parallel Transformation**: `rayon` parallelizes component transforms across `TransformerStrategy` implementations (`Standard`, `Decomposed`, `NonDecomposed`, `StaticResource`, `Profile`, `UiBundle`, `DecomposedLabels`, `DecomposedPermissionSet`, `DecomposeExternalServiceRegistration`).
3. **Finalization**:
   - **Decomposition (`MDAPI -> SFDX`)**: Splits multi-record XML files (such as `CustomObject` fields/validation rules, `CustomLabels`, `Workflow`, `SharingRules`, `PermissionSet` variants, and `ExternalServiceRegistration` OpenAPI schemas) into individual source files.
   - **Recomposition (`SFDX -> MDAPI`)**: Single-pass indexed parent XML assembly using thread-local `feruca::Collator` (non-ignorable UCA punctuation weights) to match Node/ICU `String.prototype.localeCompare` ordering identically.
4. **Output Writers**:
   - `StandardWriter` writes files and directories on disk, normalizing roots once and validating every parent directory against symlink traversal.
   - `ArchiveWriter` writes deterministic ZIP entries in memory or on disk.

## Safety Model

- **No Boolean Parameter Ambiguity**: Workspace `clippy.toml` (`max-fn-params-bools = 0`) enforces explicit enums (`TargetFormat`, `UniqueDirPolicy`, `ForceIgnorePolicy`, `ManifestPurpose`, `ResolveMode`) at API boundaries.
- **Root Containment & Symlink Rejection**: `busbar_sf_source_format::tree` safety guards validate registry `directoryName` segments, ZIP entry paths, `UIBundle` descriptor paths, and every write target with `normalize_lexical`, `ensure_within_root`, and `ensure_no_symlink_on_path`.
- **Deploy-Time `UIBundle` Validation**: `validate_ui_bundle_for_deploy` enforces `ui-bundle.json` and `mcp-resources.json` schema constraints when targeting `TargetFormat::Metadata` while bypassing them on retrieve (`TargetFormat::Source`).

