# Run all CI checks (format, clippy, test)
ci: fmt-check clippy test

# Format check
fmt-check:
	cargo fmt --all -- --check

# Auto-format code
fmt:
	cargo fmt --all

# Run clippy lints (fail on warnings)
clippy:
	cargo clippy --workspace --all-targets --locked -- -D warnings

# Run tests
test:
	cargo test --workspace --locked

# Build release binary
build:
	cargo build --release

# Clean build artifacts
clean:
	cargo clean
