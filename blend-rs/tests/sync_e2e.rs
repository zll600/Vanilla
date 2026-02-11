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
    let target = home.path().join(".config/test-plain/config.toml");
    assert!(!target.exists());

    let output = run_blend(home.path(), &orders, &["sync", "--push", "test-plain"]);
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
    run_blend(home.path(), &orders, &["sync", "--push", "test-plain"]);

    // Second sync should show no changes (nothing to do)
    let output = run_blend(
        home.path(),
        &orders,
        &["sync", "--push", "-v", "test-plain"],
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
    let target = home.path().join(".config/test-plain/config.toml");
    let output = run_blend(home.path(), &orders, &["sync", "-n", "test-plain"]);

    assert!(output.status.success());
    assert!(!target.exists(), "Dry run should not create files");
}

#[test]
fn test_sync_push_from_file() {
    let home = TempDir::new().unwrap();
    let orders = fixtures_dir().join("orders");

    let target = home.path().join(".config/test-file/config.txt");
    assert!(!target.exists());

    let output = run_blend(home.path(), &orders, &["sync", "--push", "test-file"]);
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
    let pkg_dir = orders.join("test-file");
    std::fs::create_dir_all(&pkg_dir).unwrap();
    std::fs::copy(
        fixtures_dir().join("orders/test-file/order.ncl"),
        pkg_dir.join("order.ncl"),
    )
    .unwrap();
    std::fs::copy(
        fixtures_dir().join("orders/test-file/config.txt"),
        pkg_dir.join("config.txt"),
    )
    .unwrap();

    // First push to deploy
    run_blend(home.path(), orders, &["sync", "--push", "test-file"]);

    let target = home.path().join(".config/test-file/config.txt");
    assert!(target.exists());

    // Modify the deployed file
    std::fs::write(&target, "modified by user\nnew line\n").unwrap();

    // Pull back
    let output = run_blend(home.path(), orders, &["sync", "--pull", "test-file"]);
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
    run_blend(home.path(), &orders, &["sync", "--push", "test-plain"]);

    // View should show no changes
    let output = run_blend(home.path(), &orders, &["view", "test-plain"]);
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
        stdout.contains("test-plain"),
        "Should list test-plain package"
    );
    assert!(
        stdout.contains("test-file"),
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

    let target = home.path().join(".config/test-match/config.toml");
    let output = run_blend(home.path(), &orders, &["sync", "--push", "test-match"]);
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
    let temp_orders = copy_fixture("test-match");
    let orders = temp_orders.path();

    // Push first
    let output = run_blend(home.path(), orders, &["sync", "--push", "test-match"]);
    assert!(output.status.success(), "Initial push failed");

    let target = home.path().join(".config/test-match/config.toml");
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
    let output = run_blend(home.path(), orders, &["sync", "--pull", "test-match"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "Pull failed:\nstdout: {stdout}\nstderr: {stderr}"
    );

    // Read the modified order.ncl
    let ncl_content = std::fs::read_to_string(orders.join("test-match/order.ncl")).unwrap();

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
    let output = run_blend(home.path(), orders, &["sync", "--push", "-v", "test-match"]);
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

    let target = home.path().join(".config/test-if/config.toml");
    let output = run_blend(home.path(), &orders, &["sync", "--push", "test-if"]);
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
    let temp_orders = copy_fixture("test-if");
    let orders = temp_orders.path();

    // Push first
    let output = run_blend(home.path(), orders, &["sync", "--push", "test-if"]);
    assert!(output.status.success(), "Initial push failed");

    let target = home.path().join(".config/test-if/config.toml");

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
    let output = run_blend(home.path(), orders, &["sync", "--pull", "test-if"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "Pull failed:\nstdout: {stdout}\nstderr: {stderr}"
    );

    // Read modified order.ncl — the active branch should have flipped
    let ncl_content = std::fs::read_to_string(orders.join("test-if/order.ncl")).unwrap();

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

    let toml_target = home.path().join(".config/test-multi/config.toml");
    let txt_target = home.path().join(".config/test-multi/extra.txt");

    let output = run_blend(home.path(), &orders, &["sync", "--push", "test-multi"]);
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
    let temp_orders = copy_fixture("test-multi");
    let orders = temp_orders.path();

    // Push both files
    let output = run_blend(home.path(), orders, &["sync", "--push", "test-multi"]);
    assert!(output.status.success());

    let txt_target = home.path().join(".config/test-multi/extra.txt");

    // Save original order.ncl for comparison
    let original_ncl = std::fs::read_to_string(orders.join("test-multi/order.ncl")).unwrap();

    // Modify only the text file
    std::fs::write(&txt_target, "modified extra content\n").unwrap();

    // Pull back
    let output = run_blend(home.path(), orders, &["sync", "--pull", "test-multi"]);
    assert!(output.status.success());

    // extra.txt source should be updated
    let pulled_txt = std::fs::read_to_string(orders.join("test-multi/extra.txt")).unwrap();
    assert_eq!(pulled_txt, "modified extra content\n");

    // order.ncl should be unchanged (only the from_file was modified)
    let current_ncl = std::fs::read_to_string(orders.join("test-multi/order.ncl")).unwrap();
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

    let target = home.path().join(".config/test-json/settings.json");
    let output = run_blend(home.path(), &orders, &["sync", "--push", "test-json"]);
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
    let temp_orders = copy_fixture("test-json");
    let orders = temp_orders.path();

    // Push first
    let output = run_blend(home.path(), orders, &["sync", "--push", "test-json"]);
    assert!(output.status.success());

    let target = home.path().join(".config/test-json/settings.json");

    // Modify deployed JSON
    let mut parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&target).unwrap()).unwrap();
    parsed["fontSize"] = serde_json::json!(16);
    std::fs::write(&target, serde_json::to_string_pretty(&parsed).unwrap()).unwrap();

    // Pull back
    let output = run_blend(home.path(), orders, &["sync", "--pull", "test-json"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "Pull failed:\nstdout: {stdout}\nstderr: {stderr}"
    );

    // order.ncl should have the updated value
    let ncl_content = std::fs::read_to_string(orders.join("test-json/order.ncl")).unwrap();
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
    let output = run_blend(home.path(), &orders, &["sync", "--push", "test-ignore"]);
    assert!(output.status.success());

    let target = home.path().join(".config/test-ignore/config.toml");

    // Add an ignored field to the deployed file
    let mut content = std::fs::read_to_string(&target).unwrap();
    content.push_str("timestamp = \"2026-01-01\"\n");
    std::fs::write(&target, &content).unwrap();

    // View should not show timestamp as a diff
    let output = run_blend(home.path(), &orders, &["view", "test-ignore"]);
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
    let temp_orders = copy_fixture("test-match");
    let orders = temp_orders.path();

    // Push first
    let output = run_blend(home.path(), orders, &["sync", "--push", "test-match"]);
    assert!(output.status.success());

    let target = home.path().join(".config/test-match/config.toml");

    // Save original order.ncl
    let original_ncl = std::fs::read_to_string(orders.join("test-match/order.ncl")).unwrap();

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
        &["sync", "--pull", "--no-rewrite", "test-match"],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "sync --pull --no-rewrite failed:\nstdout: {stdout}\nstderr: {stderr}"
    );

    // order.ncl should be unchanged
    let current_ncl = std::fs::read_to_string(orders.join("test-match/order.ncl")).unwrap();
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
