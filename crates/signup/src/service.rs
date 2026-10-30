use std::sync::Arc;

use async_trait::async_trait;
use office_db::{PgPool, locks, mail_outbox, signups};
use office_mail::{IssuedToken, KIND_SIGNUP_VERIFY, SendError, SignupNotice, SignupTokens};
use tokio::sync::Notify;
use uuid::Uuid;

use crate::{
    PER_ADDRESS_DAILY_CAP, SignupError, SignupPolicy, SignupRequest, TOKEN_TTL, hash_password,
    input::validate, token,
};

const DAY_SECS: i64 = 24 * 3600;

/// The sign-up service behind `/signup`; also issues the tokens of its verification mails.
pub struct Signups {
    db: PgPool,
    policy: SignupPolicy,
    /// Wakes the PI job as soon as an address is verified, so the account exists within seconds.
    verified: Arc<Notify>,
}

impl Signups {
    pub fn new(db: PgPool, policy: SignupPolicy) -> Self {
        Self {
            db,
            policy,
            verified: Arc::new(Notify::new()),
        }
    }

    pub fn is_open(&self) -> bool {
        self.policy.enabled
    }

    /// Notified after each verification; the PI job waits on it between rounds.
    pub fn verified_signal(&self) -> Arc<Notify> {
        self.verified.clone()
    }

    pub fn db(&self) -> &PgPool {
        &self.db
    }

    /// Accepts a sign-up and queues its verification mail. Every accepted request gets the same
    /// answer: an address already pending is updated, one already verified or past its daily mails
    /// is left alone, and a bot's request does nothing.
    pub async fn submit(&self, request: &SignupRequest) -> Result<(), SignupError> {
        if !self.policy.enabled {
            return Err(SignupError::Closed);
        }
        if request.is_bot() {
            return Ok(());
        }
        let valid = validate(request)?;
        // Hashed before any lookup, so the answer takes as long whatever the address's state.
        let password_hash = hash_password(valid.password)
            .await
            .map_err(|_| SignupError::Unavailable)?;
        let mut tx = self.db.begin().await?;
        locks::signup_caps(&mut tx).await?;
        let today =
            mail_outbox::queued_of_kind_since(&mut *tx, KIND_SIGNUP_VERIFY, DAY_SECS).await?;
        if today >= i64::from(self.policy.daily_cap) {
            tracing::warn!(cap = self.policy.daily_cap, "daily sign-up cap reached");
            return Err(SignupError::DailyLimit);
        }
        let email = valid.email.as_str();
        let mails =
            mail_outbox::received_of_kind_since(&mut *tx, email, KIND_SIGNUP_VERIFY, DAY_SECS)
                .await?;
        if mails >= PER_ADDRESS_DAILY_CAP {
            return Ok(());
        }
        let signup = signups::NewSignup {
            id: Uuid::now_v7(),
            email,
            display_name: &valid.name,
            password_hash: &password_hash,
        };
        if let Some(id) = signups::submit(&mut tx, &signup).await? {
            let payload = SignupNotice { signup_id: id }.to_json();
            let mail = mail_outbox::NewMail {
                kind: KIND_SIGNUP_VERIFY,
                sender_id: None,
                to_email: email,
                payload: &payload,
            };
            mail_outbox::enqueue(&mut tx, &mail).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Uses a verification token once; the sign-up then waits for the PI job.
    pub async fn verify(&self, token: &str) -> Result<(), SignupError> {
        let hash = token::hash(token).ok_or(SignupError::InvalidToken)?;
        signups::verify(&self.db, &hash)
            .await?
            .ok_or(SignupError::InvalidToken)?;
        self.verified.notify_one();
        Ok(())
    }
}

#[async_trait]
impl SignupTokens for Signups {
    async fn issue(&self, signup_id: Uuid) -> Result<Option<IssuedToken>, SendError> {
        let token = token::generate().ok_or(SendError::Transient)?;
        let hash = token::hash(&token).ok_or(SendError::Transient)?;
        let ttl = i64::try_from(TOKEN_TTL.as_secs()).unwrap_or(i64::MAX);
        let name = signups::issue_token(&self.db, signup_id, &hash, ttl)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "sign-up token not stored");
                SendError::Transient
            })?;
        Ok(name.map(|display_name| IssuedToken {
            display_name,
            token,
        }))
    }
}
