# deepseeknova-graph

Code graph engine for token-efficient retrieval: tree-sitter parsing (Rust/Python/JS/TS),
SQLite-persisted heterogeneous graph with FTS5 BM25 and personalized PageRank, powering
graph search tools and token-budgeted repo maps.

## Why

Agents locating code with plain `grep` + whole-file `read_file` burn tokens fast — a
3000-line file just to reach a 40-line function. This crate parses the workspace into a
directed heterogeneous graph and exposes precise, budget-bounded retrieval so the model
sees only what matters.

## Model

- **Nodes** (`NodeKind`): `Directory`, `File`, `Struct`, `Enum`, `Trait`, `Class`,
  `Function`, `Method` — each carries `path`, line range, single-line `signature`, first
  doc line, and a PageRank `score`.
- **Edges** (`EdgeKind`): `Contains`, `Imports`, `Calls`, `Implements`, `References`.
  Call/reference edges use name-level matching (like aider / LocAgent); same-name
  collisions are diluted by PageRank rather than resolved via full type analysis.

## Storage

Persisted to `.deepseeknova/graph.db` (SQLite). Incremental refresh keys off file
`mtime` then content `hash`: only changed files are re-parsed, and unchanged node ids stay
stable. A `symbol_fts` FTS5 table provides built-in BM25 search. The db is derived data —
delete it to force a full rebuild; no schema migrations.

## API

```rust
use deepseeknova_graph::{GraphIndex, Direction, EdgeKind, NodeKind};

// Open (fast; does not parse) then refresh (incremental; recomputes PageRank).
let mut index = GraphIndex::open(workspace_root, 512 * 1024)?;
index.refresh()?;

// Locate by name / keyword (FTS5 BM25 + exact-name boost).
let hits = index.search("PermissionGate", Some(NodeKind::Struct), 10)?;

// Multi-hop relationships (callers/callees), siblings ranked by PageRank.
let callers = index.neighbors("permission_gate_for", &[EdgeKind::Calls], Direction::Callers, 2)?;

// Skeleton (signature + doc + child signatures) vs. exact line range.
let sk = index.skeleton("build_agent")?;
let (path, start, end) = index.location("build_agent")?;

// Token-budgeted repo map; empty seeds = global, or pass symbol/path seeds for
// personalized PageRank.
let map = index.repo_map(1024, &[])?;
```

## Integration

- `deepseeknova-tools` exposes three read-only tools backed by a shared
  `GraphHandle` (`Arc<Mutex<GraphIndex>>`) injected via `ToolContext.extensions`:
  `search_code`, `traverse_graph`, `retrieve_entity`. When the index is absent they
  degrade to a "use grep" hint instead of erroring.
- `deepseeknova-runtime::build_agent` opens the index, refreshes it in the background
  (non-blocking), injects the handle, and appends a retrieval-strategy hint to the system
  prompt. All gated by `[graph] enabled` in config; disabled = behavior identical to before.
- `deepseeknova-context::PromptBuilder` accepts an optional repo map, injected into the
  stable prefix region (after project context) so DeepSeek prefix caching is preserved.

## Config (`[graph]`)

```toml
[graph]
enabled = true          # false = zero overhead, behavior unchanged
repo_map_tokens = 1024  # 0 = no map injection, tools only
max_file_size = 524288  # bytes; larger files are skipped during parsing
```

Design doc: `docs/superpowers/specs/2026-07-26-graph-engineering-design.md`.
