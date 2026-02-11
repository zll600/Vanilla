default:
    @just --list

# Cargo build profile: "debug" (default) or "release"
profile := "debug"
cargo_flags := if profile == "release" { "--release" } else { "" }

# Build all executables
build: build-blend

# Build blend-rs and symlink into bin/
build-blend:
    cd blend-rs && cargo build {{cargo_flags}}
    ln -sf ../blend-rs/target/{{profile}}/blend bin/blend
    ln -sf blend-rs/target/{{profile}}/blend blend

# Validate all orders
check:
    ./blend view --dry-run

# Deploy all configs
deploy:
    ./blend sync --push

# Interactive sync
sync *ARGS:
    ./blend sync {{ARGS}}

# System upgrade
upgrade *STEP:
    ./blend upgrade {{STEP}}

# Full bootstrap (called by bootstrap.sh after deps are installed)
bootstrap:
    just build profile=release
    just deploy
    @echo "Bootstrap complete. Restart your shell."
