# Contributing to dpronix

## Setup

```bash
git clone https://github.com/W117C/DPronix.git
cd dpronix-rs
cargo build
```

## Development Workflow

1. Fork the repository
2. Create a feature branch: `git checkout -b feat/my-feature`
3. Write code with tests
4. Run verification before committing:

```bash
cargo fmt --all --check
cargo clippy --all-targets --workspace -- -D warnings
cargo test --all --workspace
```

5. Commit with a conventional commit message
6. Open a pull request

## Commit Format

```
<type>: <description>
```

Types: `feat`, `fix`, `refactor`, `docs`, `test`, `chore`, `perf`, `ci`

## Project Structure

```
crates/
├── dpronix-core/       # Foundation: types, traits, registry
├── dpronix-agent/      # Agent loop, memory, plan mode
├── dpronix-provider/   # LLM provider abstraction
├── dpronix-tools/      # Built-in tools
├── dpronix-mcp/        # MCP client
├── dpronix-config/     # Configuration
├── dpronix-context/    # Workspace indexing, context
├── dpronix-permission/ # Permission gating
├── dpronix-event/      # Event bus
├── dpronix-runtime/    # Composition root
├── dpronix-sandbox/    # Process sandbox
├── dpronix-checkpoint/ # File checkpoint/rollback
├── dpronix-store/      # Session persistence
├── dpronix-tui/        # Terminal UI
├── dpronix-serve/      # HTTP server
├── dpronix-skills/     # Skill system
└── dpronix-cli/        # CLI binary
```

## Testing

- **Unit tests**: `#[cfg(test)]` modules in each crate
- **Integration tests**: `crates/<crate>/tests/` directories
- **Run all tests**: `cargo test --all --workspace`
- **Run specific crate tests**: `cargo test -p <crate-name>`
- **Run benchmarks**: `cargo bench -p dpronix-core`

## Code Style

- Follow Rust conventions: `cargo fmt` and `cargo clippy`
- Functions under 50 lines, files under 500 lines
- Prefer immutability — use `let` by default, `let mut` only when needed
- Use `thiserror` for library errors, `anyhow` for binary code
- No `unwrap()` or `expect()` in production code — use `?` and `Context`

## CI/CD

GitHub Actions runs on every push and PR:
- `cargo check --all-targets --workspace`
- `cargo clippy --all-targets --workspace -- -D warnings`
- `cargo fmt --all --check`
- `cargo test --all --workspace` (ubuntu, macos, windows)
- `cargo doc --no-deps --workspace --document-private-items`

## License

Licensed under either of MIT or Apache-2.0 at your option.
Contributions are made under the same terms.
