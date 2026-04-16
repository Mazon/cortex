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

# Run full CI pipeline locally (fmt-check + clippy + test)
ci: fmt-check clippy test
