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

## Optimization history

Two waves of optimization took the engine from slower than SDR on some workloads to the speedups on the [Benchmarks](../BENCHMARKS.md) page. The figures below were measured on an Apple M2 Max at the time; current results are always on the Benchmarks page.

### Wave 1: Algorithmic Complexity, `Arc` Sharing & Initial Parallelism
1. **`O(N²)` -> `O(N)` `NonDecomposed` (`CustomLabels`) recomposition** ([finalizers.rs](crates/source-format/src/convert/finalizers.rs)): Indexed each parsed parent XML document once in a single pass instead of cloning the full parent `XmlDocument` and linearly scanning for each child, reused a thread-local `feruca::Collator` with pre-extracted sort keys, and parallelized recomposition entries with `rayon`.
2. **`Arc`-shared registries & `.forceignore` + indexed strict-folder lookup** ([access.rs](crates/source-format/src/registry/access.rs), [variants.rs](crates/source-format/src/registry/variants.rs), [force_ignore.rs](crates/source-format/src/tree/force_ignore.rs), [resolver.rs](crates/source-format/src/resolve/resolver.rs)): Eliminated deep copies of the 543-type `MetadataRegistry` and `Gitignore` matchers across components, fast-pathed `from_project_dir` when no presets/customizations exist, and indexed `strictDirectoryNames` by `directoryName` without per-file `Vec` allocations.
3. **Single-buffer XML serialization & `FirstOnly` scope** ([serialize.rs](crates/source-format/src/xml/serialize.rs), [types.rs](crates/source-format/src/convert/types.rs), [converter.rs](crates/source-format/src/convert/converter.rs)): Serialized into a single pre-sized buffer and matched SDR's `FirstOnly` post-write re-resolution scope.

### Wave 2: Syscall Reduction, Bounded I/O Pool & Parallel ZIP DEFLATE
1. **Bounded filesystem I/O pool** ([io_pool.rs](crates/source-format/src/io_pool.rs)): Separated disk I/O onto a bounded Rayon pool (`min(cores, 4)` on macOS) to eliminate APFS VFS/vnode lock contention observed when 12 threads hammered `open`/`write`/`stat` concurrently, while leaving pure CPU work on the global pool.
2. **Syscall & `.forceignore` elimination in `tree` and `StandardWriter`** ([fs_tree.rs](crates/source-format/src/tree/fs_tree.rs), [force_ignore.rs](crates/source-format/src/tree/force_ignore.rs), [tree.rs](crates/source-format/src/tree/tree.rs), [writer.rs](crates/source-format/src/convert/writer.rs)):
   - Replaced per-entry `is_directory()` calls with `DirEntry::file_type()` (`DirEntryInfo`) and added read-through `FsTree::new_cached()` for `ComponentSetBuilder::build` ([component_set.rs](crates/source-format/src/resolve/component_set.rs)) and post-write re-resolution.
   - Removed the upfront `resolved.is_dir()` `stat()` before `fs::read()` in `FsTree::read_file`.
   - Added zero-`stat` `ForceIgnore::denies_with_kind(PathKindHint)` and cached parsed `ForceIgnore` instances by discovered `.forceignore` path in `resolve_converted_paths_parallel`.
   - Replaced per-file `lstat()` + `fs::write()` with Unix `O_NOFOLLOW` writes and used `fs::copy` (APFS copy-on-write) for unchanged `FsTree` files targeting fresh empty output directories, plus parallelized directory creation.
3. **Parallel ZIP DEFLATE** ([writer.rs](crates/source-format/src/convert/writer.rs), [transformers.rs](crates/source-format/src/convert/transformers.rs)): Compressed entries concurrently across all CPU cores using `zip::PreparedZipFile::from_data` in both `ArchiveWriter::finish` and `zip_expanded_static_resource`.

| Metric | Initial Baseline | Wave 1 | Wave 2 | Total Improvement |
| --- | ---: | ---: | ---: | ---: |
| `registry_new/default_embedded` (Criterion) | 176.45 µs | 5.51 ns | **5.58 ns** | **~31,600x** |
| `customLabels-multiple` (SFDX -> MDAPI dir) | 2,447.02 ms (`0.25x`) | 52.91 ms (`9.47x`) | **55.56 ms (`9.70x`)** | **44.0x** |
| `large-org` (MDAPI -> SFDX, `7,451` files) | 1,376.99 ms (`1.79x`) | 595.18 ms (`2.94x`) | **391.39 ms (`4.52x`)** | **3.5x** |
| `large-org` (SFDX -> MDAPI dir) | 2,188.44 ms (`0.60x`) | 310.53 ms (`3.33x`) | **186.34 ms (`5.19x`)** | **11.7x** |
| `large-org` (SFDX -> MDAPI zip) | 2,093.07 ms (`0.60x`) | 204.98 ms (`5.52x`) | **131.77 ms (`7.21x`)** | **15.9x** |
| `npsp` (MDAPI -> SFDX, `12,063` files) | — | 1,454.10 ms (`2.37x`) | **801.98 ms (`4.55x`)** | **1.81x vs W1** |
| `npsp` (SFDX -> MDAPI dir, `3,927` files) | — | 957.36 ms (`2.65x`) | **530.12 ms (`5.92x`)** | **1.81x vs W1** |
| `npsp` (SFDX -> MDAPI zip, ~9.2 MB) | — | 916.85 ms (`3.12x`) | **374.18 ms (`8.07x`)** | **2.45x vs W1** |
| `sdr_snapshots` full suite (`cargo test`, debug) | 8.56 s | 0.44 s | **0.30 s** | **28.5x** |
