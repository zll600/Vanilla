# blend: Design & Architecture

## 1. Overview

blend is a cross-platform dotfiles manager that uses Nickel DSL to define, build, and deploy configuration files. It replaces the legacy nushell-based blend + GNU Stow symlinks with an explicit build-and-deploy model.

**Key properties:**
- Configs defined in Nickel (`.ncl` files) under `orders/`
- Platform conditionals via Nickel's native `match`/`if` expressions with runtime metadata injection
- Format-aware rendering: Nickel data evaluates to JSON, then renders to TOML/JSON/YAML/delimited formats
- Bidirectional sync: push configs to targets or pull deployed changes back into `.ncl` source
- Semantic diffing: structured formats are compared by parsed values, not text

**Migration context:** The `packages/` directory contains the legacy static config files managed by stow. New configs go into `orders/` as `.ncl` files. Active development branch is `dev/brand-new-blend`. macOS orders are fully migrated.

---

## 2. Architecture

```
orders/<pkg>/order.ncl     Nickel source (data + optional logic)
        │
        ▼
  NickelEvaluator          Injects metadata, evaluates to JSON
        │
        ▼
  FormatRenderer           Renders JSON → TOML/JSON/delimited/plaintext
        │
        ▼
  ~/.config/<app>/file     Deployed config file
```

Two config modes per file entry:

| Mode | Source | Rendering | Sync-back |
|------|--------|-----------|-----------|
| `from_config` | Inline Nickel data/expressions | Evaluated → rendered to target format | Context-aware AST rewrite |
| `from_file` | Files/dirs in `orders/<pkg>/` | Copied as-is | File copy back |

---

## 3. Order Schema

Each package is defined by `orders/<pkg>/order.ncl`. The evaluated result must conform to the `OrderPackage` structure:

```nickel
{
  blend = {
    files = [ ... ],           # array of FileEntry
    prefix = ["~/.config/"],   # default target prefix for all files
    when = { os = [...] },     # package-level condition (optional)
    ignore = [...],            # global diff-ignore keys (optional)
  },
}
```

### FileEntry fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | String | Yes (for `from_config`) | Destination filename. Combined with prefix for target path. Auto-set from `from_file` if omitted. |
| `from_config` | Record/Array | One of these | Inline structured config data, evaluated by Nickel and rendered to target format |
| `from_file` | String | required | Path to file/directory in the package dir, copied as-is |
| `prefix` | Array<String> | No | Per-file prefix override (default: inherits global `blend.prefix`) |
| `format` | String | No | Output format override (default: inferred from `name` extension) |
| `ignore` | Array<String> | No | Keys/patterns to exclude from diff (merged with global) |
| `when` | Record | No | Per-file condition: `{ os, arch, hostname }` |
| `symlink` | Bool | No | Create symlink instead of copying (`from_file` only) |
| `exclude` | Array<String> | No | Glob patterns to skip in `from_file` directories |
| `local` | String | No | Local overlay directory for machine-specific overrides (auto-created, gitignored) |
| `immutable` | Bool | No | Set OS immutable flag after deploying (macOS `chflags uchg`, Linux `chattr +i`) |

### Example: structured config (from_config)

```nickel
# orders/starship/order.ncl
let { Order, .. } = import "../order.contract.ncl" in
{
  blend = {
    prefix = ["~/.config/"],
    files = [
      {
        name = "starship.toml",
        from_config = {
          "$schema" = "https://starship.rs/config-schema.json",
          command_timeout = 10000,
          git_branch = { style = "bold bright-green" },
          # Nerd Font icons use \u{xxxx} escapes
          rust = { symbol = "\u{e7a8} " },
        },
      },
    ],
    when = { os = ["darwin", "linux"] },
  },
} | Order
```

Generates `~/.config/starship.toml`:
```toml
"$schema" = "https://starship.rs/config-schema.json"
command_timeout = 10000

[git_branch]
style = "bold bright-green"

[rust]
symbol = " "
```

### Example: plaintext config (from_file)

```nickel
# orders/neovim/order.ncl
let { Order, .. } = import "../order.contract.ncl" in
{
  blend = {
    prefix = ["~/.config/"],
    files = [
      { from_file = "nvim" },  # copies orders/neovim/nvim/ directory
    ],
    when = { os = ["darwin", "linux"] },
  },
} | Order
```

### Example: delimited format (kitty)

```nickel
# orders/kitty/order.ncl — space-delimited pairs format
{
  blend = {
    prefix = ["~/.config/kitty/"],
    files = [
      {
        name = "kitty.conf",
        format = "space_delimited_pairs",
        from_config = [
          ["font_size", "16.0"],
          ["background_opacity", "0.6"],
          ["map", "cmd+v paste_from_clipboard"],
        ],
      },
    ],
  },
}
```

### Example: per-file prefix override and ignore

```nickel
# orders/git/order.ncl — multiple files with different prefixes
{
  blend = {
    prefix = ["~/.config/git/"],
    files = [
      {
        from_file = "git",
        prefix = ["~/.config/"],   # override: deploy to ~/.config/git/ not ~/.config/git/git/
      },
      {
        from_file = "gitk",
        ignore = ["^set geometry"],  # ignore geometry changes in diff
      },
    ],
  },
}
```

### Conditional values

Use Nickel's native expressions with injected runtime metadata:

```nickel
let metadata = import "blend://metadata" in
{
  blend = {
    files = [
      {
        name = "config.toml",
        from_config = {
          # Platform conditional via match
          shell = metadata.os |> match {
            "darwin" => "/bin/zsh",
            "linux" => "/bin/bash",
            _ => "/bin/sh",
          },
          # Architecture conditional
          homebrew_prefix = metadata.arch |> match {
            "aarch64" => "/opt/homebrew",
            _ => "/usr/local",
          },
          # Boolean conditional
          font_size = if metadata.os == "darwin" then 14 else 12,
        },
      },
    ],
  },
}
```

### Format auto-detection

Format is inferred from `name` extension when `format` is not set:

| Extension | Format |
|-----------|--------|
| `.toml` | Toml |
| `.jsonc` | Jsonc |
| `.json` | Json (auto-falls back to JSONC parsing if strict JSON fails) |
| `.yaml`, `.yml` | Yaml |
| anything else | Plaintext |

Explicit `format` values: `"toml"`, `"json"`, `"jsonc"`, `"yaml"`, `"space_delimited_pairs"`, `"space_delimited_record"`, `"equal_delimited_record"`, `"plaintext"`.

---

## 4. Sync Strategy

### The Problem

When dotfiles are managed via DSL (not symlinks), deployed files can diverge from repo state. The old nushell blend used GNU Stow symlinks — editing `~/.gitconfig` edited the repo file directly. The new blend explicitly builds and writes files, so deployed configs can be edited independently and changes can be lost.

### Research: How Other Tools Handle This

No mainstream dotfiles manager achieves true bidirectional sync with templates:

| Tool | Approach | Sync-back? |
|------|----------|-----------|
| GNU Stow | Symlinks | Trivial (same file) |
| chezmoi | Templates to copy | `re-add` destroys templates; `merge` opens 3-way diff |
| home-manager | Nix to read-only | Impossible by design |
| yadm | Git + templates | No. Embeds "DO NOT EDIT" warnings |
| dotter | Handlebars | No reverse sync |
| DFS (Zig) | Custom reversible syntax | Yes, but requires syntax designed for reversibility |

**Key insight**: Reverse sync can recover *data* changes but cannot infer *logic* changes. The data/logic separation is the fundamental tension.

### Our Approach: Context-Aware Shadow Walk

`blend sync` is a unified bidirectional sync command.

**For `from_file` entries** (plaintext): bidirectional — copy files in either direction.

**For `from_config` entries**: context-aware shadow walk using runtime metadata:
1. Parse `.ncl` file using `nickel-lang-parser` (parse only, no evaluation) to get AST with byte spans
2. Walk the AST using the known runtime context (os, arch, hostname, etc.)
3. When the walk encounters a conditional (`match` or `if/then/else`), evaluate the condition against metadata and follow only the active branch
4. If the walk reaches a literal leaf: auto-pull is available. Pull surgically replaces just that leaf's bytes using the `TermPos` span from the AST — even if the value is nested inside a conditional branch
5. If the walk reaches a non-literal expression (e.g., `base_size + 2`): fall back to showing diff for manual merge

**TermPos insight**: Nickel's parser preserves exact source byte spans (`TermPos`) on every AST node, including values inside match arms (e.g., the `14` in `"darwin" => 14`). The shadow walk doesn't need separate span-tracking — it returns the leaf node's existing `.pos` field. Whether a value is at the top level or nested inside conditionals, the byte span points to exactly the right bytes.

**Supported patterns** (auto-pullable):
- `metadata.FIELD |> match { "VALUE" => LITERAL, _ => LITERAL }` — platform-specific values
- `if metadata.FIELD == "VALUE" then LITERAL else LITERAL` — boolean conditionals
- Plain data (no conditionals) — the trivial case, handled as a superset

**Graceful degradation**: Fields are analyzed individually. A `from_config` block can have some auto-pullable fields and some manual-merge fields (`Partial` result).

### Surgical .ncl Rewrite (Lens Put)

The sync-back system follows a Lens framework: S × V' → S' where S is the `.ncl` source, V' is the modified deployed config, and S' is the new `.ncl`. The "complement" (information needed for write-back that isn't in the deployed config) comes from two sources:

1. **Shadow walk (nickel-lang-parser AST)** — `LeafSpan` byte offsets for existing value modification
2. **StructureMap (tree-sitter-nickel CST)** — record boundaries, field ranges, comma positions for field insertion/deletion

When pulling deployed changes back:
1. Shadow walk finds each field's leaf value byte span (`TermPos`)
2. StructureMap provides record `}` positions and field full ranges
3. For values inside conditional branches, the walk follows the active branch
4. Compute structural diff between current JSON (from Nickel eval) and deployed JSON
5. Three edit types via `surgical_rewrite_with_structure()`:
   - **Modify**: replace value bytes at `LeafSpan` offset (existing behavior)
   - **Insert**: add new field before record's `}` with matching indentation
   - **Delete**: remove field's full line including whitespace
6. Edits applied back-to-front (descending byte offset) to preserve positions
7. Falls back to modify-only if StructureMap build fails

```nickel
# Before pull (user changed font_size to 16 on macOS):
font_size = metadata.os |> match {
  "darwin" => 14,    # ← this "14" gets replaced with "16"
  _ => 12,           # ← untouched
},

# After pull:
font_size = metadata.os |> match {
  "darwin" => 16,
  _ => 12,
},
```

### Diff Strategies

| Format | Diff strategy | How it works |
|--------|--------------|--------------|
| TOML, JSON, YAML | Semantic diff | Parse both sides to JSON values, compare by key/value |
| SpaceDelimitedRecord, EqualDelimitedRecord | Semantic diff | Parse to key-value map, compare |
| SpaceDelimitedPairs, Plaintext | Text diff | Line-by-line unified diff |

Semantic diff ignores formatting differences and respects `ignore` keys. Text diff supports regex-based ignore patterns.

### Future: Three-Way Merge

Store last-synced snapshots in `orders/.blend-state/snapshots/<pkg>/<file>` to enable automatic conflict detection:
- `base == deployed` → repo changed only → auto push
- `base == generated` → deployed changed only → auto pull
- both differ → true conflict, prompt user

---

## 5. CLI Reference

```
blend                              Status: show all packages and sync state
blend sync [packages...]           Interactive bidirectional sync (default)
blend sync --push [packages...]    Push all (repo wins)
blend sync --pull [packages...]    Pull all (deployed wins)
blend sync --no-rewrite            Disable .ncl rewrite; show diff for manual merge
blend view [packages...]           Preview diffs (read-only)
blend view -c [packages...]        Show generated content only (no diff)
blend view -a [packages...]        Show both content and diff
blend view -s [packages...]        Short mode: omit up-to-date entries
blend table                        Output package info as HTML table (for README)
blend upgrade                      System upgrade: package managers + sync
blend s                            Alias for upgrade
blend s homebrew                   Run only Homebrew upgrade step
blend s pacman                     Run only Pacman upgrade step
blend s proto                      Run only Proto upgrade step
```

**Global flags:** `--dry-run` (`-n`), `--verbose` (`-v`), `--home <PATH>`, `--orders <PATH>`

---

## 6. Implemented Formats

| Format | Renderer | Usage | Render | Parse |
|--------|----------|-------|--------|-------|
| `Toml` | `TomlRenderer` | starship, aerospace, alacritty | JSON → TOML via `toml` crate | TOML → JSON |
| `Json` | `JsonRenderer` | vscode settings | JSON → pretty JSON | JSON → JSON (auto-falls back to JSONC) |
| `Jsonc` | `JsoncRenderer` | JSONC files | JSON → pretty JSON | Strips comments + trailing commas → JSON |
| `Yaml` | `JsonRenderer` | pueue | Same as JSON (YAML-compatible) | Same as JSON |
| `SpaceDelimitedPairs` | `SpaceDelimitedPairsRenderer` | kitty | Array of `[key, val]` → `key val\n` lines | Lines → pairs |
| `SpaceDelimitedRecord` | `SpaceDelimitedRecordRenderer` | ncdu | Object → `key val\n` lines | Lines → object |
| `EqualDelimitedRecord` | `EqualDelimitedRecordRenderer` | npm | Object → `key=val\n` lines | Lines → object |
| `Plaintext` | `PlaintextRenderer` | shell, lua, CSS | String passthrough | String passthrough |

All renderers implement `FormatRenderer` trait with `render(&serde_json::Value) -> Result<String>` and `parse(&str) -> Result<serde_json::Value>`.

---

## 7. Runtime Metadata

Detected at startup and injected into Nickel via `import "blend://metadata"`:

| Field | Source | Example |
|-------|--------|---------|
| `metadata.os` | Compile-time target OS | `"darwin"`, `"linux"` |
| `metadata.arch` | `std::env::consts::ARCH` | `"aarch64"`, `"x86_64"` |
| `metadata.hostname` | `hostname` crate | `"chimame-tai"` |
| `metadata.desktop` | `$XDG_CURRENT_DESKTOP` or `$DESKTOP_SESSION` | `"sway"`, `null` |
| `metadata.home` | `$HOME` or `--home` flag | `"/Users/kafuuchino"` |
| `metadata.user` | `$USER` or `$USERNAME` | `"kafuuchino"` |

---

## 8. Comparison with Other Tools

### Tool Snapshot

| Tool | Source style | Deploy model | Sync-back story | Main trade-off |
|------|--------------|--------------|-----------------|----------------|
| GNU Stow | Plain files/dirs | Symlink farm | Trivial because repo and deployed are the same files | Extremely transparent, but repo path/layout leaks into deployed state |
| yadm | Git repo in `$HOME`, alternate files, optional templates | Files in home, with alternates/templates materialized per machine | Not a first-class feature; mainly edit tracked files directly | Simple Git workflow, but conditionals and generated files become file-variant/template problems |
| chezmoi | Source state with templates, data, scripts, attributes | Copies/rendered files applied to target state | `add`, `re-add`, and 3-way `merge`, but templates are still not naturally reversible | Mature workflow, but templated outputs remain a manual-merge world |
| home-manager | Nix expressions/modules | Declarative generations + activation | Not designed for reverse sync; manual edits are outside the happy path | Very powerful for full user environments, but heavy and not GUI-edit friendly |
| DFS | Custom reversible template syntax | Sync engine with persisted records/meta | Explicit 2-way sync is the headline feature | Strong reverse-sync ambition, but requires buying into a custom template language |
| **blend** | Nickel config DSL plus `from_file` escape hatch | Rendered/copy targets, optional symlink entries | Auto-pulls `from_file`; selectively rewrites `from_config` values through active conditional branches | More structured and ergonomic than text templates, but currently only partially reversible |

### Capability Comparison

| Aspect | GNU Stow | yadm | chezmoi | DFS | **blend** |
|--------|----------|------|---------|-----|-----------|
| **Repo and deployed separated** | No | Partially | Yes | Yes | **Yes** |
| **Template syntax embedded in target files** | No | Sometimes | Yes | Yes | **No** |
| **Structured config as source** | No | No | Partial | Partial | **Yes** |
| **Native conditionals** | No | File variants | Template logic | Template logic | **Nickel `match` / `if`** |
| **Format-aware rendering** | No | No | Mostly text templates | Template-driven | **TOML / JSON / JSONC / YAML / delimited** |
| **Semantic diff** | No | No | Limited | Limited | **Yes** |
| **Reverse sync for generated configs** | N/A via symlinks | Weak | Partial/manual | Strongest among peers | **Partial, context-aware** |
| **Good fit for GUI-mutated configs** | Only while symlinks stay healthy | Mixed | Mixed | Better | **Good, but key add/delete remains manual** |
| **Best fit** | Static files, minimal abstraction | Git-centric dotfiles | Mature one-way apply workflow | Template-first 2-way sync | **Config-DSL-first dotfiles with selective reversibility** |

**blend's unique value:**
1. **Avoids template markers in target-file syntax** — logic lives in Nickel source rather than being embedded into TOML/JSON/INI-like files
2. **Context-aware sync-back** — auto-pulls deployed value changes back into `.ncl` source, even through active conditional branches
3. **Format-aware semantic diff** — compares structured configs by parsed values, not just text
4. **Hybrid source model** — structured (`from_config`) and literal (`from_file`) configs handled by one tool with different sync strategies
5. **Expandable format story** — even files that currently fall back to plaintext can gain semantic diff/sync-back later by adding parsers instead of inventing more template syntax

---

## 9. Design Decisions

### Why Nickel

Evaluated Nickel, KCL, Pkl, CUE, Dhall, and Jsonnet. Chose Nickel because:
- Written in Rust — native embedding via `nickel-lang` crate (no subprocess, no FFI)
- Contracts (gradual typing) for config validation
- JSON-superset syntax — familiar and readable
- LSP with auto-complete and type hints
- Stable since 1.0 (May 2023), actively maintained by Tweag

Trade-off: smaller ecosystem than Jsonnet/CUE, but sufficient for dotfiles config.

### Why explicit files over symlinks

GNU Stow symlinks make bidirectional sync trivial but can't support:
- Conditional values (platform-specific settings in the same file)
- Format rendering (Nickel data → TOML/JSON)
- Semantic diffing (structured comparison)

The explicit build model enables all three, at the cost of needing the shadow walk for sync-back.

### Diff ignore strategy

Single `ignore` field, interpreted based on format:
- Structured formats (TOML/JSON/YAML): key paths filtered recursively from JSON values before comparison
- Text formats (Plaintext, SpaceDelimitedPairs): regex patterns filtering lines

### Non-ASCII handling

In `.ncl` files, use `\u{xxxx}` escape sequences for non-ASCII characters (e.g., Nerd Font icons) instead of raw unicode codepoints, for readability and consistent rendering across editors.

### Resolved design questions

- **Three-way merge**: Deferred. Two-way diff with user decision for now. Snapshot store planned.
- **Secrets management**: Deferred to v2. Focus on core config management first.
- **JSONC round-trip**: Output JSON without comments. Comments live in Nickel source.
- **Schema validation**: Via Nickel contracts (`| Order`). Not yet enforced at runtime.

---

## 10. Future Work

- **Three-way merge snapshots** — store last-synced state for automatic conflict detection
- **Secrets management** — integration with system keychains or sops
- **Schema validation** — enforce Nickel contracts at build time, json-schema import
- **INI format renderer** — for git config and similar `[section]` formats
- **`--no-rewrite` info display** — show branch context and Nickel snippets when auto-pull is disabled
- **Watch mode** — auto-sync on source file changes
