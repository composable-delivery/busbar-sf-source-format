//! `sfdx-project.json` replacement marking and binary-safe substitution.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::registry::{ReplacementConfig, ReplacementSource, ReplacementTarget, SfdxProjectConfig};
use globset::Glob;
use regex::Regex;

use crate::convert::error::ConvertError;
use crate::convert::types::MarkedReplacement;
use crate::resolve::SourceComponent;

/// Checks whether a byte buffer appears binary (NUL bytes or invalid UTF-8).
pub(crate) fn is_binary_bytes(bytes: &[u8]) -> bool {
    bytes.contains(&0) || std::str::from_utf8(bytes).is_err()
}

/// Marks matching files on `components` with compiled [`MarkedReplacement`] rules from
/// `sfdx-project.json` when conversion replacements are active.
pub(crate) fn mark_replacements(
    components: &mut [SourceComponent],
    project_dir: Option<&Path>,
) -> Result<(), ConvertError> {
    let Some(dir) = project_dir else {
        return Ok(());
    };
    let Some((project_file, config)) = SfdxProjectConfig::find_and_load(dir)? else {
        return Ok(());
    };
    if config.replacements.is_empty() {
        return Ok(());
    }

    let project_root = project_file.parent().unwrap_or(dir);
    let compiled = compile_rules(&config.replacements, project_root)?;
    if compiled.is_empty() {
        return Ok(());
    }

    for comp in components {
        let mut candidates = comp.walk_content()?;
        if let Some(xml) = comp.xml() {
            candidates.push(xml.to_path_buf());
        }
        let mut by_file: BTreeMap<PathBuf, Vec<MarkedReplacement>> = BTreeMap::new();
        for file_path in candidates {
            let rel_path = file_path.strip_prefix(project_root).unwrap_or(&file_path);
            let rel_posix = rel_path.to_string_lossy().replace('\\', "/");
            for rule in &compiled {
                if rule.matches(project_root, &file_path, &rel_posix) {
                    by_file
                        .entry(file_path.clone())
                        .or_default()
                        .push(MarkedReplacement {
                            to_replace: rule.regex.clone(),
                            replace_with: rule.replace_with.clone(),
                        });
                }
            }
        }
        if !by_file.is_empty() {
            comp.replacements = by_file;
        }
    }

    Ok(())
}

/// Applies compiled replacements sequentially to `input`.
pub(crate) fn apply_replacements(input: &str, rules: &[MarkedReplacement]) -> String {
    let mut current = input.to_owned();
    for rule in rules {
        current = rule
            .to_replace
            .replace_all(&current, rule.replace_with.as_str())
            .into_owned();
    }
    current
}

struct CompiledRule {
    exact_file: Option<PathBuf>,
    glob: Option<globset::GlobMatcher>,
    regex: Regex,
    replace_with: String,
}

impl CompiledRule {
    fn matches(&self, project_root: &Path, abs_file: &Path, rel_posix: &str) -> bool {
        if let Some(exact) = &self.exact_file {
            return abs_file == exact || project_root.join(rel_posix) == *exact;
        }
        if let Some(glob) = &self.glob {
            return glob.is_match(rel_posix);
        }
        false
    }
}

fn compile_rules(
    configs: &[ReplacementConfig],
    project_root: &Path,
) -> Result<Vec<CompiledRule>, ConvertError> {
    let mut compiled = Vec::new();
    for cfg in configs {
        let all_env_match = cfg.replace_when_env.iter().all(|cond| {
            env::var(&cond.env).is_ok_and(|val| val == cond.value.as_env_str())
        });
        if !all_env_match {
            continue;
        }

        let replace_with = match &cfg.source {
            ReplacementSource::Env {
                replace_with_env,
                allow_unset_env_variable,
            } => match env::var(replace_with_env) {
                Ok(v) => v,
                Err(_) if *allow_unset_env_variable == Some(true) => String::new(),
                Err(_) => {
                    return Err(ConvertError::MissingEnvVar {
                        var_name: replace_with_env.clone(),
                    });
                }
            },
            ReplacementSource::File { replace_with_file } => {
                let p = project_root.join(replace_with_file);
                fs::read_to_string(&p).map_err(|source| ConvertError::Io { path: p, source })?
            }
        };

        let regex = match &cfg.target {
            ReplacementTarget::Literal { string_to_replace } => {
                let escaped = regex::escape(string_to_replace);
                Regex::new(&escaped).map_err(|source| ConvertError::InvalidRegex {
                    pattern: escaped,
                    source,
                })?
            }
            ReplacementTarget::Regex { regex_to_replace } => {
                Regex::new(regex_to_replace).map_err(|source| ConvertError::InvalidRegex {
                    pattern: regex_to_replace.clone(),
                    source,
                })?
            }
        };

        let exact_file = cfg.filename.as_ref().map(|f| project_root.join(f));
        let glob = match &cfg.glob {
            Some(pattern) => Some(
                Glob::new(pattern)
                    .map_err(|source| ConvertError::InvalidGlob {
                        pattern: pattern.clone(),
                        source,
                    })?
                    .compile_matcher(),
            ),
            None => None,
        };

        compiled.push(CompiledRule {
            exact_file,
            glob,
            regex,
            replace_with,
        });
    }
    Ok(compiled)
}
