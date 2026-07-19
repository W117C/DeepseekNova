# deepseeknova-tui

ratatui-based interactive terminal UI for deepseeknova.

Wraps a `Runner` and displays streaming output in a split-pane TUI:
- **Conversation pane** (top) — scrollable, shows agent text + tool calls.
- **Status bar** — current model, token usage.
- **Input pane** (bottom) — user prompt entry.

```rust,no_run
use deepseeknova_tui::TuiRunner;
TuiRunner::new(runner).run().await?;
```

## License

Licensed under the same terms as deepseeknova.
