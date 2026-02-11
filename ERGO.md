# blend-rs User Journey Analysis & Improvement Opportunities

## Context

Investigating the current blend-rs implementation (at `~/Vanilla/blend-rs/`) to map essential user journeys and identify friction points. blend-rs is a Rust-based dotfiles manager using Nickel DSL, managing ~55 packages across macOS and Linux.

**Current CLI commands:** `sync`, `view`, `table`, `upgrade` (alias `s`). The previous `ship` and `sample` commands have been replaced by `sync`.

---

## Journey 1: Onboarding (fresh machine bootstrap)

### Current Flow

```
1. Install Xcode CLT manually (prerequisite for git)
2. Clone Vanilla repo:  git clone <repo> ~/Vanilla
3. Run bootstrap:       ./bootstrap_macos.sh
   3a. Install Homebrew
   3b. brew bundle install (installs packages including Rust toolchain?)
   3c. ./blend install          <-- BROKEN: subcommand doesn't exist
   3d. Install proto (toolchain manager)
   3e. Change shell to elvish
   3f. Set immutable flags on critical configs
```

### Friction Points

| # | Issue | Severity | Detail |
|---|-------|----------|--------|
| 1 | **`blend install` doesn't exist** | Critical | `bootstrap_macos.sh:47` calls `./blend install` but should be `./blend sync --push`. The `sync --push` flag pushes all configs without interactive prompts |
| 2 | **Chicken-and-egg: Rust needed to build blend** | High | Need `cargo build --release` before blend can run, but blend manages toolchain configs (proto). Bootstrap must install Rust independently first |
| 3 | **No pre-built binary** | Medium | No release artifacts, no `cargo install blend-rs`. Every fresh machine requires a full Rust compile (~minutes) |
| 4 | **blend not on PATH initially** | Medium | Binary is at `Vanilla/blend-rs/target/release/blend` (symlinked to `Vanilla/blend`). User's shell PATH isn't configured until blend syncs the shell configs |
| 5 | **Orders dir discovery is fragile** | Low | `find_orders_dir()` in `context.rs` searches relative to exe and CWD. Fails silently with fallback to `../orders` if both miss |
| 6 | **No first-run guidance** | Low | Running `blend` on a clean machine shows status table with all "pending" — no hint about what to do next |

### Improvement Ideas

- Fix bootstrap script: `./blend install` -> `./blend sync --push`
- Consider distributing a pre-built binary (GitHub releases, or a small bootstrap binary that self-compiles)
- Add a `blend init` or first-run message: "X packages pending. Run `blend sync --push` to deploy all."
- Make orders dir resolution more explicit with better error messages

---

## Journey 2: New Config (adding a new app to blend)

### Current Flow

```
1. Create dir:           mkdir orders/my-app
2. Write order.ncl:      (manually, from memory or by copying another package)
3. For plaintext:        cp ~/.config/my-app/config orders/my-app/config
4. For structured:       Manually transcribe TOML/JSON/YAML into Nickel from_config syntax
5. Preview:              blend view my-app
6. Deploy:               blend sync --push my-app
```

### Friction Points

| # | Issue | Severity | Detail |
|---|-------|----------|--------|
| 1 | **No scaffolding command** | High | No `blend add my-app` to create package skeleton with boilerplate order.ncl |
| 2 | **Manual config transcription for structured** | High | User must hand-convert a TOML/JSON file into Nickel `from_config = { ... }` syntax. For a 200-line starship.toml, this is painful and error-prone |
| 3 | **Must know Nickel syntax** | Medium | No inline documentation, no `blend help new-package` with examples |
| 4 | **No validation before deploy** | Low | If order.ncl has syntax errors, you only discover them when running a command (though Nickel does give decent error messages) |
| 5 | **Schema contract usage is implicit** | Low | User should pipe to `| Order` at end of order.ncl for validation, but nothing enforces or suggests this |

### Improvement Ideas

- `blend add <name> [--from <path>]` command that:
  - Creates `orders/<name>/order.ncl` with sensible defaults
  - If `--from ~/.config/app/config.toml` is given: auto-detects format, parses the file, generates `from_config` Nickel syntax using `json_to_nickel()` (already implemented in `ast_utils.rs`)
  - For directories: creates `from_file` entry pointing to copied dir
- `blend check`: validate all order.ncl files without deploying (fast Nickel eval + schema check)

---

## Journey 3: Updated Config (reflecting deployed changes back to orders)

### Current Flow

```
1. App modifies its deployed config (e.g., VS Code updates settings.json)
2. Notice:               blend         → shows ≠ in DIFF column
3. Inspect:              blend view my-app  → shows semantic diff
4. Sync:                 blend sync my-app  → interactive per-file push/pull/skip
   4a. For each changed file: see diff, choose [p]ush / [u]ll / [s]kip
   4b. Pull: blend surgically rewrites order.ncl (even through conditional branches)
5. Verify:               blend view my-app  → should show no changes
```

### Friction Points — Resolved

These friction points from the original analysis have been addressed by `blend sync`:

| # | Original Issue | Status | How Resolved |
|---|---------------|--------|--------------|
| 1 | No assisted reverse sync | **Resolved** | `blend sync` with interactive pull. Context-aware shadow walk handles values inside match/if branches |
| 2 | Diff output in target format, not source format | **Partially resolved** | Semantic diff shows structural changes. Branch context shown for conditional values. Full Nickel-syntax diff output is a future improvement |
| 3 | No interactive accept/reject per change | **Resolved** | Per-file `[p]ush / [u]ll / [s]kip / [q]uit` prompts in interactive sync |
| 4 | Auto-patch for simple data-only orders | **Resolved** | Surgical `.ncl` rewrite via AST byte spans. Works for plain data and conditional branches resolving to literals |

### Remaining Friction Points

| # | Issue | Severity | Detail |
|---|-------|----------|--------|
| 1 | **Per-field granularity** | Medium | Sync operates per-file, not per-field. Can't accept some fields and reject others within a single file |
| 2 | **Discovering which fields to `ignore`** | Low | Fields that apps frequently auto-update (zoom levels, timestamps) cause noisy diffs. Finding which to ignore is trial-and-error |
| 3 | **No watch/auto-detect mode** | Low | Can't monitor deployed configs for changes and notify/prompt. Must manually check `blend` status |
| 4 | **Non-rewritable fields info display** | Low | When `--no-rewrite` is active or a field can't be auto-pulled, the info display (branch context + Nickel snippet) is not yet fully implemented |

### Improvement Ideas

- Per-field interactive sync (show each changed key individually, not just per-file)
- Suggest ignore patterns: when a field keeps changing across consecutive syncs, suggest adding it to `ignore`
- Watch mode: monitor deployed configs, auto-run `blend sync` or notify on changes

---

## Journey 4: Debugging & Recovery

### Current Flow

```
1. blend sync fails or produces wrong config
2. Check error message (Nickel eval error, IO error)
3. Run blend view to see generated output
4. Manually inspect order.ncl
5. No rollback — must manually restore from backup or git
```

### Friction Points

| # | Issue | Severity | Detail |
|---|-------|----------|--------|
| 1 | **No validation-only command** | Medium | No `blend check` or `blend lint` to validate all orders without building/deploying |
| 2 | **No rollback** | Medium | If `blend sync --push` overwrites a config and breaks an app, there's no `blend rollback` or automatic backup |
| 3 | **Nickel errors can be opaque** | Low | Nickel evaluation errors include source info but can be hard to trace for contract violations |
| 4 | **No pre-sync backup** | Low | Sync overwrites in-place. A backup of the previous deployed version would help recovery |

### Improvement Ideas

- `blend check`: validate all orders without building (fast Nickel eval + schema check)
- Auto-backup before sync push: copy previous deployed file to `~/.cache/blend/backups/<pkg>/<file>.bak`
- `blend rollback <package>`: restore from backup

---

## Summary: Priority Improvements

### Quick Wins (low effort, high impact)
1. **Fix bootstrap script**: `./blend install` -> `./blend sync --push`
2. **First-run message**: When all packages are pending, show "Run `blend sync --push` to deploy"
3. **`blend check` command**: Validate all order.ncl files without deploying

### Medium Effort
4. **`blend add <name> [--from <path>]`**: Scaffold new packages with auto-import (can reuse existing `json_to_nickel()` for format conversion)
5. **`--no-rewrite` info display**: Show branch context and Nickel snippets for manual merge
6. **Suggest ignore patterns**: Auto-detect frequently changing fields

### Larger Effort
7. **Per-field interactive sync**: Accept/reject individual field changes within a file
8. **Pre-sync backups + rollback**: Safety net for force deployments
9. **Pre-built binary distribution**: GitHub releases or cargo-binstall support

---

## Implementation Status

Features that were in "Improvement Ideas" and are now implemented:

- **`blend sync`** — bidirectional sync with interactive push/pull/skip (Journey 3, items 1/3/4)
- **`blend sync --push`** — non-interactive push all (replaces `blend ship --force`)
- **`blend sync --pull`** — non-interactive pull all
- **Surgical .ncl rewrite** — auto-patches Nickel source for data-only and conditional values
- **Context-aware shadow walk** — follows active match/if branches using runtime metadata
- **`--no-rewrite` flag** — disables auto-pull for review-only mode
- **`--dry-run` flag** — preview sync actions without changes
- **Semantic diffing** — format-aware structured comparison for TOML/JSON/YAML

---

## Files Referenced

- `~/Vanilla/bootstrap_macos.sh` — bootstrap script (line 47: broken `install` subcommand)
- `~/Vanilla/blend-rs/src/cli.rs` — CLI definition (Sync, View, Table, Upgrade commands)
- `~/Vanilla/blend-rs/src/main.rs` — command handlers (cmd_sync, cmd_view, cmd_status)
- `~/Vanilla/blend-rs/src/compose.rs` — package discovery and build pipeline
- `~/Vanilla/blend-rs/src/sync.rs` — bidirectional sync: pull_from_file, pull_from_config, Prompter trait
- `~/Vanilla/blend-rs/src/nickel/ast_utils.rs` — shadow walk, surgical rewrite, json_to_nickel
- `~/Vanilla/blend-rs/src/context.rs` — orders dir discovery logic
- `~/Vanilla/blend-rs/src/nickel/schema.rs` — order.ncl schema types (OrderPackage, FileEntry)
- `~/Vanilla/NEW_BLEND.md` — architecture and design document
