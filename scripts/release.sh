#!/usr/bin/env bash
set -euo pipefail

# deepseeknova-rs release script
# Usage: ./scripts/release.sh 0.1.0

VERSION="${1:?Usage: $0 <version> (e.g. 0.1.0)}"

echo "=== deepseeknova-rs release v${VERSION} ==="

# 1. Verify clean working tree
if [ -d .git ]; then
    if ! git diff --quiet || ! git diff --cached --quiet; then
        echo "ERROR: working tree is dirty — commit or stash changes first"
        exit 1
    fi
fi

# 2. Run checks
echo ""
echo "--- cargo check ---"
cargo check --all-targets --workspace

echo ""
echo "--- cargo fmt ---"
cargo fmt --all --check

echo ""
echo "--- cargo clippy ---"
cargo clippy --all-targets --workspace -- -D warnings

echo ""
echo "--- cargo test ---"
cargo test --all --workspace

# 3. Update version in all Cargo.toml files
echo ""
echo "--- updating version to ${VERSION} ---"
for crate_dir in crates/*/; do
    crate=$(basename "$crate_dir")
    toml_file="${crate_dir}Cargo.toml"
    # Replace version.workspace = true with hardcoded version for publishing
    sed -i.bak -E 's/^version\.workspace = true$/version = "'"${VERSION}"'" /' "$toml_file"
    rm -f "${toml_file}.bak"
done

# Update workspace package version
sed -i.bak -E 's/^version = "[0-9.]+"/version = "'"${VERSION}"'" /' Cargo.toml
rm -f Cargo.toml.bak

# 4. Dry-run publish (library crates first, then binary)
echo ""
echo "--- cargo publish --dry-run (library crates) ---"
LIBS=(
    "deepseeknova-core"
    "deepseeknova-config"
    "deepseeknova-telemetry"
    "deepseeknova-event"
    "deepseeknova-store"
    "deepseeknova-context"
    "deepseeknova-sandbox"
    "deepseeknova-security"
    "deepseeknova-checkpoint"
    "deepseeknova-skills"
    "deepseeknova-serve"
    "deepseeknova-tui"
    "deepseeknova-permission"
    "deepseeknova-provider"
    "deepseeknova-mcp"
    "deepseeknova-tools"
    "deepseeknova-agent"
    "deepseeknova-orch"
    "deepseeknova-runtime"
)
for lib in "${LIBS[@]}"; do
    echo "  checking $lib..."
    cargo publish -p "$lib" --dry-run --allow-dirty
done

echo ""
echo "--- cargo publish --dry-run (cli binary) ---"
cargo publish -p deepseeknova-cli --dry-run --allow-dirty

# 5. Build release artifacts
echo ""
echo "--- building release artifacts ---"
cargo build --release -p deepseeknova-cli

# Copy to dist/
mkdir -p dist
if [[ "$(uname -s)" == "Darwin" ]]; then
    cp target/release/deepseeknova dist/deepseeknova-${VERSION}-aarch64-apple-darwin
    echo "  dist/deepseeknova-${VERSION}-aarch64-apple-darwin"
elif [[ "$(uname -s)" == "Linux" ]]; then
    cp target/release/deepseeknova dist/deepseeknova-${VERSION}-x86_64-unknown-linux-gnu
    echo "  dist/deepseeknova-${VERSION}-x86_64-unknown-linux-gnu"
fi

# 6. Restore version.workspace = true in all Cargo.toml files
echo ""
echo "--- restoring version.workspace = true ---"
for crate_dir in crates/*/; do
    toml_file="${crate_dir}Cargo.toml"
    sed -i.bak -E 's/^version = "[0-9.]+"$/version.workspace = true/' "$toml_file"
    rm -f "${toml_file}.bak"
done
# Restore workspace version placeholder (optional — keep the release version in root)
# sed -i.bak -E 's/^version = "[0-9.]+"/version = "0.0.0-dev"/' Cargo.toml
# rm -f Cargo.toml.bak

# 7. Create git tag
if [ -d .git ]; then
    echo ""
    echo "--- git tag v${VERSION} ---"
    git add -A
    git commit -m "chore: release v${VERSION}"
    git tag "v${VERSION}"
    echo "Tag created. Push with:"
    echo "  git push origin main v${VERSION}"
fi

echo ""
echo "=== Release v${VERSION} complete ==="
echo "Next steps:"
echo "  1. Review: git diff HEAD~1"
echo "  2. Push:   git push origin main v${VERSION}"
echo "  3. Publish: cargo publish -p deepseeknova-core && ... && cargo publish -p deepseeknova-cli"
