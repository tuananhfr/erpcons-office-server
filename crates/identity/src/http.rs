use std::time::Duration;

use reqwest::redirect;

/// The HTTP client for calls to PI; each request sets its own total timeout.
pub(crate) fn client(connect_timeout: Duration) -> Result<reqwest::Client, reqwest::Error> {
    // reqwest is built without a crypto provider (workspace Cargo.toml); this installs ring once
    // per process and is a no-op when a provider is already installed.
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .connect_timeout(connect_timeout)
        // A redirect must never carry the user's token anywhere else.
        .redirect(redirect::Policy::none())
        .user_agent(concat!("erpcons-office-server/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// The body, or `None` when it cannot be read or exceeds `max` bytes.
pub(crate) async fn read_capped(mut response: reqwest::Response, max: usize) -> Option<Vec<u8>> {
    if response.content_length().is_some_and(|n| n > max as u64) {
        return None;
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if body.len() + chunk.len() > max {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    Some(body)
}
