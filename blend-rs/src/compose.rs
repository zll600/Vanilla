use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context as AnyhowContext, Result};
use walkdir::WalkDir;

use crate::context::Context;
use crate::formats::get_renderer;
use crate::nickel::{FileEntry, NickelEvaluator, OrderPackage};
use crate::output::log;

/// Discover packages in the orders directory
pub fn discover_packages(orders_dir: &Path) -> HashSet<String> {
    let mut packages = HashSet::new();

    let Ok(entries) = std::fs::read_dir(orders_dir) else {
        return packages;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };

        // Skip hidden directories
        if name.starts_with('.') {
            continue;
        }

        // Check for order.ncl
        if path.join("order.ncl").exists() {
            packages.insert(name.to_string());
        }
    }

    packages
}

/// Result of building a single file entry target
#[derive(Debug)]
pub struct BuildResult {
    /// Target path (expanded)
    pub target: PathBuf,
    /// Rendered content (empty for plaintext)
    pub content: String,
    /// Whether this is a plaintext copy
    pub is_plaintext: bool,
    /// Source path for plaintext copies
    pub source_path: Option<PathBuf>,
    /// Name from FileEntry
    pub name: String,
    /// Merged ignore keys (global + per-file)
    pub ignore_keys: Vec<String>,
    /// Whether this entry should be symlinked instead of copied
    pub is_symlink: bool,
    /// Canonical (absolute) source path for symlink entries
    pub canonical_source: Option<PathBuf>,
}

/// Build a single package, returning results for all file entries and targets
pub fn build_package(ctx: &Context, package: &str) -> Result<Vec<BuildResult>> {
    let pkg_dir = ctx.orders_dir.join(package);
    let ncl_path = pkg_dir.join("order.ncl");

    if !ncl_path.exists() {
        return Ok(vec![]);
    }

    let evaluator = NickelEvaluator::new(&ctx.metadata);
    let order_pkg = evaluator.evaluate(&ncl_path)?;

    // Check if package should be applied for this system
    if !order_pkg.should_apply(&ctx.metadata.os, &ctx.metadata.arch, &ctx.metadata.hostname) {
        return Ok(vec![]);
    }

    let mut results = Vec::new();
    let global_ignore = order_pkg.global_ignore();
    let global_prefix = order_pkg.global_prefix();

    for file_entry in &order_pkg.blend.files {
        // Check per-file condition
        if !file_entry.should_apply(&ctx.metadata.os, &ctx.metadata.arch, &ctx.metadata.hostname) {
            if ctx.verbose {
                log::info(&format!(
                    "Skipping file {} (when condition not met)",
                    file_entry.name,
                ));
            }
            continue;
        }

        // Merge ignore keys: global + per-file
        let mut ignore_keys: Vec<String> = global_ignore.to_vec();
        ignore_keys.extend(file_entry.ignore.iter().cloned());

        // Build for each target prefix (file-level overrides global)
        for target_path in file_entry.target_paths(global_prefix) {
            let expanded_target = ctx.expand_path(&target_path);
            let result = build_file_entry(
                ctx,
                &pkg_dir,
                file_entry,
                expanded_target,
                ignore_keys.clone(),
            )?;
            results.push(result);
        }
    }

    Ok(results)
}

/// Build a single file entry to a specific target (public wrapper)
pub fn build_file_entry_pub(
    ctx: &Context,
    pkg_dir: &Path,
    entry: &FileEntry,
    target: PathBuf,
    ignore_keys: Vec<String>,
) -> Result<BuildResult> {
    build_file_entry(ctx, pkg_dir, entry, target, ignore_keys)
}

/// Build a single file entry to a specific target
fn build_file_entry(
    _ctx: &Context,
    pkg_dir: &Path,
    entry: &FileEntry,
    target: PathBuf,
    ignore_keys: Vec<String>,
) -> Result<BuildResult> {
    if let Some(file) = &entry.from_file {
        let source_path = pkg_dir.join(file);
        if !source_path.exists() {
            return Err(anyhow::anyhow!(
                "File entry '{}': source file not found at {}",
                entry.name,
                source_path.display()
            ));
        }

        if entry.symlink {
            let canonical = source_path.canonicalize().with_context(|| {
                format!(
                    "Failed to canonicalize source path {}",
                    source_path.display()
                )
            })?;
            return Ok(BuildResult {
                target,
                content: String::new(),
                is_plaintext: true,
                source_path: Some(source_path),
                name: entry.name.clone(),
                ignore_keys,
                is_symlink: true,
                canonical_source: Some(canonical),
            });
        }

        return Ok(BuildResult {
            target,
            content: String::new(),
            is_plaintext: true,
            source_path: Some(source_path),
            name: entry.name.clone(),
            ignore_keys,
            is_symlink: false,
            canonical_source: None,
        });
    }

    if let Some(config) = &entry.from_config {
        let format = entry.effective_format();
        let renderer = get_renderer(format);
        let content = renderer.render(config)?;

        return Ok(BuildResult {
            target,
            content,
            is_plaintext: false,
            source_path: None,
            name: entry.name.clone(),
            ignore_keys,
            is_symlink: false,
            canonical_source: None,
        });
    }

    // Unreachable after resolve_defaults validation
    Err(anyhow::anyhow!(
        "File entry '{}' has neither 'from_file' nor 'from_config'",
        entry.name,
    ))
}

/// If the target path is a symlink, remove it so we write a regular file
/// instead of writing through the symlink to its target.
fn remove_symlink_if_exists(path: &Path, dry_run: bool) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            if dry_run {
                log::info(&format!("Would remove symlink {}", path.display()));
                return Ok(());
            }
            std::fs::remove_file(path)
                .with_context(|| format!("Failed to remove symlink {}", path.display()))?;
        }
        _ => {}
    }
    Ok(())
}

/// Write build result to target
pub fn write_result(result: &BuildResult, dry_run: bool) -> Result<()> {
    if result.is_symlink {
        if let Some(canonical) = &result.canonical_source {
            return create_symlink(canonical, &result.target, dry_run);
        }
        return Err(anyhow::anyhow!(
            "Symlink entry '{}' has no canonical source path",
            result.name
        ));
    }

    if result.is_plaintext {
        if let Some(source_path) = &result.source_path {
            if source_path.is_dir() {
                copy_directory(source_path, &result.target, dry_run)?;
            } else {
                copy_file(source_path, &result.target, dry_run)?;
            }
        }
    } else {
        if dry_run {
            remove_symlink_if_exists(&result.target, true)?;
            log::info(&format!("Would write to {}", result.target.display()));
            return Ok(());
        }

        // Ensure parent directory exists
        if let Some(parent) = result.target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        }

        remove_symlink_if_exists(&result.target, false)?;
        std::fs::write(&result.target, &result.content)
            .with_context(|| format!("Failed to write {}", result.target.display()))?;
    }

    Ok(())
}

/// Create a symlink at target pointing to source
fn create_symlink(source: &Path, target: &Path, dry_run: bool) -> Result<()> {
    if dry_run {
        log::info(&format!(
            "Would symlink {} -> {}",
            target.display(),
            source.display()
        ));
        return Ok(());
    }

    // Ensure parent directory exists
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory {}", parent.display()))?;
    }

    // Remove existing file, symlink, or directory at target
    match std::fs::symlink_metadata(target) {
        Ok(meta) => {
            if meta.is_dir() && !meta.file_type().is_symlink() {
                std::fs::remove_dir_all(target)
                    .with_context(|| format!("Failed to remove directory {}", target.display()))?;
            } else {
                std::fs::remove_file(target)
                    .with_context(|| format!("Failed to remove {}", target.display()))?;
            }
        }
        Err(_) => {} // Target doesn't exist, that's fine
    }

    #[cfg(unix)]
    std::os::unix::fs::symlink(source, target).with_context(|| {
        format!(
            "Failed to create symlink {} -> {}",
            target.display(),
            source.display()
        )
    })?;

    #[cfg(not(unix))]
    return Err(anyhow::anyhow!(
        "Symlinks are only supported on Unix platforms"
    ));

    Ok(())
}

/// Copy a single file to target
fn copy_file(source: &Path, target: &Path, dry_run: bool) -> Result<()> {
    if dry_run {
        remove_symlink_if_exists(target, true)?;
        log::info(&format!(
            "Would copy {} to {}",
            source.display(),
            target.display()
        ));
        return Ok(());
    }

    // Ensure parent directory exists
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory {}", parent.display()))?;
    }

    remove_symlink_if_exists(target, false)?;
    std::fs::copy(source, target).with_context(|| {
        format!(
            "Failed to copy {} to {}",
            source.display(),
            target.display()
        )
    })?;

    Ok(())
}

/// Copy a source directory to target
fn copy_directory(source: &Path, target: &Path, dry_run: bool) -> Result<()> {
    if !source.exists() {
        return Err(anyhow::anyhow!(
            "Source directory does not exist: {}",
            source.display()
        ));
    }

    // If the top-level target is a symlink, remove it first
    remove_symlink_if_exists(target, dry_run)?;

    for entry in WalkDir::new(source).min_depth(1) {
        let entry = entry?;
        let rel_path = entry.path().strip_prefix(source)?;
        let target_path = target.join(rel_path);

        if dry_run {
            if entry.file_type().is_dir() {
                log::info(&format!("Would create dir {}", target_path.display()));
            } else {
                log::info(&format!("Would copy to {}", target_path.display()));
            }
            continue;
        }

        if entry.file_type().is_dir() {
            remove_symlink_if_exists(&target_path, false)?;
            std::fs::create_dir_all(&target_path)?;
        } else {
            if let Some(parent) = target_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            remove_symlink_if_exists(&target_path, false)?;
            std::fs::copy(entry.path(), &target_path)?;
        }
    }

    Ok(())
}

/// Get the evaluated OrderPackage for a package
pub fn get_order_package(ctx: &Context, package: &str) -> Result<OrderPackage> {
    let pkg_dir = ctx.orders_dir.join(package);
    let ncl_path = pkg_dir.join("order.ncl");

    let evaluator = NickelEvaluator::new(&ctx.metadata);
    evaluator.evaluate(&ncl_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_discover_packages() {
        let temp = TempDir::new().unwrap();
        let orders = temp.path();

        // Create package with order.ncl
        let pkg1 = orders.join("package1");
        std::fs::create_dir(&pkg1).unwrap();
        std::fs::write(pkg1.join("order.ncl"), "{}").unwrap();

        // Create package without order.ncl
        let pkg2 = orders.join("package2");
        std::fs::create_dir(&pkg2).unwrap();

        let packages = discover_packages(orders);
        assert!(packages.contains("package1"));
        assert!(!packages.contains("package2"));
    }
}
