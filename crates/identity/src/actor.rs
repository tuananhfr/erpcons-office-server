use std::fmt;

use time::OffsetDateTime;

/// The caller behind a verified token.
#[derive(Clone, PartialEq, Eq)]
pub struct Actor {
    /// PI `sub`; immutable, the owner key of everything the actor stores.
    pub actor_id: String,
    pub display_name: String,
    /// Until when this verification may be reused without asking PI again.
    pub expires_at: OffsetDateTime,
}

// The display name is personal data and must not reach logs through `{:?}`.
impl fmt::Debug for Actor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Actor")
            .field("actor_id", &self.actor_id)
            .field("display_name", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_hides_the_display_name() {
        let actor = Actor {
            actor_id: "usr_1".into(),
            display_name: "Nguyễn Văn An".into(),
            expires_at: OffsetDateTime::UNIX_EPOCH,
        };
        let shown = format!("{actor:?}");
        assert!(shown.contains("usr_1"));
        assert!(!shown.contains("Nguyễn"));
    }
}
