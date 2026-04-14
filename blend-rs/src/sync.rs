use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context as AnyhowContext, Result};
use console::style;
use walkdir::WalkDir;

use crate::context::Context;
use crate::diff::{DiffResult, KeyChange};
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

    // Perform surgical rewrite
    let new_source = nickel::ast_utils::surgical_rewrite(
        &source,
        leaf_spans,
        current_json,
        &deployed_json,
        base_indent,
    )?;

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
pub fn pull_from_config_keys(
    ctx: &Context,
    package: &str,
    file_entry_index: usize,
    target: &Path,
    format: nickel::Format,
    pulled_keys: &[String],
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

    // Build a "selective deployed" JSON that only has the pulled keys changed
    // while keeping other values from current_json
    let selective_deployed = build_selective_deployed(current_json, &deployed_json, pulled_keys);

    let new_source = nickel::ast_utils::surgical_rewrite(
        &source,
        leaf_spans,
        current_json,
        &selective_deployed,
        base_indent,
    )?;

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

/// Build a JSON value where only pulled keys take deployed values;
/// all other keys keep their current (repo) values.
fn build_selective_deployed(
    current: &serde_json::Value,
    deployed: &serde_json::Value,
    pulled_keys: &[String],
) -> serde_json::Value {
    match (current, deployed) {
        (serde_json::Value::Object(cur_obj), serde_json::Value::Object(dep_obj)) => {
            let mut result = serde_json::Map::new();
            // Keep all current keys with their current values, unless pulled
            for (key, cur_val) in cur_obj {
                if pulled_keys.iter().any(|k| k == key) {
                    // This top-level key is pulled -> use deployed value
                    if let Some(dep_val) = dep_obj.get(key) {
                        result.insert(key.clone(), dep_val.clone());
                    } else {
                        // Key was removed in deployed; keep current to avoid data loss
                        result.insert(key.clone(), cur_val.clone());
                    }
                } else {
                    // Not pulled -> keep current value
                    result.insert(key.clone(), cur_val.clone());
                }
            }
            serde_json::Value::Object(result)
        }
        _ => {
            // Non-object from_config: if any key is pulled, use deployed
            if !pulled_keys.is_empty() {
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
