//! Org-level API version configuration (`org-api-version`), matching how `@salesforce/core`'s
//! `ConfigAggregator` resolves it for SDR's `ComponentSet.getApiVersion()`.
//!
//! SDR consults it after `sourceApiVersion` in `sfdx-project.json`. Precedence, highest first:
//!
//! 1. Environment: `SF_ORG_API_VERSION`, then its synonym `SFDX_API_VERSION`.
//! 2. Local project config: `<project>/.sf/config.json` (`org-api-version`).
//! 3. Global config: `~/.sf/config.json` (`org-api-version`).
//!
//! Within each location the legacy `.sfdx/sfdx-config.json` (`apiVersion`) is merged on top of
//! `config.json` unless `SF_SFDX_INTEROPERABILITY=false`, exactly as `@salesforce/core` does.
//!
//! SDR has one further fallback, a network call to Salesforce for the newest API version. It is
//! deliberately **not** ported: conversion stays offline and deterministic, and an unconfigured
//! run uses the fixed fallback version instead.

use std::path::{Path, PathBuf};

/// Looks up an environment variable. Injected so resolution is testable without mutating the
/// process environment.
pub(crate) type EnvLookup<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Reads the real process environment.
pub(crate) fn process_env(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// First of `keys` present in the JSON file at `file` as a non-empty string or number.
/// Unreadable or malformed files yield `None` (SDR also falls through on any config error).
fn read_version(file: &Path, keys: &[&str]) -> Option<String> {
    let text = std::fs::read_to_string(file).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    keys.iter().find_map(|key| match json.get(*key)? {
        serde_json::Value::String(s) => non_empty(s),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

/// Whether the legacy `.sfdx/sfdx-config.json` is merged over `config.json`
/// (`SF_SFDX_INTEROPERABILITY`, on by default in `@salesforce/core`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum LegacySfdxConfig {
    Merge,
    Ignore,
}

/// The version configured in one location (a project root or the home directory).
fn location_version(root: &Path, legacy_config: LegacySfdxConfig) -> Option<String> {
    let current = read_version(&root.join(".sf").join("config.json"), &["org-api-version"]);
    if legacy_config == LegacySfdxConfig::Merge
        && let Some(legacy) = read_version(
            &root.join(".sfdx").join("sfdx-config.json"),
            &["apiVersion", "org-api-version"],
        )
    {
        return Some(legacy);
    }
    current
}

/// Resolves the configured `org-api-version`, or `None` when nothing is configured.
pub(crate) fn org_api_version(project_dir: Option<&Path>, env: EnvLookup<'_>) -> Option<String> {
    for name in ["SF_ORG_API_VERSION", "SFDX_API_VERSION"] {
        if let Some(version) = env(name).as_deref().and_then(non_empty) {
            return Some(version);
        }
    }

    let legacy_config = match env("SF_SFDX_INTEROPERABILITY") {
        Some(value) if value.trim().eq_ignore_ascii_case("false") => LegacySfdxConfig::Ignore,
        _ => LegacySfdxConfig::Merge,
    };

    if let Some(dir) = project_dir
        && let Some(version) = location_version(dir, legacy_config)
    {
        return Some(version);
    }

    let home = ["HOME", "USERPROFILE"]
        .iter()
        .find_map(|name| env(name).as_deref().and_then(non_empty))
        .map(PathBuf::from)?;
    location_version(&home, legacy_config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |key| map.get(key).cloned()
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let file = root.join(rel);
        fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        fs::write(file, body).expect("write");
    }

    #[test]
    fn nothing_configured_is_none() {
        let project = tempfile::tempdir().expect("project");
        let home = tempfile::tempdir().expect("home");
        let env = env_of(&[("HOME", home.path().to_str().expect("utf8"))]);
        assert_eq!(org_api_version(Some(project.path()), &env), None);
        assert_eq!(org_api_version(None, &env), None);
    }

    #[test]
    fn environment_beats_every_file_and_sf_name_beats_synonym() {
        let project = tempfile::tempdir().expect("project");
        write(
            project.path(),
            ".sf/config.json",
            r#"{"org-api-version":"60.0"}"#,
        );
        let both = env_of(&[("SFDX_API_VERSION", "61.0"), ("SF_ORG_API_VERSION", "62.0")]);
        assert_eq!(
            org_api_version(Some(project.path()), &both).as_deref(),
            Some("62.0")
        );
        let synonym = env_of(&[("SFDX_API_VERSION", "61.0")]);
        assert_eq!(
            org_api_version(Some(project.path()), &synonym).as_deref(),
            Some("61.0")
        );
    }

    #[test]
    fn blank_environment_values_are_ignored() {
        let project = tempfile::tempdir().expect("project");
        write(
            project.path(),
            ".sf/config.json",
            r#"{"org-api-version":"60.0"}"#,
        );
        let env = env_of(&[("SF_ORG_API_VERSION", "   ")]);
        assert_eq!(
            org_api_version(Some(project.path()), &env).as_deref(),
            Some("60.0")
        );
    }

    #[test]
    fn local_config_beats_global_config() {
        let project = tempfile::tempdir().expect("project");
        let home = tempfile::tempdir().expect("home");
        write(
            project.path(),
            ".sf/config.json",
            r#"{"org-api-version":"60.0"}"#,
        );
        write(
            home.path(),
            ".sf/config.json",
            r#"{"org-api-version":"59.0"}"#,
        );
        let env = env_of(&[("HOME", home.path().to_str().expect("utf8"))]);
        assert_eq!(
            org_api_version(Some(project.path()), &env).as_deref(),
            Some("60.0")
        );
        assert_eq!(
            org_api_version(None, &env).as_deref(),
            Some("59.0"),
            "no project: global only"
        );
    }

    #[test]
    fn legacy_sfdx_config_overrides_config_json_in_the_same_location() {
        let project = tempfile::tempdir().expect("project");
        write(
            project.path(),
            ".sf/config.json",
            r#"{"org-api-version":"60.0"}"#,
        );
        write(
            project.path(),
            ".sfdx/sfdx-config.json",
            r#"{"apiVersion":"58.0"}"#,
        );
        let env = env_of(&[]);
        assert_eq!(
            org_api_version(Some(project.path()), &env).as_deref(),
            Some("58.0")
        );
    }

    #[test]
    fn legacy_file_alone_is_honored_and_can_be_disabled() {
        let home = tempfile::tempdir().expect("home");
        write(
            home.path(),
            ".sfdx/sfdx-config.json",
            r#"{"apiVersion":"57.0"}"#,
        );
        let home_str = home.path().to_str().expect("utf8");
        assert_eq!(
            org_api_version(None, &env_of(&[("HOME", home_str)])).as_deref(),
            Some("57.0")
        );
        let disabled = env_of(&[("HOME", home_str), ("SF_SFDX_INTEROPERABILITY", "false")]);
        assert_eq!(org_api_version(None, &disabled), None);
    }

    #[test]
    fn windows_style_home_variable_is_used_when_home_is_unset() {
        let home = tempfile::tempdir().expect("home");
        write(
            home.path(),
            ".sf/config.json",
            r#"{"org-api-version":"56.0"}"#,
        );
        let env = env_of(&[("USERPROFILE", home.path().to_str().expect("utf8"))]);
        assert_eq!(org_api_version(None, &env).as_deref(), Some("56.0"));
    }

    #[test]
    fn numbers_are_accepted_and_malformed_or_empty_files_fall_through_to_global() {
        let project = tempfile::tempdir().expect("project");
        let home = tempfile::tempdir().expect("home");
        write(project.path(), ".sf/config.json", "{ not json");
        write(
            home.path(),
            ".sf/config.json",
            r#"{"org-api-version":63.0}"#,
        );
        let env = env_of(&[("HOME", home.path().to_str().expect("utf8"))]);
        assert_eq!(
            org_api_version(Some(project.path()), &env).as_deref(),
            Some("63.0")
        );
        write(
            project.path(),
            ".sf/config.json",
            r#"{"org-api-version":""}"#,
        );
        assert_eq!(
            org_api_version(Some(project.path()), &env).as_deref(),
            Some("63.0")
        );
    }
}
