# deepseeknova-tui

ratatui-based interactive terminal UI for deepseeknova.

Wraps a `Runner` and displays streaming output in a split-pane TUI:
- **Conversation pane** (top) — scrollable, renders text / reasoning (dimmed) /
  tool calls and truncated results / deterministic verification (✓/✗) /
  pauses / approval requests / errors; 2000-line cap.
- **Status bar** — current model, phase, turn, token usage, scroll position.
- **Input pane** (bottom) — single-line editing with a visible cursor,
  horizontal follow for long prompts, and input history.
- **Hint line** — edit keys (`Ctrl+U` / `Ctrl+W` / `Home` / `End`) and `/help`.

### Design

Follows Codex CLI's semantic color rules: user input and status indicators are
cyan, agent output is magenta, secondary information (reasoning, tool calls,
system messages) is dim, verification success is green, and failures/errors
are red. No hardcoded colors are used, so the UI reads correctly in both light
and dark terminals.

### Keys

- `Enter` submit; `Esc` quit when idle; `Ctrl+C` cancel the current run
- `↑`/`↓` input history; `←`/`→`/`Home`/`End` cursor movement (idle);
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
`with_mcp_servers`, and `with_undo_controller` (`/undo`).

```rust,no_run
use deepseeknova_tui::TuiRunner;
TuiRunner::new(runner).run().await?;
```

## License

Licensed under the same terms as deepseeknova.
