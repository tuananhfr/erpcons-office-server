//! Opaque page cursors: base64url of a small versioned text, never a name or title.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::DocError;

/// Where a folder listing resumes: after this folder, or after this document (folders done).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildrenCursor {
    Folder(Uuid),
    Document(Uuid),
}

/// Position in the trash order (newest first; at equal times folders, rank 0, before documents).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrashCursor {
    pub trashed_at: OffsetDateTime,
    pub rank: i16,
    pub id: Uuid,
}

fn encode(text: &str) -> String {
    URL_SAFE_NO_PAD.encode(text)
}

fn decode(cursor: &str) -> Result<String, DocError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| DocError::InvalidRequest)?;
    String::from_utf8(bytes).map_err(|_| DocError::InvalidRequest)
}

fn uuid(s: &str) -> Result<Uuid, DocError> {
    Uuid::parse_str(s).map_err(|_| DocError::InvalidRequest)
}

impl ChildrenCursor {
    pub fn encode(self) -> String {
        match self {
            Self::Folder(id) => encode(&format!("c1:f:{id}")),
            Self::Document(id) => encode(&format!("c1:d:{id}")),
        }
    }

    pub fn decode(cursor: &str) -> Result<Self, DocError> {
        let text = decode(cursor)?;
        match text.split(':').collect::<Vec<_>>().as_slice() {
            ["c1", "f", id] => Ok(Self::Folder(uuid(id)?)),
            ["c1", "d", id] => Ok(Self::Document(uuid(id)?)),
            _ => Err(DocError::InvalidRequest),
        }
    }
}

impl TrashCursor {
    pub fn encode(self) -> String {
        let micros = self.trashed_at.unix_timestamp_nanos() / 1000;
        encode(&format!("t1:{micros}:{}:{}", self.rank, self.id))
    }

    pub fn decode(cursor: &str) -> Result<Self, DocError> {
        let text = decode(cursor)?;
        let (micros, rank, id) = match text.split(':').collect::<Vec<_>>().as_slice() {
            ["t1", micros, rank, id] => (*micros, *rank, *id),
            _ => return Err(DocError::InvalidRequest),
        };
        let micros: i128 = micros.parse().map_err(|_| DocError::InvalidRequest)?;
        let nanos = micros.checked_mul(1000).ok_or(DocError::InvalidRequest)?;
        let trashed_at = OffsetDateTime::from_unix_timestamp_nanos(nanos)
            .map_err(|_| DocError::InvalidRequest)?;
        let rank = match rank {
            "0" => 0,
            "1" => 1,
            _ => return Err(DocError::InvalidRequest),
        };
        Ok(Self {
            trashed_at,
            rank,
            id: uuid(id)?,
        })
    }
}

/// Versions resume below this `seq`.
pub fn encode_version(seq: i32) -> String {
    encode(&format!("v1:{seq}"))
}

pub fn decode_version(cursor: &str) -> Result<i32, DocError> {
    let text = decode(cursor)?;
    text.strip_prefix("v1:")
        .and_then(|s| s.parse().ok())
        .filter(|&seq: &i32| seq >= 1)
        .ok_or(DocError::InvalidRequest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip_and_stay_short() {
        let id = Uuid::now_v7();
        for c in [ChildrenCursor::Folder(id), ChildrenCursor::Document(id)] {
            assert_eq!(ChildrenCursor::decode(&c.encode()), Ok(c));
            assert!(c.encode().len() <= 200);
        }
        let t = TrashCursor {
            trashed_at: OffsetDateTime::from_unix_timestamp_nanos(1_761_000_000_123_456_000)
                .expect("time"),
            rank: 1,
            id,
        };
        assert_eq!(TrashCursor::decode(&t.encode()), Ok(t));
        assert_eq!(decode_version(&encode_version(7)), Ok(7));
    }

    #[test]
    fn garbage_is_an_invalid_request() {
        for bad in ["", "!!", "dDE6eA", &encode("c1:x:1"), &encode("v1:0")] {
            assert_eq!(ChildrenCursor::decode(bad), Err(DocError::InvalidRequest));
            assert_eq!(TrashCursor::decode(bad), Err(DocError::InvalidRequest));
            assert_eq!(decode_version(bad), Err(DocError::InvalidRequest));
        }
    }
}
