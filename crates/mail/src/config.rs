/// Invite mails one actor may cause per rolling 24 hours, unless configured otherwise.
pub const DEFAULT_DAILY_CAP: u32 = 100;
/// Invite mails the whole server may queue per rolling 24 hours, unless configured otherwise.
pub const DEFAULT_GLOBAL_DAILY_CAP: u32 = 2000;
/// Invite mails one address may receive per rolling 24 hours, from all senders together.
pub const PER_RECIPIENT_DAILY_CAP: u32 = 10;

/// What the document service needs to know when it queues a notice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MailPolicy {
    /// SMTP is configured; otherwise nothing is queued.
    pub enabled: bool,
    /// Put the document title into the mail (off by default: mail leaves the server unencrypted at rest).
    pub include_title: bool,
    /// Keeps one account from turning the server into a spam relay; sharing itself is not capped.
    pub daily_cap: u32,
    /// Bounds what many accounts together can send, protecting the sending domain's reputation.
    pub global_daily_cap: u32,
}

impl Default for MailPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            include_title: false,
            daily_cap: DEFAULT_DAILY_CAP,
            global_daily_cap: DEFAULT_GLOBAL_DAILY_CAP,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TlsMode {
    /// Plain connection upgraded with STARTTLS (port 587); refused when the server does not offer it.
    Starttls,
    /// TLS from the first byte (port 465).
    Tls,
    /// No encryption: a relay on the same host or a test sink only.
    None,
}

impl TlsMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "starttls" => Some(Self::Starttls),
            "tls" => Some(Self::Tls),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Self::Starttls => 587,
            Self::Tls => 465,
            Self::None => 25,
        }
    }
}

/// SMTP server. No `Debug`: holds the password.
#[derive(Clone, PartialEq, Eq)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub tls: TlsMode,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// Everything the outbox job needs. No `Debug`: holds the SMTP password.
#[derive(Clone, PartialEq, Eq)]
pub struct MailConfig {
    pub smtp: SmtpConfig,
    /// `From:` of every mail, e.g. `ERPCons Office <no-reply@erpcons.vn>`.
    pub from: String,
    /// The site's origin (`https://office.erpcons.vn`), for links in mail.
    pub public_origin: String,
    pub include_title: bool,
    pub daily_cap: u32,
    pub global_daily_cap: u32,
}

impl MailConfig {
    pub fn policy(&self) -> MailPolicy {
        MailPolicy {
            enabled: true,
            include_title: self.include_title,
            daily_cap: self.daily_cap,
            global_daily_cap: self.global_daily_cap,
        }
    }
}
