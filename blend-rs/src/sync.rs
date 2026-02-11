use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context as AnyhowContext, Result};
use console::style;
use walkdir::WalkDir;

use crate::context::Context;
use crate::diff::DiffResult;
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
            format!("  {} ", style("[p]ush  [u]ll  [s]kip  [q]uit:").bold())
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
}

/// Pull a from_file entry: copy deployed file/directory back to source in orders/
pub fn pull_from_file(source_path: &Path, target: &Path, dry_run: bool) -> Result<()> {
    if dry_run {
        log::info(&format!(
            "[dry-run] Would pull {} -> {}",
            target.display(),
            source_path.display()
        ));
        return Ok(());
    }

    if target.is_dir() && source_path.is_dir() {
        pull_directory(target, source_path)?;
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

/// Copy a deployed directory back to the source directory in orders/
fn pull_directory(deployed_dir: &Path, source_dir: &Path) -> Result<()> {
    for entry in WalkDir::new(deployed_dir).min_depth(1) {
        let entry = entry?;
        let rel_path = entry.path().strip_prefix(deployed_dir)?;
        let target_path = source_dir.join(rel_path);

        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target_path)?;
        } else {
            if let Some(parent) = target_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(entry.path(), &target_path)?;
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
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Mock prompter for testing
    struct MockPrompter {
        answers: std::cell::RefCell<VecDeque<SyncAction>>,
    }

    impl MockPrompter {
        fn new(answers: Vec<SyncAction>) -> Self {
            Self {
                answers: std::cell::RefCell::new(answers.into()),
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

        pull_from_file(&source, &target, false).unwrap();

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

        pull_from_file(&source, &target, true).unwrap();

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

        pull_from_file(&source, &deployed, false).unwrap();

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
        // Exhausted — defaults to Skip
        assert_eq!(
            prompter.ask_sync_action("pkg", "file", Path::new("/tmp"), &diff, true),
            SyncAction::Skip
        );
    }
}
