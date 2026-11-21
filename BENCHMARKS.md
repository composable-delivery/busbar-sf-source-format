# Performance Benchmarks: `busbar-sf-source-format` (Rust) vs `@salesforce/source-deploy-retrieve` & `sf` CLI (Node.js)

Benchmarks comparing this Rust workspace (`busbar-sf-source-format` / `busbar-sf-source-format-cli`) against:
1. **In-Process Conversion Engine**: `@salesforce/source-deploy-retrieve@13.13.0` (`benchmarks/node-sdr/bench.mjs`) vs Rust release harness (`crates/source-format/examples/bench_harness.rs`).
2. **Cold CLI End-to-End**: `@salesforce/cli` (`sf project convert mdapi` / `sf project convert source`) vs `busbar-sf-source-format-cli`.
3. **Microbenchmarks**: Rust Criterion suite (`crates/source-format/benches/convert_bench.rs`).

## Environment

Measured locally (saved with `benchmarks/save-local-results.mjs`) from [benchmarks/results/local/apple-m2-max-macos.json](benchmarks/results/local/apple-m2-max-macos.json):

- **Machine**: Apple M2 Max, 12 cores, 64 GiB RAM (`68,719,476,736` bytes)
- **OS**: macOS 14.4.1 (`Build 23E224`)
- **Rust**: `rustc 1.96.0 (ac68faa20 2026-05-25)` (`--release` / `bench` profile)
- **Node.js**: `v22.23.4`
- **Node Library**: `@salesforce/source-deploy-retrieve@13.13.0`
- **Salesforce CLI**: `@salesforce/cli/2.156.8 darwin-arm64 node-v22.23.4`

## Methodology

- **Fair Timing Boundaries**: Output directory preparation/cleanup and output file/ZIP counting run **outside** the timed interval in both `bench_harness.rs` (`std::time::Instant`) and `bench.mjs` (`process.hrtime.bigint()`). Both engines time the exact pipeline: `RegistryAccess` initialization (`from_project_dir` when present), `ComponentSetBuilder.build({ sourcepath })`, and `MetadataConverter.convert(...)`.
- **Parity Verification**: Every in-process (`22/22`) and cold-CLI (`8/8`) workload asserts output file counts (non-directory ZIP entry count for ZIP targets; recursive file count for directory targets) match between Rust and Node. On the real-world `SalesforceFoundation/NPSP` dataset, a separate head-to-head byte comparison against `@salesforce/source-deploy-retrieve@13.13.0` confirmed `12,063 / 12,063` SFDX files raw byte-identical (`MDAPI -> SFDX`), `3,926 / 3,926` non-ZIP MDAPI files raw byte-identical plus `staticresources/CumulusStaticResources.resource`'s `809` inner files byte-identical (`SFDX -> MDAPI dir`), and all `3,927 / 3,927` entries matched inside `SFDX -> MDAPI zip`.
- **Warmup & Iterations**:
  - Real SDR snapshot fixtures (6 projects): `3` warmup runs + `10` timed iterations (`Part A`).
  - Synthetic `medium-org` (25 `CustomObject`s with 40 fields / 8 validation rules / 4 list views each, 100 `ApexClass`es, 15 `Profile`s, 10 `PermissionSet`s, 500 `CustomLabels`, 10 `LightningComponentBundle`s — `1,591` SFDX / `292` MDAPI files): `2` warmup + `5` timed iterations (`Part A`), `1` warmup + `5` timed CLI invocations (`Part B`).
  - Synthetic `large-org` (100 `CustomObject`s with 50 fields / 10 validation rules / 4 list views each, 400 `ApexClass`es, 30 `Profile`s, 20 `PermissionSet`s, 2,000 `CustomLabels`, 25 `LightningComponentBundle`s — `7,451` SFDX / `1,052` MDAPI files): `1` warmup + `3` timed iterations.
  - Real-world **`npsp`** ([`SalesforceFoundation/NPSP`](https://github.com/SalesforceFoundation/NPSP) pinned commit `f5072b44781e7f31cb396c6919cfa7d0ead0ec6a`, BSD-3-Clause; `12,194` raw SFDX files / `12,063` after `.forceignore` / `3,927` MDAPI files / ~9.2 MB compressed `.zip` across 28 metadata directories, cached in gitignored `benchmarks/.cache/npsp`): `1` warmup + `3` timed iterations in both `Part A` and `Part B`.
  - Cold CLI (`Part B`): subprocess invocations with `SF_DISABLE_AUTOUPDATE=true`, `SF_SKIP_NEW_VERSION_CHECK=true`, `SF_DISABLE_TELEMETRY=true`, `SF_USE_PROGRESS_BAR=false`, `SF_NO_COLOR=true`.
- **Memory**: Peak RSS (`maximum resident set size`) and `peak memory footprint` measured via `/usr/bin/time -l`.

---

## Part A: Warm In-Process Engine Comparison

| Workload | Rust Median (ms) | Rust Mean (ms) | Rust P95 (ms) | Node Median (ms) | Node Mean (ms) | Node P95 (ms) | Median Speedup | Output Files (R / N) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :---: |
| `customObjects-and-children` (MDAPI -> SFDX) | 3.15 | 3.18 | 3.37 | 17.77 | 18.81 | 25.47 | **5.65x** | 38 / 38 |
| `customObjects-and-children` (SFDX -> MDAPI dir) | 2.20 | 2.17 | 2.29 | 9.77 | 10.02 | 17.03 | **4.45x** | 3 / 3 |
| `customObjects-and-children` (SFDX -> MDAPI zip) | 1.58 | 1.59 | 1.88 | 11.15 | 11.37 | 16.61 | **7.05x** | 3 / 3 |
| `digitalExperienceBundle` (MDAPI -> SFDX) | 15.39 | 15.46 | 17.06 | 78.30 | 78.58 | 104.80 | **5.09x** | 115 / 115 |
| `digitalExperienceBundle` (SFDX -> MDAPI dir) | 13.92 | 13.82 | 14.27 | 66.93 | 68.62 | 88.58 | **4.81x** | 116 / 116 |
| `digitalExperienceBundle` (SFDX -> MDAPI zip) | 4.40 | 4.46 | 4.82 | 49.29 | 48.55 | 58.13 | **11.20x** | 116 / 116 |
| `staticResource` (MDAPI -> SFDX) | 2.01 | 2.13 | 2.57 | 11.97 | 12.27 | 17.62 | **5.97x** | 14 / 14 |
| `staticResource` (SFDX -> MDAPI dir) | 4.57 | 4.56 | 4.73 | 27.41 | 27.90 | 38.50 | **5.99x** | 9 / 9 |
| `preset-decomposedPS2` (MDAPI -> SFDX) | 2.91 | 3.02 | 3.40 | 31.57 | 32.23 | 49.84 | **10.84x** | 26 / 26 |
| `preset-decomposedPS2` (SFDX -> MDAPI dir) | 2.41 | 2.41 | 2.59 | 11.03 | 12.61 | 22.83 | **4.58x** | 7 / 7 |
| `preset-decomposedESR` (MDAPI -> SFDX) | 1.05 | 1.04 | 1.20 | 8.22 | 9.99 | 21.13 | **7.86x** | 4 / 4 |
| `preset-decomposedESR` (SFDX -> MDAPI dir) | 0.90 | 0.91 | 0.98 | 4.45 | 4.74 | 7.21 | **4.96x** | 3 / 3 |
| `customLabels-multiple` (SFDX -> MDAPI dir) | 55.56 | 57.80 | 69.41 | 538.64 | 536.93 | 578.74 | **9.70x** | 2 / 2 |
| `medium-org` (MDAPI -> SFDX) | 80.58 | 81.84 | 89.94 | 399.18 | 411.38 | 467.32 | **4.95x** | 1591 / 1591 |
| `medium-org` (SFDX -> MDAPI dir) | 63.00 | 66.16 | 83.76 | 200.58 | 200.89 | 210.70 | **3.18x** | 292 / 292 |
| `medium-org` (SFDX -> MDAPI zip) | 28.70 | 29.75 | 34.26 | 173.32 | 171.65 | 175.61 | **6.04x** | 292 / 292 |
| `large-org` (MDAPI -> SFDX) | 391.39 | 403.02 | 440.52 | 1770.24 | 1798.93 | 1910.45 | **4.52x** | 7451 / 7451 |
| `large-org` (SFDX -> MDAPI dir) | 186.34 | 185.89 | 188.10 | 966.42 | 967.47 | 970.02 | **5.19x** | 1052 / 1052 |
| `large-org` (SFDX -> MDAPI zip) | 131.77 | 130.68 | 135.48 | 949.96 | 951.03 | 961.55 | **7.21x** | 1052 / 1052 |
| `npsp` (MDAPI -> SFDX) | 801.98 | 818.12 | 866.10 | 3650.17 | 3929.99 | 4510.14 | **4.55x** | 12063 / 12063 |
| `npsp` (SFDX -> MDAPI dir) | 530.12 | 530.52 | 580.14 | 3138.12 | 3227.35 | 3915.36 | **5.92x** | 3927 / 3927 |
| `npsp` (SFDX -> MDAPI zip) | 374.18 | 375.74 | 388.85 | 3020.53 | 3014.05 | 3024.24 | **8.07x** | 3927 / 3927 |

**Full-suite process memory (`/usr/bin/time -l`)**:
- **Rust (`bench_harness`)**: `388.4 MiB` max RSS (`128.6 MiB` peak memory footprint)
- **Node (`bench.mjs`)**: `913.1 MiB` max RSS (`460.4 MiB` peak memory footprint) — **2.35x higher max RSS / 3.58x higher peak memory footprint** than Rust.

---

## Part B: Cold CLI Comparison (`busbar-sf-source-format` vs `sf`)

| Workload | Rust CLI Median (ms) | Rust CLI Mean (ms) | `sf` CLI Median (ms) | `sf` CLI Mean (ms) | Median Speedup | Rust Peak RSS | `sf` Peak RSS | Output Files (R / `sf`) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :---: |
| `customObjects-and-children` (MDAPI -> SFDX) | 8.0 | 8.0 | 1188.0 | 1187.2 | **148.91x** | 6.9 MiB | 263.9 MiB | 38 / 38 |
| `customObjects-and-children` (SFDX -> MDAPI) | 7.9 | 8.6 | 1189.6 | 990.9 | **150.52x** | 8.7 MiB | 241.8 MiB | 3 / 3 |
| `digitalExperienceBundle` (MDAPI -> SFDX) | 22.7 | 22.7 | 1187.7 | 1186.4 | **52.28x** | 12.5 MiB | 283.4 MiB | 115 / 115 |
| `digitalExperienceBundle` (SFDX -> MDAPI) | 19.4 | 20.6 | 1189.1 | 1196.2 | **61.23x** | 13.5 MiB | 233.3 MiB | 116 / 116 |
| `medium-org` (MDAPI -> SFDX) | 92.5 | 92.3 | 5327.5 | 5115.0 | **57.58x** | 39.0 MiB | 676.0 MiB | 1591 / 1591 |
| `medium-org` (SFDX -> MDAPI) | 58.6 | 59.8 | 2278.3 | 2308.5 | **38.85x** | 55.3 MiB | 261.7 MiB | 292 / 292 |
| `npsp` (MDAPI -> SFDX) | 887.9 | 918.7 | 6270.8 | 7618.0 | **7.06x** | 178.8 MiB | 496.2 MiB | 12063 / 12063 |
| `npsp` (SFDX -> MDAPI) | 607.2 | 643.3 | 4226.1 | 4226.2 | **6.96x** | 240.4 MiB | 458.8 MiB | 3927 / 3927 |

---

## Part C: Native Rust Criterion Microbenchmarks

Captured with `cargo bench -p sf-source-format --bench convert_bench`:

| Group / Benchmark | Low | Estimate | High |
| --- | ---: | ---: | ---: |
| `mdapi_to_source/customObjects-and-children` | 3.68 ms | **3.94 ms** | 4.24 ms |
| `mdapi_to_source/digitalExperienceBundle` | 13.38 ms | **14.12 ms** | 14.53 ms |
| `mdapi_to_source/staticResource` | 2.24 ms | **2.46 ms** | 2.62 ms |
| `mdapi_to_source/preset-decomposedPS2` | 3.08 ms | **3.20 ms** | 3.31 ms |
| `mdapi_to_source/preset-decomposedESR` | 953.99 µs | **971.58 µs** | 1.00 ms |
| `source_to_mdapi_dir/customObjects-and-children` | 2.09 ms | **2.16 ms** | 2.25 ms |
| `source_to_mdapi_dir/digitalExperienceBundle` | 12.24 ms | **12.55 ms** | 13.00 ms |
| `source_to_mdapi_dir/staticResource` | 4.31 ms | **4.38 ms** | 4.46 ms |
| `source_to_mdapi_dir/preset-decomposedPS2` | 2.71 ms | **2.76 ms** | 2.81 ms |
| `source_to_mdapi_dir/preset-decomposedESR` | 978.00 µs | **1.01 ms** | 1.05 ms |
| `source_to_mdapi_zip/customObjects-and-children` | 1.50 ms | **1.56 ms** | 1.66 ms |
| `source_to_mdapi_zip/digitalExperienceBundle` | 4.97 ms | **5.21 ms** | 5.47 ms |
| `source_to_mdapi_zip/staticResource` | 4.35 ms | **4.38 ms** | 4.40 ms |
| `registry_new/default_embedded` | 5.55 ns | **5.58 ns** | 5.62 ns |
| `registry_new/from_project_dir_default` | 12.68 µs | **12.73 µs** | 12.78 µs |
| `registry_new/from_project_dir_preset` | 205.02 µs | **206.53 µs** | 208.15 µs |

---

## Optimization Impact (Waves 1 & 2)

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

| Metric | Initial Baseline | Wave 1 | Wave 2 (Current) | Total Improvement |
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

---

## Part D: Incremental `blake3` Cache Benchmarks (Real-World NPSP)

End-to-end release CLI timings (`./target/release/sf-source-format` with `--cache-dir`) on `SalesforceFoundation/NPSP` (`force-app`: 12,194 files on disk; MDAPI output: 3,927 files / 9,160,874-byte `.zip`; 2,329 incremental work units across 9,658 decomposed MDAPI->SFDX components).

Features three major optimization layers:
1. **Content-diffed decomposed writes**: Modifying an input recomposed/decomposed component (such as an `.object` in `MDAPI -> SFDX`) hashes candidates in parallel against valid on-disk outputs; unchanged child files are preserved without rewriting, dropping child write volume to only modified files (e.g. **1 written / 12,062 skipped** instead of rewriting all 29 object files).
2. **Git `.git/index` blob OID reuse**: Cold cache builds parse `.git/index` (v2, v3, and v4 prefix-compressed) in pure Rust to reuse Git's blob OIDs for stat-clean tracked files, bypassing disk reads and hashing on cold repository builds when safe (no `.gitattributes`, CRLF filters, or racy timestamps).
3. **Cold/warm parallelism & syscall reduction**: Redundant parent directory walks on recomposition components are eliminated, input collection and digests are parallelized on Rayon, and parent symlink validations are deduplicated per directory.

Median wall-clock time across 5 runs per scenario (Apple Silicon, local NVMe):

### 1. `SFDX -> MDAPI` Directory (3,927 output files, 2,329 work units)

| Scenario | Units Cached / Rebuilt | Files Written / Skipped | Median | vs Uncached Cold |
| :--- | :--- | :--- | ---: | ---: |
| Cold, uncached (`--exact-dir`) | — | 3,927 written | 744.5 ms | 1.00x |
| Cold, populate cache (`--cache-dir`, Git OID reuse) | 0 / 2,329 rebuilt | 3,927 written / 0 skipped | 929.8 ms | 0.80x |
| Warm no-op (0 edits) | 2,329 / 0 rebuilt | 0 written / 3,927 skipped | 330.4 ms | **2.25x** |
| 1-file Apex edit (`fflib_Application.cls`) | 2,328 / 1 rebuilt | 1 written / 3,926 skipped | 347.6 ms | **2.14x** |
| 1-file CustomField edit (`BindingObjectAlternate__c.field-meta.xml`) | 2,328 / 1 rebuilt | 1 written / 3,926 skipped | 315.5 ms | **2.36x** |

### 2. `SFDX -> MDAPI` ZIP (9,160,874-byte `pkg.zip`, 2,329 work units)

Untouched archive entries are transferred with zero decompression/recompression via `zip::ZipWriter::raw_copy_file`; warm no-op runs perform zero archive writes.

| Scenario | Units Cached / Rebuilt | Entries Written / Skipped | Median | vs Uncached Cold |
| :--- | :--- | :--- | ---: | ---: |
| Cold, uncached (`--zip`) | — | 3,927 written | 587.1 ms | 1.00x |
| Cold, populate cache (`--zip --cache-dir`, Git OID reuse) | 0 / 2,329 rebuilt | 3,927 written / 0 skipped | 730.8 ms | 0.80x |
| Warm no-op (0 edits) | 2,329 / 0 rebuilt | 0 written / 3,927 skipped | 310.2 ms | **1.89x** |
| 1-file Apex edit (`fflib_Application.cls`) | 2,328 / 1 rebuilt | 2 written / 3,925 skipped | 360.7 ms | **1.63x** |
| 1-file CustomField edit (`BindingObjectAlternate__c.field-meta.xml`) | 2,328 / 1 rebuilt | 1 written / 3,926 skipped | 342.7 ms | **1.71x** |

### 3. `MDAPI -> SFDX` Directory (12,063 output files, 2,329 work units)

| Scenario | Units Cached / Rebuilt | Files Written / Skipped | Median | vs Uncached Cold |
| :--- | :--- | :--- | ---: | ---: |
| Cold, uncached (`--exact-dir`) | — | 12,063 written | 996.5 ms | 1.00x |
| Cold, populate cache (`--cache-dir`) | 0 / 2,329 rebuilt | 12,063 written / 0 skipped | 1,153.5 ms | 0.86x |
| Warm no-op (0 edits) | 2,329 / 0 rebuilt | 0 written / 12,063 skipped | 207.4 ms | **4.80x** |
| 1-file `.object` edit (`objects/Account.object`) | 2,328 / 1 rebuilt | **1 written / 12,062 skipped** | **159.5 ms** | **6.25x** |

- **Cache footprint**: `3.46 MB`–`3.77 MB` per `.sfsfc` binary index (`3,586,425 B` for `SFDX -> MDAPI dir`, `3,770,729 B` for `SFDX -> MDAPI zip`, and `3,461,086 B` for `MDAPI -> SFDX dir`).
- **Byte-for-byte parity**: **0 byte diffs** vs cold uncached conversion across every tested scenario (cold cached, warm no-op, 1-file Apex edit, 1-file CustomField recomposition edit, and `MDAPI -> SFDX` 1-file `Account.object` edit).
- **Sub-200ms incremental single-file workflow**: Rebuilding and converting a single `.object` edit in `MDAPI -> SFDX` on NPSP's 12k files drops from `996.5 ms` down to `159.5 ms` (a **6.25x** speedup), writing only the single modified `.object-meta.xml` file while skipping all other 12,062 untouched XML files.
- Note on cold CLI floor: In warm/1-file invocations, `~160–330 ms` is dominated by subprocess startup and full sourcepath/component-set discovery of 12k source files; persistent in-process library consumers bypass subprocess startup.

---

## Reproducing

Run the full cross-engine & cold-CLI comparison (fetches/caches `SalesforceFoundation/NPSP` under gitignored `benchmarks/.cache/npsp` and writes [benchmarks/results.json](benchmarks/results.json)):

```bash
python3 benchmarks/run-comparison.py
```

Run native Rust Criterion microbenchmarks:

```bash
cargo bench -p sf-source-format --bench convert_bench
```
