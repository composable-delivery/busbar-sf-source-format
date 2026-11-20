//! Representation of `sfdx-project.json` fields relevant to registry resolution and conversion.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::error::RegistryError;
use super::types::MetadataRegistryPatch;

/// Subset of `sfdx-project.json` used by `busbar-sf-source-format`.
///
/// Unknown top-level keys (such as CLI plugin settings) are ignored so real
/// Salesforce DX project manifests parse cleanly.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SfdxProjectConfig {
    /// Optional project name.
    #[serde(default)]
    pub name: Option<String>,
    /// Optional managed package namespace.
    #[serde(default)]
    pub namespace: Option<String>,
    /// Package directories configured for this project.
    #[serde(rename = "packageDirectories", default)]
    pub package_directories: Vec<PackageDirectory>,
    /// Project source API version (e.g., `"60.0"`).
    #[serde(rename = "sourceApiVersion", default)]
    pub source_api_version: Option<String>,
    /// Opt-in decomposition preset names.
    #[serde(rename = "sourceBehaviorOptions", default)]
    pub source_behavior_options: Vec<String>,
    /// Deprecated alias for `sourceBehaviorOptions`.
    #[serde(rename = "registryPresets", default)]
    pub registry_presets: Vec<String>,
    /// Custom metadata registry overrides declared directly in `sfdx-project.json`.
    #[serde(rename = "registryCustomizations", default)]
    pub registry_customizations: Option<MetadataRegistryPatch>,
    /// String/regex replacements applied during conversion.
    #[serde(default)]
    pub replacements: Vec<ReplacementConfig>,
}

/// A package directory entry in `sfdx-project.json`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PackageDirectory {
    /// Relative path to the package directory.
    pub path: String,
    /// Whether this directory is the project's default package directory.
    #[serde(default)]
    pub default: Option<bool>,
}

/// A single replacement rule from `sfdx-project.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplacementConfig {
    /// Exact project-relative file path to target (mutually exclusive with `glob`).
    #[serde(default)]
    pub filename: Option<String>,
    /// Glob pattern matching files to target (mutually exclusive with `filename`).
    #[serde(default)]
    pub glob: Option<String>,
    /// Target string or regex pattern to replace.
    #[serde(flatten)]
    pub target: ReplacementTarget,
    /// Source supplying the replacement text.
    #[serde(flatten)]
    pub source: ReplacementSource,
    /// Environment conditions that must all match for this rule to apply.
    #[serde(rename = "replaceWhenEnv", default)]
    pub replace_when_env: Vec<EnvCondition>,
}

/// Pattern matched by a [`ReplacementConfig`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum ReplacementTarget {
    /// Exact literal substring to replace.
    Literal {
        /// Literal substring to find.
        #[serde(rename = "stringToReplace")]
        string_to_replace: String,
    },
    /// Regular expression pattern to replace.
    Regex {
        /// Regular expression string to compile.
        #[serde(rename = "regexToReplace")]
        regex_to_replace: String,
    },
}

/// Replacement text source in a [`ReplacementConfig`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum ReplacementSource {
    /// Replace matches with the value of an environment variable.
    Env {
        /// Name of the environment variable.
        #[serde(rename = "replaceWithEnv")]
        replace_with_env: String,
        /// Whether an unset variable evaluates to empty/skip instead of erroring.
        #[serde(rename = "allowUnsetEnvVariable", default)]
        allow_unset_env_variable: Option<bool>,
    },
    /// Replace matches with the contents of a file.
    File {
        /// Path to the file supplying replacement text.
        #[serde(rename = "replaceWithFile")]
        replace_with_file: String,
    },
}

/// Environment variable predicate inside `replaceWhenEnv`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvCondition {
    /// Environment variable name to inspect.
    pub env: String,
    /// Expected string, numeric, or boolean value.
    pub value: EnvConditionValue,
}

/// Expected value inside an [`EnvCondition`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum EnvConditionValue {
    /// String value.
    String(String),
    /// Signed integer value.
    Integer(i64),
    /// Boolean value.
    Bool(bool),
}

impl EnvConditionValue {
    /// Returns the expected value formatted as a string for comparison with env vars.
    pub fn as_env_str(&self) -> String {
        match self {
            Self::String(s) => s.clone(),
            Self::Integer(n) => n.to_string(),
            Self::Bool(b) => b.to_string(),
        }
    }
}

impl SfdxProjectConfig {
    /// Searches upward from `start_dir` for `sfdx-project.json` and parses it if present.
    ///
    /// Returns `Ok(None)` when no `sfdx-project.json` exists in `start_dir` or any ancestor.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Io`] if an existing `sfdx-project.json` cannot be read, or
    /// [`RegistryError::Json`] if its contents are invalid JSON.
    pub fn find_and_load(start_dir: &Path) -> Result<Option<(PathBuf, Self)>, RegistryError> {
        let Some(project_file) = find_project_file(start_dir) else {
            return Ok(None);
        };
        let contents = fs::read_to_string(&project_file).map_err(|source| RegistryError::Io {
            path: project_file.clone(),
            source,
        })?;
        let config: Self =
            serde_json::from_str(&contents).map_err(|source| RegistryError::Json {
                context: project_file.display().to_string(),
                source,
            })?;
        Ok(Some((project_file, config)))
    }

    /// Returns the path of the default package directory, falling back to the first entry.
    pub fn default_package_dir(&self) -> Option<&str> {
        self.package_directories
            .iter()
            .find(|dir| dir.default == Some(true))
            .or_else(|| self.package_directories.first())
            .map(|dir| dir.path.as_str())
    }
}

fn find_project_file(start_dir: &Path) -> Option<PathBuf> {
    let mut current = if start_dir.is_file() {
        start_dir.parent()?
    } else {
        start_dir
    };

    loop {
        let candidate = current.join("sfdx-project.json");
        if candidate.is_file() {
            return Some(candidate);
        }
        current = current.parent()?;
    }
}
