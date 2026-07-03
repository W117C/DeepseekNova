.PHONY: all build check test clean run release cross

# Default build target
all: build

# Build the project in debug mode
build:
	cargo build

# Run formatting and linting checks
check:
	cargo fmt --all -- --check
	cargo clippy --all-targets --all-features -- -D warnings

# Format code
fmt:
	cargo fmt --all

# Run all tests
test:
	cargo test --all

# Run the CLI app locally
run:
	cargo run --bin reasonix-cli

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
