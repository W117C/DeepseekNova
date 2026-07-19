# deepnova-desktop

Native desktop application for the deepnova AI agent framework.
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
│  deepnova-runtime / deepnova-agent (Rust)     │
│  (same kernel as CLI, TUI, HTTP server)       │
└──────────────────────────────────────────────┘
```
