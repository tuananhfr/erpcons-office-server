//! The JSON messages of the live channel (`docs/openapi.yaml`, "Live channel"). Every message has
//! `type` and `v`. Presence: the server sends `hello`, `presence`, `mode`, `leader`, `bye`, `error`;
//! a client sends `presence` and `bye`. Co-editing (M4): a client sends `lock.acquire`,
//! `lock.renew`, `lock.release`, `region.update`, `cursor` and `region.replay`; the server answers
//! `lock.granted`, `lock.denied`, `ack`, `region.replayed`, and sends `locks`, `region.update`,
//! `cursor`, `save.request` and `saved`.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{ByeReason, MemberId, Mode, Notice, PresenceState, region};

pub const VERSION: u32 = 1;

/// Longest client message id (`cid`), in bytes.
pub const MAX_CID_BYTES: usize = 128;
/// Longest cursor `range`, in bytes.
pub const MAX_RANGE_BYTES: usize = 1024;

/// `error.code` of the live channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LiveErrorCode {
    /// Not JSON, not an object with `type` and `v`, an unknown type, a missing or malformed field,
    /// or a binary frame.
    InvalidMessage,
    /// A `v` this server does not speak.
    UnsupportedVersion,
    /// A `lock.*` or `region.*` type this server does not know (reserved).
    Unsupported,
    /// Messages sent faster than allowed; the connection closes.
    RateLimited,
    /// The document has as many connections as it may.
    RoomFull,
    /// The client address has as many connections as it may.
    TooManyConnections,
    /// The server has as many connections as it may, or is shutting down.
    ServerBusy,
    /// The member's mode does not allow this (locks and updates need `edit`, cursors `comment`).
    ForbiddenMode,
    /// A `region.update` for a region the sender holds no lock on.
    LockRequired,
    /// The member already holds as many locks as it may.
    LockLimit,
    /// The room no longer holds every update after `since`: reload the saved content.
    ReplayGone,
}

impl LiveErrorCode {
    pub const ALL: [LiveErrorCode; 11] = [
        Self::InvalidMessage,
        Self::UnsupportedVersion,
        Self::Unsupported,
        Self::RateLimited,
        Self::RoomFull,
        Self::TooManyConnections,
        Self::ServerBusy,
        Self::ForbiddenMode,
        Self::LockRequired,
        Self::LockLimit,
        Self::ReplayGone,
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
    /// The same for every connection of one account in this room; absent like `name`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub person: Option<&'a str>,
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
    pub max_editors: usize,
    pub max_connections: usize,
    pub max_message_bytes: usize,
    pub max_region_message_bytes: usize,
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
    pub saved_seq: u64,
    pub saved_version: Option<i32>,
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

/// One entry of the lock table as members see it.
#[derive(Serialize)]
pub(crate) struct LockView {
    pub holder: MemberId,
    pub colour: &'static str,
}

#[derive(Serialize)]
struct Locks<'a> {
    seq: u64,
    locks: &'a BTreeMap<&'a str, LockView>,
}

pub(crate) fn locks(seq: u64, locks: &BTreeMap<&str, LockView>) -> String {
    encode("locks", Locks { seq, locks })
}

#[derive(Serialize)]
struct LockAnswer<'a> {
    region: &'a str,
    cid: Option<&'a str>,
    holder: MemberId,
    expires_ms: u128,
}

pub(crate) fn lock_granted(region: &str, cid: &str, holder: MemberId, expires_ms: u128) -> String {
    let body = LockAnswer {
        region,
        cid: Some(cid),
        holder,
        expires_ms,
    };
    encode("lock.granted", body)
}

/// `cid` is `None` when a renewal was refused.
pub(crate) fn lock_denied(
    region: &str,
    cid: Option<&str>,
    holder: MemberId,
    expires_ms: u128,
) -> String {
    let body = LockAnswer {
        region,
        cid,
        holder,
        expires_ms,
    };
    encode("lock.denied", body)
}

#[derive(Serialize)]
struct RegionUpdate<'a> {
    region: &'a str,
    author: MemberId,
    payload: &'a str,
}

/// A `region.update` without its closing brace; [`region_update_tail`] adds `seq`. Built before the
/// hub lock is taken, as escaping a large payload is the costly part.
pub(crate) fn region_update_head(region: &str, author: MemberId, payload: &str) -> String {
    let mut text = encode(
        "region.update",
        RegionUpdate {
            region,
            author,
            payload,
        },
    );
    text.pop();
    text
}

pub(crate) fn region_update_tail(seq: u64) -> String {
    format!(",\"seq\":{seq}}}")
}

#[derive(Serialize)]
struct Ack<'a> {
    cid: &'a str,
    seq: u64,
}

pub(crate) fn ack(cid: &str, seq: u64) -> String {
    encode("ack", Ack { cid, seq })
}

#[derive(Serialize)]
struct Cursor<'a> {
    author: MemberId,
    region: &'a str,
    range: &'a str,
}

pub(crate) fn cursor(author: MemberId, region: &str, range: &str) -> String {
    let body = Cursor {
        author,
        region,
        range,
    };
    encode("cursor", body)
}

#[derive(Serialize)]
struct Replayed {
    since: u64,
    seq: u64,
}

pub(crate) fn region_replayed(since: u64, seq: u64) -> String {
    encode("region.replayed", Replayed { since, seq })
}

#[derive(Serialize)]
struct Empty {}

pub(crate) fn save_request() -> String {
    encode("save.request", Empty {})
}

#[derive(Serialize)]
struct Saved {
    saved_seq: u64,
    version: i32,
}

pub(crate) fn saved(saved_seq: u64, version: i32) -> String {
    encode("saved", Saved { saved_seq, version })
}

#[derive(Serialize)]
struct Bye {
    reason: ByeReason,
}

pub fn bye(reason: ByeReason) -> String {
    encode("bye", Bye { reason })
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    code: LiveErrorCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    cid: Option<&'a str>,
}

pub fn error(code: LiveErrorCode) -> String {
    encode("error", ErrorBody { code, cid: None })
}

/// An error answering the client message `cid`.
pub(crate) fn error_for(code: LiveErrorCode, cid: &str) -> String {
    let body = ErrorBody {
        code,
        cid: Some(cid),
    };
    encode("error", body)
}

#[derive(Serialize)]
struct ReplayGone {
    code: LiveErrorCode,
    saved_seq: u64,
    saved_version: Option<i32>,
}

pub(crate) fn replay_gone(saved_seq: u64, saved_version: Option<i32>) -> String {
    let body = ReplayGone {
        code: LiveErrorCode::ReplayGone,
        saved_seq,
        saved_version,
    };
    encode("error", body)
}

/// What a client may send.
#[derive(Debug, PartialEq, Eq)]
pub enum ClientMessage {
    Presence(PresenceState),
    Bye,
    LockAcquire {
        region: String,
        cid: String,
    },
    LockRenew {
        region: String,
    },
    LockRelease {
        region: String,
    },
    RegionUpdate {
        region: String,
        cid: String,
        payload: String,
    },
    Cursor {
        region: String,
        range: String,
    },
    Replay {
        since: u64,
    },
}

#[derive(Deserialize)]
struct Raw {
    #[serde(rename = "type")]
    kind: String,
    v: u32,
    state: Option<PresenceState>,
    region: Option<String>,
    cid: Option<String>,
    payload: Option<String>,
    range: Option<String>,
    since: Option<u64>,
}

/// Parses one text message from a client.
pub fn parse(text: &str) -> Result<ClientMessage, LiveErrorCode> {
    use LiveErrorCode::InvalidMessage;
    let raw: Raw = serde_json::from_str(text).map_err(|_| InvalidMessage)?;
    if raw.v != VERSION {
        return Err(LiveErrorCode::UnsupportedVersion);
    }
    let Raw {
        kind,
        state,
        region,
        cid,
        payload,
        range,
        since,
        ..
    } = raw;
    let region = || region.filter(|r| region::valid(r)).ok_or(InvalidMessage);
    let cid = cid.filter(|c| valid_cid(c)).ok_or(InvalidMessage);
    match kind.as_str() {
        "presence" => state.map(ClientMessage::Presence).ok_or(InvalidMessage),
        "bye" => Ok(ClientMessage::Bye),
        "lock.acquire" => Ok(ClientMessage::LockAcquire {
            region: region()?,
            cid: cid?,
        }),
        "lock.renew" => Ok(ClientMessage::LockRenew { region: region()? }),
        "lock.release" => Ok(ClientMessage::LockRelease { region: region()? }),
        "region.update" => Ok(ClientMessage::RegionUpdate {
            region: region()?,
            cid: cid?,
            payload: payload.ok_or(InvalidMessage)?,
        }),
        "region.replay" => since
            .map(|since| ClientMessage::Replay { since })
            .ok_or(InvalidMessage),
        "cursor" => Ok(ClientMessage::Cursor {
            region: region()?,
            range: range
                .filter(|r| r.len() <= MAX_RANGE_BYTES)
                .ok_or(InvalidMessage)?,
        }),
        k if k.starts_with("lock.") || k.starts_with("region.") => Err(LiveErrorCode::Unsupported),
        _ => Err(InvalidMessage),
    }
}

/// A client message id: 1 to 128 visible ASCII characters.
fn valid_cid(cid: &str) -> bool {
    !cid.is_empty() && cid.len() <= MAX_CID_BYTES && cid.bytes().all(|b| b.is_ascii_graphic())
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
        assert_eq!(
            parse(r#"{"type":"lock.acquire","v":1,"region":"cell:s1:A1","cid":"c-1"}"#),
            Ok(ClientMessage::LockAcquire {
                region: "cell:s1:A1".into(),
                cid: "c-1".into()
            })
        );
        assert_eq!(
            parse(r#"{"type":"lock.renew","v":1,"region":"*"}"#),
            Ok(ClientMessage::LockRenew { region: "*".into() })
        );
        assert_eq!(
            parse(r#"{"type":"lock.release","v":1,"region":"pages"}"#),
            Ok(ClientMessage::LockRelease {
                region: "pages".into()
            })
        );
        assert_eq!(
            parse(r#"{"type":"region.update","v":1,"region":"para:1","cid":"c2","payload":""}"#),
            Ok(ClientMessage::RegionUpdate {
                region: "para:1".into(),
                cid: "c2".into(),
                payload: String::new()
            })
        );
        assert_eq!(
            parse(r#"{"type":"cursor","v":1,"region":"para:1","range":"3-5"}"#),
            Ok(ClientMessage::Cursor {
                region: "para:1".into(),
                range: "3-5".into()
            })
        );
        assert_eq!(
            parse(r#"{"type":"region.replay","v":1,"since":42}"#),
            Ok(ClientMessage::Replay { since: 42 })
        );
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
                r#"{"type":"lock.acquire","v":1,"region":"p1"}"#,
                LiveErrorCode::InvalidMessage,
            ),
            (
                r#"{"type":"lock.acquire","v":1,"region":"","cid":"c"}"#,
                LiveErrorCode::InvalidMessage,
            ),
            (
                r#"{"type":"lock.acquire","v":1,"region":"p1","cid":"a b"}"#,
                LiveErrorCode::InvalidMessage,
            ),
            (
                r#"{"type":"region.update","v":1,"region":"p1","cid":"c"}"#,
                LiveErrorCode::InvalidMessage,
            ),
            (
                r#"{"type":"region.update","v":1,"region":"p1","cid":"c","payload":7}"#,
                LiveErrorCode::InvalidMessage,
            ),
            (
                r#"{"type":"region.replay","v":1,"since":-1}"#,
                LiveErrorCode::InvalidMessage,
            ),
            (
                r#"{"type":"region.changed","v":1}"#,
                LiveErrorCode::Unsupported,
            ),
            (r#"{"type":"ack","v":1}"#, LiveErrorCode::InvalidMessage),
            (r#"{"type":"locks","v":1}"#, LiveErrorCode::InvalidMessage),
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
        let m1 = MemberId::new(1);
        let granted: Value =
            serde_json::from_str(&lock_granted("p1", "c1", m1, 5000)).expect("json");
        assert_eq!(
            granted,
            json!({ "type": "lock.granted", "v": 1, "region": "p1", "cid": "c1", "holder": "m1", "expires_ms": 5000 })
        );
        let denied: Value = serde_json::from_str(&lock_denied("p1", None, m1, 10)).expect("json");
        assert_eq!(denied["cid"], Value::Null);
        let gone: Value = serde_json::from_str(&replay_gone(9, Some(3))).expect("json");
        assert_eq!(
            gone,
            json!({ "type": "error", "v": 1, "code": "REPLAY_GONE", "saved_seq": 9, "saved_version": 3 })
        );
        let refused: Value =
            serde_json::from_str(&error_for(LiveErrorCode::LockRequired, "c9")).expect("json");
        assert_eq!(
            refused,
            json!({ "type": "error", "v": 1, "code": "LOCK_REQUIRED", "cid": "c9" })
        );
    }

    const OPENAPI: &str = include_str!("../../../docs/openapi.yaml");

    #[test]
    fn error_codes_match_the_openapi_contract() {
        let schema = OPENAPI
            .split("\n    LiveErrorCode:\n")
            .nth(1)
            .expect("LiveErrorCode schema in openapi.yaml");
        let list = schema.split("enum:\n").nth(1).expect("code enum");
        let documented: Vec<&str> = list
            .lines()
            .map_while(|l| l.trim().strip_prefix("- "))
            .collect();
        let ours: Vec<String> = LiveErrorCode::ALL
            .iter()
            .map(|c| serde_json::to_value(c).expect("serialize"))
            .map(|v| v.as_str().expect("string").to_owned())
            .collect();
        assert_eq!(ours, documented);
    }
}
