# sf-source-format

[![CI](https://github.com/composable-delivery/sf-source-format/actions/workflows/ci.yml/badge.svg)](https://github.com/composable-delivery/sf-source-format/actions/workflows/ci.yml)
[![Security](https://github.com/composable-delivery/sf-source-format/actions/workflows/security.yml/badge.svg)](https://github.com/composable-delivery/sf-source-format/actions/workflows/security.yml)
[![Docs](https://github.com/composable-delivery/sf-source-format/actions/workflows/docs.yml/badge.svg)](https://github.com/composable-delivery/sf-source-format/actions/workflows/docs.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT_OR_Apache--2.0-blue.svg)](LICENSE-MIT)
[![MSRV: 1.86](https://img.shields.io/badge/MSRV-1.86-informational.svg)](Cargo.toml)

A high-performance, idiomatic Rust port of Salesforce CLI's [`@salesforce/source-deploy-retrieve`](https://github.com/forcedotcom/source-deploy-retrieve) (SDR) metadata source format resolution, decomposition, recomposition, and manifest engine.

> **Pre-Release Notice (v0.0.1)**: Published on [crates.io](https://crates.io/crates/busbar-sf-source-format) as `0.0.1` by [Jason Lantz](https://github.com/jlantz) and [MuseLab LLC](https://muselab.com). Pre-built, attested binaries with SLSA provenance are available for Linux, macOS, and Windows.

Designed for drop-in byte-level parity with `@salesforce/source-deploy-retrieve@13.13.0` while delivering **2.2x–8.4x** faster in-process conversion, **3.7x–131x** faster cold CLI execution, and **5.07x** lower peak memory footprint.

## Features

- **Full SDR 13.13.0 Parity**: Verified against all **33** upstream SDR snapshot projects (`34/34` snapshot tests), all **7** `sourceBehaviorOptions` presets, and real-world `SalesforceFoundation/NPSP` (`12,063 / 12,063` SFDX files raw byte-identical on `MDAPI -> SFDX`, `3,926 / 3,926` non-ZIP MDAPI files plus `809` extracted static resource entries byte-identical on `SFDX -> MDAPI`).
- **543+ Embedded Metadata Types**: Built-in `metadataRegistry.json`, `stdValueSetRegistry.json`, all 7 presets, and `sfdx-project.json` `registryCustomizations` & `replacements` layering.
- **Order-Preserving XML & JSON**: Lossless XML DOM (`sf-xml`) preserving tag ordering, inline comments, CDATA, numeric entities, and leading-zero string values, plus `IndexMap`-backed JSON/YAML ordering and locale-independent UCA collation (`feruca`).
- **Path & Archive Safety**: Lexical root containment, symlink-traversal rejection (`ensure_no_symlink_on_path`), `.forceignore` filtering, and `UIBundle` deploy-time descriptor validation.
- **Library + CLI**: Modular workspace crates for embedding in Rust tools or running standalone via the `sf-source-format` CLI.

## Workspace Crates

| Crate | Purpose |
| --- | --- |
| [`busbar-sf-source-format`](crates/source-format) | Core library crate bundling `registry` (`543+` types & 7 presets), `xml` (order-preserving XML DOM), `tree` (`FsTree`/`ZipTree`/`MemoryTree`, `.forceignore`, safety guards), `resolve`, `manifest`, and `convert` |
| [`busbar-sf-source-format-cli`](crates/cli) | CLI crate installing the `sf-source-format` binary (`convert mdapi` / `convert source`) |

## Quick Start

### CLI Installation

#### Option 1: Standalone Attested Binary (No Node.js or Rust required)
Download pre-built, cryptographically attested release binaries with SLSA provenance:

```bash
# macOS (Apple Silicon M1/M2/M3/M4)
curl -fsSL https://github.com/composable-delivery/sf-source-format/releases/download/v0.0.1/sf-source-format-aarch64-apple-darwin.tar.gz | tar -xz
sudo mv sf-source-format /usr/local/bin/

# Linux x86_64
curl -fsSL https://github.com/composable-delivery/sf-source-format/releases/download/v0.0.1/sf-source-format-x86_64-unknown-linux-gnu.tar.gz | tar -xz
sudo mv sf-source-format /usr/local/bin/

# Verify build provenance
gh attestation verify sf-source-format --owner composable-delivery
```

#### Option 2: Cargo Install (Rust 1.86+)
```bash
cargo install busbar-sf-source-format-cli
# or instant pre-compiled install:
cargo binstall busbar-sf-source-format-cli
```

Convert SFDX source format to Metadata API format (directory or `.zip`):

```bash
sf-source-format convert mdapi --sourcepath force-app --output-dir mdapi-out --exact-dir
sf-source-format convert mdapi --sourcepath force-app --output-dir mdapi-out --zip --package-name my-pkg
```

Convert Metadata API format back to SFDX source format (or merge into existing source):

```bash
sf-source-format convert source --sourcepath mdapi-out --output-dir force-app --exact-dir
sf-source-format convert source --sourcepath mdapi-out --merge-with force-app --project-dir .
```

Select specific components with `--manifest` (`package.xml`) or `--metadata` (`Type:FullName`):

```bash
sf-source-format convert mdapi --manifest manifest/package.xml --sourcepath force-app --output-dir mdapi-out
sf-source-format convert mdapi --metadata CustomObject:Account --sourcepath force-app --output-dir mdapi-out
```

### Rust Library

Add `busbar-sf-source-format` to your `Cargo.toml`:

```toml
[dependencies]
busbar-sf-source-format = "0.0.1"
```

Resolve components and convert them between formats:

```rust,no_run
use std::path::PathBuf;
use busbar_sf_source_format::convert::{
    ConvertOutput, MetadataConverter, TargetFormat, UniqueDirPolicy,
};
use busbar_sf_source_format::registry::RegistryAccess;
use busbar_sf_source_format::resolve::{ComponentSetBuilder, ComponentSetOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let project_dir = PathBuf::from(".");
    let registry = RegistryAccess::from_project_dir(&project_dir)?;

    let components = ComponentSetBuilder::build(&ComponentSetOptions {
        sourcepath: vec![project_dir.join("force-app")],
        project_dir: Some(project_dir),
        ..Default::default()
    })?;

    let converter = MetadataConverter::new(registry);
    let result = converter.convert(
        &components,
        TargetFormat::Metadata,
        ConvertOutput::Directory {
            output_dir: PathBuf::from("mdapi-out"),
            package_name: None,
            unique_dir: UniqueDirPolicy::Exact,
        },
    )?;

    println!("Converted {} components", result.converted.len());
    Ok(())
}
```

## Benchmarks

Summary of measured results on Apple M2 Max (12 cores, 64 GiB RAM) comparing against `@salesforce/source-deploy-retrieve@13.13.0` and `@salesforce/cli/2.156.8`:

- **In-process engine (`22` workloads)**: `2.21x–8.43x` median speedup across SDR snapshots, synthetic `medium-org` (`1,591` SFDX files) and `large-org` (`7,451` SFDX files), and `SalesforceFoundation/NPSP` (`12,063` SFDX files); `467.0 MiB` vs `836.1 MiB` max RSS and `86.6 MiB` vs `439.2 MiB` peak memory footprint (**5.07x lower**).
- **Cold CLI (`8` workloads)**: `3.68x–131.33x` median speedup (`9.0 ms` vs `1,188.5 ms` on `customObjects-and-children`; `1.42 s` vs `5.23 s` on `npsp`).

Live benchmark results are automatically executed and viewable directly in GitHub Actions across Ubuntu, macOS, and Windows runners:
👉 **[View Live GitHub Actions Benchmark Runs](https://github.com/composable-delivery/sf-source-format/actions/workflows/bench.yml)**

See [BENCHMARKS.md](BENCHMARKS.md) for full tables, Criterion microbenchmarks, and reproduction instructions.

## Documentation

- [Documentation Portal (Markdown)](docs/index.mdx) — Getting started, CLI reference, library guide, and architecture
- [Porting Notes](PORTING.md) — TypeScript-to-Rust mapping, design decisions, and upstream SDR parity details
- [Benchmarks](BENCHMARKS.md) — Full benchmark methodology and results
- [Contributing](CONTRIBUTING.md) — Development setup, lint rules, and PR checklist

## Attribution & Licensing

Dual-licensed under either of:

- **MIT License** ([LICENSE-MIT](LICENSE-MIT))
- **Apache License, Version 2.0** ([LICENSE-APACHE](LICENSE-APACHE))

at your option. Copyright (c) 2026 **MuseLab LLC**.

Upstream metadata registries and snapshot test fixtures are derived from [`@salesforce/source-deploy-retrieve`](https://github.com/forcedotcom/source-deploy-retrieve) (Apache-2.0, Salesforce, Inc.). See [ATTRIBUTION.md](ATTRIBUTION.md) for details.

