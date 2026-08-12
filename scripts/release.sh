#!/usr/bin/env bash
set -euo pipefail

# deepseeknova release script
#
# ⚠️ 状态说明：发布资产构建与 GitHub Release 已由 `.github/workflows/release.yml`
# 专管（tag push `v*` 或 workflow_dispatch 触发；5 平台矩阵 + checksums + 资产
# 上传，见 AGENTS.md §3.1 与 release.yml）。本脚本保留作 **本地发布前自检** 参考：
#   - 工作区检查 + `cargo check/fmt/clippy/test`
#   - 全 crate `cargo publish --dry-run`（发布前验证打包与文档完整性）
#   - 本地 release 构建（单平台冒烟，产物在 target/release/deepseeknova-cli）
# 版本号由 `scripts/bump-version.sh` 统一提升（含内部依赖钉），不再在此 sed 改写。
#
# Usage: ./scripts/release.sh

# ── 1. Verify clean working tree ─────────────────────────────────
if [ -d .git ]; then
    if ! git diff --quiet || ! git diff --cached --quiet; then
        echo "ERROR: working tree is dirty — commit or stash changes first"
        exit 1
    fi
fi

# ── 2. Run checks ────────────────────────────────────────────────
echo "--- cargo check ---"
cargo check --all-targets --workspace

echo "--- cargo fmt ---"
cargo fmt --all --check

echo "--- cargo clippy ---"
cargo clippy --all-targets --workspace -- -D warnings

echo "--- cargo test ---"
cargo test --all --workspace

# ── 3. Dry-run publish（发布前打包自检）─────────────────────────
# 版本统一由 workspace（`[workspace.package] version` + 内部依赖钉）继承，
# 各 crate 使用 `version.workspace = true`，无需逐个改写。
echo "--- cargo publish --dry-run (library crates) ---"
LIBS=(
    "deepseeknova-core"
    "deepseeknova-config"
    "deepseeknova-telemetry"
    "deepseeknova-store"
    "deepseeknova-context"
    "deepseeknova-sandbox"
    "deepseeknova-security"
    "deepseeknova-checkpoint"
    "deepseeknova-skills"
    "deepseeknova-graph"
    "deepseeknova-metrics"
    "deepseeknova-scanner"
    "deepseeknova-serve"
    "deepseeknova-tui"
    "deepseeknova-permission"
    "deepseeknova-provider"
    "deepseeknova-mcp"
    "deepseeknova-tools"
    "deepseeknova-agent"
    "deepseeknova-runtime"
)
for lib in "${LIBS[@]}"; do
    echo "  checking $lib..."
    cargo publish -p "$lib" --dry-run --allow-dirty
done

echo "--- cargo publish --dry-run (cli binary) ---"
cargo publish -p deepseeknova-cli --dry-run --allow-dirty

# ── 4. Local release build（冒烟；多平台资产由 release.yml 产出）───
echo "--- building release binary (local smoke) ---"
cargo build --release -p deepseeknova-cli
echo "  binary: target/release/deepseeknova-cli"

echo ""
echo "=== Release pre-check complete ==="
echo "Next steps:"
echo "  1. Bump version: ./scripts/bump-version.sh <patch|minor|major>"
echo "  2. Tag & push:   git tag vX.Y.Z && git push origin main vX.Y.Z"
echo "  3. Assets:       release.yml 自动构建 5 平台 + checksums + GitHub Release"
