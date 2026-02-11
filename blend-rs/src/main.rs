mod cli;
mod compose;
mod context;
mod diff;
mod formats;
mod metadata;
mod nickel;
mod output;
mod sync;
mod upgrade;

use clap::Parser;
use console::style;
use rayon::prelude::*;

use cli::{Cli, Commands};
use compose::{BuildResult, build_package, discover_packages, get_order_package, write_result};
use context::Context;
use diff::{DiffResult, check_file_sync, diff_configs};
use output::log;
use sync::{Prompter, SyncAction, SyncMode, TerminalPrompter};

fn main() {
    let cli = Cli::parse();
    let ctx = Context::new(&cli);

    if ctx.verbose {
        log::info(&format!("Home directory: {}", ctx.home_dir.display()));
        log::info(&format!("Orders directory: {}", ctx.orders_dir.display()));
        log::info(&format!(
            "OS: {}, Arch: {}",
            ctx.metadata.os, ctx.metadata.arch
        ));
    }

    let result = match cli.command {
        Some(Commands::Sync {
            packages,
            push,
            pull,
            no_rewrite,
        }) => {
            let mode = if push {
                SyncMode::PushAll
            } else if pull {
                SyncMode::PullAll
            } else {
                SyncMode::Interactive
            };
            cmd_sync(&ctx, &packages, mode, no_rewrite, &TerminalPrompter)
        }
        Some(Commands::View {
            packages,
            content_only,
            all,
        }) => cmd_view(&ctx, &packages, content_only, all),
        Some(Commands::Table) => cmd_table(&ctx),
        Some(Commands::Upgrade { step }) => upgrade::cmd_upgrade(&ctx, &step, |ctx| {
            cmd_sync(ctx, &[], SyncMode::Interactive, false, &TerminalPrompter)
        }),
        None => cmd_status(&ctx),
    };

    if let Err(e) = result {
        log::error(&format!("Error: {e}"));
        std::process::exit(1);
    }
}

/// Compute the diff between a build result and the deployed file
fn compute_diff_for_result(result: &BuildResult) -> DiffResult {
    if result.is_symlink {
        return DiffResult::no_changes();
    }

    if !result.target.exists() {
        return DiffResult::no_changes();
    }

    if result.is_plaintext {
        if let Some(source_path) = &result.source_path {
            if source_path.is_dir() {
                return DiffResult::no_changes(); // skip directory-level diff
            }
            if let (Ok(source_content), Ok(deployed)) = (
                std::fs::read_to_string(source_path),
                std::fs::read_to_string(&result.target),
            ) {
                return diff_configs(
                    nickel::Format::Plaintext,
                    &source_content,
                    &deployed,
                    &result.ignore_keys,
                );
            }
        }
        DiffResult::no_changes()
    } else if let Ok(deployed) = std::fs::read_to_string(&result.target) {
        let format = nickel::Format::from_path(&result.name);
        diff_configs(format, &result.content, &deployed, &result.ignore_keys)
    } else {
        DiffResult::no_changes()
    }
}

/// Sync command: bidirectional sync between repo and deployed configs
fn cmd_sync(
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

            if !diff_result.has_changes {
                if ctx.verbose {
                    log::info(&format!("{}:{} in sync", pkg, result.name));
                }
                continue;
            }

            // Display the diff
            sync::display_conflict(
                pkg,
                &result.name,
                &result.target,
                &diff_result,
                &ctx.home_dir,
            );

            // Determine if auto-pull is possible for this entry
            let can_pull = !no_rewrite && can_auto_pull(ctx, pkg, result, *file_entry_index);

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
                            sync::pull_from_file(source_path, &result.target, ctx.dry_run)
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
                            log::error(&format!("Failed to pull {}:{}: {}", pkg, result.name, e));
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

/// View command: show generated config and/or diff from deployed
fn cmd_view(
    ctx: &Context,
    packages: &[String],
    content_only: bool,
    show_all: bool,
) -> anyhow::Result<()> {
    let all_packages = discover_packages(&ctx.orders_dir);
    let viewing_specific = !packages.is_empty();

    let to_view: Vec<String> = if packages.is_empty() {
        all_packages.into_iter().collect()
    } else {
        packages.to_vec()
    };

    let show_content = content_only || show_all;
    let show_diff = !content_only;
    let mut has_changes = false;

    let shorten_path = |path: &std::path::Path| -> String {
        let s = path.to_string_lossy();
        let home = ctx.home_dir.to_string_lossy();
        if s.starts_with(home.as_ref()) {
            format!("~{}", &s[home.len()..])
        } else {
            s.into_owned()
        }
    };

    for pkg in &to_view {
        match build_package(ctx, pkg) {
            Ok(results) => {
                if results.is_empty() {
                    if viewing_specific {
                        log::info(&format!("{pkg} skipped (condition not met)"));
                    }
                    continue;
                }

                println!("\n{}", style(pkg).cyan().bold());

                for result in results {
                    let target_display = shorten_path(&result.target);
                    let file_header = format!("  {} -> {}", result.name, target_display);

                    if result.is_symlink {
                        if let Some(canonical) = &result.canonical_source {
                            let link_status = match std::fs::read_link(&result.target) {
                                Ok(existing) if existing == *canonical => {
                                    style("(linked)").green().to_string()
                                }
                                Ok(_) => style("(wrong target)").yellow().to_string(),
                                Err(_) => style("(not linked)").yellow().to_string(),
                            };
                            println!(
                                "{} {} {}",
                                file_header,
                                style("(symlink)").dim(),
                                link_status
                            );
                        }
                        continue;
                    }

                    if result.is_plaintext {
                        if let Some(source_path) = &result.source_path {
                            let is_dir = source_path.is_dir();
                            let mut annotations = Vec::new();
                            if show_content {
                                let kind = if is_dir { "directory" } else { "file" };
                                annotations
                                    .push(style(format!("(plaintext {})", kind)).dim().to_string());
                            }

                            if show_diff && !is_dir {
                                let diff_result = compute_diff_for_result(&result);
                                if diff_result.has_changes {
                                    println!("{}", file_header);
                                    for line in diff_result.output.lines() {
                                        println!("    {}", line);
                                    }
                                    has_changes = true;
                                } else if !result.target.exists() {
                                    annotations.push(style("(not deployed)").yellow().to_string());
                                    println!("{} {}", file_header, annotations.join(" "));
                                    has_changes = true;
                                } else {
                                    annotations.push(style("(no changes)").dim().to_string());
                                    println!("{} {}", file_header, annotations.join(" "));
                                }
                            } else if show_diff && is_dir {
                                annotations.push(style("(directory)").dim().to_string());
                                println!("{} {}", file_header, annotations.join(" "));
                            } else if annotations.is_empty() {
                                println!("{}", file_header);
                            } else {
                                println!("{} {}", file_header, annotations.join(" "));
                            }
                        }
                        continue;
                    }

                    // Structured config
                    let diff_status = if show_diff {
                        Some(compute_diff_for_result(&result))
                    } else {
                        None
                    };

                    let has_diff_output = match &diff_status {
                        Some(dr) => dr.has_changes,
                        None if show_diff => !result.target.exists(),
                        _ => false,
                    };

                    if has_diff_output || show_content {
                        if !show_diff {
                            println!("{}", file_header);
                        } else if !result.target.exists() {
                            println!("{} {}", file_header, style("(not deployed)").yellow());
                            has_changes = true;
                        } else {
                            println!("{}", file_header);
                        }

                        if show_content {
                            for line in result.content.lines() {
                                println!("    {}", style(line).dim());
                            }
                        }

                        if let Some(dr) = &diff_status
                            && dr.has_changes
                        {
                            for line in dr.output.lines() {
                                println!("    {}", line);
                            }
                            has_changes = true;
                        }
                    } else {
                        println!("{} {}", file_header, style("(no changes)").dim());
                    }
                }
            }
            Err(e) => {
                log::error(&format!("Failed to evaluate {pkg}: {e}"));
            }
        }
    }

    if show_diff && !has_changes && !to_view.is_empty() {
        println!();
        log::success("All packages are up to date");
    }

    Ok(())
}

/// Table command: output package info as HTML table for README
fn cmd_table(ctx: &Context) -> anyhow::Result<()> {
    let packages = discover_packages(&ctx.orders_dir);

    let profiles: &[(&str, &str, &str)] = &[
        ("linux", "x86_64", "linux-x86_64"),
        ("darwin", "x86_64", "macos-x86_64"),
        ("darwin", "aarch64", "macos-aarch64"),
    ];

    let mut pkg_data: Vec<(String, Vec<bool>, usize)> = Vec::new();

    for pkg in &packages {
        match get_order_package(ctx, pkg) {
            Ok(order_pkg) => {
                let matches: Vec<bool> = profiles
                    .iter()
                    .map(|(os, arch, _)| order_pkg.applies_on_platform(os, arch))
                    .collect();
                let match_count = matches.iter().filter(|&&m| m).count();
                pkg_data.push((pkg.clone(), matches, match_count));
            }
            Err(e) => {
                log::warn(&format!("Skipping {pkg} (eval error: {e})"));
            }
        }
    }

    pkg_data.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));

    print!("<table><thead><tr><th>package</th><th colspan=\"3\">profiles</th></tr></thead><tbody>");
    for (name, matches, _) in &pkg_data {
        print!("\n<tr><td><a href=\"orders/{name}\">{name}</a></td>");
        for (i, (_os, _arch, label)) in profiles.iter().enumerate() {
            if matches[i] {
                print!("<td><code>{label}</code></td>");
            } else {
                print!("<td><code>&nbsp;...</code></td>");
            }
        }
        print!("</tr>");
    }
    println!("\n</tbody></table>");

    Ok(())
}

/// Status command: show available packages and their state
fn cmd_status(ctx: &Context) -> anyhow::Result<()> {
    if !ctx.orders_dir.is_dir() {
        log::error(&format!(
            "Orders directory not found: {}",
            ctx.orders_dir.display()
        ));
        std::process::exit(1);
    }

    let packages = discover_packages(&ctx.orders_dir);
    log::success(&format!("Found {} packages in orders/", packages.len()));

    let pkg_w = 20;
    let file_w = 20;
    let status_w = 10;
    let diff_w = 5;

    println!(
        "\n{} {} {} {} {}",
        style(format!("{:<pkg_w$}", "PACKAGE")).bold(),
        style(format!("{:<file_w$}", "FILE")).bold(),
        style(format!("{:<status_w$}", "STATUS")).bold(),
        style(format!("{:<diff_w$}", "DIFF")).bold(),
        style("TARGET").bold()
    );
    println!("{}", "-".repeat(pkg_w + file_w + status_w + diff_w + 40));

    let mut pkg_list: Vec<_> = packages.into_iter().collect::<Vec<_>>();
    pkg_list.sort();

    let row_groups: Vec<Vec<String>> = pkg_list
        .par_iter()
        .map(|pkg| {
            let mut rows = Vec::new();
            match get_order_package(ctx, pkg) {
                Ok(order_pkg) => {
                    let applies = order_pkg.should_apply(
                        &ctx.metadata.os,
                        &ctx.metadata.arch,
                        &ctx.metadata.hostname,
                    );

                    if !applies {
                        rows.push(format!(
                            "{} {} {} {} {}",
                            style(format!("{:<pkg_w$}", pkg)).dim(),
                            style(format!("{:<file_w$}", "-")).dim(),
                            style(format!("{:<status_w$}", "skipped")).dim(),
                            style(format!("{:<diff_w$}", "\u{00b7}")).dim(),
                            style("(condition not met)").dim()
                        ));
                        return rows;
                    }

                    let files = &order_pkg.blend.files;
                    let global_prefix = order_pkg.global_prefix();
                    for (i, file_entry) in files.iter().enumerate() {
                        let file_applies = file_entry.should_apply(
                            &ctx.metadata.os,
                            &ctx.metadata.arch,
                            &ctx.metadata.hostname,
                        );

                        if !file_applies {
                            if ctx.verbose {
                                let pkg_display = if i == 0 { pkg.as_str() } else { "" };
                                rows.push(format!(
                                    "{} {} {} {} {}",
                                    style(format!("{:<pkg_w$}", pkg_display)).dim(),
                                    style(format!("{:<file_w$}", &file_entry.name)).dim(),
                                    style(format!("{:<status_w$}", "skipped")).dim(),
                                    style(format!("{:<diff_w$}", "\u{00b7}")).dim(),
                                    style("(condition not met)").dim()
                                ));
                            }
                            continue;
                        }

                        for (j, target_path) in
                            file_entry.target_paths(global_prefix).iter().enumerate()
                        {
                            let target = ctx.expand_path(target_path);

                            let pkg_display = if i == 0 && j == 0 {
                                style(format!("{:<pkg_w$}", pkg)).cyan().to_string()
                            } else {
                                format!("{:<pkg_w$}", "")
                            };

                            let source_name = &file_entry.name;
                            let is_dir = file_entry
                                .from_file
                                .as_ref()
                                .map(|f| ctx.orders_dir.join(pkg).join(f).is_dir())
                                .unwrap_or(false);
                            let source_display = if source_name.len() > file_w {
                                format!("{:<file_w$}", format!("{}...", &source_name[..file_w - 3]))
                            } else if is_dir {
                                format!("{:<file_w$}", format!("{}/", source_name))
                            } else {
                                format!("{:<file_w$}", source_name)
                            };

                            let (status, diff_display) = if file_entry.symlink {
                                // Symlink entry: check if symlink exists and points correctly
                                let source_path = ctx
                                    .orders_dir
                                    .join(pkg)
                                    .join(file_entry.from_file.as_deref().unwrap_or(""));
                                let canonical = source_path.canonicalize().ok();
                                let linked_ok = match std::fs::read_link(&target) {
                                    Ok(existing) => {
                                        canonical.as_deref() == Some(existing.as_path())
                                    }
                                    Err(_) => false,
                                };
                                if linked_ok {
                                    (
                                        style(format!("{:<status_w$}", "linked"))
                                            .green()
                                            .to_string(),
                                        style(format!("{:<diff_w$}", "\u{2713}"))
                                            .green()
                                            .to_string(),
                                    )
                                } else if target.exists() || target.symlink_metadata().is_ok() {
                                    (
                                        style(format!("{:<status_w$}", "linked"))
                                            .yellow()
                                            .to_string(),
                                        style(format!("{:<diff_w$}", "\u{2260}"))
                                            .yellow()
                                            .to_string(),
                                    )
                                } else {
                                    (
                                        style(format!("{:<status_w$}", "pending"))
                                            .yellow()
                                            .to_string(),
                                        style(format!("{:<diff_w$}", "\u{00b7}")).dim().to_string(),
                                    )
                                }
                            } else if target.exists() {
                                let pkg_dir = ctx.orders_dir.join(pkg);
                                let sync = check_file_sync(
                                    &pkg_dir,
                                    file_entry,
                                    &target,
                                    order_pkg.global_ignore(),
                                );
                                let diff_col = match sync {
                                    Some(true) => style(format!("{:<diff_w$}", "\u{2713}"))
                                        .green()
                                        .to_string(),
                                    Some(false) => style(format!("{:<diff_w$}", "\u{2260}"))
                                        .yellow()
                                        .to_string(),
                                    None => {
                                        style(format!("{:<diff_w$}", "\u{00b7}")).dim().to_string()
                                    }
                                };
                                (
                                    style(format!("{:<status_w$}", "deployed"))
                                        .green()
                                        .to_string(),
                                    diff_col,
                                )
                            } else {
                                (
                                    style(format!("{:<status_w$}", "pending"))
                                        .yellow()
                                        .to_string(),
                                    style(format!("{:<diff_w$}", "\u{00b7}")).dim().to_string(),
                                )
                            };

                            let target_str = target.to_string_lossy();
                            let home_str = ctx.home_dir.to_string_lossy();
                            let target_display = if target_str.starts_with(home_str.as_ref()) {
                                format!("~{}", &target_str[home_str.len()..])
                            } else {
                                target_str.into_owned()
                            };

                            rows.push(format!(
                                "{} {} {} {} {}",
                                pkg_display, source_display, status, diff_display, target_display
                            ));
                        }
                    }
                }
                Err(e) => {
                    let dash_display = format!("{:<file_w$}", "-");
                    rows.push(format!(
                        "{} {} {} {} {}",
                        style(format!("{:<pkg_w$}", pkg)).red(),
                        dash_display,
                        style(format!("{:<status_w$}", "error")).red(),
                        style(format!("{:<diff_w$}", "\u{00b7}")).dim(),
                        style(e.to_string()).red()
                    ));
                }
            }
            rows
        })
        .collect();

    for rows in row_groups {
        for row in rows {
            println!("{}", row);
        }
    }

    println!();
    log::info(&format!(
        "System: {} / {} / {}",
        ctx.metadata.os, ctx.metadata.arch, ctx.metadata.hostname
    ));

    Ok(())
}
