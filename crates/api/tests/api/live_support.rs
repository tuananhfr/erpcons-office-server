//! A WebSocket client of the live channel, and the ticket requests before it.
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use reqwest::Method;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Error as WsError, Message, protocol::CloseFrame},
};

use crate::support::{TestApp, ok_json};

const WAIT: Duration = Duration::from_secs(5);

pub fn ticket_path(doc: &str) -> String {
    format!("/api/v1/documents/{doc}/live-ticket")
}

/// A ticket for `doc` with a PI token; asserts 200.
pub async fn ticket(app: &TestApp, token: &str, doc: &str) -> String {
    let response = app
        .request(Method::POST, &ticket_path(doc), token)
        .send()
        .await
        .expect("ticket");
    let body = ok_json(response, 200).await;
    assert_eq!(body["expires_in"], 30);
    body["ticket"].as_str().expect("ticket").to_owned()
}

/// A ticket for `doc` with a public link's token.
pub async fn link_ticket(app: &TestApp, link: &str, doc: &str) -> reqwest::Response {
    app.http
        .post(app.url(&ticket_path(doc)))
        .header("x-link-token", link)
        .send()
        .await
        .expect("link ticket")
}

pub fn live_url(app: &TestApp, doc: &str, ticket: &str) -> String {
    app.url(&format!("/api/v1/documents/{doc}/live?ticket={ticket}"))
        .replacen("http://", "ws://", 1)
}

/// The HTTP status a refused upgrade answered.
pub async fn refused(app: &TestApp, doc: &str, ticket: &str) -> u16 {
    match connect_async(live_url(app, doc, ticket)).await {
        Err(WsError::Http(response)) => response.status().as_u16(),
        Err(e) => panic!("unexpected error: {e}"),
        Ok(_) => panic!("the upgrade was accepted"),
    }
}

pub struct Ws {
    stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

/// Opens the room with `ticket`.
pub async fn connect(app: &TestApp, doc: &str, ticket: &str) -> Ws {
    let (stream, _) = connect_async(live_url(app, doc, ticket))
        .await
        .expect("upgrade");
    Ws { stream }
}

/// Ticket, then connection, then its `hello`.
pub async fn join(app: &TestApp, token: &str, doc: &str) -> (Ws, Value) {
    let ticket = ticket(app, token, doc).await;
    let mut ws = connect(app, doc, &ticket).await;
    let hello = ws.next_of("hello").await;
    (ws, hello)
}

/// What ended a connection: the last `bye` reason (or error code) and the close code.
#[derive(Debug, PartialEq, Eq)]
pub struct Ended {
    pub said: Option<String>,
    pub code: Option<u16>,
}

impl Ws {
    /// The next protocol message; pings and pongs are skipped.
    pub async fn next_json(&mut self) -> Value {
        loop {
            let frame = tokio::time::timeout(WAIT, self.stream.next())
                .await
                .expect("a message in time")
                .expect("open")
                .expect("frame");
            match frame {
                Message::Text(text) => return serde_json::from_str(text.as_str()).expect("json"),
                Message::Ping(_) | Message::Pong(_) => {}
                other => panic!("unexpected frame: {other:?}"),
            }
        }
    }

    pub async fn next_of(&mut self, kind: &str) -> Value {
        loop {
            let message = self.next_json().await;
            if message["type"] == kind {
                return message;
            }
        }
    }

    /// The next `presence` with `n` members.
    pub async fn presence_of(&mut self, n: usize) -> Value {
        loop {
            let message = self.next_of("presence").await;
            if message["members"].as_array().map(Vec::len) == Some(n) {
                return message;
            }
        }
    }

    /// Keeps reading (and so answering pings) for `period`; returns the messages.
    pub async fn listen(&mut self, period: Duration) -> Vec<Value> {
        let mut messages = Vec::new();
        let deadline = tokio::time::Instant::now() + period;
        while let Ok(Some(Ok(frame))) = tokio::time::timeout_at(deadline, self.stream.next()).await
        {
            if let Message::Text(text) = frame {
                messages.push(serde_json::from_str(text.as_str()).expect("json"));
            }
        }
        messages
    }

    pub async fn send_text(&mut self, text: String) {
        self.stream
            .send(Message::Text(text.into()))
            .await
            .expect("send");
    }

    pub async fn send_json(&mut self, value: &Value) {
        self.send_text(value.to_string()).await;
    }

    /// Reads until the server closes; skips other messages.
    pub async fn ended(&mut self) -> Ended {
        let mut said = None;
        loop {
            let frame = tokio::time::timeout(WAIT, self.stream.next())
                .await
                .expect("closed in time");
            match frame {
                Some(Ok(Message::Text(text))) => {
                    let v: Value = serde_json::from_str(text.as_str()).expect("json");
                    if v["type"] == "bye" {
                        said = v["reason"].as_str().map(str::to_owned);
                    } else if v["type"] == "error" {
                        said = v["code"].as_str().map(str::to_owned);
                    }
                }
                Some(Ok(Message::Close(frame))) => {
                    return Ended {
                        said,
                        code: frame.map(|f: CloseFrame| u16::from(f.code)),
                    };
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => return Ended { said, code: None },
            }
        }
    }

    /// Leaves with `bye`, as a client closing the tab would.
    pub async fn bye(mut self) -> Ended {
        self.send_json(&serde_json::json!({ "type": "bye", "v": 1 }))
            .await;
        self.ended().await
    }

    /// Drops the TCP connection without a goodbye.
    pub fn vanish(self) {
        drop(self.stream);
    }
}

/// A ticket for `doc` with a public link's token, then the connection.
pub async fn join_link(app: &TestApp, link: &str, doc: &str) -> Ws {
    let body = ok_json(link_ticket(app, link, doc).await, 200).await;
    connect(app, doc, body["ticket"].as_str().expect("ticket")).await
}

/// Waits until the room of `doc` has `n` members.
pub async fn wait_members(app: &TestApp, doc: &str, n: usize) {
    let id = doc.parse().expect("uuid");
    tokio::time::timeout(WAIT, async {
        while app.live().members_of(id) != n {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("room size in time");
}
