# Attribution

`busbar-sf-source-format` is maintained by **MuseLab LLC** and dual-licensed under `MIT OR Apache-2.0` (see [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE)).

## Upstream Salesforce Metadata Registry & Snapshot Fixtures

The embedded Salesforce metadata registry files (`crates/source-format/data/metadataRegistry.json`, `crates/source-format/data/stdValueSetRegistry.json`, `crates/source-format/data/presets/*.json`) and the snapshot conformance fixtures (`fixtures/sdr-snapshots/`) are derived from:

- **Package**: [`@salesforce/source-deploy-retrieve`](https://www.npmjs.com/package/@salesforce/source-deploy-retrieve) (`13.13.0`)
- **Source Repository**: [forcedotcom/source-deploy-retrieve](https://github.com/forcedotcom/source-deploy-retrieve)
- **Pinned Commit**: `a2d677f6daccde1af5d35e6bb2fc0867c67db57b` (recorded in [fixtures/sdr-snapshots/UPSTREAM_VERSION](fixtures/sdr-snapshots/UPSTREAM_VERSION))
- **Upstream License**: Apache-2.0
- **Upstream Copyright**: Salesforce, Inc.

## Benchmark Reference Dataset

The optional real-world benchmark target in `benchmarks/generate-workloads.mjs` clones:

- **Repository**: [SalesforceFoundation/NPSP](https://github.com/SalesforceFoundation/NPSP)
- **Pinned Commit**: `f5072b44781e7f31cb396c6919cfa7d0ead0ec6a`
- **License**: BSD-3-Clause

NPSP sources are not redistributed in this repository; they are fetched on demand into gitignored `benchmarks/.cache/npsp` when running `benchmarks/run-comparison.py`.

