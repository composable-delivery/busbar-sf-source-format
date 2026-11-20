# Security Policy

## Supported Versions

This project is pre-`1.0`. Security fixes are applied to the `main` branch and included in subsequent `0.x` releases.

## Reporting a Vulnerability

Report suspected vulnerabilities privately through GitHub Security Advisories:

https://github.com/composable-delivery/sf-source-format/security/advisories/new

Do not open a public issue, discussion, or pull request for a suspected vulnerability.

Please include:

- the crate or CLI version (or commit SHA), operating system, and Rust toolchain version;
- a minimal reproduction case (sample directory layout, `package.xml`, `sfdx-project.json`, or ZIP archive) with any sensitive org or proprietary metadata redacted;
- what you expected, what happened, and the security impact.

We aim to acknowledge reports within a few working days and will credit reporters in the advisory unless asked not to.

## In Scope

- Path traversal, root-escape, or symlink-traversal bypasses in `sf-tree::safety`, `ZipTree`, `StandardWriter`, `ArchiveWriter`, or `UIBundle` descriptor resolution.
- Zip extraction / bomb / malformed archive handling in `ZipTree` or `StaticResource` archive unpacking.
- XML parser denial-of-service or entity/encoding corruption in `sf-xml`.
- Environment variable interpolation or regex/glob handling in `sfdx-project.json` replacements.
- `UIBundle` deploy-time descriptor validation (`ui-bundle.json` / `mcp-resources.json`).
- Repository CI, benchmark, and documentation publishing workflows.

## Out of Scope

- Vulnerabilities in Salesforce, Salesforce CLI (`sf`), or upstream `@salesforce/source-deploy-retrieve` (report those to their respective vendors).
- Advisory findings in dev/benchmark-only dependencies that do not ship in the Rust crates or CLI binary.

