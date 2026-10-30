//! Notification mail (plan v2 §5): notices are queued in `mail_outbox` inside the transaction of the
//! change they announce, and a background job sends them over SMTP with retries. Without SMTP nothing
//! is queued and people learn of shares in the app only.
mod config;
mod notice;
mod outbox;
mod render;
mod transport;

pub use config::{
    DEFAULT_DAILY_CAP, DEFAULT_GLOBAL_DAILY_CAP, MailConfig, MailPolicy, PER_RECIPIENT_DAILY_CAP,
    SmtpConfig, TlsMode,
};
pub use notice::{KIND_SHARED, SharedNotice};
pub use outbox::{OutboxConfig, OutboxReport, run_outbox};
pub use render::{Rendered, render_shared};
pub use transport::{MailTransport, SendError, SmtpTransport, TransportError};
