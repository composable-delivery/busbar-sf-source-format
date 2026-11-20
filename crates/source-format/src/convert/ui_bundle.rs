//! Deploy-time validation for `UIBundle` (`ui-bundle.json` and `mcp-resources.json`),
//! matching SDR's `src/resolve/adapters/uiBundleValidation.ts`.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::OnceLock;

use crate::tree::{ForceIgnore, TreeContainer, normalize_lexical};
use regex::Regex;
use serde_json::{Map, Value};

use crate::convert::error::ConvertError;
use crate::resolve::TreeHandle;

const MAX_UI_BUNDLE_JSON_BYTES: usize = 102_400;
const MAX_MCP_RESOURCES_JSON_BYTES: usize = 102_400;
const MAX_RECURSION_DEPTH: usize = 20;
const MAX_MCP_RESOURCE_PATH_LENGTH: usize = 512;
const UI_BUNDLE_JSON: &str = "ui-bundle.json";
const MCP_RESOURCES_JSON: &str = "mcp-resources.json";

static MCP_RESOURCE_NAME_RE: OnceLock<Regex> = OnceLock::new();

fn mcp_resource_name_regex() -> &'static Regex {
    MCP_RESOURCE_NAME_RE.get_or_init(|| {
        Regex::new(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,79}$")
            .expect("mcp resource name regex is a valid literal")
    })
}

fn config_err(message: impl Into<String>) -> ConvertError {
    ConvertError::UiBundleValidation {
        message: message.into(),
    }
}

/// Validates optional `ui-bundle.json` and `mcp-resources.json` files inside a `UIBundle`
/// directory prior to deployment when backed by the local filesystem.
pub(crate) fn validate_ui_bundle_for_deploy(
    content_path: &Path,
    tree: &TreeHandle,
    force_ignore: &ForceIgnore,
) -> Result<(), ConvertError> {
    if !tree.is_fs() {
        return Ok(());
    }

    let descriptor_path = content_path.join(UI_BUNDLE_JSON);
    if tree.exists(&descriptor_path) && !force_ignore.denies(&descriptor_path) {
        let raw = tree.read_file(&descriptor_path)?;
        validate_ui_bundle_json(&raw, &descriptor_path, content_path, tree)?;
    }

    let mcp_path = content_path.join(MCP_RESOURCES_JSON);
    if tree.exists(&mcp_path) && !force_ignore.denies(&mcp_path) {
        let raw = tree.read_file(&mcp_path)?;
        validate_mcp_resources_json(&raw, &mcp_path, content_path, tree)?;
    }

    Ok(())
}

fn validate_ui_bundle_json(
    raw: &[u8],
    descriptor_path: &Path,
    content_path: &Path,
    tree: &TreeHandle,
) -> Result<(), ConvertError> {
    if raw.is_empty() {
        return Err(config_err(format!(
            "ui-bundle.json at '{}' is empty",
            descriptor_path.display()
        )));
    }
    if raw.len() > MAX_UI_BUNDLE_JSON_BYTES {
        return Err(config_err(format!(
            "ui-bundle.json exceeds maximum allowed size of 100 KB ({} bytes)",
            raw.len()
        )));
    }
    let text = std::str::from_utf8(raw)
        .map_err(|err| config_err(format!("ui-bundle.json is not valid UTF-8: {err}")))?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(config_err(format!(
            "ui-bundle.json at '{}' contains only whitespace",
            descriptor_path.display()
        )));
    }

    let parsed: Value = serde_json::from_str(trimmed)
        .map_err(|err| config_err(format!("invalid JSON in ui-bundle.json: {err}")))?;
    let Some(obj) = parsed.as_object() else {
        return Err(config_err(format!(
            "ui-bundle.json must be a JSON object, got {}",
            describe_value_type(&parsed)
        )));
    };
    if obj.is_empty() {
        return Err(config_err("ui-bundle.json cannot be an empty object"));
    }

    let allowed: HashSet<&str> = ["outputDir", "routing", "headers"].into_iter().collect();
    let unknown: Vec<&str> = obj
        .keys()
        .map(String::as_str)
        .filter(|k| !allowed.contains(k))
        .collect();
    if !unknown.is_empty() {
        return Err(config_err(format!(
            "unknown ui-bundle.json property(s): {}",
            unknown.join(", ")
        )));
    }

    let output_dir = match obj.get("outputDir") {
        Some(v) => Some(validate_output_dir(v)?),
        None => None,
    };
    if let Some(routing) = obj.get("routing") {
        validate_routing(routing)?;
    }
    if let Some(headers) = obj.get("headers") {
        validate_headers(headers)?;
    }

    if output_dir.is_some() || obj.contains_key("routing") {
        validate_file_existence(obj, output_dir.as_deref(), content_path, tree)?;
    }
    Ok(())
}

fn validate_mcp_resources_json(
    raw: &[u8],
    descriptor_path: &Path,
    content_path: &Path,
    tree: &TreeHandle,
) -> Result<(), ConvertError> {
    let trimmed = std::str::from_utf8(raw)
        .map(str::trim)
        .map_err(|err| config_err(format!("mcp-resources.json is not valid UTF-8: {err}")))?;
    if raw.is_empty() || trimmed.is_empty() {
        return Err(config_err(format!(
            "mcp-resources.json at '{}' is empty",
            descriptor_path.display()
        )));
    }
    if raw.len() > MAX_MCP_RESOURCES_JSON_BYTES {
        return Err(config_err(format!(
            "mcp-resources.json exceeds maximum allowed size of 100 KB ({} bytes)",
            raw.len()
        )));
    }

    let parsed: Value = serde_json::from_str(trimmed)
        .map_err(|err| config_err(format!("invalid JSON in mcp-resources.json: {err}")))?;
    let Some(obj) = parsed.as_object() else {
        return Err(config_err(format!(
            "mcp-resources.json must be a JSON object, got {}",
            describe_value_type(&parsed)
        )));
    };
    let unknown: Vec<&str> = obj
        .keys()
        .map(String::as_str)
        .filter(|k| *k != "mcpResources")
        .collect();
    if !unknown.is_empty() {
        return Err(config_err(format!(
            "unknown mcp-resources.json property(s): {}",
            unknown.join(", ")
        )));
    }
    let Some(resources_val) = obj.get("mcpResources") else {
        return Err(config_err("mcp-resources.json is missing 'mcpResources'"));
    };

    let resources = validate_mcp_resources(resources_val)?;
    validate_mcp_resource_files(&resources, content_path, tree)
}

fn validate_output_dir(val: &Value) -> Result<String, ConvertError> {
    let Some(raw) = val.as_str() else {
        return Err(config_err(format!(
            "outputDir must be a string, got {}",
            describe_value_type(val)
        )));
    };
    let stripped = strip_leading_slashes(raw);
    if stripped.is_empty() {
        return Err(config_err("outputDir cannot be empty"));
    }
    assert_safe_path(stripped, "outputDir")?;
    Ok(stripped.to_owned())
}

fn validate_routing(val: &Value) -> Result<(), ConvertError> {
    let Some(routing) = val.as_object() else {
        return Err(config_err(format!(
            "routing must be an object, got {}",
            describe_value_type(val)
        )));
    };
    if routing.is_empty() {
        return Err(config_err("routing must specify at least one property"));
    }
    let allowed: HashSet<&str> = [
        "rewrites",
        "redirects",
        "fallback",
        "trailingSlash",
        "fileBasedRouting",
    ]
    .into_iter()
    .collect();
    let unknown: Vec<&str> = routing
        .keys()
        .map(String::as_str)
        .filter(|k| !allowed.contains(k))
        .collect();
    if !unknown.is_empty() {
        return Err(config_err(format!(
            "unknown routing property(s): {}",
            unknown.join(", ")
        )));
    }
    if let Some(ts) = routing.get("trailingSlash") {
        let Some(ts_str) = ts.as_str() else {
            return Err(config_err("routing.trailingSlash must be a string"));
        };
        if !matches!(ts_str, "always" | "never" | "auto") {
            return Err(config_err(format!(
                "invalid routing.trailingSlash '{ts_str}' (expected always, never, or auto)"
            )));
        }
    }
    if let Some(fbr) = routing.get("fileBasedRouting")
        && !fbr.is_boolean()
    {
        return Err(config_err("routing.fileBasedRouting must be a boolean"));
    }
    if let Some(fb) = routing.get("fallback") {
        let Some(fb_str) = fb.as_str() else {
            return Err(config_err("routing.fallback must be a string"));
        };
        let stripped = strip_leading_slashes(fb_str);
        if stripped.is_empty() {
            return Err(config_err("routing.fallback cannot be empty"));
        }
        assert_safe_path(stripped, "routing.fallback")?;
    }
    if let Some(rewrites) = routing.get("rewrites") {
        validate_rewrites(rewrites)?;
    }
    if let Some(redirects) = routing.get("redirects") {
        validate_redirects(redirects)?;
    }
    Ok(())
}

fn validate_rewrites(val: &Value) -> Result<(), ConvertError> {
    let Some(arr) = val.as_array() else {
        return Err(config_err("routing.rewrites must be an array"));
    };
    if arr.is_empty() {
        return Err(config_err("routing.rewrites cannot be empty"));
    }
    for (idx, item) in arr.iter().enumerate() {
        let key = format!("routing.rewrites[{idx}]");
        let Some(obj) = item.as_object() else {
            return Err(config_err(format!("{key} must be an object")));
        };
        if obj.is_empty() {
            return Err(config_err(format!("{key} cannot be empty")));
        }
        for k in obj.keys() {
            if k != "route" && k != "rewrite" {
                return Err(config_err(format!("unknown property '{k}' in {key}")));
            }
        }
        if let Some(route) = obj.get("route")
            && route.as_str().is_none_or(str::is_empty)
        {
            return Err(config_err(format!(
                "{key}.route must be a non-empty string"
            )));
        }
        if let Some(rewrite) = obj.get("rewrite") {
            let Some(s) = rewrite.as_str().filter(|s| !s.is_empty()) else {
                return Err(config_err(format!(
                    "{key}.rewrite must be a non-empty string"
                )));
            };
            let stripped = strip_leading_slashes(s);
            if stripped.is_empty() {
                return Err(config_err(format!("{key}.rewrite cannot be empty")));
            }
            assert_safe_path(stripped, &format!("{key}.rewrite"))?;
        }
    }
    Ok(())
}

fn validate_redirects(val: &Value) -> Result<(), ConvertError> {
    let Some(arr) = val.as_array() else {
        return Err(config_err("routing.redirects must be an array"));
    };
    if arr.is_empty() {
        return Err(config_err("routing.redirects cannot be empty"));
    }
    for (idx, item) in arr.iter().enumerate() {
        let key = format!("routing.redirects[{idx}]");
        let Some(obj) = item.as_object() else {
            return Err(config_err(format!("{key} must be an object")));
        };
        if obj.is_empty() {
            return Err(config_err(format!("{key} cannot be empty")));
        }
        for k in obj.keys() {
            if k != "route" && k != "redirect" && k != "statusCode" {
                return Err(config_err(format!("unknown property '{k}' in {key}")));
            }
        }
        if let Some(route) = obj.get("route")
            && route.as_str().is_none_or(str::is_empty)
        {
            return Err(config_err(format!(
                "{key}.route must be a non-empty string"
            )));
        }
        if let Some(redirect) = obj.get("redirect")
            && redirect.as_str().is_none_or(str::is_empty)
        {
            return Err(config_err(format!(
                "{key}.redirect must be a non-empty string"
            )));
        }
        if let Some(code) = obj.get("statusCode") {
            let valid = code
                .as_i64()
                .is_some_and(|n| matches!(n, 301 | 302 | 307 | 308));
            if !valid {
                return Err(config_err(format!(
                    "invalid {key}.statusCode {code} (expected 301, 302, 307, or 308)"
                )));
            }
        }
    }
    Ok(())
}

fn validate_headers(val: &Value) -> Result<(), ConvertError> {
    let Some(arr) = val.as_array() else {
        return Err(config_err("headers must be an array"));
    };
    if arr.is_empty() {
        return Err(config_err("headers cannot be empty"));
    }
    for (i, item) in arr.iter().enumerate() {
        let key = format!("headers[{i}]");
        let Some(obj) = item.as_object() else {
            return Err(config_err(format!("{key} must be an object")));
        };
        if obj.is_empty() {
            return Err(config_err(format!("{key} cannot be empty")));
        }
        for k in obj.keys() {
            if k != "source" && k != "headers" {
                return Err(config_err(format!("unknown property '{k}' in {key}")));
            }
        }
        if let Some(src) = obj.get("source")
            && src.as_str().is_none_or(str::is_empty)
        {
            return Err(config_err(format!(
                "{key}.source must be a non-empty string"
            )));
        }
        if let Some(kv_val) = obj.get("headers") {
            let Some(kvs) = kv_val.as_array() else {
                return Err(config_err(format!("{key}.headers must be an array")));
            };
            if kvs.is_empty() {
                return Err(config_err(format!("{key}.headers cannot be empty")));
            }
            for (j, kv) in kvs.iter().enumerate() {
                let kv_key = format!("{key}.headers[{j}]");
                let Some(kv_obj) = kv.as_object() else {
                    return Err(config_err(format!("{kv_key} must be an object")));
                };
                if kv_obj.is_empty() {
                    return Err(config_err(format!("{kv_key} cannot be empty")));
                }
                for k in kv_obj.keys() {
                    if k != "key" && k != "value" {
                        return Err(config_err(format!("unknown property '{k}' in {kv_key}")));
                    }
                }
                if let Some(k) = kv_obj.get("key")
                    && k.as_str().is_none_or(str::is_empty)
                {
                    return Err(config_err(format!(
                        "{kv_key}.key must be a non-empty string"
                    )));
                }
                if let Some(v) = kv_obj.get("value")
                    && v.as_str().is_none_or(str::is_empty)
                {
                    return Err(config_err(format!(
                        "{kv_key}.value must be a non-empty string"
                    )));
                }
            }
        }
    }
    Ok(())
}

struct McpResource {
    path: String,
}

fn validate_mcp_resources(val: &Value) -> Result<Vec<McpResource>, ConvertError> {
    let Some(arr) = val.as_array() else {
        return Err(config_err("mcpResources must be an array"));
    };
    if arr.is_empty() {
        return Err(config_err("mcpResources cannot be empty"));
    }
    let mut seen_names: HashMap<String, usize> = HashMap::new();
    let mut out = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        let key = format!("mcpResources[{i}]");
        let Some(obj) = item.as_object() else {
            return Err(config_err(format!("{key} must be an object")));
        };
        if let Some(unknown) = obj.keys().find(|k| *k != "name" && *k != "path") {
            return Err(config_err(format!("unknown property '{unknown}' in {key}")));
        }
        let name = obj
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if name.is_empty() {
            return Err(config_err(format!("{key}.name must be a non-empty string")));
        }
        if !mcp_resource_name_regex().is_match(name) {
            return Err(config_err(format!(
                "invalid MCP resource name '{name}' at {key}.name"
            )));
        }
        let path = obj
            .get("path")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if path.is_empty() {
            return Err(config_err(format!("{key}.path must be a non-empty string")));
        }
        if path.len() > MAX_MCP_RESOURCE_PATH_LENGTH {
            return Err(config_err(format!(
                "{key}.path exceeds max length {MAX_MCP_RESOURCE_PATH_LENGTH}"
            )));
        }
        let problems = mcp_path_problems(path);
        if !problems.is_empty() {
            return Err(config_err(format!(
                "unsafe MCP resource path '{path}' at {key}.path: {}",
                problems.join(", ")
            )));
        }
        if !path.to_ascii_lowercase().ends_with(".html") {
            return Err(config_err(format!(
                "MCP resource path '{path}' at {key}.path must point to an .html file"
            )));
        }
        if let Some(first) = seen_names.insert(name.to_owned(), i) {
            return Err(config_err(format!(
                "duplicate MCP resource name '{name}' at {key}.name (first seen at mcpResources[{first}])"
            )));
        }
        let canonical = path
            .split('/')
            .filter(|seg| *seg != ".")
            .collect::<Vec<_>>()
            .join("/");
        out.push(McpResource { path: canonical });
    }
    Ok(out)
}

fn validate_file_existence(
    obj: &Map<String, Value>,
    output_dir: Option<&str>,
    content_path: &Path,
    tree: &TreeHandle,
) -> Result<(), ConvertError> {
    let mut base_path = content_path.to_path_buf();
    if let Some(dir) = output_dir {
        let output_dir_path = content_path.join(dir);
        assert_no_traversal(&output_dir_path, content_path, "outputDir", dir)?;
        if normalize_abs(&output_dir_path) == normalize_abs(content_path) {
            return Err(config_err(format!(
                "outputDir '{dir}' cannot resolve to bundle root"
            )));
        }
        if !tree.exists(&output_dir_path) || !tree.is_directory(&output_dir_path)? {
            return Err(config_err(format!("ui-bundle outputDir '{dir}' not found")));
        }
        if !has_file_under(&output_dir_path, tree, 0)? {
            return Err(config_err(format!("ui-bundle outputDir '{dir}' is empty")));
        }
        base_path = output_dir_path;
    }

    if let Some(routing) = obj.get("routing").and_then(Value::as_object) {
        if let Some(fb) = routing.get("fallback").and_then(Value::as_str) {
            let stripped = strip_leading_slashes(fb);
            let target = base_path.join(stripped);
            assert_no_traversal(&target, &base_path, "routing.fallback", fb)?;
            if !tree.exists(&target) {
                return Err(config_err(format!(
                    "routing.fallback file '{fb}' not found"
                )));
            }
        }
        if let Some(rewrites) = routing.get("rewrites").and_then(Value::as_array) {
            for (i, rw) in rewrites.iter().enumerate() {
                if let Some(target_str) = rw.get("rewrite").and_then(Value::as_str) {
                    let stripped = strip_leading_slashes(target_str);
                    let target = base_path.join(stripped);
                    let key = format!("routing.rewrites[{i}].rewrite");
                    assert_no_traversal(&target, &base_path, &key, target_str)?;
                    if !tree.exists(&target) {
                        return Err(config_err(format!("{key} file '{target_str}' not found")));
                    }
                }
            }
        }
    }
    Ok(())
}

fn validate_mcp_resource_files(
    resources: &[McpResource],
    content_path: &Path,
    tree: &TreeHandle,
) -> Result<(), ConvertError> {
    for (i, res) in resources.iter().enumerate() {
        let full = content_path.join(&res.path);
        assert_no_traversal(
            &full,
            content_path,
            &format!("mcpResources[{i}].path"),
            &res.path,
        )?;
        if !tree.exists(&full) || tree.is_directory(&full)? {
            return Err(config_err(format!(
                "MCP resource file '{}' not found for mcpResources[{i}].path",
                res.path
            )));
        }
    }
    Ok(())
}

fn has_file_under(dir_path: &Path, tree: &TreeHandle, depth: usize) -> Result<bool, ConvertError> {
    if depth >= MAX_RECURSION_DEPTH {
        return Ok(false);
    }
    for entry in tree.read_directory(dir_path)? {
        let full = dir_path.join(entry);
        if tree.exists(&full) {
            if tree.is_directory(&full)? {
                if has_file_under(&full, tree, depth + 1)? {
                    return Ok(true);
                }
            } else {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn strip_leading_slashes(s: &str) -> &str {
    s.trim_start_matches('/')
}

fn unsafe_path_reason(value: &str) -> Option<&'static str> {
    if value == ".."
        || value.starts_with("../")
        || value.starts_with("..\\")
        || value.contains("/../")
        || value.contains("\\..\\")
        || value.contains("/..\\")
        || value.contains("\\../")
        || value.ends_with("/..")
        || value.ends_with("\\..")
    {
        return Some("path traversal (..)");
    }
    if value == "." || value == "./" {
        return Some("current directory reference");
    }
    if value.starts_with('/') || value.starts_with('\\') {
        return Some("absolute path");
    }
    if value.contains('\0') {
        return Some("null byte");
    }
    if value.bytes().any(|b| b < 0x20) {
        return Some("control character");
    }
    if value.contains('*') || value.contains('?') {
        return Some("glob wildcard");
    }
    if value.contains('\\') {
        return Some("backslash");
    }
    if value.contains('%') {
        return Some("percent-encoding");
    }
    None
}

fn assert_safe_path(value: &str, config_key: &str) -> Result<(), ConvertError> {
    if let Some(reason) = unsafe_path_reason(value) {
        return Err(config_err(format!(
            "unsafe path '{value}' in {config_key}: {reason}"
        )));
    }
    Ok(())
}

fn mcp_path_problems(value: &str) -> Vec<&'static str> {
    let mut reasons = unsafe_path_reason(value).into_iter().collect::<Vec<_>>();
    if value.contains("//") {
        reasons.push("consecutive slashes (//)");
    }
    if value.contains('#') {
        reasons.push("a fragment marker (#)");
    }
    if value.contains(':') {
        reasons.push("a colon (:)");
    }
    reasons
}

fn assert_no_traversal(
    resolved: &Path,
    parent_dir: &Path,
    config_key: &str,
    raw_value: &str,
) -> Result<(), ConvertError> {
    let norm_res = normalize_abs(resolved);
    let norm_parent = normalize_abs(parent_dir);
    if !norm_res.starts_with(&norm_parent) {
        return Err(config_err(format!(
            "path traversal outside bundle in {config_key}: '{raw_value}'"
        )));
    }
    Ok(())
}

fn normalize_abs(path: &Path) -> std::path::PathBuf {
    normalize_lexical(&std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()))
}

fn describe_value_type(val: &Value) -> &'static str {
    match val {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
