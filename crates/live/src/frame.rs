//! A protocol message on its way to members, shared by every member it goes to. Building one under
//! the hub lock costs a few pointer copies: a region update's payload is encoded before the lock
//! is taken and only its seq after, and a lock table is serialized by the first session sending it.
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};

use crate::{
    MemberId,
    protocol::{self, LockView},
};

#[derive(Clone)]
pub struct Frame(Inner);

#[derive(Clone)]
enum Inner {
    Text(Arc<str>),
    /// An encoded message without its closing brace, then the rest (`,"seq":N}`).
    Split(Arc<str>, Arc<str>),
    Locks(Arc<LocksSnapshot>),
}

/// The lock table as it stood at `seq`; regions are shared with the table, not copied.
pub(crate) struct LocksSnapshot {
    pub seq: u64,
    pub entries: Vec<(Arc<str>, MemberId, &'static str)>,
    text: OnceLock<String>,
}

impl LocksSnapshot {
    pub fn new(seq: u64, entries: Vec<(Arc<str>, MemberId, &'static str)>) -> Self {
        Self {
            seq,
            entries,
            text: OnceLock::new(),
        }
    }

    fn text(&self) -> &str {
        self.text.get_or_init(|| {
            let table: BTreeMap<&str, LockView> = self
                .entries
                .iter()
                .map(|(region, holder, colour)| {
                    let view = LockView {
                        holder: *holder,
                        colour,
                    };
                    (&**region, view)
                })
                .collect();
            protocol::locks(self.seq, &table)
        })
    }
}

impl Frame {
    pub(crate) fn split(head: Arc<str>, tail: String) -> Self {
        Self(Inner::Split(head, tail.into()))
    }

    pub(crate) fn locks(snapshot: LocksSnapshot) -> Self {
        Self(Inner::Locks(Arc::new(snapshot)))
    }

    /// The message as sent on the socket.
    pub fn to_text(&self) -> String {
        match &self.0 {
            Inner::Text(text) => String::from(&**text),
            Inner::Split(head, tail) => {
                let mut text = String::with_capacity(head.len() + tail.len());
                text.push_str(head);
                text.push_str(tail);
                text
            }
            Inner::Locks(snapshot) => snapshot.text().to_owned(),
        }
    }

    /// Bytes on the socket; what a log entry is charged. A lock table is never logged.
    pub(crate) fn len(&self) -> usize {
        match &self.0 {
            Inner::Text(text) => text.len(),
            Inner::Split(head, tail) => head.len() + tail.len(),
            Inner::Locks(snapshot) => snapshot.text().len(),
        }
    }
}

impl From<String> for Frame {
    fn from(text: String) -> Self {
        Self(Inner::Text(text.into()))
    }
}

impl From<&str> for Frame {
    fn from(text: &str) -> Self {
        Self(Inner::Text(text.into()))
    }
}
