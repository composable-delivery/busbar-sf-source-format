# Contributing

Thank you for considering a contribution to `sf-source-format`.

By participating you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md). Report security vulnerabilities privately as described in [SECURITY.md](SECURITY.md).

## Prerequisites

- **Rust**: `1.86` or later (MSRV), with `rustfmt` and `clippy` components (`rust-toolchain.toml` selects `stable` with both).
- **Optional tooling**:
  - `cargo-deny` and `cargo-audit` for license/advisory checks (`cargo install cargo-deny cargo-audit`)
  - **Node.js 22+** for benchmark comparison scripts (`benchmarks/`) and building the documentation site (`site/`)

## Workspace Layout

| Path | Contents |
| --- | --- |
| `crates/source-format` | `busbar-sf-source-format` library crate: `registry` (`metadataRegistry.json`, presets, `sfdx-project.json`), `xml` (ordered XML DOM), `tree` (`FsTree`, `ZipTree`, `MemoryTree`, `.forceignore`, path/symlink guards), `resolve`, `manifest`, and `convert` (`MetadataConverter`, Criterion benches) |
| `crates/cli` | `busbar-sf-source-format-cli` crate providing the `sf-source-format` CLI binary (`convert mdapi` / `convert source`) |
| `fixtures/sdr-snapshots/` | 33 upstream `@salesforce/source-deploy-retrieve` snapshot test projects |
| `benchmarks/` | Cross-engine comparison harness (`Rust` vs Node SDR & `sf` CLI) |
| `docs/` | Plain Markdown user & architecture documentation |
| `site/` | Astro Starlight documentation build tooling |

## Required Checks

Run the full check suite before opening a pull request:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps --all-features
```

Optional security and license checks:

```bash
cargo deny check
cargo audit
```

## Coding & API Standards

This workspace enforces strict Rust API and lint rules via `[workspace.lints]` in [Cargo.toml](Cargo.toml) and [clippy.toml](clippy.toml):

- **Zero boolean parameters**: `max-fn-params-bools = 0` — use two-variant enums (e.g., `TargetFormat`, `UniqueDirPolicy`, `ForceIgnorePolicy`) or distinct methods rather than `bool` parameters.
- **No panics on fallible paths**: `unwrap_used = "warn"` and `panic_in_result_fn = "warn"` — return typed `thiserror` errors (`RegistryError`, `TreeError`, `XmlError`, `ResolveError`, `ManifestError`, `ConvertError`).
- **Explicit conversions**: `as_conversions = "warn"` — use `TryFrom`, `From`, or checked casts instead of lossy `as`.
- **Document public items**: `missing_docs = "warn"`, `missing_errors_doc = "warn"`, and `missing_panics_doc = "warn"` — keep `cargo doc` warning-free.
- **Justification for lint suppressions**: `allow_attributes_without_reason = "warn"` — prefer `#[expect(clippy::..., reason = "...")]` over bare `#[allow(...)]`.

## Parity & Upstream Sync

- Any behavior change to metadata resolution, XML serialization, decomposition, or recomposition must keep all 34 tests in `crates/source-format/tests/sdr_snapshots.rs` passing.
- To update the embedded registry and snapshot fixtures for a newer `@salesforce/source-deploy-retrieve` release, run:

```bash
./scripts/sync-sdr-fixtures.sh <sdr-tag>
```

## Documentation

Documentation prose lives in `docs/` as standard Markdown, while [PORTING.md](PORTING.md) and [BENCHMARKS.md](BENCHMARKS.md) live at the repository root. To preview the Starlight documentation site locally:

```bash
npm --prefix site ci
npm --prefix site run dev
```

## Licensing

By submitting a contribution, you agree that your work will be dual-licensed under `MIT OR Apache-2.0` (`LICENSE-MIT` and `LICENSE-APACHE`), consistent with the repository's licensing.

