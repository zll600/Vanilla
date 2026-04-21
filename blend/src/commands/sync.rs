use crate::commands::helpers::{
    compute_diff_for_result, only_structural_symlink_changes, target_is_unexpected_symlink,
};
use crate::compose::{self, BuildResult, discover_packages, write_result};
use crate::context::Context;
use crate::diff::semantic_diff_keys;
use crate::formats::get_renderer;
use crate::nickel;
use crate::output::log;
use crate::sync::{self, KeyAction, Prompter, SyncAction, SyncMode};

/// Sync command: bidirectional sync between repo and deployed configs
pub fn cmd_sync(
    ctx: &Context,
    packages: &[String],
    mode: SyncMode,
    no_rewrite: bool,
    prompter: &dyn Prompter,
) -> anyhow::Result<()> {
    let all_packages = discover_packages(&ctx.orders_dir);

    if all_packages.is_empty() {
        log::warn("No packages found in orders directory");
        return Ok(());
    }

    let to_sync: Vec<String> = if packages.is_empty() {
        all_packages.into_iter().collect()
    } else {
        packages
            .iter()
            .filter(|p| {
                if all_packages.contains(*p) {
                    true
                } else {
                    log::warn(&format!("Package '{p}' not found"));
                    false
                }
            })
            .cloned()
            .collect()
    };

    // Phase 1: build all packages and collect results
    // Also track file_entry_index per result for from_config pull-back
    let mut built: Vec<(String, Vec<(BuildResult, usize)>)> = Vec::new();
    let mut build_errors = 0;

    for pkg in &to_sync {
        match build_package_with_indices(ctx, pkg) {
            Ok(results) => {
                if !results.is_empty() {
                    built.push((pkg.clone(), results));
                }
            }
            Err(e) => {
                log::error(&format!("Failed to build {pkg}: {e}"));
                build_errors += 1;
            }
        }
    }

    // Phase 2: for each built file, compute diff and determine action
    let mut pushed = 0;
    let mut pulled = 0;
    let mut skipped = 0;

    for (pkg, results) in &built {
        for (result, file_entry_index) in results {
            // Symlink entries: ensure the symlink exists and points to the right place
            if result.is_symlink {
                let needs_update = match std::fs::read_link(&result.target) {
                    Ok(existing) => result.canonical_source.as_deref() != Some(existing.as_path()),
                    Err(_) => true, // not a symlink or doesn't exist
                };

                if needs_update {
                    match write_result(result, ctx.dry_run) {
                        Ok(()) => {
                            if !ctx.dry_run {
                                log::success(&format!(
                                    "Linked {}:{} -> {}",
                                    pkg,
                                    result.name,
                                    result.target.display()
                                ));
                                pushed += 1;
                            }
                        }
                        Err(e) => {
                            log::error(&format!("Failed to link {}:{}: {}", pkg, result.name, e));
                            build_errors += 1;
                        }
                    }
                } else if ctx.verbose {
                    log::info(&format!("{}:{} already linked", pkg, result.name));
                }
                continue;
            }

            // New file — always push
            if !result.target.exists() {
                if ctx.dry_run {
                    log::info_important(&format!(
                        "[dry-run] {}:{} -> {} (new file, would push)",
                        pkg,
                        result.name,
                        result.target.display()
                    ));
                } else {
                    match write_result(result, false) {
                        Ok(()) => {
                            log::success(&format!(
                                "Pushed {}:{} -> {} (new)",
                                pkg,
                                result.name,
                                result.target.display()
                            ));
                            pushed += 1;
                        }
                        Err(e) => {
                            log::error(&format!("Failed to push {}:{}: {}", pkg, result.name, e));
                            build_errors += 1;
                        }
                    }
                }
                continue;
            }

            // Compute diff
            let diff_result = compute_diff_for_result(result);

            // Auto-redeploy when push will only change file types (replace
            // unexpected symlinks with real files), no content edits. Two
            // shapes: top-level symlink with matching content, OR a
            // directory entry whose only drift is inner-file symlinks.
            let top_level_symlink = !diff_result.has_changes
                && target_is_unexpected_symlink(&result.target, result.is_symlink);
            let inner_symlink_only = only_structural_symlink_changes(result);

            if top_level_symlink || inner_symlink_only {
                if ctx.dry_run {
                    log::info_important(&format!(
                        "[dry-run] {}:{} content matches but target is a symlink — would re-deploy",
                        pkg, result.name
                    ));
                } else {
                    match write_result(result, false) {
                        Ok(()) => {
                            log::success(&format!(
                                "Re-deployed {}:{} (replaced symlink with real file/directory)",
                                pkg, result.name
                            ));
                            pushed += 1;
                        }
                        Err(e) => {
                            log::error(&format!(
                                "Failed to re-deploy {}:{}: {}",
                                pkg, result.name, e
                            ));
                            build_errors += 1;
                        }
                    }
                }
                continue;
            }

            if !diff_result.has_changes {
                if ctx.verbose {
                    log::info(&format!("{}:{} in sync", pkg, result.name));
                }
                continue;
            }

            // Determine if auto-pull is possible for this entry
            let can_pull = !no_rewrite && can_auto_pull(ctx, pkg, result, *file_entry_index);

            // For from_config entries in interactive mode, use per-key flow
            let is_from_config = !result.is_plaintext && !result.is_symlink;
            // Per-key only works if the deployed file can be semantically parsed
            let deployed_parseable = if is_from_config {
                let format = nickel::Format::from_path(&result.name);
                std::fs::read_to_string(&result.target)
                    .ok()
                    .and_then(|s| get_renderer(format).parse(&s).ok())
                    .is_some()
            } else {
                false
            };
            let use_per_key = is_from_config
                && matches!(mode, SyncMode::Interactive)
                && !ctx.dry_run
                && can_pull
                && deployed_parseable;

            if use_per_key {
                // Per-key interactive sync for from_config entries
                let format = nickel::Format::from_path(&result.name);
                let key_changes = semantic_diff_keys(
                    format,
                    &result.content,
                    &std::fs::read_to_string(&result.target).unwrap_or_default(),
                    &result.ignore_keys,
                );

                if key_changes.is_empty() {
                    if ctx.verbose {
                        log::info(&format!("{}:{} in sync (per-key)", pkg, result.name));
                    }
                    continue;
                }

                // Display file header
                let target_display = sync::shorten_path(&result.target, &ctx.home_dir);
                println!(
                    "\n  {}:{} ({})",
                    console::style(pkg).cyan(),
                    result.name,
                    target_display
                );

                let mut decisions = std::collections::HashMap::new();
                let mut all_mode: Option<bool> = None; // Some(true)=AllPush, Some(false)=AllPull
                let mut quit = false;

                for change in &key_changes {
                    let action = if let Some(push) = all_mode {
                        if push {
                            KeyAction::Push
                        } else {
                            KeyAction::Pull
                        }
                    } else {
                        prompter.ask_key_action(pkg, &result.name, change)
                    };

                    match action {
                        KeyAction::Push => {
                            decisions.insert(change.path.clone(), true);
                        }
                        KeyAction::Pull => {
                            decisions.insert(change.path.clone(), false);
                        }
                        KeyAction::Skip => {
                            // No decision for this key -- skip means keep as-is
                        }
                        KeyAction::AllPush => {
                            all_mode = Some(true);
                            decisions.insert(change.path.clone(), true);
                        }
                        KeyAction::AllPull => {
                            all_mode = Some(false);
                            decisions.insert(change.path.clone(), false);
                        }
                        KeyAction::Quit => {
                            quit = true;
                            break;
                        }
                    }
                }

                if quit {
                    log::info("Sync aborted by user");
                    return Ok(());
                }

                if decisions.is_empty() {
                    skipped += 1;
                    continue;
                }

                // Separate push and pull decisions
                let has_pushes = decisions.values().any(|&v| v);
                let has_pulls = decisions.values().any(|&v| !v);

                // Apply pushes: build merged JSON and write to target
                if has_pushes || has_pulls {
                    let format_renderer = get_renderer(format);
                    let repo_json: serde_json::Value =
                        format_renderer.parse(&result.content).unwrap_or_default();
                    let deployed_json: serde_json::Value = std::fs::read_to_string(&result.target)
                        .ok()
                        .and_then(|s| format_renderer.parse(&s).ok())
                        .unwrap_or_default();

                    // Build merged JSON from decisions
                    let merged = sync::build_merged_json(&repo_json, &deployed_json, &decisions);

                    // Write merged result to target
                    match format_renderer.render(&merged) {
                        Ok(merged_content) => {
                            let merged_result = BuildResult {
                                target: result.target.clone(),
                                content: merged_content,
                                is_plaintext: false,
                                source_path: None,
                                name: result.name.clone(),
                                ignore_keys: result.ignore_keys.clone(),
                                is_symlink: false,
                                canonical_source: None,
                                exclude_patterns: vec![],
                                local_dir: None,
                                immutable: false,
                            };
                            match write_result(&merged_result, false) {
                                Ok(()) => {
                                    pushed += 1;
                                    log::success(&format!(
                                        "Synced {}:{} ({} keys resolved)",
                                        pkg,
                                        result.name,
                                        decisions.len()
                                    ));
                                }
                                Err(e) => {
                                    log::error(&format!(
                                        "Failed to write merged {}:{}: {}",
                                        pkg, result.name, e
                                    ));
                                    build_errors += 1;
                                }
                            }
                        }
                        Err(e) => {
                            log::error(&format!(
                                "Failed to render merged {}:{}: {}",
                                pkg, result.name, e
                            ));
                            build_errors += 1;
                        }
                    }

                    // Apply pulls: surgically rewrite .ncl for pulled keys
                    if has_pulls && !no_rewrite {
                        let pulled_keys: Vec<String> = decisions
                            .iter()
                            .filter(|(_, push)| !**push)
                            .map(|(k, _)| k.clone())
                            .collect();

                        match sync::pull_from_config_keys(
                            ctx,
                            pkg,
                            *file_entry_index,
                            &result.target,
                            format,
                            &pulled_keys,
                            false,
                        ) {
                            Ok(true) => {
                                pulled += 1;
                            }
                            Ok(false) => {
                                log::warn(&format!(
                                    "Some pulled keys in {}:{} could not be auto-rewritten",
                                    pkg, result.name
                                ));
                            }
                            Err(e) => {
                                log::error(&format!(
                                    "Failed to rewrite .ncl for {}:{}: {}",
                                    pkg, result.name, e
                                ));
                                build_errors += 1;
                            }
                        }
                    }
                } else {
                    skipped += 1;
                }
            } else {
                // Original whole-file flow for from_file, symlinks, and
                // non-interactive / non-pullable from_config entries

                // Display the diff
                sync::display_conflict(
                    pkg,
                    &result.name,
                    &result.target,
                    &diff_result,
                    &ctx.home_dir,
                );

                // Determine action
                let action = match mode {
                    SyncMode::PushAll => SyncAction::Push,
                    SyncMode::PullAll => {
                        if can_pull {
                            SyncAction::Pull
                        } else {
                            log::warn(&format!(
                                "Cannot auto-pull {}:{} (from_config contains logic), skipping",
                                pkg, result.name
                            ));
                            SyncAction::Skip
                        }
                    }
                    SyncMode::Interactive => {
                        if ctx.dry_run {
                            let pull_note = if can_pull {
                                ", pullable"
                            } else {
                                ", manual merge needed"
                            };
                            log::info_important(&format!("[dry-run] would prompt{}", pull_note));
                            SyncAction::Skip
                        } else {
                            prompter.ask_sync_action(
                                pkg,
                                &result.name,
                                &result.target,
                                &diff_result,
                                can_pull,
                            )
                        }
                    }
                };

                match action {
                    SyncAction::Push => match write_result(result, ctx.dry_run) {
                        Ok(()) => {
                            if !ctx.dry_run {
                                log::success(&format!("Pushed {}:{}", pkg, result.name));
                                pushed += 1;
                            }
                        }
                        Err(e) => {
                            log::error(&format!("Failed to push {}:{}: {}", pkg, result.name, e));
                            build_errors += 1;
                        }
                    },
                    SyncAction::Pull => {
                        let pull_result = if result.is_plaintext {
                            if let Some(source_path) = &result.source_path {
                                sync::pull_from_file(
                                    source_path,
                                    &result.target,
                                    result.local_dir.as_deref(),
                                    &result.exclude_patterns,
                                    ctx.dry_run,
                                )
                            } else {
                                Err(anyhow::anyhow!("No source path for plaintext entry"))
                            }
                        } else {
                            let format = nickel::Format::from_path(&result.name);
                            match sync::pull_from_config(
                                ctx,
                                pkg,
                                *file_entry_index,
                                &result.target,
                                format,
                                ctx.dry_run,
                            ) {
                                Ok(true) => Ok(()),
                                Ok(false) => {
                                    log::warn("Cannot auto-pull (from_config has logic)");
                                    Ok(())
                                }
                                Err(e) => Err(e),
                            }
                        };

                        match pull_result {
                            Ok(()) => {
                                if !ctx.dry_run {
                                    log::success(&format!("Pulled {}:{}", pkg, result.name));
                                    pulled += 1;
                                }
                            }
                            Err(e) => {
                                log::error(&format!(
                                    "Failed to pull {}:{}: {}",
                                    pkg, result.name, e
                                ));
                                build_errors += 1;
                            }
                        }
                    }
                    SyncAction::Skip => {
                        skipped += 1;
                    }
                    SyncAction::Quit => {
                        log::info("Sync aborted by user");
                        return Ok(());
                    }
                }
            }
        }
    }

    // Summary
    if !ctx.dry_run {
        let error_note = if build_errors > 0 {
            format!(" ({} errors)", build_errors)
        } else {
            String::new()
        };
        log::success(&format!(
            "Sync complete: {} pushed, {} pulled, {} skipped{}",
            pushed, pulled, skipped, error_note
        ));
    }

    Ok(())
}

/// Check if a build result can be auto-pulled.
/// from_file entries can always be pulled. from_config entries use context-aware
/// shadow walk — pull is possible if the walk reaches literal leaves.
fn can_auto_pull(
    ctx: &Context,
    package: &str,
    result: &BuildResult,
    file_entry_index: usize,
) -> bool {
    if result.is_plaintext {
        return true;
    }

    let pkg_dir = ctx.orders_dir.join(package);
    let ncl_path = pkg_dir.join("order.ncl");

    let source = match std::fs::read_to_string(&ncl_path) {
        Ok(s) => s,
        Err(_) => return false,
    };

    match nickel::ast_utils::locate_from_config(&source, file_entry_index, &ctx.metadata) {
        Ok(result) => result.has_any_rewritable(),
        Err(_) => false,
    }
}

/// Build a package and return results with file_entry_index for each result
fn build_package_with_indices(
    ctx: &Context,
    package: &str,
) -> anyhow::Result<Vec<(BuildResult, usize)>> {
    let pkg_dir = ctx.orders_dir.join(package);
    let ncl_path = pkg_dir.join("order.ncl");

    if !ncl_path.exists() {
        return Ok(vec![]);
    }

    let evaluator = nickel::NickelEvaluator::new(&ctx.metadata);
    let order_pkg = evaluator.evaluate(&ncl_path)?;

    if !order_pkg.should_apply(&ctx.metadata.os, &ctx.metadata.arch, &ctx.metadata.hostname) {
        return Ok(vec![]);
    }

    let mut results = Vec::new();
    let global_ignore = order_pkg.global_ignore();
    let global_prefix = order_pkg.global_prefix();

    for (file_entry_index, file_entry) in order_pkg.blend.files.iter().enumerate() {
        if !file_entry.should_apply(&ctx.metadata.os, &ctx.metadata.arch, &ctx.metadata.hostname) {
            if ctx.verbose {
                log::info(&format!(
                    "Skipping file {} (when condition not met)",
                    file_entry.name,
                ));
            }
            continue;
        }

        let mut ignore_keys: Vec<String> = global_ignore.to_vec();
        ignore_keys.extend(file_entry.ignore.iter().cloned());

        for target_path in file_entry.target_paths(global_prefix) {
            let expanded_target = ctx.expand_path(&target_path);
            let result = compose::build_file_entry_pub(
                ctx,
                &pkg_dir,
                file_entry,
                expanded_target,
                ignore_keys.clone(),
            )?;
            results.push((result, file_entry_index));
        }
    }

    Ok(results)
}
