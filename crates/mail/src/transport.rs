use std::time::Duration;

use async_trait::async_trait;
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{Mailbox, header::ContentType},
    transport::smtp::{authentication::Credentials, extension::ClientId},
};
use tracing::warn;

use crate::{MailConfig, Rendered, TlsMode};

/// A whole SMTP exchange (connect, TLS, auth, data) gives up after this.
const SMTP_TIMEOUT: Duration = Duration::from_secs(30);

/// Why one mail was not sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SendError {
    /// Worth trying again later (connection, 4xx).
    #[error("temporary SMTP failure")]
    Transient,
    /// Trying again cannot help (5xx, a malformed address).
    #[error("permanent SMTP failure")]
    Permanent,
}

/// Sends one rendered mail. Implementations never log addresses or content.
#[async_trait]
pub trait MailTransport: Send + Sync {
    async fn send(&self, to: &str, mail: &Rendered) -> Result<(), SendError>;
}

/// The configuration cannot make a transport (bad `From:` or origin).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    #[error("ERPCONS_MAIL_FROM is not a mailbox")]
    From,
    #[error("ERPCONS_SMTP_HOST cannot be used for TLS")]
    Tls,
}

pub struct SmtpTransport {
    smtp: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

impl SmtpTransport {
    pub fn new(config: &MailConfig) -> Result<Self, TransportError> {
        let from: Mailbox = config.from.parse().map_err(|_| TransportError::From)?;
        let smtp = &config.smtp;
        let builder = match smtp.tls {
            TlsMode::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&smtp.host)
                .map_err(|_| TransportError::Tls)?,
            TlsMode::Starttls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&smtp.host)
                .map_err(|_| TransportError::Tls)?,
            TlsMode::None => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&smtp.host),
        };
        let mut builder = builder
            .port(smtp.port)
            .timeout(Some(SMTP_TIMEOUT))
            // Some relays refuse `EHLO localhost`; the site's host name is ours to use.
            .hello_name(ClientId::Domain(hello_name(&config.public_origin)));
        if let (Some(user), Some(password)) = (&smtp.username, &smtp.password) {
            builder = builder.credentials(Credentials::new(user.clone(), password.clone()));
        }
        Ok(Self {
            smtp: builder.build(),
            from,
        })
    }
}

fn hello_name(origin: &str) -> String {
    let host = origin
        .split_once("://")
        .map_or(origin, |(_, rest)| rest)
        .split(['/', ':'])
        .next()
        .unwrap_or_default();
    if host.is_empty() {
        "localhost".to_owned()
    } else {
        host.to_owned()
    }
}

#[async_trait]
impl MailTransport for SmtpTransport {
    async fn send(&self, to: &str, mail: &Rendered) -> Result<(), SendError> {
        let to: Mailbox = to.parse().map_err(|_| SendError::Permanent)?;
        let message = Message::builder()
            .from(self.from.clone())
            .to(to)
            .subject(mail.subject.clone())
            .header(ContentType::TEXT_PLAIN)
            .body(mail.body.clone())
            .map_err(|_| SendError::Permanent)?;
        match self.smtp.send(message).await {
            Ok(_) => Ok(()),
            Err(e) if e.is_permanent() => {
                warn!(
                    status = e.status().map(|c| c.to_string()),
                    "SMTP refused a mail"
                );
                Err(SendError::Permanent)
            }
            Err(e) => {
                warn!(
                    status = e.status().map(|c| c.to_string()),
                    timeout = e.is_timeout(),
                    "SMTP failed, will retry"
                );
                Err(SendError::Transient)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_name_is_the_origin_host() {
        assert_eq!(hello_name("https://office.erpcons.vn"), "office.erpcons.vn");
        assert_eq!(hello_name("http://127.0.0.1:3000/x"), "127.0.0.1");
        assert_eq!(hello_name(""), "localhost");
    }
}
