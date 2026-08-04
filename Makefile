.PHONY: all build check test clean run release \
        release-patch release-minor release-major \
        check-all test-all clippy-fix example \
        install dist audit

# ── Default ─────────────────────────────────────────────────────
all: build

# ── Build ───────────────────────────────────────────────────────
build:
	cargo build

# ── Comprehensive check (CI equivalent) ────────────────────────
check:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings
	cargo test --workspace
	cargo doc --workspace --no-deps --document-private-items

check-all:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings || true
	cargo test --workspace

# ── Format ──────────────────────────────────────────────────────
fmt:
	cargo fmt --all

# ── Test ────────────────────────────────────────────────────────
test:
	cargo test --all

test-all:
	cargo test --workspace

# ── Clippy auto-fix ─────────────────────────────────────────────
clippy-fix:
	cargo clippy --workspace --all-targets --fix --allow-dirty

# ── Run ─────────────────────────────────────────────────────────
run:
	cargo run --bin deepseeknova-cli

# ── Example ─────────────────────────────────────────────────────
example:
	cargo run --example quickstart -p deepseeknova-cli

# ── Release build ───────────────────────────────────────────────
release:
	cargo build --release

# ── Version bumping ─────────────────────────────────────────────
release-patch:
	./scripts/bump-version.sh patch

release-minor:
	./scripts/bump-version.sh minor

release-major:
	./scripts/bump-version.sh major

# ── Clean ───────────────────────────────────────────────────────
clean:
	cargo clean

# ── Cross-compilation ──────────────────────────────────────────
cross-linux:
	cross build --target x86_64-unknown-linux-gnu --release

# ── Install CLI binary ─────────────────────────────────────────
install:
	cargo install --path crates/deepseeknova-cli --force

# ── Distribution package ───────────────────────────────────────
dist: release
	@echo "Release binary at target/release/deepseeknova-cli"

# ── Security audit ─────────────────────────────────────────────
audit:
	cargo audit || cargo install cargo-audit && cargo audit