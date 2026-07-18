# dpronix-serve

HTTP server for dpronix — exposes Runner via a REST + SSE API.

```rust,no_run
use dpronix_serve::Server;
let server = Server::new(runner);
server.serve("127.0.0.1:3000").await?;
```

## License

Licensed under the same terms as dpronix.
