//! Durability fail points, the purge job and the orphan sweep, run directly.
use std::time::Duration;

use std::io::Cursor;

use office_documents::{
    FailPoint,
    jobs::{AuditRetentionConfig, OrphanConfig, PurgeConfig},
};
use office_storage::{FsStorage, PutOptions, StorageAdapter};
use reqwest::Method;
use serde_json::Value;

use crate::{
    documents::doc_etag,
    support::{TestApp, docx, expect_error, ok_json, stored_objects},
};

const NOW: OrphanConfig = OrphanConfig {
    min_age: Duration::ZERO,
    page: 2,
};

/// The smallest age the server accepts (`ERPCONS_ORPHAN_MIN_AGE_SECS`). With `NOW`, a sweep may
/// delete what a concurrent save has put but not yet committed, which production rules out.
const SMALLEST_ALLOWED: OrphanConfig = OrphanConfig {
    min_age: Duration::from_secs(60),
    page: 2,
};

fn id(v: &Value) -> &str {
    v["id"].as_str().expect("id")
}

#[tokio::test]
async fn a_save_failing_between_put_and_commit_leaves_no_version_and_no_bytes() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let doc = ok_json(app.upload(&token, "a.docx", &docx("v1"), &[]).await, 201).await;
    let base = doc_etag(&app, &token, id(&doc)).await;
    for (n, point) in [FailPoint::AfterPut, FailPoint::BeforeCommit]
        .into_iter()
        .enumerate()
    {
        app.documents().fail_points().arm(point);
        let response = app
            .put_content(&token, id(&doc), Some(&base), docx(&format!("lost {n}")))
            .await;
        expect_error(response, 503, "UNAVAILABLE", true).await;
        assert_eq!(
            app.count("SELECT count(*) FROM document_versions").await,
            1,
            "{point:?}"
        );
        assert_eq!(app.count("SELECT count(*) FROM storage_objects").await, 1);
        assert_eq!(
            stored_objects(&app.store_dir()),
            1,
            "{point:?}: the failed request removed its bytes"
        );
        let swept = app.documents().sweep_orphans(&NOW).await.expect("sweep");
        assert_eq!((swept.unreferenced, swept.deleted), (0, 0));
    }
    let meta = ok_json(
        app.get_as(&format!("/api/v1/documents/{}", id(&doc)), &token)
            .await,
        200,
    )
    .await;
    assert_eq!(meta["version"], 1);
    let content = app
        .get_as(&format!("/api/v1/documents/{}/content", id(&doc)), &token)
        .await;
    assert_eq!(content.bytes().await.expect("bytes").as_ref(), docx("v1"));
    let retry = app
        .put_content(&token, id(&doc), Some(&base), docx("lost 0"))
        .await;
    assert_eq!(
        ok_json(retry, 200).await["version"],
        2,
        "the client may retry"
    );
}

#[tokio::test]
async fn bytes_a_crash_left_without_a_row_are_swept_once_old_enough() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    ok_json(app.upload(&token, "a.docx", &docx("kept"), &[]).await, 201).await;
    // As a process that died between its put and its transaction would leave them.
    let store = FsStorage::open(app.store_dir()).await.expect("store");
    store
        .put(Box::pin(Cursor::new(docx("orphan"))), PutOptions::default())
        .await
        .expect("put");
    assert_eq!(stored_objects(&app.store_dir()), 2);
    let young = app
        .documents()
        .sweep_orphans(&OrphanConfig::default())
        .await
        .expect("sweep");
    assert_eq!(
        (young.unreferenced, young.deleted),
        (1, 0),
        "too young to sweep"
    );
    let swept = app.documents().sweep_orphans(&NOW).await.expect("sweep");
    assert_eq!((swept.unreferenced, swept.deleted), (1, 1));
    assert_eq!(
        stored_objects(&app.store_dir()),
        1,
        "the live version stays"
    );
    let again = app.documents().sweep_orphans(&NOW).await.expect("sweep");
    assert_eq!((again.unreferenced, again.deleted), (0, 0), "idempotent");
}

#[tokio::test]
async fn a_create_failing_before_commit_leaves_no_document() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    app.documents().fail_points().arm(FailPoint::BeforeCommit);
    let response = app.upload(&token, "a.docx", &docx("x"), &[]).await;
    expect_error(response, 503, "UNAVAILABLE", true).await;
    assert_eq!(app.count("SELECT count(*) FROM documents").await, 0);
    assert_eq!(app.count("SELECT count(*) FROM storage_objects").await, 0);
    assert_eq!(stored_objects(&app.store_dir()), 0);
    let swept = app.documents().sweep_orphans(&NOW).await.expect("sweep");
    assert_eq!(swept.deleted, 0, "nothing was left to sweep");
}

#[tokio::test]
async fn audit_rows_past_retention_are_deleted() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    for _ in 0..4 {
        assert_eq!(app.get_as("/api/v1/me", &token).await.status(), 200);
    }
    sqlx::query(
        "UPDATE audit_events SET at = now() - make_interval(days => 181)
         WHERE id IN (SELECT id FROM audit_events ORDER BY id LIMIT 3)",
    )
    .execute(app.pool())
    .await
    .expect("age audit rows");
    let config = AuditRetentionConfig {
        batch: 2,
        ..AuditRetentionConfig::default()
    };
    let deleted = app.documents().expire_audit(&config).await.expect("expire");
    assert_eq!(deleted, 3, "in batches of 2");
    assert_eq!(app.count("SELECT count(*) FROM audit_events").await, 1);
    let again = app.documents().expire_audit(&config).await.expect("expire");
    assert_eq!(again, 0, "idempotent");
}

async fn age_trash(app: &TestApp, days: i64) {
    for table in ["documents", "folders"] {
        sqlx::query(&format!(
            "UPDATE {table} SET trashed_at = now() - make_interval(days => $1::int) \
             WHERE trashed_at IS NOT NULL"
        ))
        .bind(days)
        .execute(app.pool())
        .await
        .expect("age trash");
    }
}

#[tokio::test]
async fn purge_removes_expired_trash_and_keeps_shared_bytes() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let folder = app.create_folder(&token, None, "Cũ").await;
    let inside = ok_json(
        app.upload(
            &token,
            "old.docx",
            &docx("old"),
            &[("folder_id", id(&folder))],
        )
        .await,
        201,
    )
    .await;
    let shared = docx("shared");
    let gone = ok_json(app.upload(&token, "gone.docx", &shared, &[]).await, 201).await;
    let kept = ok_json(app.upload(&token, "kept.docx", &shared, &[]).await, 201).await;
    let recent = app
        .create_blank(&token, "word", Some("Mới xoá"), None)
        .await;
    for path in [
        format!("/api/v1/folders/{}", id(&folder)),
        format!("/api/v1/documents/{}", id(&gone)),
    ] {
        let response = app
            .request(Method::DELETE, &path, &token)
            .send()
            .await
            .expect("trash");
        assert_eq!(response.status(), 204);
    }
    age_trash(&app, 31).await;
    let recent_path = format!("/api/v1/documents/{}", id(&recent));
    app.request(Method::DELETE, &recent_path, &token)
        .send()
        .await
        .expect("trash");
    assert_eq!(stored_objects(&app.store_dir()), 3);

    let report = app
        .documents()
        .purge(&PurgeConfig::default())
        .await
        .expect("purge");
    assert_eq!(report.marked, 2);
    assert_eq!(report.documents_purged, 2);
    assert_eq!(report.objects_deleted, 1, "the shared bytes stay");
    assert_eq!(report.folders_deleted, 1);
    let purged_folder: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events
         WHERE action = 'folder.purge' AND actor_kind = 'system' AND folder_id = $1::uuid",
    )
    .bind(id(&folder))
    .fetch_one(app.pool())
    .await
    .expect("purge audit");
    assert_eq!(purged_folder, 1);
    assert_eq!(stored_objects(&app.store_dir()), 2);

    for doc in [&inside, &gone] {
        let path = format!("/api/v1/documents/{}", id(doc));
        expect_error(app.get_as(&path, &token).await, 404, "NOT_FOUND", false).await;
        let content = app.get_as(&format!("{path}/content"), &token).await;
        expect_error(content, 404, "NOT_FOUND", false).await;
        let restore = app
            .request(Method::POST, &format!("{path}/restore"), &token)
            .send()
            .await
            .expect("restore");
        expect_error(restore, 404, "NOT_FOUND", false).await;
    }
    let states: Vec<String> =
        sqlx::query_scalar("SELECT state FROM documents WHERE id = ANY($1::uuid[]) ORDER BY state")
            .bind(vec![id(&inside).to_owned(), id(&gone).to_owned()])
            .fetch_all(app.pool())
            .await
            .expect("states");
    assert_eq!(states, ["PURGED", "PURGED"], "metadata is kept");
    assert_eq!(
        app.count("SELECT count(*) FROM document_versions").await,
        4,
        "versions are kept"
    );
    let folder_children = format!("/api/v1/folders/{}/children", id(&folder));
    expect_error(
        app.get_as(&folder_children, &token).await,
        404,
        "NOT_FOUND",
        false,
    )
    .await;

    let kept_content = app
        .get_as(&format!("/api/v1/documents/{}/content", id(&kept)), &token)
        .await;
    assert_eq!(kept_content.bytes().await.expect("bytes").as_ref(), shared);
    let recent_meta = ok_json(app.get_as(&recent_path, &token).await, 200).await;
    assert_eq!(recent_meta["state"], "TRASHED", "not expired yet");
    let trash = ok_json(app.get_as("/api/v1/trash", &token).await, 200).await;
    assert_eq!(trash["folders"], serde_json::json!([]));
    assert_eq!(trash["documents"][0]["id"], recent["id"]);

    let again = app
        .documents()
        .purge(&PurgeConfig::default())
        .await
        .expect("purge again");
    assert_eq!(
        (
            again.marked,
            again.objects_deleted,
            again.documents_purged,
            again.folders_deleted
        ),
        (0, 0, 0, 0),
        "idempotent"
    );
    let me = ok_json(app.get_as("/api/v1/me", &token).await, 200).await;
    let template = office_documents::blank_template(office_documents::Format::Docx).len();
    assert_eq!(me["quota"]["used_bytes"], shared.len() + template);

    // Uploading bytes a purge deleted stores them again.
    let back = ok_json(app.upload(&token, "old.docx", &docx("old"), &[]).await, 201).await;
    let content = app
        .get_as(&format!("/api/v1/documents/{}/content", id(&back)), &token)
        .await;
    assert_eq!(content.bytes().await.expect("bytes").as_ref(), docx("old"));
}

#[tokio::test]
async fn purge_and_sweep_running_with_saves_lose_nothing() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let doc = app.create_blank(&token, "word", None, None).await;
    let mut tag = doc_etag(&app, &token, id(&doc)).await;
    for n in 0..5 {
        let save = app.put_content(&token, id(&doc), Some(&tag), docx(&format!("s{n}")));
        let documents = app.documents().clone();
        let jobs = async move {
            documents
                .sweep_orphans(&SMALLEST_ALLOWED)
                .await
                .expect("sweep");
            documents
                .purge(&PurgeConfig::default())
                .await
                .expect("purge");
        };
        let (response, ()) = tokio::join!(save, jobs);
        assert_eq!(response.status(), 200, "save {n}");
        tag = crate::support::etag(&response);
        assert_eq!(ok_json(response, 200).await["version"], n + 2);
        let content = app
            .get_as(&format!("/api/v1/documents/{}/content", id(&doc)), &token)
            .await;
        assert_eq!(content.status(), 200);
        assert_eq!(
            content.bytes().await.expect("bytes").as_ref(),
            docx(&format!("s{n}"))
        );
    }
}
