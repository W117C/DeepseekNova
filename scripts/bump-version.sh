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
if [[ "$(uname -s)" == "Darwin" ]]; then
    sed -i '' "s/^version = \"$CURRENT_VERSION\"/version = \"$NEW_VERSION\"/" Cargo.toml
else
    sed -i "s/^version = \"$CURRENT_VERSION\"/version = \"$NEW_VERSION\"/" Cargo.toml
fi

echo "✅ Cargo.toml 已更新: $CURRENT_VERSION → $NEW_VERSION"
echo ""
echo "⚠️  请手动更新 CHANGELOG.md，然后运行:"
echo "   git add Cargo.toml CHANGELOG.md"
echo "   git commit -m \"chore(release): v$NEW_VERSION\""
echo "   git tag v$NEW_VERSION"
echo "   git push origin main --tags"
echo ""
echo "或直接执行以下命令确认 CHANGELOG 已更新后继续:"

# ── 检查是否在 CI 环境 ──────────────────────────────────────────
if [ -n "${CI:-}" ]; then
    echo "CI 环境检测，跳过交互确认"
fi
