//! The sign-up service and its jobs against a real Postgres (`TEST_DATABASE_URL`), with PI replaced
//! by a scripted registrar.
use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;
use office_db::testing::TestDb;
use office_mail::{KIND_SIGNUP_EXISTING, KIND_SIGNUP_VERIFY, SignupTokens};
use office_signup::{
    PiRegistrar, PushConfig, RegisterError, RegisterRequest, Registered, SignupError, SignupPolicy,
    SignupRequest, Signups, expire, run_push,
};
use uuid::Uuid;

const PASSWORD: &str = "mật khẩu canary 7c21";

fn open(cap: u32) -> SignupPolicy {
    SignupPolicy {
        enabled: true,
        daily_cap: cap,
    }
}

fn request(name: &str, email: &str) -> SignupRequest {
    SignupRequest {
        name: name.to_owned(),
        email: email.to_owned(),
        password: PASSWORD.to_owned(),
        website: None,
    }
}

async fn setup(cap: u32) -> Option<(TestDb, Signups)> {
    let db = TestDb::create().await?;
    let signups = Signups::new(db.pool().clone(), open(cap));
    Some((db, signups))
}

async fn count(db: &TestDb, sql: &str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(db.pool())
        .await
        .expect("count")
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    state: String,
    display_name: String,
    password_hash: Option<String>,
    token_sha256: Option<String>,
}

async fn row(db: &TestDb, email: &str) -> Row {
    sqlx::query_as(
        "SELECT id, state, display_name, password_hash, token_sha256 FROM signups WHERE email = $1",
    )
    .bind(email)
    .fetch_one(db.pool())
    .await
    .expect("sign-up row")
}

/// Sends the pending mail of `id`: the token the outbox job would put in the link.
async fn mail_token(signups: &Signups, id: Uuid) -> String {
    signups
        .issue(id)
        .await
        .expect("issue")
        .expect("pending")
        .token
}

#[tokio::test]
async fn a_sign_up_is_stored_hashed_and_queues_one_mail_without_a_token() {
    let Some((db, signups)) = setup(100).await else {
        return;
    };
    signups
        .submit(&request(" Nguyễn Văn An ", " An@Erpcons.Test "))
        .await
        .expect("accepted");
    let r = row(&db, "an@erpcons.test").await;
    assert_eq!(r.state, "pending");
    assert_eq!(r.display_name, "Nguyễn Văn An");
    let hash = r.password_hash.expect("hash");
    assert!(hash.starts_with("$2y$12$"));
    assert!(bcrypt::verify(PASSWORD, &hash).expect("verify"));
    assert!(
        r.token_sha256.is_none(),
        "the token is issued when the mail goes"
    );
    let payloads: Vec<String> =
        sqlx::query_scalar("SELECT payload::text FROM mail_outbox WHERE kind = $1")
            .bind(KIND_SIGNUP_VERIFY)
            .fetch_all(db.pool())
            .await
            .expect("payloads");
    assert_eq!(payloads, [format!("{{\"signup_id\": \"{}\"}}", r.id)]);
    let dump = count(
        &db,
        "SELECT count(*) FROM signups s WHERE row_to_json(s)::text LIKE '%canary%'",
    )
    .await;
    assert_eq!(dump, 0, "no plaintext password stored");
}

#[tokio::test]
async fn a_verified_link_works_once_and_only_the_latest_one_works() {
    let Some((db, signups)) = setup(100).await else {
        return;
    };
    signups
        .submit(&request("An", "an@erpcons.test"))
        .await
        .expect("accepted");
    let id = row(&db, "an@erpcons.test").await.id;
    let first = mail_token(&signups, id).await;
    // A resend (the outbox retrying, or a second submission) voids the earlier link.
    let second = mail_token(&signups, id).await;
    assert_eq!(signups.verify(&first).await, Err(SignupError::InvalidToken));
    assert_eq!(
        signups.verify("not-a-token").await,
        Err(SignupError::InvalidToken)
    );
    signups.verify(&second).await.expect("verified");
    let r = row(&db, "an@erpcons.test").await;
    assert_eq!(r.state, "verified");
    assert!(r.token_sha256.is_none());
    assert_eq!(
        signups.verify(&second).await,
        Err(SignupError::InvalidToken)
    );
    // A verified sign-up issues no new token: its mail would be dropped.
    assert!(signups.issue(id).await.expect("issue").is_none());
}

#[tokio::test]
async fn an_expired_link_is_refused() {
    let Some((db, signups)) = setup(100).await else {
        return;
    };
    signups
        .submit(&request("An", "an@erpcons.test"))
        .await
        .expect("accepted");
    let id = row(&db, "an@erpcons.test").await.id;
    let token = mail_token(&signups, id).await;
    sqlx::query("UPDATE signups SET token_expires_at = now() - interval '1 second'")
        .execute(db.pool())
        .await
        .expect("expire");
    assert_eq!(signups.verify(&token).await, Err(SignupError::InvalidToken));
    assert_eq!(row(&db, "an@erpcons.test").await.state, "pending");
}

#[tokio::test]
async fn submitting_again_updates_a_pending_sign_up_but_never_a_verified_one() {
    let Some((db, signups)) = setup(100).await else {
        return;
    };
    signups
        .submit(&request("An", "an@erpcons.test"))
        .await
        .expect("accepted");
    let before = row(&db, "an@erpcons.test").await;
    let token = mail_token(&signups, before.id).await;
    signups
        .submit(&request("An mới", "AN@erpcons.test"))
        .await
        .expect("accepted");
    let after = row(&db, "an@erpcons.test").await;
    assert_eq!(after.id, before.id, "one open sign-up per address");
    assert_eq!(after.display_name, "An mới");
    assert_ne!(after.password_hash, before.password_hash);
    assert_eq!(signups.verify(&token).await, Err(SignupError::InvalidToken));
    assert_eq!(count(&db, "SELECT count(*) FROM mail_outbox").await, 2);

    let token = mail_token(&signups, after.id).await;
    signups.verify(&token).await.expect("verified");
    let verified = row(&db, "an@erpcons.test").await;
    signups
        .submit(&request("Kẻ khác", "an@erpcons.test"))
        .await
        .expect("same answer");
    let unchanged = row(&db, "an@erpcons.test").await;
    assert_eq!(unchanged.state, "verified");
    assert_eq!(unchanged.display_name, "An mới");
    assert_eq!(unchanged.password_hash, verified.password_hash);
    assert_eq!(
        count(&db, "SELECT count(*) FROM mail_outbox").await,
        2,
        "no mail"
    );
}

#[tokio::test]
async fn caps_bots_and_a_closed_server() {
    let Some((db, signups)) = setup(4).await else {
        return;
    };
    for _ in 0..4 {
        signups
            .submit(&request("An", "an@erpcons.test"))
            .await
            .expect("accepted");
    }
    assert_eq!(
        count(&db, "SELECT count(*) FROM mail_outbox").await,
        3,
        "an address gets 3 verification mails a day; the 4th answer is the same but sends none"
    );
    signups
        .submit(&request("Bình", "binh@erpcons.test"))
        .await
        .expect("accepted");
    assert_eq!(
        signups.submit(&request("Chí", "chi@erpcons.test")).await,
        Err(SignupError::DailyLimit)
    );
    assert_eq!(count(&db, "SELECT count(*) FROM signups").await, 2);

    let mut bot = request("Bot", "bot@erpcons.test");
    bot.website = Some("http://spam.example".to_owned());
    let Some((db2, fresh)) = setup(100).await else {
        return;
    };
    fresh
        .submit(&bot)
        .await
        .expect("a bot hears the usual answer");
    assert_eq!(count(&db2, "SELECT count(*) FROM signups").await, 0);
    assert_eq!(
        fresh.submit(&request("", "x@erpcons.test")).await,
        Err(SignupError::Invalid(office_signup::Field::Name))
    );
    let closed = Signups::new(db2.pool().clone(), SignupPolicy::default());
    assert!(!closed.is_open());
    assert_eq!(
        closed.submit(&request("An", "an@erpcons.test")).await,
        Err(SignupError::Closed)
    );
}

/// Local ref, address, name and hash PI received.
type Seen = (String, String, String, String);

/// Answers each `register` call with the next scripted outcome; records what PI received.
struct ScriptedPi {
    answers: Mutex<Vec<Result<Registered, RegisterError>>>,
    seen: Mutex<Vec<Seen>>,
}

impl ScriptedPi {
    fn new(answers: Vec<Result<Registered, RegisterError>>) -> Self {
        Self {
            answers: Mutex::new(answers),
            seen: Mutex::default(),
        }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl PiRegistrar for ScriptedPi {
    async fn register(&self, r: &RegisterRequest<'_>) -> Result<Registered, RegisterError> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((
                r.local_ref.clone(),
                r.email.to_owned(),
                r.display_name.to_owned(),
                r.password_hash.to_owned(),
            ));
        let mut answers = self.answers.lock().unwrap_or_else(PoisonError::into_inner);
        if answers.is_empty() {
            Err(RegisterError::Transient)
        } else {
            answers.remove(0)
        }
    }
}

async fn verified(db: &TestDb, signups: &Signups, name: &str, email: &str) -> Uuid {
    signups
        .submit(&request(name, email))
        .await
        .expect("accepted");
    let id = row(db, email).await.id;
    let token = mail_token(signups, id).await;
    signups.verify(&token).await.expect("verified");
    id
}

async fn make_due(db: &TestDb) {
    sqlx::query("UPDATE signups SET push_next_at = now() WHERE state = 'verified'")
        .execute(db.pool())
        .await
        .expect("due");
}

async fn existing_mails(db: &TestDb) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM mail_outbox WHERE kind = $1")
        .bind(KIND_SIGNUP_EXISTING)
        .fetch_one(db.pool())
        .await
        .expect("count")
}

#[tokio::test]
async fn pi_down_is_retried_and_the_hash_dropped_once_pi_has_the_account() {
    let Some((db, signups)) = setup(100).await else {
        return;
    };
    let id = verified(&db, &signups, "Nguyễn Văn An", "an@erpcons.test").await;
    let hash = row(&db, "an@erpcons.test")
        .await
        .password_hash
        .expect("hash");
    let pi = ScriptedPi::new(vec![
        Err(RegisterError::Transient),
        Ok(Registered::Created {
            password_adopted: true,
        }),
    ]);
    let config = PushConfig::default();

    let down = run_push(db.pool(), &pi, &config).await.expect("round");
    assert_eq!((down.retried, down.created), (1, 0));
    let waiting = row(&db, "an@erpcons.test").await;
    assert_eq!(waiting.state, "verified");
    assert!(waiting.password_hash.is_some(), "kept until PI has it");
    let idle = run_push(db.pool(), &pi, &config).await.expect("round");
    assert_eq!(idle, office_signup::PushReport::default(), "backed off");

    make_due(&db).await;
    let up = run_push(db.pool(), &pi, &config).await.expect("round");
    assert_eq!(up.created, 1);
    let done = row(&db, "an@erpcons.test").await;
    assert_eq!(done.state, "registered");
    assert!(
        done.password_hash.is_none(),
        "no password kept after PI accepted"
    );
    assert_eq!(existing_mails(&db).await, 0);
    let seen = pi.seen();
    assert_eq!(seen.len(), 2);
    assert!(seen.iter().all(|s| s
        == &(
            id.to_string(),
            "an@erpcons.test".to_owned(),
            "Nguyễn Văn An".to_owned(),
            hash.clone()
        )));
    // Finished, the address may sign up again.
    signups
        .submit(&request("An", "an@erpcons.test"))
        .await
        .expect("accepted");
    assert_eq!(
        count(&db, "SELECT count(*) FROM signups WHERE state = 'pending'").await,
        1
    );
}

#[tokio::test]
async fn an_address_that_already_had_a_pi_id_is_told_to_use_its_password() {
    let Some((db, signups)) = setup(100).await else {
        return;
    };
    verified(&db, &signups, "An", "an@erpcons.test").await;
    verified(&db, &signups, "Bình", "binh@erpcons.test").await;
    verified(&db, &signups, "Chí", "chi@erpcons.test").await;
    let pi = ScriptedPi::new(vec![
        Ok(Registered::Existing {
            password_adopted: false,
        }),
        Ok(Registered::LinkedElsewhere),
        Ok(Registered::Existing {
            password_adopted: true,
        }),
    ]);
    let report = run_push(db.pool(), &pi, &PushConfig::default())
        .await
        .expect("round");
    assert_eq!((report.existing, report.failed), (3, 0));
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM signups WHERE state = 'registered' AND password_hash IS NULL"
        )
        .await,
        3
    );
    assert_eq!(
        existing_mails(&db).await,
        2,
        "not when PI took the new password"
    );
}

#[tokio::test]
async fn a_refused_or_long_unanswered_sign_up_is_given_up_without_its_hash() {
    let Some((db, signups)) = setup(100).await else {
        return;
    };
    verified(&db, &signups, "An", "an@erpcons.test").await;
    let pi = ScriptedPi::new(vec![Err(RegisterError::Refused("account locked"))]);
    let report = run_push(db.pool(), &pi, &PushConfig::default())
        .await
        .expect("round");
    assert_eq!(report.failed, 1);
    let r = row(&db, "an@erpcons.test").await;
    assert_eq!(r.state, "failed");
    assert!(r.password_hash.is_none());

    verified(&db, &signups, "Bình", "binh@erpcons.test").await;
    sqlx::query(
        "UPDATE signups SET verified_at = now() - interval '8 days' WHERE state = 'verified'",
    )
    .execute(db.pool())
    .await
    .expect("age");
    let down = ScriptedPi::new(vec![]);
    let report = run_push(db.pool(), &down, &PushConfig::default())
        .await
        .expect("round");
    assert_eq!((report.retried, report.failed), (0, 1));
    assert_eq!(row(&db, "binh@erpcons.test").await.state, "failed");
}

#[tokio::test]
async fn unverified_and_finished_sign_ups_expire_after_seven_days() {
    let Some((db, signups)) = setup(100).await else {
        return;
    };
    signups
        .submit(&request("Cũ", "old@erpcons.test"))
        .await
        .expect("accepted");
    signups
        .submit(&request("Mới", "new@erpcons.test"))
        .await
        .expect("accepted");
    verified(&db, &signups, "Xong", "done@erpcons.test").await;
    let pi = ScriptedPi::new(vec![Ok(Registered::Created {
        password_adopted: true,
    })]);
    run_push(db.pool(), &pi, &PushConfig::default())
        .await
        .expect("round");
    sqlx::query("UPDATE signups SET submitted_at = now() - interval '8 days' WHERE email = 'old@erpcons.test'")
        .execute(db.pool())
        .await
        .expect("age");
    assert_eq!(expire(db.pool()).await.expect("expire").unverified, 1);
    sqlx::query(
        "UPDATE signups SET finished_at = now() - interval '8 days' WHERE state = 'registered'",
    )
    .execute(db.pool())
    .await
    .expect("age");
    let report = expire(db.pool()).await.expect("expire");
    assert_eq!((report.unverified, report.finished), (0, 1));
    let left: Vec<String> = sqlx::query_scalar("SELECT email FROM signups")
        .fetch_all(db.pool())
        .await
        .expect("rows");
    assert_eq!(left, ["new@erpcons.test"]);
}
