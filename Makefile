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
# 故优先探测已安装的 node 并注入其目录；找不到时保持原 PATH（CI 由 setup-node
# 提供 node；本地需自行安装 Node.js 才能跑前端 lint/test）。
NODE_BIN := $(shell command -v node 2>/dev/null | xargs dirname 2>/dev/null)
NODE_BIN := $(if $(NODE_BIN),$(NODE_BIN),$(firstword $(wildcard /Users/*/.nvm/versions/node/*/bin /Users/ze/.volta/bin /opt/homebrew/bin /usr/local/bin)))
NODE_BIN := $(if $(wildcard $(NODE_BIN)/node),$(NODE_BIN),)
ifdef NODE_BIN
check-desktop: export PATH := $(NODE_BIN):$(PATH)
endif
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
