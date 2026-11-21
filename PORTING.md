# Porting Notes: `@salesforce/source-deploy-retrieve` → Rust (`sf-source-format`)

This workspace is an idiomatic Rust port of the MDAPI ↔ SFDX source format resolution, decomposition, recomposition, and manifest engine from Salesforce CLI's [`@salesforce/source-deploy-retrieve`](https://github.com/forcedotcom/source-deploy-retrieve) (SDR).

## Upstream Pin

Recorded in [fixtures/sdr-snapshots/UPSTREAM_VERSION](fixtures/sdr-snapshots/UPSTREAM_VERSION):

- **Package**: `@salesforce/source-deploy-retrieve` `13.13.0`
- **Repository**: `https://github.com/forcedotcom/source-deploy-retrieve.git`
- **Commit**: `a2d677f6daccde1af5d35e6bb2fc0867c67db57b`

## Workspace Architecture

| Crate / Module | Purpose | Upstream Equivalent |
| --- | --- | --- |
| `busbar-sf-source-format::registry` | Typed metadata registry (`543+` types, `metadataRegistry.json`, `stdValueSetRegistry.json`), all 7 source-behavior presets, and `sfdx-project.json` `sourceBehaviorOptions` / `registryCustomizations` / `replacements` layering | `src/registry/` |
| `busbar-sf-source-format::xml` | Order-preserving XML DOM (`XmlDocument`, `XmlElement`, `XmlNode`), `quick-xml` parser, SDR-compatible serializer (4-space indent, explicit close tags `<tag></tag>`, inline comment preservation, numeric entity unescaping, `semantic_eq`) | `src/convert/streams.ts` (`JsToXml`) & `fast-xml-parser` helpers |
| `busbar-sf-source-format::tree` | `TreeContainer` abstraction (`FsTree`, `ZipTree`, `MemoryTree`), `.forceignore` (`ignore` crate with `DEFAULT_IGNORE_RULES`), lexical normalization, root containment checks, and symlink-traversal rejection (`ensure_no_symlink_on_path`) | `src/resolve/treeContainers.ts` & `src/resolve/forceIgnore.ts` |
| `busbar-sf-source-format` (`resolve`, `manifest`, `convert`) | Component resolution (`MetadataResolver`), `ComponentSet` / `ComponentSetBuilder`, `package.xml` & `destructiveChanges*.xml` parsing/generation, replacement rules, `UIBundle` deploy validation, all adapters/transformers/finalizers, `StandardWriter`, and `ArchiveWriter` | `src/collections/`, `src/resolve/`, `src/convert/` |
| `busbar-sf-source-format-cli` | Binary (`sf-source-format`) exposing `convert mdapi` and `convert source` (directory, `--zip`, `--merge-with`, `--manifest`, `--metadata`) | `sf project convert mdapi` / `sf project convert source` |

## TypeScript → Rust Mapping

### Resolution & Adapters (`crates/source-format/src/resolve/`)

| Upstream TypeScript | Rust Implementation |
| --- | --- |
| `MetadataResolver` | `MetadataResolver` (`resolver.rs`) |
| `ComponentSet` / `ComponentSetBuilder` | `ComponentSet` / `ComponentSetBuilder` (`component_set.rs`) |
| `SourceComponent` | `SourceComponent` (`component.rs`) |
| `DefaultSourceAdapter` | `AdapterStrategy::Default` in `resolve_with_adapter` (`adapters.rs`) |
| `MatchingContentSourceAdapter` | `AdapterStrategy::MatchingContentFile` (`adapters.rs`) |
| `MixedContentSourceAdapter` | `AdapterStrategy::MixedContent` (`adapters.rs`) |
| `BundleSourceAdapter` | `AdapterStrategy::Bundle` (`adapters.rs`) |
| `DecomposedSourceAdapter` | `AdapterStrategy::Decomposed` (`adapters.rs`) |
| `DigitalExperienceSourceAdapter` | `AdapterStrategy::DigitalExperience` (`adapters.rs`) |
| `UiBundlesSourceAdapter` | `AdapterStrategy::UiBundles` (`adapters.rs`) |
| Partial / custom ESR adapter (`partiallyDecomposed`) | `AdapterStrategy::PartiallyDecomposed` (`adapters.rs`) |

### Transformers & Finalizers (`crates/source-format/src/convert/`)

| Upstream TypeScript | Rust Implementation |
| --- | --- |
| `MetadataConverter` | `MetadataConverter` (`converter.rs`) |
| `DefaultMetadataTransformer` | `TransformerStrategy::Standard` (`transformers.rs`) |
| `DecomposedMetadataTransformer` + `DecompositionFinalizer` / `RecompositionFinalizer` | `TransformerStrategy::Decomposed` (`transformers.rs`) + `DecompositionFinalizer` / `RecompositionFinalizer` (`finalizers.rs`) |
| `NonDecomposedMetadataTransformer` + `NonDecompositionFinalizer` | `TransformerStrategy::NonDecomposed` (`transformers.rs`) + `NonDecompositionFinalizer` (`finalizers.rs`) |
| `StaticResourceMetadataTransformer` | `TransformerStrategy::StaticResource` (`transformers.rs`) |
| `ProfileMetadataTransformer` + `ProfileMetadataFinalizer` | `TransformerStrategy::Profile` (`transformers.rs`) + `ProfileMetadataFinalizer` (`finalizers.rs`) |
| `UiBundleMetadataTransformer` + `uiBundleValidation.ts` | `TransformerStrategy::UiBundle` (`transformers.rs`) + `validate_ui_bundle_for_deploy` (`ui_bundle.rs`) |
| `LabelsMetadataTransformer` / `LabelMetadataTransformer` + `DecomposedLabelsFinalizer` | `TransformerStrategy::DecomposedLabels` (`transformers.rs`) + `DecomposedLabelsFinalizer` (`finalizers.rs`) |
| `DecomposedPermissionSetTransformer` + `DecomposedPermissionSetFinalizer` | `TransformerStrategy::DecomposedPermissionSet` (`transformers.rs`) + `DecomposedPermissionSetFinalizer` (`finalizers.rs`) |
| `DecomposeExternalServiceRegistrationTransformer` + `DecomposedExternalServiceRegistrationFinalizer` | `TransformerStrategy::DecomposeExternalServiceRegistration` (`transformers.rs`) + `DecomposedExternalServiceRegistrationFinalizer` (`finalizers.rs`) |
| `StandardWriter` / `ZipWriter` | `StandardWriter` / `ArchiveWriter` (`writer.rs`) |
| `stringReplacements` (`src/convert/replacements.ts`) | `mark_replacements` / `apply_replacements` (`replacements.rs`) |

## Idiomatic Rust Design Decisions & Workspace Standards

- **Zero boolean parameters & explicit enums**: Enforced via `clippy.toml` (`max-fn-params-bools = 0`) and workspace lints (`fn_params_excessive_bools = "warn"`). Options use typed enums (`TargetFormat`, `UniqueDirPolicy`, `ManifestPurpose`, `DestructiveChangesKind`, `ForceIgnorePolicy`, `ResolveMode`, `ValidateSingletons`).
- **Typed error boundaries**: Every crate uses `thiserror` (`RegistryError`, `XmlError`, `TreeError`, `ResolveError`, `ManifestError`, `ConvertError`) with `unwrap_used = "warn"` and `as_conversions = "warn"` enabled workspace-wide.
- **Order-preserving XML & JSON**: Uses `XmlElement` node lists instead of JS object key iteration bugs and `serde_json` (`preserve_order` via `IndexMap`) so OpenAPI schemas converted between JSON and YAML in `preset-decomposedESR` preserve original key ordering.
- **Locale-independent UCA collation**: Recomposition (`CustomObject`, `CustomLabels`, etc.) uses `feruca::Collator::new(Tailoring::default(), false, true)` (non-ignorable punctuation weights) to match Node/ICU `String.prototype.localeCompare` ordering on underscore/mixed-case identifiers (e.g., `Zip_Not_Found` before `Zipcode_Verification_Limit`, `Addr_Id_Error` before `addrCiceroMissingAPIKey`).
- **Path traversal & symlink safety**: `busbar_sf_source_format::tree` safety guards validate registry `directoryName` segments, ZIP archive entry paths, `UIBundle` JSON descriptors, and output destinations via `normalize_lexical`, `ensure_within_root`, and `ensure_no_symlink_on_path`.

## Parity & Test Coverage

1. **`registry` (`crates/source-format/tests/registry_validation.rs`, 4 tests)**:
   - Ports `test/registry/registryValidation.test.ts` across the base registry, presets, and merged overlays.
   - Verifies all 7 preset files (`decomposeCustomLabelsBeta.json`, `decomposeCustomLabelsBeta2.json`, `decomposeExternalServiceRegistrationBeta.json`, `decomposePermissionSetBeta.json`, `decomposePermissionSetBeta2.json`, `decomposeSharingRulesBeta.json`, `decomposeWorkflowBeta.json`) and loads every `sfdx-project.json` across `fixtures/sdr-snapshots/`.
2. **`xml` (`crates/source-format/tests/xml_roundtrip.rs`, 7 tests)** & **`tree` (`crates/source-format/tests/tree_and_ignore.rs`, 4 tests)**:
   - Round-trip fidelity for comments, CDATA, numeric entities, leading-zero numbers, formula quotes, `.forceignore`, `ZipTree`, and path/symlink safety checks.
3. **`busbar-sf-source-format` SDR Snapshot Suite (`crates/source-format/tests/sdr_snapshots.rs`, 34 tests)**:
   - Covers all **33** sample project directories under `fixtures/sdr-snapshots/` (`aiAgentDefinitionVersion`, `appTemplates`, `bots`, `customLabels-merged`, `customLabels-multiple`, `customLabels-simple`, `customObjects-and-children` [2 tests: round-trip + Retrieve vs Deploy empty parent manifests], `customTranslationProject`, `destructiveCompSets`, `digitalExperienceBundle`, `digitalExperienceMobilePages`, `forceignore`, `genAiFunctions`, `legacySuffixSupport`, `mpdWithLabels`, `nestedFolders`, `preset-PermSet`, `preset-decomposeLabels`, `preset-decomposedESR`, `preset-decomposedPS2`, `preset-sharingRules`, `preset-workflow`, `preset-workflow-mpd`, `sharingRules`, `singleCustomFieldRetrieve`, `staticResource`, `territory`, `UIBundleSnapshot`, `variant-raw-PermSet`, `variant-workflow`, `waveTemplateBundle`, `workflow`, `xmlComments`).
4. **`UIBundle` deploy validation (`crates/source-format/tests/ui_bundle_validation.rs`, 5 tests)**:
   - Verifies deploy-time rejection of empty `ui-bundle.json`, unknown properties, path traversal, and invalid `mcp-resources.json`, plus bypassing validation during retrieve (`TargetFormat::Source`).
5. **Incremental cache (`crates/source-format/tests/incremental_cache.rs`, 11 tests)**:
   - Exercises persistent `blake3` directory and ZIP incremental conversion, self-healing, manifest pruning, and env invalidation.
6. **`busbar-sf-source-format-cli` (`crates/cli/tests/cli_integration.rs`, 5 tests)**:
   - Exercises CLI execution for `convert source`, `convert mdapi`, `convert mdapi --zip`, `convert source --merge-with`, `--cache-dir`, and invalid argument rejection.

## Preserved Quirks & Documented Upstream Snapshot Artifacts

The test harness copies each snapshot project into `<TempDir>/<project_name>/` (preserving parent directory naming such as `appTemplates` and `genAiFunctions` for bundle resolution) and documents five upstream `mocha-snap` / JS test quirks discovered during strict Rust parity testing:

1. **`appTemplates` & `genAiFunctions`**: Upstream `MetadataResolver.resolveDirectoryAsComponent` searches `parts.lastIndexOf(directoryName)`, including ancestor directory segments such as `appTemplates` / `genAiFunctions`; copying fixtures under `<TempDir>/<project_name>/` preserves the identical resolution behavior.
2. **`preset-PermSet` & `preset-sharingRules`**: Upstream `snapshots.test.ts` omitted `await` on `dirsAreIdentical(...)`, leaving stale single-file `dreamhouse.permissionset-meta.xml` / `account.sharingRules-meta.xml` artifacts inside `__snapshots__/verify-source-files.expected/`. `sdr_snapshots.rs` excludes those stale files while asserting strict equality on all decomposed files and semantic XML equality against `originalMdapi`.
3. **`staticResource`**: Upstream `snapshots.test.ts` omitted `await` on `dirsAreIdentical(...)`, upstream `conversions.ts` `shouldIgnore` skipped `file.includes('leafletjs.resource')` (excluding `leafletjs.resource-meta.xml` as well), and `fileSnap` read `leafletjs/` binary PNGs and CRLF CSS/JS files as UTF-8. `snapshot_static_resource` compares all extracted `leafletjs/` bytes, `.resource-meta.xml`, and generated `.resource` zip entries strictly against `originalMdapi`.
4. **`waveTemplateBundle`**: Upstream `originalMdapi/waveTemplates/.../Regional_Sales_Info.csv` uses `\r\n`, whereas `mocha-snap` normalized line endings to `\n` in `__snapshots__`. `sdr_snapshots.rs` normalizes `\r\n` → `\n` solely for `.csv` files.
5. **`mpdWithLabels`**: Upstream `@salesforce/core` `pkg.fullPath` ends with `/` while `mpdWithLabels/snapshots.test.ts` passed `path.resolve(...)` without one, causing `pkgPaths.includes(defaultDirectory)` to miss and duplicate `my_app_Label_1` into `force-app`. In Rust, `NonDecompositionFinalizer` normalizes paths canonically so multi-package ownership stays idempotent.

## Deliberate Deviations from Upstream

1. **No network lookup for the manifest API version.** SDR's `ComponentSet.getApiVersion()` falls back, after `sourceApiVersion`, the configured `org-api-version`, and so on, to an HTTP call that asks Salesforce for the highest API version, and only then to `58.0`. This port resolves everything else identically (including `SF_ORG_API_VERSION`/`SFDX_API_VERSION`, the project and global `.sf/config.json`, and the legacy `.sfdx/sfdx-config.json`; see `resolve/org_api_version.rs`) but skips the network step: a run with nothing configured uses `58.0`. Conversion stays offline and deterministic, and the incremental cache key (which includes the resolved version) stays stable.

## Maintaining for New Salesforce API Versions

To update this repository for a new Salesforce API release / `@salesforce/source-deploy-retrieve` version:

1. Run the sync script with the target SDR git tag:
   ```bash
   ./scripts/sync-sdr-fixtures.sh <sdr-tag>
   ```
   This updates:
   - `crates/source-format/data/metadataRegistry.json`
   - `crates/source-format/data/stdValueSetRegistry.json`
   - `crates/source-format/data/presets/*.json`
   - `fixtures/sdr-snapshots/` and `fixtures/sdr-snapshots/UPSTREAM_VERSION`
2. If upstream SDR introduces a new `adapter`, `transformer`, `decomposition`, or `recomposition` strategy enum value in `metadataRegistry.json` or presets, serde (`#[serde(deny_unknown_fields)]`) will fail fast in `registry_validation.rs`. Add the corresponding enum variant in `crates/source-format/src/registry/types.rs` and implement it in `crates/source-format/src/`.
3. Verify full parity and lint compliance:
   ```bash
   cargo fmt --check
   cargo clippy --workspace --all-targets --all-features -- -D warnings
   cargo test --workspace
   cargo doc --workspace --no-deps
   ```
