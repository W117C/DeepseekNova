.PHONY: all build check test clean run release cross

# Default build target
all: build

# Build the project in debug mode
build:
	cargo build

# Run formatting, clippy, tests, and documentation checks
check:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings
	cargo test --workspace
	cargo doc --workspace --no-deps --document-private-items

# Format code
fmt:
	cargo fmt --all

# Run all tests
test:
	cargo test --all

# Run the CLI app locally
run:
	cargo run --bin dpronix-cli

# Build for release
release:
	cargo build --release

# Clean the target directory
clean:
	cargo clean

# Cross-compilation (example: to linux x86_64)
# Requires cross: cargo install cross
cross-linux:
	cross build --target x86_64-unknown-linux-gnu --release
