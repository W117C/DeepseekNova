#!/usr/bin/env python3
"""Add version = "0.1.0" to all path-only dependencies for crates.io publishing."""
import os, re

ROOT = "/Users/ze/Downloads/dpronix-rs/crates"

for crate in sorted(os.listdir(ROOT)):
    toml_path = os.path.join(ROOT, crate, "Cargo.toml")
    if not os.path.isfile(toml_path):
        continue
    with open(toml_path) as f:
        content = f.read()

    # Fix: reasonix-XXX = { path = "../name" } -> reasonix-XXX = { version = "0.1.0", path = "../name" }
    def add_version(m):
        dep_name = m.group(1)
        dep_path = m.group(2)
        return f'{dep_name} = {{ version = "0.1.0", path = "{dep_path}" }}'

    content = re.sub(
        r'(reasonix-[\w-]+)\s*=\s*\{\s*path\s*=\s*"((?:\.\./)[^"]+)"\s*\}',
        add_version,
        content,
    )

    with open(toml_path, "w") as f:
        f.write(content)
    print(f"  fixed {crate}")

print("done")
