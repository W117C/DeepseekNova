.PHONY: all build check test clean run release \
        release-patch release-minor release-major \
        check-all check-desktop test-all clippy-fix example \
        frontend desktop install dist audit

# ── Default ─────────────────────────────────────────────────────
all: build

# ── Build ───────────────────────────────────────────────────────
build:
	cargo build

# ── Comprehensive check (CI equivalent) ────────────────────────
check:
	cargo fmt --all -- --check
	cargo clippy --workspace --exclude deepseeknova-desktop --all-targets -- -D warnings
	cargo test --workspace --exclude deepseeknova-desktop
	cargo doc --workspace --exclude deepseeknova-desktop --no-deps --document-private-items

check-all:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings || true
	cargo test --workspace

# ── Desktop local verification (CI check-desktop + frontend equivalent) ──
# Rust 侧需要前端产物 dist/，缺失时先运行 make frontend
# nvm 装的 node 在非交互 shell 的 PATH 中不可见（npm: command not found / exit 127），
# 故为本 target 显式注入 nvm node 路径；CI 由 setup-node 提供 node，该目录在 CI 上
# 不存在，前置一个不存在的目录无副作用。
check-desktop: export PATH := /Users/ze/.nvm/versions/node/v20.20.2/bin:$(PATH)
check-desktop:
	cd crates/deepseeknova-desktop/frontend && npm run lint && npm test
	cargo check -p deepseeknova-desktop
	cargo clippy -p deepseeknova-desktop --all-targets -- -D warnings
	cargo test -p deepseeknova-desktop

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
	cargo clippy --workspace --exclude deepseeknova-desktop --all-targets --fix --allow-dirty

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

# ── Frontend (Desktop) ─────────────────────────────────────────
frontend:
	cd crates/deepseeknova-desktop/frontend && npm ci && npm run build

# ── Desktop app ────────────────────────────────────────────────
desktop: frontend
	cargo build -p deepseeknova-desktop --release

# ── Install CLI binary ─────────────────────────────────────────
install:
	cargo install --path crates/deepseeknova-cli --force

# ── Distribution package ───────────────────────────────────────
dist: release
	@echo "Release binary at target/release/deepseeknova-cli"
	@echo "Run 'make desktop' for desktop app build"

# ── Security audit ─────────────────────────────────────────────
audit:
	cargo audit || cargo install cargo-audit && cargo audit
