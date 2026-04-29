# Cortex — justfile
# Common development commands for the Cortex TUI Kanban board application.
#
# Usage: just <recipe>
# Install just: cargo install just

# Default recipe — show available recipes
default:
    @just --list

# Run cargo check
check:
    cargo check

# Run all tests
test:
    cargo test

# Run clippy with warnings as errors
clippy:
    cargo clippy -- -D warnings

# Format code in-place
fmt:
    cargo fmt

# Check formatting without modifying files
fmt-check:
    cargo fmt --check

# Run the application
run:
    cargo run

# Build release binary
build-release:
    cargo build --release

# Audit dependencies for known vulnerabilities (requires: cargo install cargo-audit)
audit:
    cargo audit

# Run clippy with warnings as errors (alias for clippy)
lint: clippy

# Watch for file changes and auto-check + test (requires: cargo install cargo-watch)
watch:
    cargo watch -x check -x test

# Update all dependencies
upgrade:
    cargo update

# Build and open documentation (without dependencies)
docs:
    cargo doc --no-deps --open

# Clean build artifacts
clean:
    cargo clean

# Run full CI pipeline locally (fmt-check + clippy + test)
ci: fmt-check clippy test

# Run tests with verbose output
test-verbose:
    cargo test -- --nocapture

# Run tests for a specific module (usage: just test-module events)
test-module MODULE:
    cargo test --lib -p cortex {{MODULE}}

# Run only unit tests (skip slow integration tests)
test-unit:
    cargo test --lib

# Run benchmarks (usage: just bench)
bench:
    cargo bench
