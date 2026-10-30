use std::fmt;

use time::OffsetDateTime;

/// The actor's email as PI's userinfo last reported it.
#[derive(Clone, Default, PartialEq, Eq)]
pub enum EmailClaim {
    /// PI has not answered yet, or answered without any email claim (no `email` scope): nothing
    /// can be concluded, so a recorded email is kept.
    #[default]
    Unknown,
    /// PI answered with no email or one it does not mark verified.
    Unverified,
    /// Verified by PI, as PI sent it (trimmed).
    Verified(String),
}

impl EmailClaim {
    pub fn verified(&self) -> Option<&str> {
        match self {
            Self::Verified(email) => Some(email),
            Self::Unknown | Self::Unverified => None,
        }
    }
}

// The address is personal data: `{:?}` shows only which case it is.
impl fmt::Debug for EmailClaim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unknown => "Unknown",
            Self::Unverified => "Unverified",
            Self::Verified(_) => "Verified(<redacted>)",
        })
    }
}

/// The caller behind a verified token.
#[derive(Clone, PartialEq, Eq)]
pub struct Actor {
    /// PI `sub`; immutable, the owner key of everything the actor stores.
    pub actor_id: String,
    pub display_name: String,
    /// What PI's userinfo last said about the email.
    pub email: EmailClaim,
    /// Until when this verification may be reused without checking the token again; never past
    /// the token's `exp`.
    pub expires_at: OffsetDateTime,
}

// The display name and email are personal data and must not reach logs through `{:?}`.
impl fmt::Debug for Actor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Actor")
            .field("actor_id", &self.actor_id)
            .field("display_name", &"<redacted>")
            .field("email", &self.email)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_hides_the_display_name_and_email() {
        let actor = Actor {
            actor_id: "usr_1".into(),
            display_name: "Nguyễn Văn An".into(),
            email: EmailClaim::Verified("an@example.vn".into()),
            expires_at: OffsetDateTime::UNIX_EPOCH,
        };
        let shown = format!("{actor:?}");
        assert!(shown.contains("usr_1"));
        assert!(!shown.contains("Nguyễn"));
        assert!(!shown.contains("example"));
    }
}
