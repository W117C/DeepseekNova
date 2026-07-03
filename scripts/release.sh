#!/usr/bin/env bash
set -euo pipefail

# reasonix-rs release script
# Usage: ./scripts/release.sh 0.1.0

VERSION="${1:?Usage: $0 <version> (e.g. 0.1.0)}"

echo "=== reasonix-rs release v${VERSION} ==="

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
    python3 -c "
import re
with open('$toml_file', 'r') as f:
    content = f.read()
content = re.sub(r'version\.workspace\s*=\s*true', f'version = \"{VERSION}\"', content)
with open('$toml_file', 'w') as f:
    f.write(content)
"
done

# Update workspace package version
python3 -c "
import re
with open('Cargo.toml', 'r') as f:
    content = f.read()
content = re.sub(r'version = \"[0-9.]+\"', f'version = \"{VERSION}\"', content, count=1)
with open('Cargo.toml', 'w') as f:
    f.write(content)
"

# 4. Dry-run publish (library crates first, then binary)
echo ""
echo "--- cargo publish --dry-run (library crates) ---"
LIBS=(
    "reasonix-core"
    "reasonix-config"
    "reasonix-event"
    "reasonix-permission"
    "reasonix-context"
    "reasonix-provider"
    "reasonix-tools"
    "reasonix-mcp"
    "reasonix-checkpoint"
    "reasonix-sandbox"
    "reasonix-store"
    "reasonix-skills"
    "reasonix-telemetry"
    "reasonix-serve"
    "reasonix-tui"
    "reasonix-agent"
    "reasonix-runtime"
)
for lib in "${LIBS[@]}"; do
    echo "  checking $lib..."
    cargo publish -p "$lib" --dry-run --allow-dirty
done

echo ""
echo "--- cargo publish --dry-run (cli binary) ---"
cargo publish -p reasonix-cli --dry-run --allow-dirty

# 5. Build release artifacts
echo ""
echo "--- building release artifacts ---"
cargo build --release -p reasonix-cli

# Copy to dist/
mkdir -p dist
if [[ "$(uname -s)" == "Darwin" ]]; then
    cp target/release/reasonix dist/reasonix-${VERSION}-aarch64-apple-darwin
    echo "  dist/reasonix-${VERSION}-aarch64-apple-darwin"
elif [[ "$(uname -s)" == "Linux" ]]; then
    cp target/release/reasonix dist/reasonix-${VERSION}-x86_64-unknown-linux-gnu
    echo "  dist/reasonix-${VERSION}-x86_64-unknown-linux-gnu"
fi

# 6. Create git tag
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
echo "  3. Publish: cargo publish -p reasonix-core && ... && cargo publish -p reasonix-cli"
