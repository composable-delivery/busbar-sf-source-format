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

## API version

The API version only sets the `<version>` in the generated `package.xml` (and the destructive-changes manifests); component files are converted without version handling. `sf-source-format` picks it the same way `sf project convert` does. The first value found wins:

1. A version in the `package.xml` passed with `--manifest`.
2. `--api-version`.
3. `sourceApiVersion` in `sfdx-project.json`.
4. `org-api-version` from the environment or the `sf` configuration:
   - the `SF_ORG_API_VERSION` environment variable (or its legacy synonym `SFDX_API_VERSION`),
   - the project's `.sf/config.json`, then the global `~/.sf/config.json` (set with `sf config set org-api-version=63.0`),
   - the legacy `.sfdx/sfdx-config.json` (`apiVersion`) in either place, which overrides `config.json` there unless `SF_SFDX_INTEROPERABILITY=false`.
5. `58.0`.

Unlike the `sf` CLI, `sf-source-format` never contacts Salesforce to look up the newest API version. If none of the above is set, it uses `58.0`, so the same inputs always produce the same output, offline. Set `--api-version` or `sourceApiVersion` to pin the version you deploy with.

## Output

On success, `sf-source-format` prints:

- `output: <path>` when a directory or `.zip` path was written
- `converted components: <count>`
- `deleted files: <count>` when a `--merge-with` conversion removes obsolete decomposed child files

