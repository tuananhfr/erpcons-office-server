//! The JSON messages of the live channel (`docs/openapi.yaml`, "Live channel"). Every message has
//! `type` and `v`. The server sends `hello`, `presence`, `mode`, `leader`, `bye` and `error`; a
//! client sends `presence` and `bye`. `lock.*`, `region.*` and `ack` are reserved for M4.
use serde::{Deserialize, Serialize};

use crate::{ByeReason, MemberId, Mode, Notice, PresenceState};

pub const VERSION: u32 = 1;

/// `error.code` of the live channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LiveErrorCode {
    /// Not JSON, not an object with `type` and `v`, an unknown type, or a binary frame.
    InvalidMessage,
    /// A `v` this server does not speak.
    UnsupportedVersion,
    /// A type reserved for a later version (`lock.*`, `region.*`, `ack`).
    Unsupported,
    /// Messages sent faster than allowed; the connection closes.
    RateLimited,
    /// The document has as many connections as it may.
    RoomFull,
    /// The client address has as many connections as it may.
    TooManyConnections,
    /// The server has as many connections as it may, or is shutting down.
    ServerBusy,
}

impl LiveErrorCode {
    pub const ALL: [LiveErrorCode; 7] = [
        Self::InvalidMessage,
        Self::UnsupportedVersion,
        Self::Unsupported,
        Self::RateLimited,
        Self::RoomFull,
        Self::TooManyConnections,
        Self::ServerBusy,
    ];
}

/// One member as the others see it; `name` is left out for link holders and for anyone shown to a
/// link holder.
#[derive(Serialize)]
pub(crate) struct MemberView<'a> {
    pub id: MemberId,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<&'a str>,
    pub colour: &'static str,
    pub mode: Mode,
    pub state: PresenceState,
}

#[derive(Serialize)]
struct Envelope<T> {
    #[serde(rename = "type")]
    kind: &'static str,
    v: u32,
    #[serde(flatten)]
    body: T,
}

fn encode<T: Serialize>(kind: &'static str, body: T) -> String {
    let message = Envelope {
        kind,
        v: VERSION,
        body,
    };
    // Plain structs of strings and numbers always serialize.
    serde_json::to_string(&message).unwrap_or_default()
}

#[derive(Serialize)]
pub(crate) struct Limits {
    pub max_message_bytes: usize,
    pub messages_per_second: f64,
    pub ping_secs: u64,
}

#[derive(Serialize)]
pub(crate) struct Hello<'a> {
    pub you: MemberId,
    pub mode: Mode,
    pub notice: Option<Notice>,
    pub colour: &'static str,
    pub seq: u64,
    pub leader: Option<MemberId>,
    pub members: &'a [MemberView<'a>],
    pub limits: Limits,
}

pub(crate) fn hello(body: Hello<'_>) -> String {
    encode("hello", body)
}

#[derive(Serialize)]
struct Presence<'a> {
    seq: u64,
    members: &'a [MemberView<'a>],
}

pub(crate) fn presence(seq: u64, members: &[MemberView<'_>]) -> String {
    encode("presence", Presence { seq, members })
}

#[derive(Serialize)]
struct ModeChange {
    seq: u64,
    mode: Mode,
    notice: Option<Notice>,
}

pub(crate) fn mode(seq: u64, mode: Mode, notice: Option<Notice>) -> String {
    encode("mode", ModeChange { seq, mode, notice })
}

#[derive(Serialize)]
struct Leader {
    seq: u64,
    leader: Option<MemberId>,
}

pub(crate) fn leader(seq: u64, leader: Option<MemberId>) -> String {
    encode("leader", Leader { seq, leader })
}

#[derive(Serialize)]
struct Bye {
    reason: ByeReason,
}

pub fn bye(reason: ByeReason) -> String {
    encode("bye", Bye { reason })
}

#[derive(Serialize)]
struct ErrorBody {
    code: LiveErrorCode,
}

pub fn error(code: LiveErrorCode) -> String {
    encode("error", ErrorBody { code })
}

/// What a client may send.
#[derive(Debug, PartialEq, Eq)]
pub enum ClientMessage {
    Presence(PresenceState),
    Bye,
}

#[derive(Deserialize)]
struct Raw {
    #[serde(rename = "type")]
    kind: String,
    v: u32,
    state: Option<PresenceState>,
}

/// Parses one text message from a client.
pub fn parse(text: &str) -> Result<ClientMessage, LiveErrorCode> {
    let raw: Raw = serde_json::from_str(text).map_err(|_| LiveErrorCode::InvalidMessage)?;
    if raw.v != VERSION {
        return Err(LiveErrorCode::UnsupportedVersion);
    }
    match raw.kind.as_str() {
        "presence" => raw
            .state
            .map(ClientMessage::Presence)
            .ok_or(LiveErrorCode::InvalidMessage),
        "bye" => Ok(ClientMessage::Bye),
        "ack" => Err(LiveErrorCode::Unsupported),
        k if k.starts_with("lock.") || k.starts_with("region.") => Err(LiveErrorCode::Unsupported),
        _ => Err(LiveErrorCode::InvalidMessage),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    #[test]
    fn parses_what_clients_may_send() {
        assert_eq!(
            parse(r#"{"type":"presence","v":1,"state":"idle"}"#),
            Ok(ClientMessage::Presence(PresenceState::Idle))
        );
        assert_eq!(parse(r#"{"type":"bye","v":1}"#), Ok(ClientMessage::Bye));
    }

    #[test]
    fn refuses_the_rest_with_a_code() {
        let cases = [
            ("not json", LiveErrorCode::InvalidMessage),
            (r#"{"type":"bye"}"#, LiveErrorCode::InvalidMessage),
            (r#"{"type":"bye","v":2}"#, LiveErrorCode::UnsupportedVersion),
            (
                r#"{"type":"presence","v":1}"#,
                LiveErrorCode::InvalidMessage,
            ),
            (
                r#"{"type":"presence","v":1,"state":"typing"}"#,
                LiveErrorCode::InvalidMessage,
            ),
            (r#"{"type":"hello","v":1}"#, LiveErrorCode::InvalidMessage),
            (
                r#"{"type":"lock.acquire","v":1}"#,
                LiveErrorCode::Unsupported,
            ),
            (
                r#"{"type":"region.changed","v":1}"#,
                LiveErrorCode::Unsupported,
            ),
            (r#"{"type":"ack","v":1}"#, LiveErrorCode::Unsupported),
        ];
        for (text, code) in cases {
            assert_eq!(parse(text), Err(code), "{text}");
        }
    }

    #[test]
    fn server_messages_carry_type_and_version() {
        let bye: Value = serde_json::from_str(&bye(ByeReason::Revoked)).expect("json");
        assert_eq!(bye, json!({ "type": "bye", "v": 1, "reason": "revoked" }));
        let error: Value = serde_json::from_str(&error(LiveErrorCode::RoomFull)).expect("json");
        assert_eq!(
            error,
            json!({ "type": "error", "v": 1, "code": "ROOM_FULL" })
        );
        let m: Value =
            serde_json::from_str(&mode(3, Mode::View, Some(Notice::EditorLimit))).expect("json");
        assert_eq!(
            m,
            json!({ "type": "mode", "v": 1, "seq": 3, "mode": "view", "notice": "editor_limit" })
        );
        let l: Value = serde_json::from_str(&leader(4, None)).expect("json");
        assert_eq!(
            l,
            json!({ "type": "leader", "v": 1, "seq": 4, "leader": null })
        );
    }
}
