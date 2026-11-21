# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed
- The `package.xml` API version now also honors the configured `org-api-version` like SDR does: `SF_ORG_API_VERSION` (or `SFDX_API_VERSION`), the project `.sf/config.json`, the global `~/.sf/config.json`, and the legacy `.sfdx/sfdx-config.json`. Previously these were ignored and the fixed `58.0` was used whenever `--api-version` and `sourceApiVersion` were unset. SDR's network lookup for the newest API version is intentionally not performed (see `PORTING.md`).

## [0.0.1] - 2026-11-20 (Pre-release)

### Added
- Initial open-source release on crates.io:
  - `busbar-sf-source-format`: Core Rust engine bundling `registry` (543+ metadata types, 7 presets), `xml` (lossless order-preserving XML DOM), `tree` (`FsTree`, `ZipTree`, `MemoryTree`, `.forceignore`, path & symlink traversal safety), `resolve`, `manifest`, and `convert`.
  - `busbar-sf-source-format-cli`: Standalone CLI (`sf-source-format`) with subcommands `convert mdapi` and `convert source`.
- Full byte-level upstream parity with `@salesforce/source-deploy-retrieve@13.13.0` across all 33 SDR snapshot test suites and 7 source behavior presets (`sourceBehaviorOptions`).
- Pre-built, attested binaries with SHA-256 checksums and SLSA build provenance for Linux (x86_64 gnu & musl, aarch64), macOS (Apple Silicon & Intel), and Windows (x86_64).
- Dual-licensed under `MIT OR Apache-2.0` (MuseLab LLC). Authorship: Jason Lantz (`jlantz` on GitHub) and MuseLab LLC.

