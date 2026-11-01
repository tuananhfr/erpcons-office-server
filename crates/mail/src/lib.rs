//! Notification mail (plan v2 §5): notices are queued in `mail_outbox` inside the transaction of the
//! change they announce, and a background job sends them over SMTP with retries. Without SMTP nothing
//! is queued and people learn of shares in the app only. Sign-ups send their verification link and
//! the "already had a PI ID account" notice through the same outbox, and comment mails (replies,
//! and new threads batched per document) theirs, checked again with the document service when due.
mod comment_mails;
mod config;
mod notice;
mod outbox;
mod render;
mod render_comment;
mod render_signup;
mod signup_tokens;
mod transport;

pub use comment_mails::CommentMails;
pub use config::{
    DEFAULT_DAILY_CAP, DEFAULT_GLOBAL_DAILY_CAP, MailConfig, MailPolicy, PER_RECIPIENT_DAILY_CAP,
    SmtpConfig, TlsMode,
};
pub use notice::{
    CommentNotice, KIND_COMMENT_REPLY, KIND_COMMENT_THREADS, KIND_SHARED, KIND_SIGNUP_EXISTING,
    KIND_SIGNUP_VERIFY, SharedNotice, SignupNotice,
};
pub use outbox::{MailSources, OutboxConfig, OutboxReport, run_outbox};
pub use render::{Rendered, render_shared};
pub use render_comment::{comment_link, render_comment_reply, render_comment_threads};
pub use render_signup::{
    VERIFY_TTL_HOURS, render_signup_existing, render_signup_verify, verify_link,
};
pub use signup_tokens::{IssuedToken, SignupTokens};
pub use transport::{MailTransport, SendError, SmtpTransport, TransportError};
