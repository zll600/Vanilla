default:
    @just --list

# Build all executables (release by default)
build: build-blend

# Build blend and symlink into bin/
build-blend:
    cd blend && cargo build --release
    ln -sf ../blend/target/release/blend bin/blend

# Build blend in debug mode (for development)
build-debug:
    cd blend && cargo build
    ln -sf ../blend/target/debug/blend bin/blend-debug

# Validate all orders
check:
    bin/blend view --dry-run

# Run rustfmt on the blend crate
fmt:
    cd blend && cargo fmt

# Check formatting without modifying files (CI-equivalent)
fmt-check:
    cd blend && cargo fmt --check

# Run clippy on the blend crate (CI-equivalent)
clippy:
    cd blend && cargo clippy -- -D warnings

# Run the blend test suite
test:
    cd blend && cargo test --release

# Deploy all configs
deploy:
    bin/blend sync --push

# Interactive sync
sync *ARGS:
    bin/blend sync {{ARGS}}

# System upgrade
upgrade *STEP:
    bin/blend upgrade {{STEP}}

# Full bootstrap (called by bootstrap.sh after deps are installed)
bootstrap:
    just build
    just deploy
    @echo "Bootstrap complete. Restart your shell."
