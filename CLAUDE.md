# CLAUDE.md

## Project Overview

Vanilla is a cross-platform dotfiles manager. The core tool is **blend** (Rust + Nickel DSL), built from `blend/` and symlinked into `bin/blend`.

## Repository Layout

- `blend/` — Rust source for the blend CLI
- `orders/` — Nickel-based package definitions (`.ncl` files), the active config format
- `packages/` — Legacy static config files (pre-migration)
- `screenshots/` — README screenshots
- `Brewfile*` — Homebrew dependency manifests
- `bootstrap_macos.sh`, `bootstrap_archlinux.sh` — Platform bootstrap scripts
- `NEW_BLEND.md` — Design document for the blend rewrite

## Migration Status

The legacy nushell-based blend + `packages/` folder is being migrated to Rust-based `orders/` using Nickel DSL. Active development branch is `dev/brand-new-blend`.

macOS orders are fully migrated and dogfooded. Linux-only orders remain (to be migrated on a Linux machine). A few legacy orphan packages (`darwin-system`, `root`) are pending decision.

## blend Development

blend is being enhanced iteratively via manual testing. No CI yet.

**Build:**
```sh
cargo build --release    # inside blend/
```

**CLI commands:**
- `sync [packages...]` — Bidirectional sync with per-key interactive push/pull for `from_config` entries (`--push`, `--pull`, `--no-rewrite`)
- `view [packages...]` — Preview generated config and diff from deployed (`-c` content only, `-a` all, `-s` short — omit up-to-date entries)
- `table` — Output package info as HTML table (for README generation)
- `upgrade [step]` — System upgrade: update packages, tools, and dotfiles (alias: `s`)

**Global flags:** `--dry-run` (`-n`), `--verbose` (`-v`), `--home`, `--orders`

## Tech Stack

- Rust 1.92.0 (edition 2024, pinned in `blend/rust-toolchain.toml`)
- Nickel v2 for config DSL (`nickel-lang = "2"`)
- clap v4 (derive) for CLI
- Key crates: walkdir, globset, console, serde/serde_json, similar, rayon, anyhow, tree-sitter/tree-sitter-nickel (CST for surgical rewrite), json-strip-comments (JSONC support)
- In `.ncl` files, use `\u{xxxx}` escape sequences for non-ASCII characters (e.g. Nerd Font icons) instead of raw unicode codepoints, for readability
