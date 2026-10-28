use office_documents::{
    QuotaConfig,
    intake::testing::{ZipEntry, zip},
};
use serde_json::{Value, json};

use crate::{
    documents::doc_etag,
    support::{TestApp, docx, etag, expect_conflict, expect_error, ok_json, pdf, stored_objects},
};

fn id(v: &Value) -> &str {
    v["id"].as_str().expect("id")
}

async fn versions(app: &TestApp) -> i64 {
    app.count("SELECT count(*) FROM document_versions").await
}

#[tokio::test]
async fn a_save_on_the_current_etag_makes_the_next_version() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let doc = ok_json(app.upload(&token, "a.docx", &docx("v1"), &[]).await, 201).await;
    let base = doc_etag(&app, &token, id(&doc)).await;
    let next = docx("v2 with more text");
    let response = app
        .put_content(&token, id(&doc), Some(&base), next.clone())
        .await;
    let new_tag = etag(&response);
    assert_ne!(new_tag, base);
    let saved = ok_json(response, 200).await;
    assert_eq!(saved["version"], 2);
    assert_eq!(saved["size"], next.len());
    assert_eq!(saved["save_state"], "SERVER_DURABLE");

    let content = app
        .get_as(&format!("/api/v1/documents/{}/content", id(&doc)), &token)
        .await;
    assert_eq!(content.headers()["x-document-version"], "2");
    assert_eq!(etag(&content), new_tag);
    assert_eq!(content.bytes().await.expect("bytes").as_ref(), next);

    // Any listed strong tag may match; weak ones are ignored, so a header of only those is invalid.
    let listed = format!("\"other\", {new_tag}");
    let response = app
        .put_content(&token, id(&doc), Some(&listed), docx("v3"))
        .await;
    assert_eq!(ok_json(response, 200).await["version"], 3);
    let tag = doc_etag(&app, &token, id(&doc)).await;
    let weak = format!("W/{tag}");
    expect_error(
        app.put_content(&token, id(&doc), Some(&weak), docx("v4"))
            .await,
        422,
        "INVALID_REQUEST",
        false,
    )
    .await;
    expect_error(
        app.put_content(&token, id(&doc), Some("*"), docx("v4"))
            .await,
        422,
        "INVALID_REQUEST",
        false,
    )
    .await;
}

#[tokio::test]
async fn a_save_without_if_match_or_on_a_stale_etag_writes_nothing() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let doc = app.create_blank(&token, "word", None, None).await;
    let base = doc_etag(&app, &token, id(&doc)).await;

    let missing = app.put_content(&token, id(&doc), None, docx("x")).await;
    expect_error(missing, 428, "PRECONDITION_REQUIRED", false).await;
    let unknown_doc = uuid::Uuid::new_v4().to_string();
    let missing = app.put_content(&token, &unknown_doc, None, docx("x")).await;
    expect_error(missing, 428, "PRECONDITION_REQUIRED", false).await;

    let ok = app
        .put_content(&token, id(&doc), Some(&base), docx("v2"))
        .await;
    assert_eq!(ok.status(), 200);
    let objects_before = stored_objects(&app.store_dir());
    let stale = app
        .put_content(&token, id(&doc), Some(&base), docx("v3"))
        .await;
    assert_eq!(expect_conflict(stale).await, 2);
    assert_eq!(versions(&app).await, 2);
    assert_eq!(
        stored_objects(&app.store_dir()),
        objects_before,
        "a stale save is refused before its body is stored"
    );

    let current = doc_etag(&app, &token, id(&doc)).await;
    let wrong_kind = app
        .put_content(&token, id(&doc), Some(&current), pdf("x"))
        .await;
    expect_error(wrong_kind, 422, "INVALID_SIGNATURE", false).await;
    let meta = ok_json(
        app.get_as(&format!("/api/v1/documents/{}", id(&doc)), &token)
            .await,
        200,
    )
    .await;
    assert_eq!(meta["version"], 2);
    assert_eq!(versions(&app).await, 2);
}

#[tokio::test]
async fn of_two_saves_on_the_same_base_exactly_one_wins() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    for same_bytes in [false, true] {
        let doc = app.create_blank(&token, "word", None, None).await;
        let base = doc_etag(&app, &token, id(&doc)).await;
        let (a, b) = if same_bytes {
            (docx("same"), docx("same"))
        } else {
            (docx("left"), docx("right"))
        };
        let (first, second) = tokio::join!(
            app.put_content(&token, id(&doc), Some(&base), a),
            app.put_content(&token, id(&doc), Some(&base), b),
        );
        let mut statuses = [first.status().as_u16(), second.status().as_u16()];
        statuses.sort_unstable();
        assert_eq!(statuses, [200, 409], "same bytes: {same_bytes}");
        let loser = if first.status() == 409 { first } else { second };
        assert_eq!(expect_conflict(loser).await, 2);
        let rows = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM document_versions WHERE document_id = $1::uuid",
        )
        .bind(id(&doc))
        .fetch_one(app.pool())
        .await
        .expect("count");
        assert_eq!(rows, 2);
        assert_eq!(
            stored_objects(&app.store_dir()) as i64,
            app.count("SELECT count(*) FROM storage_objects").await,
            "the loser's bytes are not left behind"
        );
    }
}

#[tokio::test]
async fn equal_content_is_stored_and_counted_once() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let bytes = docx("shared");
    let a = ok_json(app.upload(&token, "a.docx", &bytes, &[]).await, 201).await;
    let b = ok_json(app.upload(&token, "b.docx", &bytes, &[]).await, 201).await;
    assert_ne!(a["id"], b["id"]);
    let base = doc_etag(&app, &token, id(&a)).await;
    let resave = app
        .put_content(&token, id(&a), Some(&base), bytes.clone())
        .await;
    assert_eq!(ok_json(resave, 200).await["version"], 2);

    assert_eq!(app.count("SELECT count(*) FROM storage_objects").await, 1);
    assert_eq!(stored_objects(&app.store_dir()), 1);
    let me = ok_json(app.get_as("/api/v1/me", &token).await, 200).await;
    assert_eq!(me["quota"]["used_bytes"], bytes.len());
}

#[tokio::test]
async fn the_quota_counts_distinct_bytes_and_the_file_limit_applies_to_saves() {
    let sample = docx("one").len() as u64;
    let Some(app) = TestApp::spawn_with(|s| {
        s.api.quota = QuotaConfig {
            limit_bytes: sample * 2 + sample / 2,
            max_file_bytes: sample * 2,
        };
    })
    .await
    else {
        return;
    };
    let token = app.token().await;
    let one = ok_json(app.upload(&token, "1.docx", &docx("one"), &[]).await, 201).await;
    ok_json(app.upload(&token, "2.docx", &docx("two"), &[]).await, 201).await;
    let full = app.upload(&token, "3.docx", &docx("tri"), &[]).await;
    expect_error(full, 413, "QUOTA_EXCEEDED", false).await;
    assert_eq!(
        stored_objects(&app.store_dir()),
        2,
        "refused bytes are not kept"
    );
    let again = app.upload(&token, "4.docx", &docx("one"), &[]).await;
    assert_eq!(again.status(), 201, "bytes already stored cost nothing");

    let big = zip(&[ZipEntry::stored(
        "word/document.xml",
        &vec![b'x'; sample as usize * 3],
    )]);
    let too_large = app.upload(&token, "big.docx", &big, &[]).await;
    expect_error(too_large, 413, "TOO_LARGE", false).await;
    let base = doc_etag(&app, &token, id(&one)).await;
    let too_large = app.put_content(&token, id(&one), Some(&base), big).await;
    expect_error(too_large, 413, "TOO_LARGE", false).await;
    let over_quota = app
        .put_content(&token, id(&one), Some(&base), docx("tri"))
        .await;
    expect_error(over_quota, 413, "QUOTA_EXCEEDED", false).await;
    assert_eq!(stored_objects(&app.store_dir()), 2);
    assert_eq!(app.count("SELECT count(*) FROM documents").await, 3);
    assert_eq!(versions(&app).await, 3);

    let other = app.token_of("usr_mock_binh", "Trần Thị Bình").await;
    let theirs = app.upload(&other, "1.docx", &docx("tri"), &[]).await;
    assert_eq!(theirs.status(), 201, "each actor has their own quota");
    let me = ok_json(app.get_as("/api/v1/me", &token).await, 200).await;
    assert_eq!(
        me["quota"],
        json!({
            "used_bytes": sample * 2,
            "limit_bytes": sample * 2 + sample / 2,
            "max_file_bytes": sample * 2,
        })
    );
}

#[tokio::test]
async fn an_actor_at_its_quota_stores_nothing() {
    let one = docx("one");
    let limit = (one.len() + docx("two").len()) as u64;
    let Some(app) = TestApp::spawn_with(|s| s.api.quota.limit_bytes = limit).await else {
        return;
    };
    let token = app.token().await;
    let doc = ok_json(app.upload(&token, "1.docx", &one, &[]).await, 201).await;
    ok_json(app.upload(&token, "2.docx", &docx("two"), &[]).await, 201).await;
    let refused = app.upload(&token, "3.docx", &docx("tri"), &[]).await;
    expect_error(refused, 413, "QUOTA_EXCEEDED", false).await;
    let blank = app
        .post_json("/api/v1/documents", &token, &json!({ "kind": "word" }))
        .await;
    expect_error(blank, 413, "QUOTA_EXCEEDED", false).await;
    let base = doc_etag(&app, &token, id(&doc)).await;
    let save = app
        .put_content(&token, id(&doc), Some(&base), docx("tri"))
        .await;
    expect_error(save, 413, "QUOTA_EXCEEDED", false).await;
    assert_eq!(stored_objects(&app.store_dir()), 2);
    assert_eq!(versions(&app).await, 2);
}

#[tokio::test]
async fn bytes_another_document_uses_survive_a_refused_request() {
    let own = docx("own");
    let shared = docx("shared");
    let limit = (own.len() + own.len() / 2) as u64;
    let Some(app) = TestApp::spawn_with(|s| s.api.quota.limit_bytes = limit).await else {
        return;
    };
    let owner = app.token().await;
    let theirs = ok_json(app.upload(&owner, "s.docx", &shared, &[]).await, 201).await;
    let other = app.token_of("usr_mock_binh", "Trần Thị Bình").await;
    let mine = ok_json(app.upload(&other, "o.docx", &own, &[]).await, 201).await;

    let upload = app.upload(&other, "copy.docx", &shared, &[]).await;
    expect_error(upload, 413, "QUOTA_EXCEEDED", false).await;
    let base = doc_etag(&app, &other, id(&mine)).await;
    let save = app
        .put_content(&other, id(&mine), Some(&base), shared.clone())
        .await;
    expect_error(save, 413, "QUOTA_EXCEEDED", false).await;

    assert_eq!(stored_objects(&app.store_dir()), 2);
    let content = app
        .get_as(
            &format!("/api/v1/documents/{}/content", id(&theirs)),
            &owner,
        )
        .await;
    assert_eq!(content.status(), 200);
    assert_eq!(content.bytes().await.expect("bytes").as_ref(), shared);
}

/// Makes the current version of `doc` one written by `author_id`, with the same content.
async fn version_by(app: &TestApp, doc: &str, author_id: &str) {
    let version = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO document_versions
             (id, document_id, seq, storage_object_id, sha256, bytes, author_id, author_name_snapshot)
         SELECT $1, v.document_id, v.seq + 1, v.storage_object_id, v.sha256, v.bytes, $3, 'Other'
         FROM documents d JOIN document_versions v ON v.id = d.current_version_id
         WHERE d.id = $2::uuid",
    )
    .bind(version)
    .bind(doc)
    .bind(author_id)
    .execute(app.pool())
    .await
    .expect("version row");
    sqlx::query("UPDATE documents SET current_version_id = $1, etag = $2 WHERE id = $3::uuid")
        .bind(version)
        .bind(format!("\"{}\"", version.simple()))
        .bind(doc)
        .execute(app.pool())
        .await
        .expect("current version");
}

#[tokio::test]
async fn a_save_repeated_after_a_lost_response_answers_the_saved_version() {
    let v1 = docx("v1");
    let v2 = docx("v2 replayed");
    let limit = (v1.len() + v2.len()) as u64;
    let Some(app) = TestApp::spawn_with(|s| s.api.quota.limit_bytes = limit).await else {
        return;
    };
    let token = app.token().await;
    let doc = ok_json(app.upload(&token, "a.docx", &v1, &[]).await, 201).await;
    let e1 = doc_etag(&app, &token, id(&doc)).await;
    let first = app
        .put_content(&token, id(&doc), Some(&e1), v2.clone())
        .await;
    let e2 = etag(&first);
    let first = ok_json(first, 200).await;
    assert_eq!(first["version"], 2);

    // The actor is now exactly at its quota: a replay costs nothing, so it still succeeds.
    let replay = app
        .put_content(&token, id(&doc), Some(&e1), v2.clone())
        .await;
    assert_eq!(etag(&replay), e2);
    assert_eq!(ok_json(replay, 200).await, first);
    assert_eq!(versions(&app).await, 2, "no version was added");
    assert_eq!(stored_objects(&app.store_dir()), 2);

    let other_bytes = app
        .put_content(&token, id(&doc), Some(&e1), docx("v2 changed"))
        .await;
    assert_eq!(expect_conflict(other_bytes).await, 2);
    let new_save = app
        .put_content(&token, id(&doc), Some(&e2), docx("v3"))
        .await;
    expect_error(new_save, 413, "QUOTA_EXCEEDED", false).await;

    let intruder = app.token_of("usr_mock_binh", "Trần Thị Bình").await;
    let foreign = app
        .put_content(&intruder, id(&doc), Some(&e1), v2.clone())
        .await;
    expect_error(foreign, 404, "NOT_FOUND", false).await;
    assert_eq!(versions(&app).await, 2);
}

#[tokio::test]
async fn only_the_save_right_before_counts_as_a_repeat() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let doc = app.create_blank(&token, "word", None, None).await;
    let e1 = doc_etag(&app, &token, id(&doc)).await;
    let v2 = docx("two");
    let e2 = etag(
        &app.put_content(&token, id(&doc), Some(&e1), v2.clone())
            .await,
    );
    let v3 = docx("three");
    let saved = app
        .put_content(&token, id(&doc), Some(&e2), v3.clone())
        .await;
    assert_eq!(saved.status(), 200);
    let two_back = app
        .put_content(&token, id(&doc), Some(&e1), v3.clone())
        .await;
    assert_eq!(expect_conflict(two_back).await, 3);
    let old_bytes = app
        .put_content(&token, id(&doc), Some(&e1), v2.clone())
        .await;
    assert_eq!(
        expect_conflict(old_bytes).await,
        3,
        "v2 is no longer current"
    );
    let replay = app
        .put_content(&token, id(&doc), Some(&e2), v3.clone())
        .await;
    assert_eq!(ok_json(replay, 200).await["version"], 3);

    // A version another author made is never taken for this actor's repeat.
    let e3 = doc_etag(&app, &token, id(&doc)).await;
    let other = app.token_of("usr_mock_binh", "Trần Thị Bình").await;
    assert_eq!(app.get_as("/api/v1/me", &other).await.status(), 200);
    version_by(&app, id(&doc), "usr_mock_binh").await;
    let not_mine = app
        .put_content(&token, id(&doc), Some(&e3), v3.clone())
        .await;
    assert_eq!(expect_conflict(not_mine).await, 4);
    assert_eq!(versions(&app).await, 4);
}
