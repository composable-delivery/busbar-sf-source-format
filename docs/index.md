---
title: busbar-sf-source-format
description: High-performance, incremental, and zero-runtime engine for Salesforce MDAPI and SFDX source format conversion.
---

# busbar-sf-source-format

> [!NOTE]
> **Pre-Release Notice (v0.0.1)**: Published on [crates.io](https://crates.io/crates/busbar-sf-source-format) as `0.0.1` by [MuseLab LLC](https://muselab.com). Pre-built, cryptographically attested binaries with SLSA build provenance are available for Linux, macOS, and Windows.

**`busbar-sf-source-format`** is a native Rust replacement for Salesforce's [`@salesforce/source-deploy-retrieve`](https://github.com/forcedotcom/source-deploy-retrieve) (SDR) library and CLI conversion tooling.

It decomposes monolithic Metadata API (MDAPI) XML files into modular SFDX source directories, and recomposes SFDX source back into deployable MDAPI packages with **100% byte-for-byte fidelity with SDR 13.13.0**.

---

## Core Features

- **[Incremental Conversion](#incremental-conversion)** — Change-aware caching using Git index OIDs and BLAKE3 hashing.
- **[Cold CLI Performance](#cold-cli-performance)** — Sub-10ms startup with zero Node.js runtime, npm dependencies, or V8 warmup.
- **[Conversion Throughput](#conversion-throughput)** — Multi-threaded conversion pipeline powered by Rayon.
- **[Resource Efficiency](#resource-efficiency)** — Deterministic RAII memory footprint eliminates container OOM crashes.
- **[Dual Distribution](#dual-distribution)** — Standalone static binary CLI or zero-dependency embeddable Rust library.
- **[Full SDR Parity](#full-sdr-parity)** — Raw byte-identical output to SDR 13.13.0 across 12,063 files and 33 snapshot suites.

---

## Feature Overview

### Incremental Conversion
Upstream SDR re-parses, transforms, and re-writes entire metadata directories on every execution. `busbar-sf-source-format` introduces change-aware caching: it inspects [Git tree index](https://git-scm.com/book/en/v2/Git-Internals-Git-Objects) object IDs for tracked files and uses fast [BLAKE3](https://github.com/BLAKE3-team/BLAKE3) cryptographic hashing for untracked or non-Git files. When running warm conversions or single-field edits, unmodified components are resolved directly from cache without re-parsing XML.

→ *See [Architecture](./architecture.md#incremental-cache) for cache keying and invalidation rules.*

### Cold CLI Performance
Setting up `@salesforce/cli` in CI/CD pipelines typically imposes a 30–90 second penalty to provision Node.js, run `npm install`, and warm up the V8 engine. The `sf-source-format` CLI is a single, self-contained binary that executes cold in **9 milliseconds** and converts metadata **3.7x to 131x faster** than Node SDR.

→ *See [CLI Reference](./cli-reference.md) for subcommands and flags.*

### Conversion Throughput
Metadata resolution, XML serialization, and archive packaging are distributed across all CPU cores using [Rayon](https://github.com/rayon-rs/rayon). To prevent the filesystem lock contention commonly seen when multiple threads hammer macOS APFS and Linux VFS, disk writes run on a dedicated bounded I/O pool while CPU-bound transforms run in parallel.

→ *See [Benchmarks](../BENCHMARKS.md) for throughput measurements across real-world org sizes.*

### Resource Efficiency
Under Node.js, dynamic object graphs and V8 garbage collection cycles routinely balloon resident memory to 400–800+ MiB, risking container OOM kills on standard CI runners. `busbar-sf-source-format` relies on deterministic [RAII](https://en.wikipedia.org/wiki/Resource_acquisition_is_initialization) with zero garbage collection, completing full 22-workload test suites with a peak memory footprint of **86.6 MiB** (**5.07x lower** than Node SDR).

→ *See [Benchmarks](../BENCHMARKS.md#memory-profiling) for memory comparisons and RSS tracking.*

### Dual Distribution
`busbar-sf-source-format` is architected for both operations and development:
- **Standalone Binary CLI (`sf-source-format`)**: A self-contained executable for CI/CD runners and Docker containers with no Node.js or npm dependencies.
- **Embeddable Library (`busbar-sf-source-format`)**: A zero-dependency Rust crate exposing the 543+ type metadata registry, order-preserving XML DOM, `.forceignore` engine, and component decomposition directly to custom developer tools.

→ *See [Getting Started](./getting-started.md) for installation and [Library API Guide](./library-api.md) for embedding in Rust.*

### Full SDR Parity
Parity with official Salesforce CLI tooling is verified across all **34** snapshot tests in all **33** upstream SDR test suites, all **7** `sourceBehaviorOptions` presets (such as `decomposeCustomLabels` and `decomposePermissionSet`), and byte-for-byte fidelity across all **12,063** source files in `SalesforceFoundation/NPSP`.

→ *See [Porting Notes](../PORTING.md) for TypeScript-to-Rust mappings and upstream quirk handling.*

---

## Performance Summary

Evaluated on Apple M2 Max (12 cores, 64 GiB RAM) against `@salesforce/source-deploy-retrieve@13.13.0` and `@salesforce/cli/2.156.8`:

| Scenario | Rust (`busbar-sf-source-format`) | Node SDR / `sf` CLI | Speedup |
| :--- | :---: | :---: | :---: |
| **Cold CLI**: `customObjects-and-children` (`MDAPI -> SFDX`) | **9.0 ms** (`8.4 MiB` RSS) | 1,188.5 ms (`255.8 MiB` RSS) | **131.3x** |
| **Cold CLI**: `medium-org` (`MDAPI -> SFDX`, 1,591 files) | **136.3 ms** (`40.6 MiB` RSS) | 3,231.3 ms (`662.3 MiB` RSS) | **23.7x** |
| **Cold CLI**: `npsp` (`SFDX -> MDAPI`, 3,927 files) | **913.8 ms** (`221.5 MiB` RSS) | 4,223.7 ms (`457.8 MiB` RSS) | **4.6x** |
| **In-Process**: `large-org` (`SFDX -> MDAPI zip`, 7,451 files) | **260.0 ms** | 917.2 ms | **3.5x** |
| **In-Process**: `npsp` (`SFDX -> MDAPI zip`, 12,063 files) | **916.9 ms** | 2,856.6 ms | **3.1x** |
| **Suite Peak Memory Footprint** | **86.6 MiB** | 439.2 MiB | **5.07x lower** |

Automated benchmark workflows execute on GitHub-hosted runners across a matrix of operating systems:
👉 **[View Live GitHub Actions Benchmark Runs](https://github.com/composable-delivery/sf-source-format/actions/workflows/bench.yml)**

---

## Published Crate

| Crate | Directory | Capabilities |
| :--- | :--- | :--- |
| **[`busbar-sf-source-format`](https://crates.io/crates/busbar-sf-source-format)** | `crates/source-format` | Single package providing both the core metadata engine library and the standalone CLI binary (`sf-source-format`). |

Companion crates in the Busbar Salesforce ecosystem:
- **`busbar-sf-api`**: High-performance Rust client for Salesforce REST, Tooling, and Metadata APIs.
- **`busbar-sf-types`**: Strongly typed Rust representations of Salesforce metadata schemas.

---

## Next Steps

- **[Getting Started](./getting-started.md)** — Install the standalone binary or add the Rust crate to your project
- **[CLI Reference](./cli-reference.md)** — Complete command reference, arguments, and flags
- **[Library API Guide](./library-api.md)** — Embedding the conversion pipeline in Rust applications
- **[Architecture](./architecture.md)** — Internal crate structure, incremental cache, and safety guarantees
- **[Benchmarks](../BENCHMARKS.md)** — Full benchmark methodology, memory profiling, and reproduction instructions
- **[Porting Notes](../PORTING.md)** — Architectural mapping and upstream SDR 13.13.0 parity notes
- **[Rustdoc API Reference](/sf-source-format/api/busbar_sf_source_format/)** — Direct Rust documentation for library modules
