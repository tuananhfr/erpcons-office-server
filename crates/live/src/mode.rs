use serde::Serialize;

/// How a member may use the document in the room; the shell passes it to the editor's `setMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Edit,
    Comment,
    View,
}

/// Why a member got less than its role allows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Notice {
    /// The room already has as many editors as it may; edit mode comes when one leaves.
    EditorLimit,
}
