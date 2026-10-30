//! PI's `POST /api/v1/service/accounts/register` (D515): PI creates the PI ID account for a verified
//! address, or reuses the account it already has, ticks Office for it and adopts the password hash
//! when the account has no password yet. Sending the same `localRef` again returns the first result.
use std::{fmt, time::Duration};

use async_trait::async_trait;
use reqwest::{StatusCode, redirect};
use serde::{Deserialize, Serialize};

const REGISTER_PATH: &str = "/api/v1/service/accounts/register";
const SERVICE_KEY_HEADER: &str = "X-PI-Service-Key";
/// What PI records as the origin of the account (`product:erpconsoffice:<source>`).
const SOURCE: &str = "office_signup";
const MAX_ANSWER_BYTES: usize = 64 * 1024;

/// PI's service key for Office (`ERPCONS_PI_SERVICE_KEY`). No `Debug` output of the key.
#[derive(Clone, PartialEq, Eq)]
pub struct ServiceKey(String);

impl ServiceKey {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }
}

impl fmt::Debug for ServiceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ServiceKey(<redacted>)")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PiRegistrarConfig {
    /// `PI_ISSUER`: PI's API lives on the same origin.
    pub issuer: String,
    pub service_key: ServiceKey,
    pub timeout: Duration,
}

/// One verified sign-up for PI. No `Debug`: personal data and a password hash.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterRequest<'a> {
    /// The sign-up's id: a repeat of the same sign-up is answered from PI's first result.
    pub local_ref: String,
    pub email: &'a str,
    pub display_name: &'a str,
    pub password_hash: &'a str,
    pub source: &'static str,
}

impl<'a> RegisterRequest<'a> {
    pub fn new(
        local_ref: String,
        email: &'a str,
        display_name: &'a str,
        password_hash: &'a str,
    ) -> Self {
        Self {
            local_ref,
            email,
            display_name,
            password_hash,
            source: SOURCE,
        }
    }
}

/// PI has the account and Office is ticked for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Registered {
    /// `password_adopted`: the PI ID password is now the one chosen at sign-up.
    Created { password_adopted: bool },
    /// The address already had a PI ID account.
    Existing { password_adopted: bool },
    /// The address's PI ID account is already tied to an earlier Office sign-up.
    LinkedElsewhere,
}

impl Registered {
    /// Whether the person must sign in with a PI ID password other than the one just chosen.
    pub fn keeps_old_password(self) -> bool {
        match self {
            Self::Created { .. } => false,
            Self::Existing { password_adopted } => !password_adopted,
            Self::LinkedElsewhere => true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RegisterError {
    /// PI answered for good (locked account, invalid data): trying again cannot help.
    #[error("PI refused the account ({0})")]
    Refused(&'static str),
    /// PI unreachable, overloaded, not deployed yet or refusing the service key: try again later.
    #[error("PI is unavailable for registration")]
    Transient,
}

#[async_trait]
pub trait PiRegistrar: Send + Sync {
    async fn register(&self, request: &RegisterRequest<'_>) -> Result<Registered, RegisterError>;
}

pub struct HttpPiRegistrar {
    client: reqwest::Client,
    url: String,
    key: ServiceKey,
    timeout: Duration,
}

#[derive(Debug, thiserror::Error)]
#[error("the HTTP client for PI could not be built")]
pub struct ClientError;

impl HttpPiRegistrar {
    pub fn new(config: PiRegistrarConfig) -> Result<Self, ClientError> {
        // reqwest is built without a crypto provider (workspace Cargo.toml); a no-op once installed.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .connect_timeout(config.timeout)
            // A redirect must never carry the service key anywhere else.
            .redirect(redirect::Policy::none())
            .user_agent(concat!("erpcons-office-server/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| ClientError)?;
        Ok(Self {
            client,
            url: format!("{}{REGISTER_PATH}", config.issuer.trim_end_matches('/')),
            key: config.service_key,
            timeout: config.timeout,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Answer {
    #[serde(default)]
    created: Option<bool>,
    #[serde(default)]
    password_adopted: bool,
    #[serde(default)]
    code: Option<String>,
}

async fn read_answer(mut response: reqwest::Response) -> Option<Answer> {
    if response
        .content_length()
        .is_some_and(|n| n > MAX_ANSWER_BYTES as u64)
    {
        return None;
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if body.len() + chunk.len() > MAX_ANSWER_BYTES {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).ok()
}

#[async_trait]
impl PiRegistrar for HttpPiRegistrar {
    async fn register(&self, request: &RegisterRequest<'_>) -> Result<Registered, RegisterError> {
        let response = self
            .client
            .post(&self.url)
            .timeout(self.timeout)
            .header(SERVICE_KEY_HEADER, &self.key.0)
            .json(request)
            .send()
            .await
            .map_err(|e| {
                tracing::warn!(timeout = e.is_timeout(), "PI register unreachable");
                RegisterError::Transient
            })?;
        let status = response.status();
        let answer = read_answer(response).await;
        match status {
            StatusCode::OK | StatusCode::CREATED => {
                let answer = answer.ok_or_else(|| {
                    tracing::warn!("PI register answered an unreadable body");
                    RegisterError::Transient
                })?;
                let password_adopted = answer.password_adopted;
                if answer.created.unwrap_or(status == StatusCode::CREATED) {
                    Ok(Registered::Created { password_adopted })
                } else {
                    Ok(Registered::Existing { password_adopted })
                }
            }
            StatusCode::CONFLICT => match answer.and_then(|a| a.code).as_deref() {
                Some("ACCOUNT_LINKED_ELSEWHERE") => Ok(Registered::LinkedElsewhere),
                Some("ACCOUNT_LOCKED") => Err(RegisterError::Refused("account locked")),
                // `localRef` is the sign-up's own id, so PI should never have it on another account.
                Some("LOCAL_REF_TAKEN") => {
                    tracing::error!("PI says the sign-up's localRef is on another account");
                    Err(RegisterError::Refused("local ref taken"))
                }
                _ => Err(RegisterError::Refused("conflict")),
            },
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
                Err(RegisterError::Refused("invalid"))
            }
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                tracing::error!(status = status.as_u16(), "PI refused Office's service key");
                Err(RegisterError::Transient)
            }
            _ => {
                tracing::warn!(status = status.as_u16(), "PI register failed, will retry");
                Err(RegisterError::Transient)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_never_prints() {
        assert!(!format!("{:?}", ServiceKey::new("s3cret")).contains("s3cret"));
    }

    #[test]
    fn only_a_reused_account_keeps_its_password() {
        assert!(
            !Registered::Created {
                password_adopted: true
            }
            .keeps_old_password()
        );
        assert!(
            !Registered::Existing {
                password_adopted: true
            }
            .keeps_old_password()
        );
        assert!(
            Registered::Existing {
                password_adopted: false
            }
            .keeps_old_password()
        );
        assert!(Registered::LinkedElsewhere.keeps_old_password());
    }

    #[test]
    fn the_body_is_what_pi_reads() {
        let r = RegisterRequest::new("id-1".into(), "an@x.vn", "An", "$2y$12$h");
        let json = serde_json::to_value(&r).expect("json");
        assert_eq!(
            json,
            serde_json::json!({"localRef": "id-1", "email": "an@x.vn", "displayName": "An",
                "passwordHash": "$2y$12$h", "source": "office_signup"})
        );
    }
}
