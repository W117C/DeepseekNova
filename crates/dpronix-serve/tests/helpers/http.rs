use reqwest::Client;

/// Create a reqwest Client configured to bypass system/environment proxies and
/// with a 5-second connection timeout for robust localhost integration testing.
pub fn localhost_client() -> Client {
    Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("failed creating test client")
}
