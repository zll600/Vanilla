//! E2E tests for blend sync using real fixtures and temporary home directories.
//!
//! These tests exercise the full sync flow (build → diff → push/pull) without
//! interactive prompts by using --push or --pull flags.

use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn blend_binary() -> PathBuf {
    // Use the debug binary from cargo build
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // remove test binary name
    path.pop(); // remove deps/
    path.push("blend");
    path
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Run blend with given args, using a temp home and the test fixtures as orders dir
fn run_blend(home: &Path, orders: &Path, args: &[&str]) -> std::process::Output {
    Command::new(blend_binary())
        .args(args)
        .arg("--home")
        .arg(home)
        .arg("--orders")
        .arg(orders)
        .output()
        .expect("Failed to execute blend")
}

/// Copy a single fixture package to a temporary orders directory.
/// Returns the TempDir (which owns the temp path) — the orders dir is at temp.path().
/// Needed for pull tests that modify source .ncl files.
fn copy_fixture(pkg_name: &str) -> TempDir {
    let temp = TempDir::new().unwrap();
    let src = fixtures_dir().join("orders").join(pkg_name);
    let dst = temp.path().join(pkg_name);
    copy_dir_recursive(&src, &dst);
    temp
}

fn copy_dir_recursive(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let target = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_recursive(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

#[test]
fn test_sync_push_plain_data_new_file() {
    let home = TempDir::new().unwrap();
    let orders = fixtures_dir().join("orders");

    // Config file doesn't exist yet — sync --push should create it
    let target = home.path().join(".config/toml-basic/config.toml");
    assert!(!target.exists());

    let output = run_blend(home.path(), &orders, &["sync", "--push", "toml-basic"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "blend sync --push failed:\nstdout: {stdout}\nstderr: {stderr}"
    );

    // File should now exist
    assert!(target.exists(), "Config file should have been created");

    let content = std::fs::read_to_string(&target).unwrap();
    assert!(content.contains("key"), "Config should contain 'key'");
    assert!(content.contains("42"), "Config should contain '42'");
}

#[test]
fn test_sync_push_then_no_changes() {
    let home = TempDir::new().unwrap();
    let orders = fixtures_dir().join("orders");

    // First push
    run_blend(home.path(), &orders, &["sync", "--push", "toml-basic"]);

    // Second sync should show no changes (nothing to do)
    let output = run_blend(
        home.path(),
        &orders,
        &["sync", "--push", "-v", "toml-basic"],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(
        stdout.contains("in sync") || stdout.contains("0 pushed"),
        "Should be in sync after push, got: {stdout}"
    );
}

#[test]
fn test_sync_dry_run_no_changes() {
    let home = TempDir::new().unwrap();
    let orders = fixtures_dir().join("orders");

    // Dry run should not create any files
    let target = home.path().join(".config/toml-basic/config.toml");
    let output = run_blend(home.path(), &orders, &["sync", "-n", "toml-basic"]);

    assert!(output.status.success());
    assert!(!target.exists(), "Dry run should not create files");
}

#[test]
fn test_sync_push_from_file() {
    let home = TempDir::new().unwrap();
    let orders = fixtures_dir().join("orders");

    let target = home.path().join(".config/plaintext-single/config.txt");
    assert!(!target.exists());

    let output = run_blend(
        home.path(),
        &orders,
        &["sync", "--push", "plaintext-single"],
    );
    assert!(output.status.success());
    assert!(target.exists());

    let content = std::fs::read_to_string(&target).unwrap();
    assert!(content.contains("original content from repo"));
}

#[test]
fn test_sync_pull_from_file() {
    let home = TempDir::new().unwrap();

    // Copy fixtures to a temp location so we can modify the orders dir
    let temp_orders = TempDir::new().unwrap();
    let orders = temp_orders.path();

    // Copy the test-file fixture
    let pkg_dir = orders.join("plaintext-single");
    std::fs::create_dir_all(&pkg_dir).unwrap();
    std::fs::copy(
        fixtures_dir().join("orders/plaintext-single/order.ncl"),
        pkg_dir.join("order.ncl"),
    )
    .unwrap();
    std::fs::copy(
        fixtures_dir().join("orders/plaintext-single/config.txt"),
        pkg_dir.join("config.txt"),
    )
    .unwrap();

    // First push to deploy
    run_blend(home.path(), orders, &["sync", "--push", "plaintext-single"]);

    let target = home.path().join(".config/plaintext-single/config.txt");
    assert!(target.exists());

    // Modify the deployed file
    std::fs::write(&target, "modified by user\nnew line\n").unwrap();

    // Pull back
    let output = run_blend(home.path(), orders, &["sync", "--pull", "plaintext-single"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "Pull failed: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Source file in orders should now have the deployed content
    let source_content = std::fs::read_to_string(pkg_dir.join("config.txt")).unwrap();
    assert_eq!(source_content, "modified by user\nnew line\n");
}

#[test]
fn test_view_shows_diffs() {
    let home = TempDir::new().unwrap();
    let orders = fixtures_dir().join("orders");

    // Push first
    run_blend(home.path(), &orders, &["sync", "--push", "toml-basic"]);

    // View should show no changes
    let output = run_blend(home.path(), &orders, &["view", "toml-basic"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(
        stdout.contains("no changes"),
        "Should show no changes: {stdout}"
    );
}

#[test]
fn test_status_shows_packages() {
    let home = TempDir::new().unwrap();
    let orders = fixtures_dir().join("orders");

    let output = run_blend(home.path(), &orders, &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(
        stdout.contains("toml-basic"),
        "Should list test-plain package"
    );
    assert!(
        stdout.contains("plaintext-single"),
        "Should list test-file package"
    );
}

// ---------------------------------------------------------------------------
// Match conditional tests
// ---------------------------------------------------------------------------

#[test]
fn test_sync_push_match_conditional() {
    let home = TempDir::new().unwrap();
    let orders = fixtures_dir().join("orders");

    let target = home.path().join(".config/os-match/config.toml");
    let output = run_blend(home.path(), &orders, &["sync", "--push", "os-match"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "sync --push test-match failed:\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(target.exists(), "Config file should have been created");

    let content = std::fs::read_to_string(&target).unwrap();

    // Should contain the platform-appropriate font_size
    let expected_font_size = match std::env::consts::OS {
        "macos" => "14",
        "linux" => "12",
        _ => "10",
    };
    assert!(
        content.contains(expected_font_size),
        "Should contain font_size = {expected_font_size} for this platform, got:\n{content}"
    );
    // Static value should always be present
    assert!(
        content.contains("catppuccin"),
        "Should contain theme = catppuccin, got:\n{content}"
    );
}

#[test]
fn test_sync_pull_from_config_match_branch() {
    let home = TempDir::new().unwrap();
    let temp_orders = copy_fixture("os-match");
    let orders = temp_orders.path();

    // Push first
    let output = run_blend(home.path(), orders, &["sync", "--push", "os-match"]);
    assert!(output.status.success(), "Initial push failed");

    let target = home.path().join(".config/os-match/config.toml");
    assert!(target.exists());

    // Read and modify the deployed file — change font_size to 20
    let content = std::fs::read_to_string(&target).unwrap();
    let modified = content
        .lines()
        .map(|line| {
            if line.starts_with("font_size") {
                "font_size = 20"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&target, &modified).unwrap();

    // Pull back
    let output = run_blend(home.path(), orders, &["sync", "--pull", "os-match"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "Pull failed:\nstdout: {stdout}\nstderr: {stderr}"
    );

    // Read the modified order.ncl
    let ncl_content = std::fs::read_to_string(orders.join("os-match/order.ncl")).unwrap();

    // The active branch should have been updated to 20
    let active_branch = match std::env::consts::OS {
        "macos" => "\"darwin\" => 20",
        "linux" => "\"linux\" => 20",
        _ => "_ => 20",
    };
    assert!(
        ncl_content.contains(active_branch),
        "Active branch should be updated to 20.\nExpected to find: {active_branch}\nGot:\n{ncl_content}"
    );

    // Other branches should be untouched
    match std::env::consts::OS {
        "macos" => {
            assert!(
                ncl_content.contains("\"linux\" => 12"),
                "Linux branch should be untouched"
            );
            assert!(
                ncl_content.contains("_ => 10"),
                "Wildcard branch should be untouched"
            );
        }
        "linux" => {
            assert!(
                ncl_content.contains("\"darwin\" => 14"),
                "Darwin branch should be untouched"
            );
            assert!(
                ncl_content.contains("_ => 10"),
                "Wildcard branch should be untouched"
            );
        }
        _ => {
            assert!(
                ncl_content.contains("\"darwin\" => 14"),
                "Darwin branch should be untouched"
            );
            assert!(
                ncl_content.contains("\"linux\" => 12"),
                "Linux branch should be untouched"
            );
        }
    }

    // Re-run sync — should show no changes (round-trip correctness)
    let output = run_blend(home.path(), orders, &["sync", "--push", "-v", "os-match"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("in sync") || stdout.contains("0 pushed"),
        "Should be in sync after pull round-trip, got: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// If-then-else conditional tests
// ---------------------------------------------------------------------------

#[test]
fn test_sync_push_if_then_else() {
    let home = TempDir::new().unwrap();
    let orders = fixtures_dir().join("orders");

    let target = home.path().join(".config/if-then-else/config.toml");
    let output = run_blend(home.path(), &orders, &["sync", "--push", "if-then-else"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "sync --push test-if failed:\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(target.exists());

    let content = std::fs::read_to_string(&target).unwrap();
    let expected_gpu = match std::env::consts::OS {
        "macos" => "true",
        _ => "false",
    };
    assert!(
        content.contains(expected_gpu),
        "Should contain use_gpu = {expected_gpu}, got:\n{content}"
    );
    assert!(content.contains("test"), "Should contain label = test");
}

#[test]
fn test_sync_pull_if_then_else_branch() {
    let home = TempDir::new().unwrap();
    let temp_orders = copy_fixture("if-then-else");
    let orders = temp_orders.path();

    // Push first
    let output = run_blend(home.path(), orders, &["sync", "--push", "if-then-else"]);
    assert!(output.status.success(), "Initial push failed");

    let target = home.path().join(".config/if-then-else/config.toml");

    // Flip the boolean value in deployed file
    let content = std::fs::read_to_string(&target).unwrap();
    let modified = content
        .lines()
        .map(|line| {
            if line.starts_with("use_gpu") {
                if line.contains("true") {
                    "use_gpu = false"
                } else {
                    "use_gpu = true"
                }
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&target, &modified).unwrap();

    // Pull back
    let output = run_blend(home.path(), orders, &["sync", "--pull", "if-then-else"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "Pull failed:\nstdout: {stdout}\nstderr: {stderr}"
    );

    // Read modified order.ncl — the active branch should have flipped
    let ncl_content = std::fs::read_to_string(orders.join("if-then-else/order.ncl")).unwrap();

    match std::env::consts::OS {
        "macos" => {
            // then branch should now be false (was true)
            assert!(
                ncl_content.contains("then false"),
                "then-branch should be flipped to false:\n{ncl_content}"
            );
            // else branch should be untouched
            assert!(
                ncl_content.contains("else false"),
                "else-branch should be untouched:\n{ncl_content}"
            );
        }
        _ => {
            // else branch should now be true (was false)
            assert!(
                ncl_content.contains("else true"),
                "else-branch should be flipped to true:\n{ncl_content}"
            );
            // then branch should be untouched
            assert!(
                ncl_content.contains("then true"),
                "then-branch should be untouched:\n{ncl_content}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Multi-file package tests
// ---------------------------------------------------------------------------

#[test]
fn test_sync_push_multi_file_package() {
    let home = TempDir::new().unwrap();
    let orders = fixtures_dir().join("orders");

    let toml_target = home.path().join(".config/mixed-entries/config.toml");
    let txt_target = home.path().join(".config/mixed-entries/extra.txt");

    let output = run_blend(home.path(), &orders, &["sync", "--push", "mixed-entries"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "sync --push test-multi failed:\nstdout: {stdout}\nstderr: {stderr}"
    );

    assert!(toml_target.exists(), "config.toml should be deployed");
    assert!(txt_target.exists(), "extra.txt should be deployed");

    let toml_content = std::fs::read_to_string(&toml_target).unwrap();
    assert!(toml_content.contains("dark"), "TOML should contain theme");
    assert!(toml_content.contains("14"), "TOML should contain font_size");

    let txt_content = std::fs::read_to_string(&txt_target).unwrap();
    assert!(
        txt_content.contains("extra file content"),
        "Text file should have original content"
    );
}

#[test]
fn test_sync_pull_multi_selective() {
    let home = TempDir::new().unwrap();
    let temp_orders = copy_fixture("mixed-entries");
    let orders = temp_orders.path();

    // Push both files
    let output = run_blend(home.path(), orders, &["sync", "--push", "mixed-entries"]);
    assert!(output.status.success());

    let txt_target = home.path().join(".config/mixed-entries/extra.txt");

    // Save original order.ncl for comparison
    let original_ncl = std::fs::read_to_string(orders.join("mixed-entries/order.ncl")).unwrap();

    // Modify only the text file
    std::fs::write(&txt_target, "modified extra content\n").unwrap();

    // Pull back
    let output = run_blend(home.path(), orders, &["sync", "--pull", "mixed-entries"]);
    assert!(output.status.success());

    // extra.txt source should be updated
    let pulled_txt = std::fs::read_to_string(orders.join("mixed-entries/extra.txt")).unwrap();
    assert_eq!(pulled_txt, "modified extra content\n");

    // order.ncl should be unchanged (only the from_file was modified)
    let current_ncl = std::fs::read_to_string(orders.join("mixed-entries/order.ncl")).unwrap();
    assert_eq!(
        current_ncl, original_ncl,
        "order.ncl should not change when only from_file was modified"
    );
}

// ---------------------------------------------------------------------------
// JSON format tests
// ---------------------------------------------------------------------------

#[test]
fn test_sync_push_json_format() {
    let home = TempDir::new().unwrap();
    let orders = fixtures_dir().join("orders");

    let target = home.path().join(".config/json-format/settings.json");
    let output = run_blend(home.path(), &orders, &["sync", "--push", "json-format"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "sync --push test-json failed:\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(target.exists(), "JSON file should be deployed");

    let content = std::fs::read_to_string(&target).unwrap();
    // Verify it's valid JSON
    let parsed: serde_json::Value =
        serde_json::from_str(&content).expect("Deployed file should be valid JSON");
    assert_eq!(parsed["fontSize"], 14);
    assert_eq!(parsed["tabSize"], 2);
    assert_eq!(parsed["fontFamily"], "JetBrains Mono");
}

#[test]
fn test_sync_pull_json_format() {
    let home = TempDir::new().unwrap();
    let temp_orders = copy_fixture("json-format");
    let orders = temp_orders.path();

    // Push first
    let output = run_blend(home.path(), orders, &["sync", "--push", "json-format"]);
    assert!(output.status.success());

    let target = home.path().join(".config/json-format/settings.json");

    // Modify deployed JSON
    let mut parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&target).unwrap()).unwrap();
    parsed["fontSize"] = serde_json::json!(16);
    std::fs::write(&target, serde_json::to_string_pretty(&parsed).unwrap()).unwrap();

    // Pull back
    let output = run_blend(home.path(), orders, &["sync", "--pull", "json-format"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "Pull failed:\nstdout: {stdout}\nstderr: {stderr}"
    );

    // order.ncl should have the updated value
    let ncl_content = std::fs::read_to_string(orders.join("json-format/order.ncl")).unwrap();
    assert!(
        ncl_content.contains("16"),
        "order.ncl should have updated fontSize to 16:\n{ncl_content}"
    );
}

// ---------------------------------------------------------------------------
// Ignore fields test
// ---------------------------------------------------------------------------

#[test]
fn test_sync_ignore_field_not_in_diff() {
    let home = TempDir::new().unwrap();
    let orders = fixtures_dir().join("orders");

    // Push first
    let output = run_blend(home.path(), &orders, &["sync", "--push", "ignore-keys"]);
    assert!(output.status.success());

    let target = home.path().join(".config/ignore-keys/config.toml");

    // Add an ignored field to the deployed file
    let mut content = std::fs::read_to_string(&target).unwrap();
    content.push_str("timestamp = \"2026-01-01\"\n");
    std::fs::write(&target, &content).unwrap();

    // View should not show timestamp as a diff
    let output = run_blend(home.path(), &orders, &["view", "ignore-keys"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(
        !stdout.contains("timestamp"),
        "Ignored field 'timestamp' should not appear in diff output:\n{stdout}"
    );
}

// ---------------------------------------------------------------------------
// --no-rewrite flag test
// ---------------------------------------------------------------------------

#[test]
fn test_sync_no_rewrite_flag() {
    let home = TempDir::new().unwrap();
    let temp_orders = copy_fixture("os-match");
    let orders = temp_orders.path();

    // Push first
    let output = run_blend(home.path(), orders, &["sync", "--push", "os-match"]);
    assert!(output.status.success());

    let target = home.path().join(".config/os-match/config.toml");

    // Save original order.ncl
    let original_ncl = std::fs::read_to_string(orders.join("os-match/order.ncl")).unwrap();

    // Modify deployed file
    let content = std::fs::read_to_string(&target).unwrap();
    let modified = content
        .lines()
        .map(|line| {
            if line.starts_with("font_size") {
                "font_size = 99"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&target, &modified).unwrap();

    // Pull with --no-rewrite — should NOT modify order.ncl
    let output = run_blend(
        home.path(),
        orders,
        &["sync", "--pull", "--no-rewrite", "os-match"],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "sync --pull --no-rewrite failed:\nstdout: {stdout}\nstderr: {stderr}"
    );

    // order.ncl should be unchanged
    let current_ncl = std::fs::read_to_string(orders.join("os-match/order.ncl")).unwrap();
    assert_eq!(
        current_ncl, original_ncl,
        "order.ncl should not be modified with --no-rewrite"
    );
}

// ---------------------------------------------------------------------------
// Error handling test
// ---------------------------------------------------------------------------

#[test]
fn test_sync_push_error_malformed_ncl() {
    let home = TempDir::new().unwrap();
    let temp = TempDir::new().unwrap();
    let pkg_dir = temp.path().join("test-bad");
    std::fs::create_dir_all(&pkg_dir).unwrap();
    std::fs::write(
        pkg_dir.join("order.ncl"),
        "{ this is not valid nickel syntax !!!",
    )
    .unwrap();

    let output = run_blend(home.path(), temp.path(), &["sync", "--push", "test-bad"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should report an error but not crash
    let has_error = !output.status.success()
        || stdout.contains("error")
        || stdout.contains("Error")
        || stdout.contains("failed")
        || stdout.contains("Failed");
    assert!(
        has_error,
        "Should report error for malformed ncl:\nstdout: {stdout}\nstderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Unexpected symlink detection and replacement tests
// ---------------------------------------------------------------------------

/// Helper: create a symlink at `link` pointing to `target`.
#[cfg(unix)]
fn create_symlink(target: &Path, link: &Path) {
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[test]
#[cfg(unix)]
fn test_sync_push_replaces_symlink_with_real_file() {
    // Simulate a stow-style symlink: target is a symlink to a file with matching content.
    // `blend sync --push` should replace the symlink with a real file.
    let home = TempDir::new().unwrap();
    let orders = fixtures_dir().join("orders");

    // First, create a real file elsewhere with the same content that blend would deploy
    let stow_dir = TempDir::new().unwrap();
    let stow_file = stow_dir.path().join("config.txt");

    // Read the source file content to know what blend would deploy
    let source_content =
        std::fs::read_to_string(fixtures_dir().join("orders/plaintext-single/config.txt")).unwrap();
    std::fs::write(&stow_file, &source_content).unwrap();

    // Create a symlink at the target location pointing to the stow file
    let target = home.path().join(".config/plaintext-single/config.txt");
    create_symlink(&stow_file, &target);

    // Verify it's a symlink with matching content
    assert!(target.symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), source_content);

    // Sync --push should detect the symlink mismatch and replace it
    let output = run_blend(
        home.path(),
        &orders,
        &["sync", "--push", "plaintext-single"],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "sync --push failed:\nstdout: {stdout}\nstderr: {stderr}"
    );

    // The target should now be a real file, not a symlink
    assert!(
        !target.symlink_metadata().unwrap().file_type().is_symlink(),
        "Target should no longer be a symlink"
    );

    // Content should still match
    assert_eq!(std::fs::read_to_string(&target).unwrap(), source_content);

    // Output should mention re-deployment
    assert!(
        stdout.contains("Re-deployed") || stdout.contains("replaced symlink"),
        "Should mention re-deployment in output:\n{stdout}"
    );
}

#[test]
#[cfg(unix)]
fn test_sync_push_replaces_symlinked_directory() {
    // Test that a symlinked directory target also gets replaced with a real directory.
    // We use test-file which has a from_file entry pointing to a single file,
    // but we need to test with a from_config entry too.
    let home = TempDir::new().unwrap();
    let orders = fixtures_dir().join("orders");

    // For from_config (structured) entries: test-plain has from_config
    // First, render to know the expected content
    let output = run_blend(home.path(), &orders, &["sync", "--push", "toml-basic"]);
    assert!(output.status.success());

    let target = home.path().join(".config/toml-basic/config.toml");
    let expected_content = std::fs::read_to_string(&target).unwrap();

    // Now remove the target and replace with a symlink to a file with same content
    let stow_dir = TempDir::new().unwrap();
    let stow_file = stow_dir.path().join("config.toml");
    std::fs::write(&stow_file, &expected_content).unwrap();
    std::fs::remove_file(&target).unwrap();
    create_symlink(&stow_file, &target);

    assert!(target.symlink_metadata().unwrap().file_type().is_symlink());

    // Sync --push should replace the symlink
    let output = run_blend(home.path(), &orders, &["sync", "--push", "toml-basic"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "sync --push failed:\nstdout: {stdout}\nstderr: {stderr}"
    );

    // Should be a real file now
    assert!(
        !target.symlink_metadata().unwrap().file_type().is_symlink(),
        "Target should no longer be a symlink"
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), expected_content);
}

#[test]
#[cfg(unix)]
fn test_view_shows_symlink_annotation() {
    // When a target is a symlink but the order doesn't specify symlink=true,
    // `blend view` should show a "(symlinked, needs re-deploy)" annotation.
    let home = TempDir::new().unwrap();
    let orders = fixtures_dir().join("orders");

    // Deploy normally first
    run_blend(
        home.path(),
        &orders,
        &["sync", "--push", "plaintext-single"],
    );

    let target = home.path().join(".config/plaintext-single/config.txt");
    let content = std::fs::read_to_string(&target).unwrap();

    // Replace with a symlink to a file with same content
    let stow_dir = TempDir::new().unwrap();
    let stow_file = stow_dir.path().join("config.txt");
    std::fs::write(&stow_file, &content).unwrap();
    std::fs::remove_file(&target).unwrap();
    create_symlink(&stow_file, &target);

    // View should show the symlink annotation
    let output = run_blend(home.path(), &orders, &["view", "plaintext-single"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "view failed:\n{}\n{}",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("symlinked, needs re-deploy"),
        "Should show symlink annotation in view output:\n{stdout}"
    );
}

#[test]
#[cfg(unix)]
fn test_sync_interactive_replaces_symlink_automatically() {
    // In interactive mode with no content changes, unexpected symlinks should
    // be auto-redeployed without prompting.
    let home = TempDir::new().unwrap();
    let orders = fixtures_dir().join("orders");

    // Deploy normally
    run_blend(
        home.path(),
        &orders,
        &["sync", "--push", "plaintext-single"],
    );

    let target = home.path().join(".config/plaintext-single/config.txt");
    let content = std::fs::read_to_string(&target).unwrap();

    // Replace with symlink
    let stow_dir = TempDir::new().unwrap();
    let stow_file = stow_dir.path().join("config.txt");
    std::fs::write(&stow_file, &content).unwrap();
    std::fs::remove_file(&target).unwrap();
    create_symlink(&stow_file, &target);

    // Run sync without --push (interactive mode), but since there's no content
    // diff, the symlink replacement should happen automatically without prompting.
    // (No stdin needed because we don't reach the prompt.)
    let output = run_blend(home.path(), &orders, &["sync", "plaintext-single"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "sync failed:\n{}\n{}",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );

    // Should now be a real file
    assert!(
        !target.symlink_metadata().unwrap().file_type().is_symlink(),
        "Target should no longer be a symlink after interactive sync"
    );
}

#[test]
#[cfg(unix)]
fn test_sync_dry_run_detects_symlink_but_does_not_replace() {
    let home = TempDir::new().unwrap();
    let orders = fixtures_dir().join("orders");

    // Deploy normally
    run_blend(
        home.path(),
        &orders,
        &["sync", "--push", "plaintext-single"],
    );

    let target = home.path().join(".config/plaintext-single/config.txt");
    let content = std::fs::read_to_string(&target).unwrap();

    // Replace with symlink
    let stow_dir = TempDir::new().unwrap();
    let stow_file = stow_dir.path().join("config.txt");
    std::fs::write(&stow_file, &content).unwrap();
    std::fs::remove_file(&target).unwrap();
    create_symlink(&stow_file, &target);

    // Dry run should detect but not modify
    let output = run_blend(
        home.path(),
        &orders,
        &["sync", "--push", "-n", "plaintext-single"],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(
        stdout.contains("symlinked") || stdout.contains("re-deploy"),
        "Dry run should mention symlink mismatch:\n{stdout}"
    );

    // Should still be a symlink
    assert!(
        target.symlink_metadata().unwrap().file_type().is_symlink(),
        "Dry run should not modify the symlink"
    );
}

/// Inner-file leftover scenario: the deployed *directory* is a real dir,
/// but one file inside it is still a stow-style symlink. Status, view, and
/// sync must all surface and replace it — these were silent before.
#[test]
#[cfg(unix)]
fn test_status_shows_symlinked_for_inner_file_symlink() {
    let home = TempDir::new().unwrap();
    let orders = fixtures_dir().join("orders");

    // Deploy normally: real dir with real files
    let deploy = run_blend(home.path(), &orders, &["sync", "--push", "plaintext-dir"]);
    assert!(deploy.status.success());

    let inner = home.path().join(".config/plaintext-dir/conf/file1.txt");
    let content = std::fs::read_to_string(&inner).unwrap();

    // Replace just file1.txt with a symlink to identical content
    let stow = TempDir::new().unwrap();
    let stow_file = stow.path().join("file1.txt");
    std::fs::write(&stow_file, &content).unwrap();
    std::fs::remove_file(&inner).unwrap();
    create_symlink(&stow_file, &inner);
    assert!(inner.symlink_metadata().unwrap().file_type().is_symlink());

    let output = run_blend(home.path(), &orders, &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(
        stdout.contains("symlinked"),
        "Status must show 'symlinked' when an inner file in a directory entry is a symlink:\n{stdout}"
    );
}

#[test]
#[cfg(unix)]
fn test_view_annotates_inner_file_symlink() {
    let home = TempDir::new().unwrap();
    let orders = fixtures_dir().join("orders");
    run_blend(home.path(), &orders, &["sync", "--push", "plaintext-dir"]);

    let inner = home.path().join(".config/plaintext-dir/conf/file1.txt");
    let content = std::fs::read_to_string(&inner).unwrap();
    let stow = TempDir::new().unwrap();
    let stow_file = stow.path().join("file1.txt");
    std::fs::write(&stow_file, &content).unwrap();
    std::fs::remove_file(&inner).unwrap();
    create_symlink(&stow_file, &inner);

    let output = run_blend(home.path(), &orders, &["view", "plaintext-dir"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(
        stdout.contains("unexpected symlink"),
        "View must annotate inner-file symlink:\n{stdout}"
    );
    assert!(
        stdout.contains("file1.txt"),
        "View must name the offending file:\n{stdout}"
    );
}

#[test]
#[cfg(unix)]
fn test_sync_interactive_auto_replaces_inner_file_symlink() {
    // Pure-symlink-no-content-diff case for an inner file. Interactive sync
    // should auto-redeploy (no prompt), matching the top-level symlink UX.
    let home = TempDir::new().unwrap();
    let orders = fixtures_dir().join("orders");
    run_blend(home.path(), &orders, &["sync", "--push", "plaintext-dir"]);

    let inner = home.path().join(".config/plaintext-dir/conf/file1.txt");
    let content = std::fs::read_to_string(&inner).unwrap();
    let stow = TempDir::new().unwrap();
    let stow_file = stow.path().join("file1.txt");
    std::fs::write(&stow_file, &content).unwrap();
    std::fs::remove_file(&inner).unwrap();
    create_symlink(&stow_file, &inner);

    // Interactive sync (no --push) — must NOT prompt because there's no
    // content diff, only a structural symlink mismatch.
    let output = run_blend(home.path(), &orders, &["sync", "plaintext-dir"]);
    assert!(output.status.success());
    assert!(
        !inner.symlink_metadata().unwrap().file_type().is_symlink(),
        "Inner-file symlink must be auto-replaced in interactive mode when content matches"
    );
}

#[test]
#[cfg(unix)]
fn test_sync_push_replaces_inner_file_symlink() {
    let home = TempDir::new().unwrap();
    let orders = fixtures_dir().join("orders");
    run_blend(home.path(), &orders, &["sync", "--push", "plaintext-dir"]);

    let inner = home.path().join(".config/plaintext-dir/conf/file1.txt");
    let content = std::fs::read_to_string(&inner).unwrap();
    let stow = TempDir::new().unwrap();
    let stow_file = stow.path().join("file1.txt");
    std::fs::write(&stow_file, &content).unwrap();
    std::fs::remove_file(&inner).unwrap();
    create_symlink(&stow_file, &inner);
    assert!(inner.symlink_metadata().unwrap().file_type().is_symlink());

    let output = run_blend(home.path(), &orders, &["sync", "--push", "plaintext-dir"]);
    assert!(output.status.success());
    assert!(
        !inner.symlink_metadata().unwrap().file_type().is_symlink(),
        "Inner file must be a real file after push"
    );
    // Stow source must remain untouched (push must not write through the symlink)
    assert_eq!(std::fs::read_to_string(&stow_file).unwrap(), content);
}

#[test]
#[cfg(unix)]
fn test_view_annotates_symlink_when_content_also_differs() {
    // Real-world ncdu shape: parent directory of the target is a symlink
    // to a legacy stow tree, AND the resolved file content differs from
    // the source. View must show BOTH the diff and the symlink annotation,
    // not just the diff (otherwise the user can't tell that a redeploy
    // is needed to restructure the path).
    let home = TempDir::new().unwrap();
    let orders = fixtures_dir().join("orders");

    // Build the stow-style tree at a path outside home, then symlink
    // the parent directory in.
    let stow = TempDir::new().unwrap();
    let stow_pkg = stow.path().join("plaintext-single");
    std::fs::create_dir_all(&stow_pkg).unwrap();
    std::fs::write(stow_pkg.join("config.txt"), "old stow content\n").unwrap();

    let parent = home.path().join(".config/plaintext-single");
    std::fs::create_dir_all(parent.parent().unwrap()).unwrap();
    create_symlink(&stow_pkg, &parent);

    // Sanity: target resolves through the symlink to differing content.
    let target = parent.join("config.txt");
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "old stow content\n"
    );
    assert!(
        parent.symlink_metadata().unwrap().file_type().is_symlink(),
        "parent must be a symlink for this test to be meaningful"
    );

    let output = run_blend(home.path(), &orders, &["view", "plaintext-single"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(
        stdout.contains("symlinked, needs re-deploy"),
        "view must annotate symlink even when content also differs:\n{stdout}"
    );
    // And the content diff must still be shown
    assert!(
        stdout.contains("old stow content"),
        "view must still show the content diff:\n{stdout}"
    );
}

#[test]
#[cfg(unix)]
fn test_status_shows_symlink_mismatch() {
    let home = TempDir::new().unwrap();
    let orders = fixtures_dir().join("orders");

    // Deploy normally
    run_blend(
        home.path(),
        &orders,
        &["sync", "--push", "plaintext-single"],
    );

    let target = home.path().join(".config/plaintext-single/config.txt");
    let content = std::fs::read_to_string(&target).unwrap();

    // Replace with symlink
    let stow_dir = TempDir::new().unwrap();
    let stow_file = stow_dir.path().join("config.txt");
    std::fs::write(&stow_file, &content).unwrap();
    std::fs::remove_file(&target).unwrap();
    create_symlink(&stow_file, &target);

    // Status should show "symlinked" status
    let output = run_blend(home.path(), &orders, &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(
        stdout.contains("symlinked"),
        "Status should show 'symlinked' for unexpected symlink target:\n{stdout}"
    );
}
