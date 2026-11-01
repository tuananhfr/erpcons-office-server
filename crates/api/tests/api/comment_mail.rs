//! Comment mail (plan-m5): who gets one, the 15-minute batch of new threads, no words in any mail,
//! and nothing for someone who lost access.
use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;
use office_mail::{
    MailSources, MailTransport, OutboxConfig, OutboxReport, Rendered, SendError, run_outbox,
};
use reqwest::Method;
use serde_json::json;

use crate::{
    comments::{COMMENTER_EMAIL, Cast, EDITOR_EMAIL, id, reply_ok, start_ok},
    people::OWNER_EMAIL,
    support::TestApp,
};

const ORIGIN: &str = "https://office.erpcons.test";
const TITLE: &str = "Hợp đồng bí mật 4471";

#[derive(Default)]
struct Recorder(Mutex<Vec<(String, Rendered)>>);

impl Recorder {
    fn sent(&self) -> Vec<(String, Rendered)> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl MailTransport for Recorder {
    async fn send(&self, to: &str, mail: &Rendered) -> Result<(), SendError> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((to.to_owned(), mail.clone()));
        Ok(())
    }
}

async fn spawn(include_title: bool) -> Option<TestApp> {
    TestApp::spawn_with(|s| {
        s.api.mail = office_api::MailPolicy {
            enabled: true,
            include_title,
            ..office_api::MailPolicy::default()
        };
    })
    .await
}

/// Makes every waiting mail due, then runs one outbox round.
async fn send_all(app: &TestApp, transport: &Recorder) -> OutboxReport {
    sqlx::query(
        "UPDATE mail_outbox SET next_at = now() - interval '1 second' WHERE sent_at IS NULL",
    )
    .execute(app.pool())
    .await
    .expect("make due");
    let sources = MailSources {
        signups: None,
        comments: Some(&**app.documents()),
    };
    run_outbox(
        app.pool(),
        transport,
        ORIGIN,
        sources,
        &OutboxConfig::default(),
    )
    .await
    .expect("outbox")
}

/// `(kind, to_email)` of the waiting mails, in order.
async fn waiting(app: &TestApp) -> Vec<(String, String)> {
    sqlx::query_as(
        "SELECT kind, to_email FROM mail_outbox WHERE sent_at IS NULL AND failed_at IS NULL ORDER BY id",
    )
    .fetch_all(app.pool())
    .await
    .expect("outbox rows")
}

fn pair(kind: &str, to: &str) -> (String, String) {
    (kind.to_owned(), to.to_owned())
}

#[tokio::test]
async fn a_reply_reaches_the_others_who_wrote_in_the_thread() {
    let Some(app) = spawn(false).await else {
        return;
    };
    let cast = Cast::new(&app, TITLE).await;
    let thread = start_ok(&app, &cast, &cast.c).await;
    let tid = id(&thread);
    reply_ok(&app, &cast, &cast.b, &tid, "Trả lời riêng tư của B").await;
    assert_eq!(
        waiting(&app).await,
        [
            pair("comment_threads", OWNER_EMAIL),
            pair("comment_reply", COMMENTER_EMAIL)
        ],
        "the owner, who has not written, gets no reply mail; B, the replier, none"
    );
    reply_ok(&app, &cast, &cast.a, &tid, "Trả lời riêng tư của A").await;
    assert_eq!(
        waiting(&app).await,
        [
            pair("comment_threads", OWNER_EMAIL),
            pair("comment_reply", COMMENTER_EMAIL),
            pair("comment_reply", EDITOR_EMAIL)
        ],
        "C already has one waiting for this thread"
    );
    let payloads: Vec<String> = sqlx::query_scalar("SELECT payload::text FROM mail_outbox")
        .fetch_all(app.pool())
        .await
        .expect("payloads");
    for payload in &payloads {
        for hidden in ["riêng tư", "Xem lại", TITLE] {
            assert!(!payload.contains(hidden), "{hidden} in {payload}");
        }
    }

    let transport = Recorder::default();
    let report = send_all(&app, &transport).await;
    assert_eq!((report.sent, report.dropped), (3, 0));
    let link = format!("{ORIGIN}/van-ban/?doc={}&comment={tid}", cast.doc);
    for (to, mail) in transport.sent() {
        let text = format!("{}\n{}", mail.subject, mail.body);
        for hidden in ["riêng tư", "Xem lại", TITLE] {
            assert!(!text.contains(hidden), "{hidden} in the mail to {to}");
        }
        assert!(text.contains(&link), "{text}");
    }
    let to_c = transport
        .sent()
        .into_iter()
        .find(|(to, _)| to == COMMENTER_EMAIL)
        .expect("mail to C");
    assert!(to_c.1.subject.contains("Bình Editor"), "{}", to_c.1.subject);
    assert!(
        to_c.1.subject.contains("một nhận xét"),
        "no title: {}",
        to_c.1.subject
    );

    reply_ok(&app, &cast, &cast.b, &tid, "Lần nữa").await;
    let mut next = waiting(&app).await;
    next.sort();
    assert_eq!(
        next,
        [
            pair("comment_reply", OWNER_EMAIL),
            pair("comment_reply", COMMENTER_EMAIL)
        ],
        "once sent, the next reply queues again; A now wrote in the thread"
    );
}

#[tokio::test]
async fn new_threads_are_gathered_into_one_mail_per_document_and_window() {
    let Some(app) = spawn(false).await else {
        return;
    };
    let cast = Cast::new(&app, TITLE).await;
    for token in [&cast.c, &cast.c, &cast.b] {
        start_ok(&app, &cast, token).await;
    }
    start_ok(&app, &cast, &cast.a).await;
    assert_eq!(waiting(&app).await, [pair("comment_threads", OWNER_EMAIL)]);
    let wait: f64 =
        sqlx::query_scalar("SELECT extract(epoch FROM next_at - now())::float8 FROM mail_outbox")
            .fetch_one(app.pool())
            .await
            .expect("next_at");
    assert!(
        (880.0..=900.0).contains(&wait),
        "sent 15 minutes after the first: {wait}"
    );

    let transport = Recorder::default();
    let early = run_outbox(
        app.pool(),
        &transport,
        ORIGIN,
        MailSources {
            signups: None,
            comments: Some(&**app.documents()),
        },
        &OutboxConfig::default(),
    )
    .await
    .expect("outbox");
    assert_eq!(early.sent, 0, "not before the window ends");
    start_ok(&app, &cast, &cast.c).await;
    let report = send_all(&app, &transport).await;
    assert_eq!(report.sent, 1);
    let (to, mail) = transport.sent().pop().expect("mail");
    assert_eq!(to, OWNER_EMAIL);
    assert!(
        mail.body.contains("Có 4 nhận xét mới"),
        "the owner's own thread is not counted: {}",
        mail.body
    );
    assert!(!mail.subject.contains(TITLE) && !mail.body.contains(TITLE));
    assert!(!mail.body.contains("Xem lại"));

    start_ok(&app, &cast, &cast.b).await;
    assert_eq!(
        waiting(&app).await,
        [pair("comment_threads", OWNER_EMAIL)],
        "the next window starts with the next thread"
    );
    send_all(&app, &transport).await;
    let (_, mail) = transport.sent().pop().expect("mail");
    assert!(mail.body.contains("Có 1 nhận xét mới"), "{}", mail.body);
}

#[tokio::test]
async fn titles_appear_only_when_the_server_includes_them() {
    let Some(app) = spawn(true).await else {
        return;
    };
    let cast = Cast::new(&app, TITLE).await;
    let tid = id(&start_ok(&app, &cast, &cast.c).await);
    reply_ok(&app, &cast, &cast.b, &tid, "x").await;
    let transport = Recorder::default();
    assert_eq!(send_all(&app, &transport).await.sent, 2);
    for (to, mail) in transport.sent() {
        assert!(mail.subject.contains(TITLE), "{to}: {}", mail.subject);
        assert!(!mail.body.contains("Xem lại"));
    }
}

#[tokio::test]
async fn people_who_lost_access_get_no_comment_mail() {
    let Some(app) = spawn(false).await else {
        return;
    };
    let cast = Cast::new(&app, TITLE).await;
    let tid = id(&start_ok(&app, &cast, &cast.c).await);
    reply_ok(&app, &cast, &cast.b, &tid, "x").await;
    let permission = format!(
        "/api/v1/documents/{}/permissions/{}",
        cast.doc, cast.grants[1]
    );
    let demoted = app
        .send_json(
            Method::PATCH,
            &permission,
            &cast.a,
            &json!({ "role": "viewer" }),
        )
        .await;
    assert_eq!(demoted.status(), 200);

    let transport = Recorder::default();
    let report = send_all(&app, &transport).await;
    assert_eq!(
        (report.sent, report.dropped),
        (1, 1),
        "checked again when due"
    );
    let sent: Vec<String> = transport.sent().into_iter().map(|(to, _)| to).collect();
    assert_eq!(sent, [OWNER_EMAIL]);

    reply_ok(&app, &cast, &cast.b, &tid, "y").await;
    assert_eq!(waiting(&app).await, Vec::new(), "and when queued");

    // The owner's batch is dropped once the document is in the trash.
    let thread = start_ok(&app, &cast, &cast.b).await;
    assert_eq!(waiting(&app).await, [pair("comment_threads", OWNER_EMAIL)]);
    let thread_path = format!("/api/v1/documents/{}/comments/{}", cast.doc, id(&thread));
    assert_eq!(app.get_as(&thread_path, &cast.b).await.status(), 200);
    app.trash(&cast.a, &format!("/api/v1/documents/{}", cast.doc))
        .await;
    let report = send_all(&app, &transport).await;
    assert_eq!((report.sent, report.dropped), (0, 1));
}

#[tokio::test]
async fn replies_close_together_share_one_mail() {
    let Some(app) = spawn(false).await else {
        return;
    };
    let cast = Cast::new(&app, TITLE).await;
    let tid = id(&start_ok(&app, &cast, &cast.c).await);
    reply_ok(&app, &cast, &cast.b, &tid, "một").await;
    let transport = Recorder::default();
    let sources = MailSources {
        signups: None,
        comments: Some(&**app.documents()),
    };
    let report = run_outbox(
        app.pool(),
        &transport,
        ORIGIN,
        sources,
        &OutboxConfig::default(),
    )
    .await
    .expect("outbox");
    assert_eq!(
        report.sent, 0,
        "a reply mail waits for the replies that follow"
    );
    reply_ok(&app, &cast, &cast.b, &tid, "hai").await;
    reply_ok(&app, &cast, &cast.b, &tid, "ba").await;
    let to_c = app
        .count("SELECT count(*) FROM mail_outbox WHERE kind = 'comment_reply'")
        .await;
    assert_eq!(to_c, 1);
}

#[tokio::test]
async fn comment_mails_leave_invitations_their_own_cap() {
    let Some(app) = spawn(false).await else {
        return;
    };
    let cast = Cast::new(&app, TITLE).await;
    sqlx::query(
        "INSERT INTO mail_outbox (id, kind, to_email, payload)
         SELECT gen_random_uuid(), 'comment_reply', 'victim@erpcons.test', '{}' FROM generate_series(1, 10)",
    )
    .execute(app.pool())
    .await
    .expect("comment mails");
    let response = app
        .send_json(
            Method::POST,
            &format!("/api/v1/documents/{}/permissions", cast.doc),
            &cast.a,
            &json!({ "emails": ["victim@erpcons.test"], "role": "viewer", "notify": true }),
        )
        .await;
    assert_eq!(
        crate::support::ok_json(response, 200).await["mail_skipped"],
        0
    );
}
