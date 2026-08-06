# deepseeknova-tools

17 built-in tools for file I/O, globbing, grep, shell execution, web fetching,
task management, memory operations, code graph, Context7 docs, and delegation.
Additional tools: `web_search` (DuckDuckGo / Tavily / Bing / SearXNG) and
`lsp_diagnostics` (rust-analyzer / pyright / gopls / typescript-language-server /
clangd, auto-invoked after write/edit/move). Each tool implements the `Tool`
trait with security-aware execution.

## Context7 library docs

`context7_docs` fetches latest third-party library documentation snippets from the
public Context7 API (no key required): pass a `library` name and a `query` topic; the
tool resolves the library id (or accepts `library_id` directly) and returns the
`type=txt` doc snippet, truncated to `max_chars` (6000 default). The endpoint is pinned
to `context7.com` and execution requires the `NetworkAccess` capability; all errors are
mapped to model-friendly hints instead of failing the run.
