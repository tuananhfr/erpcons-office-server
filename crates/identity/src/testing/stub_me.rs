use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};

/// A PI stand-in whose `GET /v1/me` answers 200 with a fixed JSON body, for bodies pi-mock cannot
/// produce (a missing or unknown account status). Stops when dropped.
pub struct StubMe {
    base_url: String,
    task: JoinHandle<()>,
}

impl StubMe {
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

impl Drop for StubMe {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Starts the stub on the current runtime; every request gets `body`.
pub async fn stub_me(body: &str) -> StubMe {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind stub");
    let base_url = format!("http://{}", listener.local_addr().expect("stub address"));
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let task = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let response = response.clone();
            tokio::spawn(async move {
                // Requests are bodiless GETs: the head ends at the first blank line.
                let mut head = Vec::new();
                let mut chunk = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match socket.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&chunk[..n]),
                    }
                }
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    StubMe { base_url, task }
}
