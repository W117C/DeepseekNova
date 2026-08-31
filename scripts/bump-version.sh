#!/usr/bin/env bash
# ── 版本发布脚本 ────────────────────────────────────────────────
# 用法:  ./scripts/bump-version.sh <patch|minor|major>
#
# 流程:
#   1. 从 Cargo.toml 读取当前版本
#   2. 计算新版本号
#   3. 更新 workspace Cargo.toml 中的版本
#   4. 提示手动更新 CHANGELOG.md
#   5. 创建 git commit + tag
#
# 示例:  ./scripts/bump-version.sh minor   # 0.3.0 → 0.4.0

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# ── 参数校验 ────────────────────────────────────────────────────
BUMP_TYPE="${1:-}"
if [[ "$BUMP_TYPE" != "patch" && "$BUMP_TYPE" != "minor" && "$BUMP_TYPE" != "major" ]]; then
    echo "用法: $0 <patch|minor|major>"
    exit 1
fi

# ── 读取当前版本 ────────────────────────────────────────────────
CURRENT_VERSION=$(grep '^version = ' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')
echo "当前版本: $CURRENT_VERSION"

IFS='.' read -r MAJOR MINOR PATCH <<< "$CURRENT_VERSION"

case "$BUMP_TYPE" in
    patch) NEW_VERSION="$MAJOR.$MINOR.$((PATCH + 1))" ;;
    minor) NEW_VERSION="$MAJOR.$((MINOR + 1)).0" ;;
    major) NEW_VERSION="$((MAJOR + 1)).0.0" ;;
esac

echo "新版本:   $NEW_VERSION"
echo ""

# ── 检查工作区是否干净 ──────────────────────────────────────────
if ! git diff --quiet; then
    echo "❌ 工作区有未提交的变更，请先提交或暂存。"
    exit 1
fi

# ── 更新 Cargo.toml ─────────────────────────────────────────────
# 除 `[workspace.package] version` 外，`[workspace.dependencies]` 中 22 个内部
# crate 钉（`deepseeknova-* = { version = "0.5.0", path = ... }`）同样需要提升，
# 因此用非锚定 + 全局替换（两处之外的 `version = "<当前版>"` 不存在）。
# 各 crate 的 `documentation` URL 已改为不带版本（docs.rs 自动跳转最新版），无需同步。
if [[ "$(uname -s)" == "Darwin" ]]; then
    sed -i '' "s/version = \"$CURRENT_VERSION\"/version = \"$NEW_VERSION\"/g" Cargo.toml
else
    sed -i "s/version = \"$CURRENT_VERSION\"/version = \"$NEW_VERSION\"/g" Cargo.toml
fi

echo "✅ Cargo.toml 已更新: $CURRENT_VERSION → ${NEW_VERSION}（workspace 版本 + 内部依赖钉）"
echo ""

# ── 同步 npm 包版本 ───────────────────────────────────────────────
# npm/deepseeknova 的 postinstall 按 package.json version 下载对应 release
# 资产；不同步会导致发布 tag 与 npm 包版本脱节（npm publish 撞旧版本或
# 下载错误版本资产）。
NPM_MANIFEST="npm/deepseeknova/package.json"
if grep -q "\"version\": \"$CURRENT_VERSION\"" "$NPM_MANIFEST"; then
    if [[ "$(uname -s)" == "Darwin" ]]; then
        sed -i '' "s/\"version\": \"$CURRENT_VERSION\"/\"version\": \"$NEW_VERSION\"/" "$NPM_MANIFEST"
    else
        sed -i "s/\"version\": \"$CURRENT_VERSION\"/\"version\": \"$NEW_VERSION\"/" "$NPM_MANIFEST"
    fi
    echo "✅ $NPM_MANIFEST 已更新: $CURRENT_VERSION → $NEW_VERSION"
else
    echo "⚠️  $NPM_MANIFEST 中未找到 version \"$CURRENT_VERSION\"，请手动核对 npm 包版本"
fi
echo ""
echo "⚠️  请手动更新 CHANGELOG.md，然后运行:"
echo "   git add Cargo.toml CHANGELOG.md npm/deepseeknova/package.json"
echo "   git commit -m \"chore(release): v$NEW_VERSION\""
echo "   git tag v$NEW_VERSION"
echo "   git push origin main --tags"
echo ""
echo "或直接执行以下命令确认 CHANGELOG 已更新后继续:"

# ── 检查是否在 CI 环境 ──────────────────────────────────────────
if [ -n "${CI:-}" ]; then
    echo "CI 环境检测，跳过交互确认"
fi
