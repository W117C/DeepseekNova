# dpronix-tui

ratatui-based interactive terminal UI for dpronix.

Wraps a `Runner` and displays streaming output in a split-pane TUI:
- **Conversation pane** (top) — scrollable, shows agent text + tool calls.
- **Status bar** — current model, token usage.
- **Input pane** (bottom) — user prompt entry.

```rust,no_run
use dpronix_tui::TuiRunner;
TuiRunner::new(runner).run().await?;
```

## License

Licensed under the same terms as dpronix.
