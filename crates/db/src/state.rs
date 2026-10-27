use std::fmt;

/// `state` of folders and documents (spec §07). Folders only use `Active` and `Trashed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemState {
    Draft,
    Active,
    Trashed,
    PurgePending,
    Purged,
}

impl ItemState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "DRAFT",
            Self::Active => "ACTIVE",
            Self::Trashed => "TRASHED",
            Self::PurgePending => "PURGE_PENDING",
            Self::Purged => "PURGED",
        }
    }
}

#[derive(Debug)]
pub struct UnknownState;

impl fmt::Display for UnknownState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unknown item state")
    }
}

impl std::error::Error for UnknownState {}

impl TryFrom<String> for ItemState {
    type Error = UnknownState;

    fn try_from(s: String) -> Result<Self, UnknownState> {
        Ok(match s.as_str() {
            "DRAFT" => Self::Draft,
            "ACTIVE" => Self::Active,
            "TRASHED" => Self::Trashed,
            "PURGE_PENDING" => Self::PurgePending,
            "PURGED" => Self::Purged,
            _ => return Err(UnknownState),
        })
    }
}
