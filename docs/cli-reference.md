---
title: CLI Reference
description: Command-line interface reference for the sf-source-format binary.
---

# CLI Reference

The `sf-source-format` binary provides subcommands for converting Salesforce metadata between SFDX source format and Metadata API (MDAPI) format.

## Synopsis

```bash
sf-source-format convert mdapi [OPTIONS]
sf-source-format convert source [OPTIONS]
```

At least one component selection flag (`--sourcepath`, `--manifest`, or `--metadata`) must be provided.

## `sf-source-format convert mdapi`

Converts SFDX source files into Metadata API format (`package.xml` + metadata files, or a `.zip` archive).

### Options

| Flag | Short | Value | Description |
| --- | --- | --- | --- |
| `--sourcepath` | `-p` | `PATH...` | Source directories or files to convert (or search paths when combined with `--manifest` or `--metadata`). |
| `--manifest` | `-x` | `PATH` | Path to a `package.xml` manifest selecting components to convert. |
| `--metadata` | `-m` | `TYPE:NAME...` | Explicit metadata components in `Type:FullName` format (for example, `CustomObject:Account`). |
| `--output-dir` | `-d` | `PATH` | Target output directory for converted files or zip archive (defaults to `.`). |
| `--zip` | | | Write output as a `.zip` archive instead of loose files. |
| `--package-name` | | `NAME` | Package directory or archive name, and `<fullName>` in generated `package.xml`. |
| `--api-version` | | `VER` | Override Metadata API version (for example, `63.0`). |
| `--project-dir` | | `PATH` | Project root directory containing `sfdx-project.json` and `.forceignore`. |
| `--exact-dir` | | | Write directly into `--output-dir` instead of creating a timestamped `metadataPackage_<timestamp>` subdirectory. |

### Examples

Convert all metadata under `force-app` into an exact MDAPI directory:

```bash
sf-source-format convert mdapi \
  --sourcepath force-app \
  --output-dir build/mdapi \
  --exact-dir
```

Create a zip package filtered by `package.xml` and an API version override:

```bash
sf-source-format convert mdapi \
  --manifest manifest/package.xml \
  --sourcepath force-app \
  --output-dir build/packages \
  --zip \
  --package-name release-pkg \
  --api-version 63.0
```

## `sf-source-format convert source`

Converts Metadata API files into SFDX source format (decomposing objects, labels, workflows, and bundles according to the project's `sfdx-project.json` presets).

### Options

Accepts all flags listed under [`convert mdapi`](#options) plus:

| Flag | Value | Description |
| --- | --- | --- |
| `--merge-with` | `PATH...` | Existing SFDX source paths to merge converted metadata into in-place (cannot be combined with `--zip`). |

### Examples

Convert an MDAPI retrieve directory into a fresh SFDX directory:

```bash
sf-source-format convert source \
  --sourcepath mdapi-retrieve \
  --output-dir force-app \
  --project-dir . \
  --exact-dir
```

Merge retrieved metadata into existing multi-package directories:

```bash
sf-source-format convert source \
  --sourcepath mdapi-retrieve \
  --merge-with force-app packages/core \
  --project-dir .
```

## Output

On success, `sf-source-format` prints:

- `output: <path>` when a directory or `.zip` path was written
- `converted components: <count>`
- `deleted files: <count>` when a `--merge-with` conversion removes obsolete decomposed child files

