use console::style;
use rayon::prelude::*;

use crate::commands::helpers::{dir_has_inner_symlinks, target_is_unexpected_symlink};
use crate::compose::{discover_packages, get_order_package};
use crate::context::Context;
use crate::diff::check_file_sync;
use crate::output::log;

/// Status command: show available packages and their state
pub fn cmd_status(ctx: &Context) -> anyhow::Result<()> {
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

    let timing = std::env::var("BLEND_TIMING").is_ok();
    let row_groups: Vec<Vec<String>> = pkg_list
        .par_iter()
        .map(|pkg| {
            let t_pkg = std::time::Instant::now();
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
                            } else if target.exists() || target.symlink_metadata().is_ok() {
                                // Check for unexpected symlink (stow leftover).
                                // For directory entries, also walk the source dir
                                // and detect per-file symlinks within the target,
                                // since the directory itself can be a real dir
                                // while inner files are still legacy symlinks.
                                let pkg_dir = ctx.orders_dir.join(pkg);
                                let unexpected_sym =
                                    target_is_unexpected_symlink(&target, file_entry.symlink);
                                let inner_sym = !file_entry.symlink
                                    && file_entry
                                        .from_file
                                        .as_ref()
                                        .map(|f| pkg_dir.join(f))
                                        .filter(|p| p.is_dir())
                                        .is_some_and(|src| dir_has_inner_symlinks(&src, &target));

                                if unexpected_sym || inner_sym {
                                    (
                                        style(format!("{:<status_w$}", "symlinked"))
                                            .yellow()
                                            .to_string(),
                                        style(format!("{:<diff_w$}", "\u{2260}"))
                                            .yellow()
                                            .to_string(),
                                    )
                                } else {
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
                                        None => style(format!("{:<diff_w$}", "\u{00b7}"))
                                            .dim()
                                            .to_string(),
                                    };
                                    (
                                        style(format!("{:<status_w$}", "deployed"))
                                            .green()
                                            .to_string(),
                                        diff_col,
                                    )
                                }
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
            if timing {
                eprintln!(
                    "[timing] pkg {} total={}us rows={}",
                    pkg,
                    t_pkg.elapsed().as_micros(),
                    rows.len()
                );
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
