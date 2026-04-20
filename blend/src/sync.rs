use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context as AnyhowContext, Result};
use console::style;
use walkdir::WalkDir;

use crate::context::Context;
use crate::diff::{DiffResult, KeyChange, KeyChangeType};
use crate::formats::get_renderer;
use crate::nickel;
use crate::output::log;

/// Action to take for a file during bidirectional sync
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SyncAction {
    /// Push repo version to target (overwrite deployed)
    Push,
    /// Pull deployed version back into repo
    Pull,
    /// Skip this file (leave both unchanged)
    Skip,
    /// Quit the sync process entirely
    Quit,
}

/// Resolution strategy for sync conflicts
#[derive(Debug, Clone, Copy)]
pub enum SyncMode {
    /// Prompt the user interactively for each conflict
    Interactive,
    /// Push all (equivalent to old ship --force)
    PushAll,
    /// Pull all deployed changes back
    PullAll,
}

/// Per-key action for interactive from_config sync
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum KeyAction {
    /// Push this key (repo value wins)
    Push,
    /// Pull this key (deployed value wins)
    Pull,
    /// Skip this key (leave unchanged)
    Skip,
    /// Push all remaining keys in this file entry
    AllPush,
    /// Pull all remaining keys in this file entry
    AllPull,
    /// Quit the sync process entirely
    Quit,
}

/// Trait for interactive prompts, enabling testability
pub trait Prompter {
    /// Ask the user what to do with a conflicting file.
    /// `can_pull` is false when the file has logic in from_config (auto-pull not possible).
    fn ask_sync_action(
        &self,
        pkg: &str,
        name: &str,
        target: &Path,
        diff: &DiffResult,
        can_pull: bool,
    ) -> SyncAction;

    /// Ask the user what to do with a single key change within a from_config entry.
    fn ask_key_action(&self, pkg: &str, name: &str, change: &KeyChange) -> KeyAction;
}

/// Production prompter that reads from stdin
pub struct TerminalPrompter;

impl Prompter for TerminalPrompter {
    fn ask_sync_action(
        &self,
        _pkg: &str,
        _name: &str,
        _target: &Path,
        _diff: &DiffResult,
        can_pull: bool,
    ) -> SyncAction {
        let prompt = if can_pull {
            format!("  {} ", style("[p]ush  p[u]ll  [s]kip  [q]uit:").bold())
        } else {
            format!("  {} ", style("[p]ush  [s]kip  [q]uit:").bold())
        };

        print!("{}", prompt);
        io::stdout().flush().unwrap();

        let mut input = String::new();
        io::stdin().read_line(&mut input).unwrap();

        match input.trim().to_lowercase().as_str() {
            "p" | "push" => SyncAction::Push,
            "u" | "pull" if can_pull => SyncAction::Pull,
            "u" | "pull" if !can_pull => {
                log::warn("Cannot auto-pull this entry (from_config contains logic)");
                log::info("Update order.ncl manually using the diff shown above");
                SyncAction::Skip
            }
            "s" | "skip" | "" => SyncAction::Skip,
            "q" | "quit" => SyncAction::Quit,
            _ => {
                log::warn("Invalid choice, skipping");
                SyncAction::Skip
            }
        }
    }

    fn ask_key_action(&self, _pkg: &str, _name: &str, change: &KeyChange) -> KeyAction {
        // Display the change
        println!("    {}", change.display);

        let prompt = format!(
            "    {} ",
            style("[p]ush  p[u]ll  [s]kip  [a]ll-push  a[l]l-pull  [q]uit:").bold()
        );

        print!("{}", prompt);
        io::stdout().flush().unwrap();

        let mut input = String::new();
        io::stdin().read_line(&mut input).unwrap();

        match input.trim().to_lowercase().as_str() {
            "p" | "push" => KeyAction::Push,
            "u" | "pull" => KeyAction::Pull,
            "s" | "skip" | "" => KeyAction::Skip,
            "a" | "all-push" => KeyAction::AllPush,
            "l" | "all-pull" => KeyAction::AllPull,
            "q" | "quit" => KeyAction::Quit,
            _ => {
                log::warn("Invalid choice, skipping");
                KeyAction::Skip
            }
        }
    }
}

/// Pull a from_file entry: copy deployed file/directory back to source in orders/
///
/// When `local_dir` is set, files that have a local override are pulled into the
/// local directory instead of the tracked source directory.
pub fn pull_from_file(
    source_path: &Path,
    target: &Path,
    local_dir: Option<&Path>,
    exclude_patterns: &[String],
    dry_run: bool,
) -> Result<()> {
    if dry_run {
        log::info(&format!(
            "[dry-run] Would pull {} -> {}",
            target.display(),
            source_path.display()
        ));
        return Ok(());
    }

    if target.is_dir() && source_path.is_dir() {
        pull_directory(target, source_path, local_dir, exclude_patterns)?;
    } else if target.is_file() {
        // Ensure parent directory exists
        if let Some(parent) = source_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        }
        std::fs::copy(target, source_path).with_context(|| {
            format!(
                "Failed to copy {} to {}",
                target.display(),
                source_path.display()
            )
        })?;
    } else {
        anyhow::bail!(
            "Cannot pull: target {} does not exist or is not a regular file/directory",
            target.display()
        );
    }

    Ok(())
}

/// Copy a deployed directory back to the source directory in orders/.
///
/// When a local overlay dir is set, files that came from the local overlay
/// (i.e., exist in the local dir) are pulled back to the local dir, not
/// the tracked source dir. Files matching exclude patterns are skipped.
fn pull_directory(
    deployed_dir: &Path,
    source_dir: &Path,
    local_dir: Option<&Path>,
    exclude_patterns: &[String],
) -> Result<()> {
    use crate::compose::build_glob_set;

    let exclude = build_glob_set(exclude_patterns)?;

    // Build a set of relative paths that are local overrides
    let local_overrides: std::collections::HashSet<std::path::PathBuf> = if let Some(ld) = local_dir
    {
        if ld.exists() {
            let mut set = std::collections::HashSet::new();
            for entry in WalkDir::new(ld).min_depth(1) {
                let entry = entry?;
                if entry.file_type().is_dir() {
                    continue;
                }
                let rel = entry.path().strip_prefix(ld)?.to_path_buf();
                set.insert(rel);
            }
            set
        } else {
            std::collections::HashSet::new()
        }
    } else {
        std::collections::HashSet::new()
    };

    for entry in WalkDir::new(deployed_dir).min_depth(1) {
        let entry = entry?;
        let rel_path = entry.path().strip_prefix(deployed_dir)?;

        // Apply exclude filter
        if let Some(ref gs) = exclude
            && gs.is_match(rel_path)
        {
            continue;
        }

        if entry.file_type().is_dir() {
            std::fs::create_dir_all(source_dir.join(rel_path))?;
            if let Some(ld) = local_dir {
                let rel_owned = rel_path.to_path_buf();
                if local_overrides.iter().any(|p| p.starts_with(&rel_owned)) {
                    std::fs::create_dir_all(ld.join(rel_path))?;
                }
            }
        } else {
            let rel_owned = rel_path.to_path_buf();
            let pull_dest = if let Some(ld) = local_dir
                && local_overrides.contains(&rel_owned)
            {
                // This file came from local overlay; pull back there
                ld.join(rel_path)
            } else {
                source_dir.join(rel_path)
            };

            if let Some(parent) = pull_dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(entry.path(), &pull_dest)?;
        }
    }

    Ok(())
}

/// Pull a from_config entry by surgically rewriting the .ncl file.
///
/// Uses context-aware shadow walk to find rewritable leaf values, including
/// values inside conditional branches (match/if-then-else).
/// When a StructureMap can be built, also supports inserting new fields and
/// deleting removed fields.
///
/// Returns Ok(true) if the rewrite succeeded, Ok(false) if it couldn't
/// be done, Err on failure.
pub fn pull_from_config(
    ctx: &Context,
    package: &str,
    file_entry_index: usize,
    target: &Path,
    format: nickel::Format,
    dry_run: bool,
) -> Result<bool> {
    let pkg_dir = ctx.orders_dir.join(package);
    let ncl_path = pkg_dir.join("order.ncl");

    let source = std::fs::read_to_string(&ncl_path)
        .with_context(|| format!("Failed to read {}", ncl_path.display()))?;

    // Shadow walk: locate rewritable leaf spans using runtime metadata
    let rewrite_result =
        nickel::ast_utils::locate_from_config(&source, file_entry_index, &ctx.metadata)?;

    let leaf_spans = rewrite_result.rewritable_spans();
    if leaf_spans.is_empty() {
        return Ok(false);
    }

    // Parse the deployed file
    let deployed_content = std::fs::read_to_string(target)
        .with_context(|| format!("Failed to read {}", target.display()))?;
    let renderer = get_renderer(format);
    let deployed_json = renderer
        .parse(&deployed_content)
        .with_context(|| format!("Failed to parse deployed file {}", target.display()))?;

    // Get current evaluated JSON for comparison
    let evaluator = nickel::NickelEvaluator::new(&ctx.metadata);
    let order_pkg = evaluator.evaluate(&ncl_path)?;
    let file_entry = order_pkg
        .blend
        .files
        .get(file_entry_index)
        .context("file entry index out of bounds")?;
    let current_json = file_entry
        .from_config
        .as_ref()
        .context("file entry has no from_config")?;

    if dry_run {
        log::info(&format!(
            "[dry-run] Would update from_config in {}",
            ncl_path.display()
        ));
        // Show branch context for non-trivial rewrites
        for span in leaf_spans {
            if !span.branch_context.is_empty() {
                log::info(&format!(
                    "  {} scoped under: {}",
                    span.name,
                    span.branch_context.join(" → ")
                ));
            }
        }
        return Ok(true);
    }

    // Detect indentation level from the first span
    let base_indent = if let Some(first) = leaf_spans.first() {
        nickel::ast_utils::detect_indent_level(&source, first.value_start)
    } else {
        0
    };

    // Try structure-aware rewrite (supports insert/delete), fall back to
    // modify-only if StructureMap cannot be built
    let new_source = match nickel::structure_map::build_structure_map(&source, file_entry_index) {
        Ok(structure) => {
            let edits = build_field_edits_from_diff(current_json, &deployed_json);
            if edits
                .iter()
                .all(|e| matches!(e, nickel::ast_utils::FieldEdit::Modify { .. }))
            {
                // Only modifications -- use the simpler path
                nickel::ast_utils::surgical_rewrite(
                    &source,
                    leaf_spans,
                    current_json,
                    &deployed_json,
                    base_indent,
                )?
            } else {
                nickel::ast_utils::surgical_rewrite_with_structure(
                    &source,
                    &structure,
                    leaf_spans,
                    &edits,
                    base_indent,
                )?
            }
        }
        Err(_) => {
            // StructureMap build failed -- fall back to modify-only
            nickel::ast_utils::surgical_rewrite(
                &source,
                leaf_spans,
                current_json,
                &deployed_json,
                base_indent,
            )?
        }
    };

    std::fs::write(&ncl_path, &new_source)
        .with_context(|| format!("Failed to write {}", ncl_path.display()))?;

    // Log non-rewritable fields for user awareness
    for field in rewrite_result.non_rewritable_fields() {
        log::warn(&format!(
            "Cannot auto-pull {}: {} (update order.ncl manually)",
            field.name, field.reason
        ));
    }

    Ok(true)
}

/// Build FieldEdit list by diffing current (repo) JSON against deployed JSON.
///
/// - Keys in deployed but not current -> Insert
/// - Keys in current but not deployed -> Delete
/// - Keys in both with different values -> Modify
fn build_field_edits_from_diff(
    current: &serde_json::Value,
    deployed: &serde_json::Value,
) -> Vec<nickel::ast_utils::FieldEdit> {
    let mut edits = Vec::new();
    collect_field_edits(current, deployed, "", &mut edits);
    edits
}

/// Recursively collect field edits between two JSON values.
fn collect_field_edits(
    current: &serde_json::Value,
    deployed: &serde_json::Value,
    path: &str,
    edits: &mut Vec<nickel::ast_utils::FieldEdit>,
) {
    match (current, deployed) {
        (serde_json::Value::Object(cur_obj), serde_json::Value::Object(dep_obj)) => {
            // Keys modified or deleted
            for (key, cur_val) in cur_obj {
                let key_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };

                if let Some(dep_val) = dep_obj.get(key) {
                    if cur_val != dep_val {
                        // Both objects: recurse; otherwise treat as leaf modification
                        if cur_val.is_object() && dep_val.is_object() {
                            collect_field_edits(cur_val, dep_val, &key_path, edits);
                        } else {
                            edits.push(nickel::ast_utils::FieldEdit::Modify {
                                path: key_path,
                                new_value: dep_val.clone(),
                            });
                        }
                    }
                } else {
                    // Key in current but not deployed -> Delete
                    edits.push(nickel::ast_utils::FieldEdit::Delete { path: key_path });
                }
            }
            // Keys added (in deployed but not current)
            for (key, dep_val) in dep_obj {
                if !cur_obj.contains_key(key) {
                    let key_path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    edits.push(nickel::ast_utils::FieldEdit::Insert {
                        path: key_path,
                        value: dep_val.clone(),
                    });
                }
            }
        }
        _ => {
            // Leaf values that differ
            if current != deployed && !path.is_empty() {
                edits.push(nickel::ast_utils::FieldEdit::Modify {
                    path: path.to_string(),
                    new_value: deployed.clone(),
                });
            }
        }
    }
}

/// Build a merged JSON value by starting from the deployed JSON and applying
/// per-key decisions. Keys marked `Push` keep the repo value; keys marked
/// `Pull` keep the deployed value (the default starting point).
///
/// `decisions` maps dotted key paths to `true` for push (repo wins) or
/// `false` for pull (deployed wins / keep deployed).
pub fn build_merged_json(
    repo_json: &serde_json::Value,
    deployed_json: &serde_json::Value,
    decisions: &std::collections::HashMap<String, bool>,
) -> serde_json::Value {
    merge_values(repo_json, deployed_json, decisions, "")
}

/// Recursively merge two JSON values according to per-key decisions.
fn merge_values(
    repo: &serde_json::Value,
    deployed: &serde_json::Value,
    decisions: &std::collections::HashMap<String, bool>,
    path: &str,
) -> serde_json::Value {
    match (repo, deployed) {
        (serde_json::Value::Object(repo_obj), serde_json::Value::Object(dep_obj)) => {
            let mut merged = serde_json::Map::new();

            // Start with all deployed keys
            for (key, dep_val) in dep_obj {
                let key_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };

                if let Some(&push) = decisions.get(&key_path) {
                    if push {
                        // Push: use repo value if it exists, otherwise omit (it was Added)
                        if let Some(repo_val) = repo_obj.get(key) {
                            merged.insert(key.clone(), repo_val.clone());
                        }
                        // If repo doesn't have it but we're pushing, that means
                        // we want to remove this key (it was a Removed change
                        // that the user chose to push, meaning "remove from deployed")
                    } else {
                        // Pull: keep deployed value
                        merged.insert(key.clone(), dep_val.clone());
                    }
                } else if let Some(repo_val) = repo_obj.get(key) {
                    // No decision at this exact path -- recurse into sub-objects
                    merged.insert(
                        key.clone(),
                        merge_values(repo_val, dep_val, decisions, &key_path),
                    );
                } else {
                    // Key only in deployed, no decision -> keep deployed (pull by default)
                    merged.insert(key.clone(), dep_val.clone());
                }
            }

            // Keys only in repo (Added changes)
            for (key, repo_val) in repo_obj {
                if dep_obj.contains_key(key) {
                    continue; // Already handled above
                }
                let key_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };

                if let Some(&push) = decisions.get(&key_path) {
                    if push {
                        // Push: add this key to the merged output
                        merged.insert(key.clone(), repo_val.clone());
                    }
                    // Pull for Added means "don't add it" -> skip
                } else {
                    // No decision for added key -> keep repo value (push by default
                    // for keys that exist only in repo and have no explicit decision)
                    merged.insert(key.clone(), repo_val.clone());
                }
            }

            serde_json::Value::Object(merged)
        }
        _ => {
            // For non-object values at this path, check the decision
            if let Some(&push) = decisions.get(path) {
                if push { repo.clone() } else { deployed.clone() }
            } else {
                // No decision -- default to deployed (conservative)
                deployed.clone()
            }
        }
    }
}

/// Pull specific keys from deployed config back into the .ncl file.
///
/// `pulled_keys` is the set of dotted key paths that should be pulled
/// (deployed value wins for these keys in the .ncl rewrite).
/// `key_changes` optionally provides change type info per key to enable
/// structure-aware insertion and deletion.
pub fn pull_from_config_keys(
    ctx: &Context,
    package: &str,
    file_entry_index: usize,
    target: &Path,
    format: nickel::Format,
    pulled_keys: &[String],
    dry_run: bool,
) -> Result<bool> {
    pull_from_config_keys_with_changes(
        ctx,
        package,
        file_entry_index,
        target,
        format,
        pulled_keys,
        &[], // no change type info -- infer from diff
        dry_run,
    )
}

/// Pull specific keys with optional KeyChange metadata for insert/delete support.
#[allow(clippy::too_many_arguments)]
pub fn pull_from_config_keys_with_changes(
    ctx: &Context,
    package: &str,
    file_entry_index: usize,
    target: &Path,
    format: nickel::Format,
    pulled_keys: &[String],
    key_changes: &[KeyChange],
    dry_run: bool,
) -> Result<bool> {
    if pulled_keys.is_empty() {
        return Ok(true); // nothing to pull
    }

    let pkg_dir = ctx.orders_dir.join(package);
    let ncl_path = pkg_dir.join("order.ncl");

    let source = std::fs::read_to_string(&ncl_path)
        .with_context(|| format!("Failed to read {}", ncl_path.display()))?;

    let rewrite_result =
        nickel::ast_utils::locate_from_config(&source, file_entry_index, &ctx.metadata)?;

    let leaf_spans = rewrite_result.rewritable_spans();

    // Parse the deployed file
    let deployed_content = std::fs::read_to_string(target)
        .with_context(|| format!("Failed to read {}", target.display()))?;
    let renderer = get_renderer(format);
    let deployed_json = renderer
        .parse(&deployed_content)
        .with_context(|| format!("Failed to parse deployed file {}", target.display()))?;

    // Get current evaluated JSON
    let evaluator = nickel::NickelEvaluator::new(&ctx.metadata);
    let order_pkg = evaluator.evaluate(&ncl_path)?;
    let file_entry = order_pkg
        .blend
        .files
        .get(file_entry_index)
        .context("file entry index out of bounds")?;
    let current_json = file_entry
        .from_config
        .as_ref()
        .context("file entry has no from_config")?;

    if dry_run {
        log::info(&format!(
            "[dry-run] Would update {} keys in {}",
            pulled_keys.len(),
            ncl_path.display()
        ));
        return Ok(true);
    }

    let base_indent = if let Some(first) = leaf_spans.first() {
        nickel::ast_utils::detect_indent_level(&source, first.value_start)
    } else {
        0
    };

    // Build a change type lookup from key_changes if provided
    let change_type_map: std::collections::HashMap<&str, &KeyChangeType> = key_changes
        .iter()
        .map(|kc| (kc.path.as_str(), &kc.change_type))
        .collect();

    // Check if any pulled keys are insertions or deletions
    let has_structural_changes = pulled_keys.iter().any(|k| {
        if let Some(ct) = change_type_map.get(k.as_str()) {
            matches!(ct, KeyChangeType::Added | KeyChangeType::Removed)
        } else {
            let in_cur = nickel::ast_utils::json_path_get(current_json, k).is_some();
            let in_dep = nickel::ast_utils::json_path_get(&deployed_json, k).is_some();
            (in_dep && !in_cur) || (in_cur && !in_dep)
        }
    });

    // Try structure-aware rewrite if we have structural changes
    let new_source = if has_structural_changes {
        match nickel::structure_map::build_structure_map(&source, file_entry_index) {
            Ok(structure) => {
                let edits = build_field_edits_for_keys(
                    current_json,
                    &deployed_json,
                    pulled_keys,
                    &change_type_map,
                );
                nickel::ast_utils::surgical_rewrite_with_structure(
                    &source,
                    &structure,
                    leaf_spans,
                    &edits,
                    base_indent,
                )?
            }
            Err(_) => {
                // Fall back: only handle modifications
                if leaf_spans.is_empty() {
                    return Ok(false);
                }
                let selective_deployed =
                    build_selective_deployed(current_json, &deployed_json, pulled_keys);
                nickel::ast_utils::surgical_rewrite(
                    &source,
                    leaf_spans,
                    current_json,
                    &selective_deployed,
                    base_indent,
                )?
            }
        }
    } else {
        if leaf_spans.is_empty() {
            return Ok(false);
        }
        // Only modifications -- use existing path
        let selective_deployed =
            build_selective_deployed(current_json, &deployed_json, pulled_keys);
        nickel::ast_utils::surgical_rewrite(
            &source,
            leaf_spans,
            current_json,
            &selective_deployed,
            base_indent,
        )?
    };

    std::fs::write(&ncl_path, &new_source)
        .with_context(|| format!("Failed to write {}", ncl_path.display()))?;

    for field in rewrite_result.non_rewritable_fields() {
        if pulled_keys
            .iter()
            .any(|k| k == &field.name || k.starts_with(&format!("{}.", field.name)))
        {
            log::warn(&format!(
                "Cannot auto-pull {}: {} (update order.ncl manually)",
                field.name, field.reason
            ));
        }
    }

    Ok(true)
}

/// Build FieldEdit list for specific pulled keys, using change type info.
///
/// Uses dotted-path resolution so nested keys (e.g. `window.opacity`) are
/// looked up through nested JSON objects.
fn build_field_edits_for_keys(
    current: &serde_json::Value,
    deployed: &serde_json::Value,
    pulled_keys: &[String],
    change_types: &std::collections::HashMap<&str, &KeyChangeType>,
) -> Vec<nickel::ast_utils::FieldEdit> {
    pulled_keys
        .iter()
        .filter_map(|key| {
            let change_type = if let Some(ct) = change_types.get(key.as_str()) {
                (*ct).clone()
            } else {
                let in_cur = nickel::ast_utils::json_path_get(current, key).is_some();
                let in_dep = nickel::ast_utils::json_path_get(deployed, key).is_some();
                if in_cur && in_dep {
                    KeyChangeType::Modified
                } else if in_dep && !in_cur {
                    KeyChangeType::Removed
                } else if in_cur && !in_dep {
                    KeyChangeType::Added
                } else {
                    return None;
                }
            };

            match change_type {
                KeyChangeType::Modified => {
                    let dep_val = nickel::ast_utils::json_path_get(deployed, key)?;
                    Some(nickel::ast_utils::FieldEdit::Modify {
                        path: key.clone(),
                        new_value: dep_val.clone(),
                    })
                }
                KeyChangeType::Removed => {
                    let dep_val = nickel::ast_utils::json_path_get(deployed, key)?;
                    Some(nickel::ast_utils::FieldEdit::Insert {
                        path: key.clone(),
                        value: dep_val.clone(),
                    })
                }
                KeyChangeType::Added => {
                    Some(nickel::ast_utils::FieldEdit::Delete { path: key.clone() })
                }
            }
        })
        .collect()
}

/// Build a JSON value where only pulled keys take deployed values;
/// all other keys keep their current (repo) values.
///
/// Pulled keys may be dotted paths (e.g. `window.opacity`); the overlay walks
/// both objects in lockstep and replaces the subtree at any path that matches.
fn build_selective_deployed(
    current: &serde_json::Value,
    deployed: &serde_json::Value,
    pulled_keys: &[String],
) -> serde_json::Value {
    selective_overlay(current, deployed, pulled_keys, "")
}

fn selective_overlay(
    current: &serde_json::Value,
    deployed: &serde_json::Value,
    pulled_keys: &[String],
    path: &str,
) -> serde_json::Value {
    if !path.is_empty() && pulled_keys.iter().any(|k| k == path) {
        return deployed.clone();
    }

    match (current, deployed) {
        (serde_json::Value::Object(cur_obj), serde_json::Value::Object(dep_obj)) => {
            let mut result = serde_json::Map::new();
            for (key, cur_val) in cur_obj {
                let key_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                if let Some(dep_val) = dep_obj.get(key) {
                    result.insert(
                        key.clone(),
                        selective_overlay(cur_val, dep_val, pulled_keys, &key_path),
                    );
                } else {
                    // Key only in current; keep it (deletions are handled by
                    // FieldEdit::Delete in the structural path).
                    result.insert(key.clone(), cur_val.clone());
                }
            }
            // Keys only in deployed are picked up only if explicitly pulled
            // at this exact path or under it.
            for (key, dep_val) in dep_obj {
                if cur_obj.contains_key(key) {
                    continue;
                }
                let key_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                let prefix = format!("{key_path}.");
                if pulled_keys
                    .iter()
                    .any(|k| k == &key_path || k.starts_with(&prefix))
                {
                    result.insert(key.clone(), dep_val.clone());
                }
            }
            serde_json::Value::Object(result)
        }
        _ => {
            if !pulled_keys.is_empty() && path.is_empty() {
                deployed.clone()
            } else {
                current.clone()
            }
        }
    }
}

/// Display a sync conflict diff for a file
pub fn display_conflict(pkg: &str, name: &str, target: &Path, diff: &DiffResult, home_dir: &Path) {
    let target_display = shorten_path(target, home_dir);
    println!("\n  {}:{} ({})", style(pkg).cyan(), name, target_display);
    for line in diff.output.lines() {
        println!("    {}", line);
    }
}

/// Shorten a path by replacing the home directory with ~
pub fn shorten_path(path: &Path, home_dir: &Path) -> String {
    let s = path.to_string_lossy();
    let home = home_dir.to_string_lossy();
    if s.starts_with(home.as_ref()) {
        format!("~{}", &s[home.len()..])
    } else {
        s.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::KeyChangeType;
    use std::collections::{HashMap, VecDeque};
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Mock prompter for testing
    struct MockPrompter {
        answers: std::cell::RefCell<VecDeque<SyncAction>>,
        key_answers: std::cell::RefCell<VecDeque<KeyAction>>,
    }

    impl MockPrompter {
        fn new(answers: Vec<SyncAction>) -> Self {
            Self {
                answers: std::cell::RefCell::new(answers.into()),
                key_answers: std::cell::RefCell::new(VecDeque::new()),
            }
        }

        fn with_key_answers(answers: Vec<SyncAction>, key_answers: Vec<KeyAction>) -> Self {
            Self {
                answers: std::cell::RefCell::new(answers.into()),
                key_answers: std::cell::RefCell::new(key_answers.into()),
            }
        }
    }

    impl Prompter for MockPrompter {
        fn ask_sync_action(
            &self,
            _pkg: &str,
            _name: &str,
            _target: &Path,
            _diff: &DiffResult,
            _can_pull: bool,
        ) -> SyncAction {
            self.answers
                .borrow_mut()
                .pop_front()
                .unwrap_or(SyncAction::Skip)
        }

        fn ask_key_action(&self, _pkg: &str, _name: &str, _change: &KeyChange) -> KeyAction {
            self.key_answers
                .borrow_mut()
                .pop_front()
                .unwrap_or(KeyAction::Skip)
        }
    }

    #[test]
    fn test_shorten_path() {
        let home = PathBuf::from("/Users/test");
        assert_eq!(
            shorten_path(&PathBuf::from("/Users/test/.config/foo"), &home),
            "~/.config/foo"
        );
        assert_eq!(shorten_path(&PathBuf::from("/etc/foo"), &home), "/etc/foo");
    }

    #[test]
    fn test_pull_from_file() {
        let temp = TempDir::new().unwrap();
        let target = temp.path().join("deployed.txt");
        let source = temp.path().join("source.txt");

        std::fs::write(&target, "deployed content").unwrap();
        std::fs::write(&source, "old content").unwrap();

        pull_from_file(&source, &target, None, &[], false).unwrap();

        assert_eq!(
            std::fs::read_to_string(&source).unwrap(),
            "deployed content"
        );
    }

    #[test]
    fn test_pull_from_file_dry_run() {
        let temp = TempDir::new().unwrap();
        let target = temp.path().join("deployed.txt");
        let source = temp.path().join("source.txt");

        std::fs::write(&target, "deployed content").unwrap();
        std::fs::write(&source, "old content").unwrap();

        pull_from_file(&source, &target, None, &[], true).unwrap();

        // Should not have changed
        assert_eq!(std::fs::read_to_string(&source).unwrap(), "old content");
    }

    #[test]
    fn test_pull_directory() {
        let temp = TempDir::new().unwrap();
        let deployed = temp.path().join("deployed");
        let source = temp.path().join("source");

        std::fs::create_dir_all(deployed.join("sub")).unwrap();
        std::fs::write(deployed.join("file.txt"), "content").unwrap();
        std::fs::write(deployed.join("sub/nested.txt"), "nested").unwrap();

        std::fs::create_dir(&source).unwrap();

        pull_from_file(&source, &deployed, None, &[], false).unwrap();

        assert_eq!(
            std::fs::read_to_string(source.join("file.txt")).unwrap(),
            "content"
        );
        assert_eq!(
            std::fs::read_to_string(source.join("sub/nested.txt")).unwrap(),
            "nested"
        );
    }

    #[test]
    fn test_mock_prompter() {
        let prompter =
            MockPrompter::new(vec![SyncAction::Push, SyncAction::Pull, SyncAction::Quit]);
        let diff = DiffResult::no_changes();

        assert_eq!(
            prompter.ask_sync_action("pkg", "file", Path::new("/tmp"), &diff, true),
            SyncAction::Push
        );
        assert_eq!(
            prompter.ask_sync_action("pkg", "file", Path::new("/tmp"), &diff, true),
            SyncAction::Pull
        );
        assert_eq!(
            prompter.ask_sync_action("pkg", "file", Path::new("/tmp"), &diff, true),
            SyncAction::Quit
        );
        // Exhausted -- defaults to Skip
        assert_eq!(
            prompter.ask_sync_action("pkg", "file", Path::new("/tmp"), &diff, true),
            SyncAction::Skip
        );
    }

    #[test]
    fn test_mock_prompter_key_actions() {
        let prompter = MockPrompter::with_key_answers(
            vec![],
            vec![KeyAction::Push, KeyAction::Pull, KeyAction::AllPush],
        );
        let change = KeyChange {
            path: "key".to_string(),
            change_type: KeyChangeType::Modified,
            repo_value: Some(serde_json::json!("new")),
            deployed_value: Some(serde_json::json!("old")),
            display: "~ key".to_string(),
        };

        assert_eq!(
            prompter.ask_key_action("pkg", "file", &change),
            KeyAction::Push
        );
        assert_eq!(
            prompter.ask_key_action("pkg", "file", &change),
            KeyAction::Pull
        );
        assert_eq!(
            prompter.ask_key_action("pkg", "file", &change),
            KeyAction::AllPush
        );
        // Exhausted
        assert_eq!(
            prompter.ask_key_action("pkg", "file", &change),
            KeyAction::Skip
        );
    }

    #[test]
    fn test_build_selective_deployed_nested_key() {
        use serde_json::json;
        // semantic_diff_keys produces dotted paths like "window.opacity".
        // build_selective_deployed must resolve them through nested JSON.
        let current = json!({
            "window": {"opacity": 0.7, "decorations": "Buttonless"},
            "font": {"size": 12},
        });
        let deployed = json!({
            "window": {"opacity": 0.8, "decorations": "Buttonless"},
            "font": {"size": 12},
        });
        let pulled_keys = vec!["window.opacity".to_string()];
        let result = build_selective_deployed(&current, &deployed, &pulled_keys);
        assert_eq!(
            result,
            json!({
                "window": {"opacity": 0.8, "decorations": "Buttonless"},
                "font": {"size": 12},
            }),
            "Only window.opacity should adopt deployed value; got:\n{:#?}",
            result
        );
    }

    #[test]
    fn test_build_merged_json_all_push() {
        use serde_json::json;
        let repo = json!({"a": 1, "b": 2});
        let deployed = json!({"a": 10, "b": 20});
        let mut decisions = HashMap::new();
        decisions.insert("a".to_string(), true); // push
        decisions.insert("b".to_string(), true); // push

        let merged = build_merged_json(&repo, &deployed, &decisions);
        assert_eq!(merged, json!({"a": 1, "b": 2}));
    }

    #[test]
    fn test_build_merged_json_all_pull() {
        use serde_json::json;
        let repo = json!({"a": 1, "b": 2});
        let deployed = json!({"a": 10, "b": 20});
        let mut decisions = HashMap::new();
        decisions.insert("a".to_string(), false); // pull
        decisions.insert("b".to_string(), false); // pull

        let merged = build_merged_json(&repo, &deployed, &decisions);
        assert_eq!(merged, json!({"a": 10, "b": 20}));
    }

    #[test]
    fn test_build_merged_json_mixed() {
        use serde_json::json;
        let repo = json!({"a": 1, "b": 2, "c": 3});
        let deployed = json!({"a": 10, "b": 20, "c": 30});
        let mut decisions = HashMap::new();
        decisions.insert("a".to_string(), true); // push: repo wins
        decisions.insert("b".to_string(), false); // pull: deployed wins
        // c has no decision -> defaults to deployed for unchanged keys

        let merged = build_merged_json(&repo, &deployed, &decisions);
        assert_eq!(merged, json!({"a": 1, "b": 20, "c": 30}));
    }

    #[test]
    fn test_build_merged_json_added_key_push() {
        use serde_json::json;
        let repo = json!({"a": 1, "new_key": "hello"});
        let deployed = json!({"a": 1});
        let mut decisions = HashMap::new();
        decisions.insert("new_key".to_string(), true); // push the addition

        let merged = build_merged_json(&repo, &deployed, &decisions);
        assert_eq!(merged, json!({"a": 1, "new_key": "hello"}));
    }

    #[test]
    fn test_build_merged_json_added_key_pull() {
        use serde_json::json;
        let repo = json!({"a": 1, "new_key": "hello"});
        let deployed = json!({"a": 1});
        let mut decisions = HashMap::new();
        decisions.insert("new_key".to_string(), false); // pull: don't add it

        let merged = build_merged_json(&repo, &deployed, &decisions);
        assert_eq!(merged, json!({"a": 1}));
    }

    #[test]
    fn test_build_merged_json_removed_key_push() {
        use serde_json::json;
        let repo = json!({"a": 1});
        let deployed = json!({"a": 1, "extra": "deployed"});
        let mut decisions = HashMap::new();
        decisions.insert("extra".to_string(), true); // push: remove from deployed

        let merged = build_merged_json(&repo, &deployed, &decisions);
        assert_eq!(merged, json!({"a": 1}));
    }

    #[test]
    fn test_build_merged_json_removed_key_pull() {
        use serde_json::json;
        let repo = json!({"a": 1});
        let deployed = json!({"a": 1, "extra": "deployed"});
        let mut decisions = HashMap::new();
        decisions.insert("extra".to_string(), false); // pull: keep in deployed

        let merged = build_merged_json(&repo, &deployed, &decisions);
        assert_eq!(merged, json!({"a": 1, "extra": "deployed"}));
    }
}
