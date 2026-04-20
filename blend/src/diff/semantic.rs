use console::style;
use indexmap::IndexMap;

use crate::formats::get_renderer;
use crate::nickel::Format;

use super::DiffResult;

/// Type of change detected for a single key path
#[derive(Debug, Clone, PartialEq)]
pub enum KeyChangeType {
    /// Key exists in repo config but not in deployed file
    Added,
    /// Key exists in deployed file but not in repo config
    Removed,
    /// Key exists in both but with different values
    Modified,
}

/// A single key-level change between repo and deployed configs
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct KeyChange {
    /// Dotted key path, e.g. "section.subsection.key"
    pub path: String,
    /// What kind of change this is
    pub change_type: KeyChangeType,
    /// Value in the repo config (None for Removed)
    pub repo_value: Option<serde_json::Value>,
    /// Value in the deployed file (None for Added)
    pub deployed_value: Option<serde_json::Value>,
    /// Formatted display line for this change
    pub display: String,
}

/// Compute per-key diff between two structured configs, returning individual
/// `KeyChange` entries instead of a monolithic string.
///
/// Reuses the same recursive comparison logic as `semantic_diff` but collects
/// structured results.
pub fn semantic_diff_keys(
    format: Format,
    generated: &str,
    deployed: &str,
    ignore_keys: &[String],
) -> Vec<KeyChange> {
    let renderer = get_renderer(format);

    let gen_value = match renderer.parse(generated) {
        Ok(v) => v,
        Err(_) => return vec![],
    };

    let dep_value = match renderer.parse(deployed) {
        Ok(v) => v,
        Err(_) => return vec![],
    };

    let gen_filtered = filter_keys(&gen_value, ignore_keys);
    let dep_filtered = filter_keys(&dep_value, ignore_keys);

    let mut changes = Vec::new();
    collect_key_changes(&gen_filtered, &dep_filtered, "", &mut changes);
    changes
}

/// Recursively collect per-key changes between two JSON values
fn collect_key_changes(
    generated: &serde_json::Value,
    deployed: &serde_json::Value,
    path: &str,
    changes: &mut Vec<KeyChange>,
) {
    if generated == deployed {
        return;
    }

    // Treat numbers as equal if their float values match (e.g., 12 == 12.0)
    if let (serde_json::Value::Number(g), serde_json::Value::Number(d)) = (generated, deployed)
        && g.as_f64() == d.as_f64()
    {
        return;
    }

    match (generated, deployed) {
        (serde_json::Value::Object(gen_obj), serde_json::Value::Object(dep_obj)) => {
            // Keys in generated but not in deployed (additions)
            for (key, gen_val) in gen_obj {
                let key_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };

                if let Some(dep_val) = dep_obj.get(key) {
                    collect_key_changes(gen_val, dep_val, &key_path, changes);
                } else {
                    let display = format!(
                        "{} {} = {}",
                        style("+").green(),
                        style(&key_path).green(),
                        style(format_value(gen_val)).green()
                    );
                    changes.push(KeyChange {
                        path: key_path,
                        change_type: KeyChangeType::Added,
                        repo_value: Some(gen_val.clone()),
                        deployed_value: None,
                        display,
                    });
                }
            }

            // Keys in deployed but not in generated (removals)
            for (key, dep_val) in dep_obj {
                let key_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };

                if !gen_obj.contains_key(key) {
                    let display = format!(
                        "{} {} = {}",
                        style("-").red(),
                        style(&key_path).red(),
                        style(format_value(dep_val)).red()
                    );
                    changes.push(KeyChange {
                        path: key_path,
                        change_type: KeyChangeType::Removed,
                        repo_value: None,
                        deployed_value: Some(dep_val.clone()),
                        display,
                    });
                }
            }
        }
        (serde_json::Value::Array(gen_arr), serde_json::Value::Array(dep_arr)) => {
            if gen_arr != dep_arr {
                let display = format!(
                    "{} {} = {}",
                    style("~").yellow(),
                    style(path).yellow(),
                    style(format!(
                        "{} -> {}",
                        format_array(dep_arr),
                        format_array(gen_arr)
                    ))
                    .yellow()
                );
                changes.push(KeyChange {
                    path: path.to_string(),
                    change_type: KeyChangeType::Modified,
                    repo_value: Some(generated.clone()),
                    deployed_value: Some(deployed.clone()),
                    display,
                });
            }
        }
        _ => {
            let display = format!(
                "{} {} = {} {} {}",
                style("~").yellow(),
                style(path).yellow(),
                style(format_value(deployed)).red(),
                style("->").dim(),
                style(format_value(generated)).green()
            );
            changes.push(KeyChange {
                path: path.to_string(),
                change_type: KeyChangeType::Modified,
                repo_value: Some(generated.clone()),
                deployed_value: Some(deployed.clone()),
                display,
            });
        }
    }
}

/// Perform semantic diff on structured configs (JSON, TOML, YAML)
pub fn semantic_diff(
    format: Format,
    generated: &str,
    deployed: &str,
    ignore_keys: &[String],
) -> DiffResult {
    let renderer = get_renderer(format);

    let gen_value = match renderer.parse(generated) {
        Ok(v) => v,
        Err(_) => return super::text::text_diff(generated, deployed, &[]),
    };

    let dep_value = match renderer.parse(deployed) {
        Ok(v) => v,
        Err(_) => return super::text::text_diff(generated, deployed, &[]),
    };

    // Filter out ignored keys
    let gen_filtered = filter_keys(&gen_value, ignore_keys);
    let dep_filtered = filter_keys(&dep_value, ignore_keys);

    // Compare
    let mut output = Vec::new();
    let has_changes = diff_values(&gen_filtered, &dep_filtered, "", &mut output);

    if has_changes {
        DiffResult::with_changes(output.join("\n"))
    } else {
        DiffResult::no_changes()
    }
}

/// Filter out ignored keys from a JSON value
fn filter_keys(value: &serde_json::Value, ignore_keys: &[String]) -> serde_json::Value {
    match value {
        serde_json::Value::Object(obj) => {
            let filtered: IndexMap<String, serde_json::Value> = obj
                .iter()
                .filter(|(k, _)| !ignore_keys.contains(k))
                .map(|(k, v)| (k.clone(), filter_keys(v, ignore_keys)))
                .collect();
            serde_json::Value::Object(filtered.into_iter().collect())
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(|v| filter_keys(v, ignore_keys)).collect())
        }
        _ => value.clone(),
    }
}

/// Recursively diff two JSON values
fn diff_values(
    generated: &serde_json::Value,
    deployed: &serde_json::Value,
    path: &str,
    output: &mut Vec<String>,
) -> bool {
    if generated == deployed {
        return false;
    }

    // Treat numbers as equal if their float values match (e.g., 12 == 12.0)
    if let (serde_json::Value::Number(g), serde_json::Value::Number(d)) = (generated, deployed)
        && g.as_f64() == d.as_f64()
    {
        return false;
    }

    match (generated, deployed) {
        (serde_json::Value::Object(gen_obj), serde_json::Value::Object(dep_obj)) => {
            let mut has_changes = false;

            // Keys in generated but not in deployed (additions)
            for (key, gen_val) in gen_obj {
                let key_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };

                if let Some(dep_val) = dep_obj.get(key) {
                    if diff_values(gen_val, dep_val, &key_path, output) {
                        has_changes = true;
                    }
                } else {
                    output.push(format!(
                        "{} {} = {}",
                        style("+").green(),
                        style(&key_path).green(),
                        style(format_value(gen_val)).green()
                    ));
                    has_changes = true;
                }
            }

            // Keys in deployed but not in generated (deletions)
            for (key, dep_val) in dep_obj {
                let key_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };

                if !gen_obj.contains_key(key) {
                    output.push(format!(
                        "{} {} = {}",
                        style("-").red(),
                        style(&key_path).red(),
                        style(format_value(dep_val)).red()
                    ));
                    has_changes = true;
                }
            }

            has_changes
        }
        (serde_json::Value::Array(gen_arr), serde_json::Value::Array(dep_arr)) => {
            if gen_arr != dep_arr {
                output.push(format!(
                    "{} {} = {}",
                    style("~").yellow(),
                    style(path).yellow(),
                    style(format!(
                        "{} -> {}",
                        format_array(dep_arr),
                        format_array(gen_arr)
                    ))
                    .yellow()
                ));
                true
            } else {
                false
            }
        }
        _ => {
            output.push(format!(
                "{} {} = {} {} {}",
                style("~").yellow(),
                style(path).yellow(),
                style(format_value(deployed)).red(),
                style("->").dim(),
                style(format_value(generated)).green()
            ));
            true
        }
    }
}

fn format_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => format!("\"{s}\""),
        serde_json::Value::Array(arr) => format_array(arr),
        serde_json::Value::Object(_) => "{...}".to_string(),
        _ => value.to_string(),
    }
}

fn format_array(arr: &[serde_json::Value]) -> String {
    if arr.len() <= 3 {
        let items: Vec<_> = arr.iter().map(format_value).collect();
        format!("[{}]", items.join(", "))
    } else {
        format!("[...{} items]", arr.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_filter_keys() {
        let value = json!({
            "keep": "this",
            "remove": "this",
            "nested": {
                "keep": "nested",
                "remove": "also"
            }
        });

        let filtered = filter_keys(&value, &["remove".to_string()]);
        assert!(filtered.get("keep").is_some());
        assert!(filtered.get("remove").is_none());
        assert!(filtered["nested"].get("keep").is_some());
        assert!(filtered["nested"].get("remove").is_none());
    }

    #[test]
    fn test_semantic_diff_no_changes() {
        let config = r#"{"key": "value"}"#;
        let result = semantic_diff(Format::Json, config, config, &[]);
        assert!(!result.has_changes);
    }

    #[test]
    fn test_semantic_diff_with_changes() {
        let generated = r#"{"key": "new"}"#;
        let deployed = r#"{"key": "old"}"#;
        let result = semantic_diff(Format::Json, generated, deployed, &[]);
        assert!(result.has_changes);
        assert!(result.output.contains("key"));
    }

    #[test]
    fn test_semantic_diff_keys_no_changes() {
        let config = r#"{"key": "value"}"#;
        let changes = semantic_diff_keys(Format::Json, config, config, &[]);
        assert!(changes.is_empty());
    }

    #[test]
    fn test_semantic_diff_keys_modified() {
        let generated = r#"{"key": "new", "other": 1}"#;
        let deployed = r#"{"key": "old", "other": 1}"#;
        let changes = semantic_diff_keys(Format::Json, generated, deployed, &[]);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "key");
        assert_eq!(changes[0].change_type, KeyChangeType::Modified);
        assert_eq!(changes[0].repo_value, Some(json!("new")));
        assert_eq!(changes[0].deployed_value, Some(json!("old")));
    }

    #[test]
    fn test_semantic_diff_keys_added_and_removed() {
        let generated = r#"{"repo_only": 1, "shared": true}"#;
        let deployed = r#"{"deployed_only": 2, "shared": true}"#;
        let changes = semantic_diff_keys(Format::Json, generated, deployed, &[]);
        assert_eq!(changes.len(), 2);

        let added = changes.iter().find(|c| c.path == "repo_only").unwrap();
        assert_eq!(added.change_type, KeyChangeType::Added);
        assert!(added.deployed_value.is_none());

        let removed = changes.iter().find(|c| c.path == "deployed_only").unwrap();
        assert_eq!(removed.change_type, KeyChangeType::Removed);
        assert!(removed.repo_value.is_none());
    }

    #[test]
    fn test_semantic_diff_keys_nested() {
        let generated = r#"{"section": {"a": 1, "b": 2}}"#;
        let deployed = r#"{"section": {"a": 1, "b": 3}}"#;
        let changes = semantic_diff_keys(Format::Json, generated, deployed, &[]);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "section.b");
        assert_eq!(changes[0].change_type, KeyChangeType::Modified);
    }

    #[test]
    fn test_semantic_diff_keys_ignores_keys() {
        let generated = r#"{"keep": "new", "skip": "new"}"#;
        let deployed = r#"{"keep": "old", "skip": "old"}"#;
        let changes = semantic_diff_keys(Format::Json, generated, deployed, &["skip".to_string()]);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "keep");
    }
}
