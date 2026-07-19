# deepseeknova-serve

HTTP server for deepseeknova — exposes Runner via a REST + SSE API.

```rust,no_run
use deepseeknova_serve::Server;
let server = Server::new(runner);
server.serve("127.0.0.1:3000").await?;
```

## License

Licensed under the same terms as deepseeknova.
