#!/usr/bin/env python3
"""Add version from workspace Cargo.toml to all path-only dependencies for crates.io publishing."""
import os, re

ROOT = "crates"
WORKSPACE_TOML = "Cargo.toml"

def get_workspace_version():
    """Extract [workspace.package] version from root Cargo.toml."""
    with open(WORKSPACE_TOML) as f:
        content = f.read()
    # Match version = "x.y.z" under [workspace.package]
    m = re.search(r'\[workspace\.package\][^[]*?version\s*=\s*"([^"]+)"', content, re.DOTALL)
    if not m:
        raise SystemExit("ERROR: could not find [workspace.package] version in Cargo.toml")
    return m.group(1)

VERSION = get_workspace_version()
print(f"Workspace version: {VERSION}")

for crate in sorted(os.listdir(ROOT)):
    toml_path = os.path.join(ROOT, crate, "Cargo.toml")
    if not os.path.isfile(toml_path):
        continue
    with open(toml_path) as f:
        content = f.read()

    # Fix: dpronix-XXX = { path = "../name" } -> dpronix-XXX = { version = "x.y.z", path = "../name" }
    def add_version(m):
        dep_name = m.group(1)
        dep_path = m.group(2)
        return f'{dep_name} = {{ version = "{VERSION}", path = "{dep_path}" }}'

    content = re.sub(
        r'(dpronix-[\w-]+)\s*=\s*\{\s*path\s*=\s*"((?:\.\./)[^"]+)"\s*\}',
        add_version,
        content,
    )

    with open(toml_path, "w") as f:
        f.write(content)
    print(f"  fixed {crate}")

print("done")
