# deepseeknova-serve

HTTP server for deepseeknova — exposes Runner via a REST + SSE API.

```rust,no_run
use deepseeknova_serve::Server;
let server = Server::new(runner);
server.serve("127.0.0.1:3000").await?;
```

Also ships an Agent Client Protocol (ACP) v1 stdio server:

```rust,no_run
use deepseeknova_serve::{serve_acp, AcpRunnerFactory};
// serve_acp(factory).await?  // factory: cwd + history -> Arc<dyn Runner>
```

`deepseeknova-cli serve --acp` uses the same implementation. The stdio adapter
supports `initialize`, `session/new`, `session/prompt`, `session/cancel` and
`session/close`; sessions keep multi-turn history and deny permission `Ask`
requests (fail-closed).

## License

Licensed under the same terms as deepseeknova.
