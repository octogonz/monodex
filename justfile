# Justfile for monodex
# Run CI checks locally

# Run all CI checks (format, clippy, check, test)
ci: fmt-check clippy check test

# Format check
fmt-check:
    cargo fmt --all -- --check

# Auto-format code
fmt:
    cargo fmt --all

# Run clippy lints
clippy:
    cargo clippy --workspace --all-targets --locked

# Run cargo check
check:
    cargo check --workspace --all-targets --locked

# Run tests
test:
    cargo test --workspace --all-targets --locked

# Build release binary
build:
    cargo build --release

# Clean build artifacts
clean:
    cargo clean
