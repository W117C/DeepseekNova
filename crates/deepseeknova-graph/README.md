# deepseeknova-graph

Code graph engine for token-efficient retrieval: tree-sitter parsing (Rust/Python/JS/TS/Go),
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
- **Edges** (`EdgeKind`): `Contains`, `Imports`, `Calls`, `Implements`, `References`,
  `Dispatch` (trait method → same-name impl method, Rust only). Call/reference edges use
  name-level matching (like aider / LocAgent); same-name collisions are diluted by
  PageRank rather than resolved via full type analysis.

## Multi-hop reasoning

Beyond single-hop neighbors, the store can trace bounded call paths and expose them as
three read-only tools:

- `trace_code` — DFS over Calls / References / Dispatch (depth cap 6, truncation marked),
  normalized to call order for the `callers` direction.
- `impact_code` — reverse-reachability aggregated by file (symbols + paths), i.e. the
  blast radius of a refactor.
- `explore_code` — line-numbered source (or skeleton) for several entities, grouped by
  file with overlapping ranges merged.

Rust trait polymorphism is bridged by `Dispatch` edges: `impl Trait for Type` methods are
linked to the trait declaration, so a `dyn Trait` / generic call site can list every
same-name impl candidate without type inference.

## Symbol references & dependency graph

- `References` edges are built from identifiers inside each definition body
  (name-level, capped per definition, skipping call callees and self-references;
  a `(src, dst)` pair already covered by `Calls` is not duplicated). Ask "who
  references X" via `traverse_graph` with `edge_kinds=["references"]`.
- Structured imports: Rust `use` path segments, Python `import/from` segments,
  JS/TS `import/require` specifiers, and Go `import` paths (stdlib / third-party
  bare paths become external deps; relative paths resolve to file edges). Local
  symbols become 文件→符号 `Imports` edges; JS relative specifiers resolve to
  文件→文件 edges (with common extension fallbacks); bare package names and
  manifest dependencies (`Cargo.toml`, `package.json`, `pyproject.toml`,
  `go.mod`, parsed with no new deps) are recorded as external dependencies.
- `deps_code` (registered with the other graph query tools) lists a file's
  imports / importers plus its nearest manifest's external deps, or a workspace
  external-dependency summary when no entity is given.

## Storage

Persisted to `.deepseeknova/graph.db` (SQLite). Incremental refresh keys off file
`mtime` then content `hash`: only changed files are re-parsed, and unchanged node ids stay
stable. A `symbol_fts` FTS5 table provides built-in BM25 search. The db is derived data —
delete it to force a full rebuild; a `schema_version` bump also clears the file table once
to force re-parsing (raw dispatch facts need it).

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

// Bounded multi-hop paths, call-order normalized (callers direction).
let tr = index.trace(
    "permission_gate_for",
    &[EdgeKind::Calls, EdgeKind::Dispatch],
    Direction::Callers,
    6,
)?;

// Skeleton (signature + doc + child signatures) vs. exact line range.
let sk = index.skeleton("build_agent")?;
let (path, start, end) = index.location("build_agent")?;

// Token-budgeted repo map; empty seeds = global, or pass symbol/path seeds for
// personalized PageRank.
let map = index.repo_map(1024, &[])?;
```

## Integration

- `deepseeknova-tools` exposes six read-only tools backed by a shared
  `GraphHandle` (`Arc<Mutex<GraphIndex>>`) injected via `ToolContext.extensions`:
  `search_code`, `traverse_graph`, `retrieve_entity` (built-ins), plus `trace_code`,
  `impact_code`, `explore_code` (registered by the runtime when the graph is enabled).
  When the index is absent they degrade to a "use grep" hint instead of erroring.
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
