# deepseeknova-tui

ratatui-based interactive terminal UI for deepseeknova.

Wraps a `Runner` and displays streaming output in a split-pane TUI:
- **Conversation pane** (top) — scrollable, renders text / reasoning (dimmed) /
  tool calls and truncated results / deterministic verification (✓/✗) /
  pauses / approval requests / errors; 2000-line cap.
- **Status bar** — current model, phase, turn, token usage, session cost (`$`,
  refreshed from the model router ledger), scroll position.
- **Input pane** (bottom) — multi-line editing with a visible cursor:
  `Shift+Enter` / `Ctrl+J` inserts a newline, `Home`/`End` move per line,
  horizontal window and vertical scroll both follow the cursor, plus input
  history.
- **Hint line** — edit keys (`Ctrl+U` / `Ctrl+W` / `Shift+Enter` / `Home` / `End`)
  and `/help`.

### Design

Follows Codex CLI's semantic color rules: user input and status indicators are
cyan, agent output is magenta, secondary information (reasoning, tool calls,
system messages) is dim, verification success is green, and failures/errors
are red. Diff output is highlighted per line (`+` green, `-` red, `@@` cyan).
No hardcoded colors are used, so the UI reads correctly in both light and dark
terminals.

### Keys

- `Enter` submit; `Shift+Enter` / `Ctrl+J` newline; `Esc` quit when idle;
  `Ctrl+C` cancel the current run
- `↑`/`↓` input history (line movement when the input is multi-line);
  `←`/`→` cursor movement; `Home`/`End` per-line (idle);
  `Home`/`End` scroll when running
- `Backspace`/`Delete` edit; `Ctrl+U` clear input; `Ctrl+W` delete word before
- `PageUp`/`PageDown` scroll the conversation pane

### Slash commands

`/help` `/clear` `/new` `/sessions` `/resume <id>` `/model` (effort / thinking /
switch / use) `/cost` `/skills` `/mcp` `/raw` (normal/lite/raw) `/undo`
(`all` / `list`) `/quit`

Optional capabilities are enabled by builders:
`with_agent_factory` (`/model` hot-switch), `with_model_router`
(`/model use` + `/cost`), `with_session_controller`
(`/new` `/sessions` `/resume` + turn persistence), `with_skills_paths`,
`with_mcp_servers` + `with_mcp_probe` (`/mcp` live status: short-timeout spawn
probe that marks a still-alive stdio server as connected), and
`with_undo_controller` (`/undo`).

```rust,no_run
use deepseeknova_tui::TuiRunner;
TuiRunner::new(runner).run().await?;
```

## License

Licensed under the same terms as deepseeknova.
