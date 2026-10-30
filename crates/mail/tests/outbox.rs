//! The outbox job against a real Postgres (`TEST_DATABASE_URL`) and a fake SMTP server on loopback.
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;

use base64::{Engine, engine::general_purpose::STANDARD};
use office_db::{mail_outbox, testing::TestDb};
use office_mail::{
    IssuedToken, KIND_SHARED, KIND_SIGNUP_EXISTING, KIND_SIGNUP_VERIFY, MailConfig, OutboxConfig,
    SendError, SharedNotice, SignupNotice, SignupTokens, SmtpConfig, SmtpTransport, TlsMode,
    run_outbox,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream, tcp::OwnedWriteHalf},
};
use uuid::Uuid;

const ORIGIN: &str = "https://office.erpcons.test";

/// How the fake server answers `MAIL FROM`, in order; once the list is used up it accepts.
#[derive(Clone, Default)]
struct FakeSmtp {
    replies: Arc<Mutex<Vec<&'static str>>>,
    received: Arc<Mutex<Vec<String>>>,
}

impl FakeSmtp {
    async fn start(replies: Vec<&'static str>) -> (Self, u16) {
        let smtp = Self {
            replies: Arc::new(Mutex::new(replies)),
            received: Arc::default(),
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("address").port();
        let server = smtp.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let server = server.clone();
                tokio::spawn(async move { server.session(stream).await });
            }
        });
        (smtp, port)
    }

    fn next_mail_reply(&self) -> &'static str {
        let mut replies = self.replies.lock().unwrap_or_else(PoisonError::into_inner);
        if replies.is_empty() {
            "250 2.1.0 OK"
        } else {
            replies.remove(0)
        }
    }

    fn received(&self) -> Vec<String> {
        self.received
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    async fn session(&self, stream: TcpStream) {
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        let w = &mut write;
        say(w, "220 fake.smtp ESMTP").await;
        while let Ok(Some(line)) = lines.next_line().await {
            let verb = line.to_ascii_uppercase();
            if verb.starts_with("EHLO") || verb.starts_with("HELO") {
                say(w, "250 fake.smtp").await;
            } else if verb.starts_with("MAIL FROM") {
                say(w, self.next_mail_reply()).await;
            } else if verb.starts_with("DATA") {
                say(w, "354 go ahead").await;
                let mut message = String::new();
                while let Ok(Some(line)) = lines.next_line().await {
                    if line == "." {
                        break;
                    }
                    message.push_str(&line);
                    message.push('\n');
                }
                self.received
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(message);
                say(w, "250 2.0.0 queued").await;
            } else if verb.starts_with("QUIT") {
                say(w, "221 bye").await;
                return;
            } else {
                say(w, "250 OK").await;
            }
        }
    }
}

async fn say(write: &mut OwnedWriteHalf, text: &str) {
    write
        .write_all(format!("{text}\r\n").as_bytes())
        .await
        .expect("write");
}

fn transport(port: u16) -> SmtpTransport {
    let config = MailConfig {
        smtp: SmtpConfig {
            host: "127.0.0.1".to_owned(),
            port,
            tls: TlsMode::None,
            username: None,
            password: None,
        },
        from: "ERPCons Office <no-reply@erpcons.test>".to_owned(),
        public_origin: ORIGIN.to_owned(),
        include_title: false,
        daily_cap: office_mail::DEFAULT_DAILY_CAP,
        global_daily_cap: office_mail::DEFAULT_GLOBAL_DAILY_CAP,
    };
    SmtpTransport::new(&config).expect("transport")
}

async fn queue(db: &TestDb, document_id: Uuid) {
    let notice = SharedNotice {
        document_id,
        kind: "sheet".to_owned(),
        role: "editor".to_owned(),
        sharer_name: "Nguyễn Văn An".to_owned(),
        sharer_email: Some("an@erpcons.test".to_owned()),
        message: Some("Nhờ chị xem giúp bảng này".to_owned()),
        title: None,
    };
    let mut conn = db.pool().acquire().await.expect("connection");
    let payload = notice.to_json();
    let mail = mail_outbox::NewMail {
        kind: KIND_SHARED,
        sender_id: None,
        to_email: "chi@erpcons.test",
        payload: &payload,
    };
    mail_outbox::enqueue(&mut conn, &mail)
        .await
        .expect("enqueue");
}

async fn row(db: &TestDb) -> (i32, bool, bool) {
    sqlx::query_as("SELECT attempts, sent_at IS NOT NULL, failed_at IS NOT NULL FROM mail_outbox")
        .fetch_one(db.pool())
        .await
        .expect("outbox row")
}

async fn make_due(db: &TestDb) {
    sqlx::query("UPDATE mail_outbox SET next_at = now()")
        .execute(db.pool())
        .await
        .expect("due");
}

/// The text of a received message, its body decoded whatever transfer encoding lettre chose.
fn decoded(message: &str) -> String {
    let (head, body) = message.split_once("\n\n").expect("headers and body");
    if head
        .to_ascii_lowercase()
        .contains("content-transfer-encoding: base64")
    {
        let joined: String = body.lines().collect();
        let bytes = STANDARD.decode(joined.trim()).expect("base64 body");
        return String::from_utf8(bytes).expect("utf-8 body");
    }
    if head
        .to_ascii_lowercase()
        .contains("content-transfer-encoding: quoted-printable")
    {
        let joined = body.replace("=\n", "");
        let mut bytes = Vec::new();
        let mut rest = joined.as_bytes();
        while let Some((&b, tail)) = rest.split_first() {
            if b == b'=' && tail.len() >= 2 {
                let hex = std::str::from_utf8(&tail[..2]).expect("hex");
                bytes.push(u8::from_str_radix(hex, 16).expect("hex byte"));
                rest = &tail[2..];
            } else {
                bytes.push(b);
                rest = tail;
            }
        }
        return String::from_utf8(bytes).expect("utf-8 body");
    }
    body.to_owned()
}

#[tokio::test]
async fn a_temporary_refusal_is_retried_then_sent() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let (smtp, port) = FakeSmtp::start(vec!["451 4.3.0 try again later"]).await;
    let transport = transport(port);
    let document_id = Uuid::now_v7();
    queue(&db, document_id).await;
    let config = OutboxConfig::default();

    let first = run_outbox(db.pool(), &transport, ORIGIN, None, &config)
        .await
        .expect("first round");
    assert_eq!((first.sent, first.retried, first.failed), (0, 1, 0));
    assert_eq!(row(&db).await, (1, false, false));
    // Backed off: a second round right away sends nothing.
    let waiting = run_outbox(db.pool(), &transport, ORIGIN, None, &config)
        .await
        .expect("waiting round");
    assert_eq!(waiting.sent + waiting.retried, 0);

    make_due(&db).await;
    let second = run_outbox(db.pool(), &transport, ORIGIN, None, &config)
        .await
        .expect("second round");
    assert_eq!((second.sent, second.retried, second.failed), (1, 0, 0));
    assert_eq!(row(&db).await, (2, true, false));
    let received = smtp.received();
    assert_eq!(received.len(), 1);
    let text = decoded(&received[0]);
    assert!(
        text.contains(&format!("{ORIGIN}/bang-tinh/?doc={document_id}")),
        "{text}"
    );
    assert!(text.contains("Nguyễn Văn An"), "{text}");
    assert!(text.contains("Nhờ chị xem giúp bảng này"), "{text}");
    assert!(text.contains("chỉnh sửa"), "{text}");
}

#[tokio::test]
async fn a_permanent_refusal_or_a_day_of_failures_gives_up() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let (_smtp, port) =
        FakeSmtp::start(vec!["550 5.1.0 no such sender", "421 4.3.2 shutting down"]).await;
    let transport = transport(port);
    let config = OutboxConfig::default();
    queue(&db, Uuid::now_v7()).await;
    let report = run_outbox(db.pool(), &transport, ORIGIN, None, &config)
        .await
        .expect("round");
    assert_eq!((report.sent, report.retried, report.failed), (0, 0, 1));
    assert_eq!(row(&db).await, (1, false, true));

    sqlx::query("DELETE FROM mail_outbox")
        .execute(db.pool())
        .await
        .expect("clear");
    queue(&db, Uuid::now_v7()).await;
    sqlx::query("UPDATE mail_outbox SET created_at = now() - interval '25 hours'")
        .execute(db.pool())
        .await
        .expect("age");
    let report = run_outbox(db.pool(), &transport, ORIGIN, None, &config)
        .await
        .expect("round");
    assert_eq!((report.sent, report.retried, report.failed), (0, 0, 1));

    // Done rows hold an address: they go once old enough.
    sqlx::query("UPDATE mail_outbox SET created_at = now() - interval '8 days'")
        .execute(db.pool())
        .await
        .expect("age");
    let report = run_outbox(db.pool(), &transport, ORIGIN, None, &config)
        .await
        .expect("round");
    assert_eq!(report.deleted, 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM mail_outbox")
            .fetch_one(db.pool())
            .await
            .expect("count"),
        0
    );
}

/// Issues `token` for `live` only, as the sign-up service does for a pending sign-up.
struct Tokens {
    live: Uuid,
    issued: Mutex<u32>,
}

#[async_trait]
impl SignupTokens for Tokens {
    async fn issue(&self, signup_id: Uuid) -> Result<Option<IssuedToken>, SendError> {
        if signup_id != self.live {
            return Ok(None);
        }
        *self.issued.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        Ok(Some(IssuedToken {
            display_name: "Lê Minh Chí".to_owned(),
            token: "tok-0123456789".to_owned(),
        }))
    }
}

async fn queue_kind(db: &TestDb, kind: &str, to: &str, signup_id: Uuid) {
    let mut conn = db.pool().acquire().await.expect("connection");
    let payload = SignupNotice { signup_id }.to_json();
    let mail = mail_outbox::NewMail {
        kind,
        sender_id: None,
        to_email: to,
        payload: &payload,
    };
    mail_outbox::enqueue(&mut conn, &mail)
        .await
        .expect("enqueue");
}

#[tokio::test]
async fn sign_up_mails_get_their_token_when_sent_and_stale_ones_are_dropped() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let (smtp, port) = FakeSmtp::start(vec![]).await;
    let transport = transport(port);
    let live = Uuid::now_v7();
    let tokens = Tokens {
        live,
        issued: Mutex::new(0),
    };
    queue_kind(&db, KIND_SIGNUP_VERIFY, "chi@erpcons.test", live).await;
    queue_kind(&db, KIND_SIGNUP_VERIFY, "old@erpcons.test", Uuid::now_v7()).await;
    queue_kind(&db, KIND_SIGNUP_EXISTING, "an@erpcons.test", Uuid::now_v7()).await;

    // Without a token issuer a verification mail cannot be written: given up, never sent bare.
    let none = run_outbox(
        db.pool(),
        &transport,
        ORIGIN,
        None,
        &OutboxConfig::default(),
    )
    .await
    .expect("round");
    assert_eq!((none.sent, none.failed), (1, 2));
    sqlx::query("UPDATE mail_outbox SET failed_at = NULL, next_at = now() WHERE kind = $1")
        .bind(KIND_SIGNUP_VERIFY)
        .execute(db.pool())
        .await
        .expect("reset");

    let report = run_outbox(
        db.pool(),
        &transport,
        ORIGIN,
        Some(&tokens),
        &OutboxConfig::default(),
    )
    .await
    .expect("round");
    assert_eq!((report.sent, report.dropped, report.failed), (1, 1, 0));
    assert_eq!(*tokens.issued.lock().expect("count"), 1);
    let texts: Vec<String> = smtp.received().iter().map(|m| decoded(m)).collect();
    assert_eq!(texts.len(), 2);
    assert!(
        texts
            .iter()
            .any(|t| t.contains("Đã có tài khoản PI ID") || t.contains("đã có tài khoản PI ID")),
        "{texts:?}"
    );
    let verify = texts
        .iter()
        .find(|t| t.contains("Xin chào Lê Minh Chí"))
        .expect("verification mail");
    assert!(
        verify.contains(&format!("{ORIGIN}/dang-ky/xac-minh/#token=tok-0123456789")),
        "{verify}"
    );
    // The outbox row never held the token.
    let payloads: Vec<String> = sqlx::query_scalar("SELECT payload::text FROM mail_outbox")
        .fetch_all(db.pool())
        .await
        .expect("payloads");
    assert!(payloads.iter().all(|p| !p.contains("tok-")), "{payloads:?}");
}
