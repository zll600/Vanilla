default:
    @just --list

# Build all executables (release by default)
build: build-blend

# Build blend-rs and symlink into bin/
build-blend:
    cd blend-rs && cargo build --release
    ln -sf ../blend-rs/target/release/blend bin/blend
    ln -sf blend-rs/target/release/blend blend

# Build blend-rs in debug mode (for development)
build-debug:
    cd blend-rs && cargo build
    ln -sf ../blend-rs/target/debug/blend bin/blend-debug
    ln -sf blend-rs/target/debug/blend blend-debug

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
    just build
    just deploy
    @echo "Bootstrap complete. Restart your shell."
