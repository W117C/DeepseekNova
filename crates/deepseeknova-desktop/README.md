# deepseeknova-desktop

Native desktop application for the deepseeknova AI agent framework.
Built with Tauri 2.x — Rust backend with a React/TypeScript frontend.

## Architecture

```text
┌─────────────────────────────────────────────┐
│  Webview (React + TS, Vite)                  │
│    bridge.ts ──invoke──▶ Tauri Commands      │
│    bridge.ts ◀─Channel── agent:event stream   │
└───────────────▲──────────────────────────────┘
        commands │                  events
┌───────────────┴──────────────────────────────┐
│  commands.rs  (Tauri command handlers)        │
│    └── runner::run_stream() → Channel         │
└───────────────▲──────────────────────────────┘
                │
┌───────────────┴──────────────────────────────┐
│  deepseeknova-runtime / deepseeknova-agent (Rust)     │
│  (same kernel as CLI, TUI, HTTP server)       │
└──────────────────────────────────────────────┘
```
