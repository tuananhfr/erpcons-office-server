use office_documents::{
    Format, blank_template,
    intake::testing::{ZipEntry, zip},
};
use reqwest::Method;
use serde_json::{Value, json};

use crate::support::{TestApp, docx, etag, expect_error, multipart, new_key, ok_json, pdf};

fn id(v: &Value) -> &str {
    v["id"].as_str().expect("id")
}

#[tokio::test]
async fn blank_documents_of_each_kind_start_from_the_template() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let cases = [
        ("word", Format::Docx, "docx", "Văn bản không tên"),
        ("sheet", Format::Xlsx, "xlsx", "Bảng tính không tên"),
        ("slide", Format::Pptx, "pptx", "Trình chiếu không tên"),
        ("pdf", Format::Pdf, "pdf", "PDF không tên"),
    ];
    for (kind, format, extension, title) in cases {
        let response = app
            .post_json("/api/v1/documents", &token, &json!({ "kind": kind }))
            .await;
        let tag = etag(&response);
        let doc = ok_json(response, 201).await;
        assert_eq!(doc["kind"], kind);
        assert_eq!(doc["format"], extension);
        assert_eq!(doc["title"], title);
        assert_eq!(doc["version"], 1);
        assert_eq!(doc["state"], "ACTIVE");
        assert_eq!(doc["save_state"], "SERVER_DURABLE");
        assert_eq!(doc["folder_id"], Value::Null);
        let template = blank_template(format);
        assert_eq!(doc["size"], template.len());

        let content = app
            .get_as(&format!("/api/v1/documents/{}/content", id(&doc)), &token)
            .await;
        assert_eq!(content.status(), 200);
        assert_eq!(etag(&content), tag);
        assert_eq!(content.headers()["x-document-version"], "1");
        assert_eq!(
            content.headers()["content-type"],
            "application/octet-stream"
        );
        assert_eq!(content.headers()["x-content-type-options"], "nosniff");
        assert_eq!(content.bytes().await.expect("bytes").as_ref(), template);
    }
    let unknown = app
        .post_json("/api/v1/documents", &token, &json!({ "kind": "drawing" }))
        .await;
    expect_error(unknown, 422, "INVALID_REQUEST", false).await;
}

#[tokio::test]
async fn uploads_keep_their_bytes_and_take_the_file_name_as_title() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let folder = app.create_folder(&token, None, "Hồ sơ").await;
    let bytes = docx("hop-dong");
    let response = app
        .upload(
            &token,
            "Hợp đồng thi công.DOCX",
            &bytes,
            &[("folder_id", id(&folder))],
        )
        .await;
    let doc = ok_json(response, 201).await;
    assert_eq!(doc["title"], "Hợp đồng thi công");
    assert_eq!(doc["format"], "docx");
    assert_eq!(doc["kind"], "word");
    assert_eq!(doc["folder_id"], folder["id"]);
    assert_eq!(doc["size"], bytes.len());

    let meta = app
        .get_as(&format!("/api/v1/documents/{}", id(&doc)), &token)
        .await;
    assert!(meta.headers().contains_key("etag"));
    assert_eq!(ok_json(meta, 200).await, doc);
    let content = app
        .get_as(&format!("/api/v1/documents/{}/content", id(&doc)), &token)
        .await;
    assert_eq!(content.bytes().await.expect("bytes").as_ref(), bytes);

    let titled = ok_json(
        app.upload(&token, "scan.pdf", &pdf("a"), &[("title", "Bản vẽ")])
            .await,
        201,
    )
    .await;
    assert_eq!(titled["title"], "Bản vẽ");
    assert_eq!(titled["kind"], "pdf");
    let listing = ok_json(
        app.get_as(&format!("/api/v1/folders/{}/children", id(&folder)), &token)
            .await,
        200,
    )
    .await;
    assert_eq!(listing["documents"][0]["id"], doc["id"]);
}

#[tokio::test]
async fn uploads_with_the_wrong_type_or_content_are_refused_and_not_kept() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let ole = {
        let mut b = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
        b.resize(2048, 0);
        b
    };
    let bomb = zip(&[ZipEntry::claiming("word/document.xml", b"tiny", 3 << 30)]);
    let cases: [(&str, Vec<u8>, &str); 6] = [
        ("notes.txt", b"plain text".to_vec(), "UNSUPPORTED_TYPE"),
        ("no-extension", docx("x"), "UNSUPPORTED_TYPE"),
        ("legacy.docx", ole, "UNSUPPORTED_TYPE"),
        ("fake.docx", pdf("x"), "INVALID_SIGNATURE"),
        ("fake.pdf", docx("x"), "INVALID_SIGNATURE"),
        ("bomb.xlsx", bomb, "INVALID_SIGNATURE"),
    ];
    for (name, bytes, code) in cases {
        let response = app.upload(&token, name, &bytes, &[]).await;
        expect_error(response, 422, code, false).await;
    }
    assert_eq!(app.count("SELECT count(*) FROM documents").await, 0);
    assert_eq!(app.count("SELECT count(*) FROM storage_objects").await, 0);
    assert_eq!(
        crate::support::stored_objects(&app.store_dir()),
        0,
        "refused bytes are not kept"
    );
}

#[tokio::test]
async fn malformed_forms_are_invalid_requests() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let send = |content_type: String, body: Vec<u8>| {
        app.request(Method::POST, "/api/v1/documents", &token)
            .header("idempotency-key", new_key())
            .header("content-type", content_type)
            .body(body)
            .send()
    };
    let file = docx("x");
    let cases = [
        multipart(&[("title", "a")], None),
        multipart(&[("title", "a"), ("title", "b")], Some(("a.docx", &file))),
        multipart(&[("other", "a")], Some(("a.docx", &file))),
        multipart(&[("folder_id", "not-a-uuid")], Some(("a.docx", &file))),
        ("multipart/form-data".to_owned(), b"--x--\r\n".to_vec()),
        ("text/plain".to_owned(), b"hello".to_vec()),
        ("application/json".to_owned(), b"{not json".to_vec()),
    ];
    for (content_type, body) in cases {
        let response = send(content_type, body).await.expect("request");
        expect_error(response, 422, "INVALID_REQUEST", false).await;
    }
    let bad_title = app
        .upload(&token, "a.docx", &file, &[("title", "a/b")])
        .await;
    expect_error(bad_title, 422, "INVALID_REQUEST", false).await;
    assert_eq!(app.count("SELECT count(*) FROM documents").await, 0);
}

#[tokio::test]
async fn documents_rename_move_trash_and_restore() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let folder = app.create_folder(&token, None, "Lưu trữ").await;
    let doc = app
        .create_blank(&token, "sheet", Some("Dự toán"), None)
        .await;
    let path = format!("/api/v1/documents/{}", id(&doc));
    let patch = |body: Value| {
        let (app, path, token) = (&app, &path, &token);
        async move { app.send_json(Method::PATCH, path, token, &body).await }
    };

    let renamed = ok_json(patch(json!({ "title": " Dự toán 2026 " })).await, 200).await;
    assert_eq!(renamed["title"], "Dự toán 2026");
    assert_eq!(renamed["version"], 1, "metadata changes make no version");
    let moved = ok_json(patch(json!({ "folder_id": folder["id"] })).await, 200).await;
    assert_eq!(moved["folder_id"], folder["id"]);
    assert_eq!(moved["title"], "Dự toán 2026");
    let same_title = app
        .create_blank(&token, "sheet", Some("Dự toán 2026"), Some(id(&folder)))
        .await;
    assert_eq!(same_title["title"], "Dự toán 2026", "titles are not unique");
    expect_error(
        patch(json!({ "folder_id": uuid::Uuid::new_v4() })).await,
        404,
        "NOT_FOUND",
        false,
    )
    .await;
    expect_error(
        patch(json!({ "title": "" })).await,
        422,
        "INVALID_REQUEST",
        false,
    )
    .await;

    let trashed = app
        .request(Method::DELETE, &path, &token)
        .send()
        .await
        .expect("trash");
    assert_eq!(trashed.status(), 204);
    let meta = ok_json(app.get_as(&path, &token).await, 200).await;
    assert_eq!(meta["state"], "TRASHED");
    assert!(meta["trashed_at"].is_string());
    let trash = ok_json(app.get_as("/api/v1/trash", &token).await, 200).await;
    assert_eq!(trash["documents"][0]["id"], doc["id"]);
    expect_error(
        patch(json!({ "title": "x" })).await,
        404,
        "NOT_FOUND",
        false,
    )
    .await;
    let listing = ok_json(
        app.get_as(&format!("/api/v1/folders/{}/children", id(&folder)), &token)
            .await,
        200,
    )
    .await;
    assert_eq!(listing["documents"].as_array().map(Vec::len), Some(1));

    // Its folder is trashed meanwhile, so the document comes back in the root.
    let folder_path = format!("/api/v1/folders/{}", id(&folder));
    app.request(Method::DELETE, &folder_path, &token)
        .send()
        .await
        .expect("trash folder");
    let restored = ok_json(
        app.request(Method::POST, &format!("{path}/restore"), &token)
            .send()
            .await
            .expect("restore"),
        200,
    )
    .await;
    assert_eq!(restored["state"], "ACTIVE");
    assert_eq!(restored["folder_id"], Value::Null);
    assert_eq!(restored["trashed_at"], Value::Null);
}

#[tokio::test]
async fn versions_are_listed_newest_first_in_pages() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let doc = app.create_blank(&token, "pdf", None, None).await;
    let mut tag = doc_etag(&app, &token, id(&doc)).await;
    for n in 2..=4 {
        let response = app
            .put_content(&token, id(&doc), Some(&tag), pdf(&format!("v{n}")))
            .await;
        tag = etag(&response);
        assert_eq!(ok_json(response, 200).await["version"], n);
    }
    let path = format!("/api/v1/documents/{}/versions", id(&doc));
    let first = ok_json(app.get_as(&format!("{path}?limit=3"), &token).await, 200).await;
    let seqs: Vec<i64> = first["versions"]
        .as_array()
        .expect("versions")
        .iter()
        .map(|v| v["version"].as_i64().expect("version"))
        .collect();
    assert_eq!(seqs, [4, 3, 2]);
    let v = &first["versions"][0];
    assert_eq!(v["author_name"], "Nguyễn Văn An");
    assert_eq!(v["size"], pdf("v4").len());
    assert_eq!(v["sha256"].as_str().map(str::len), Some(64));
    let cursor = first["next_cursor"].as_str().expect("more");
    let rest = ok_json(
        app.get_as(&format!("{path}?limit=3&cursor={cursor}"), &token)
            .await,
        200,
    )
    .await;
    assert_eq!(rest["versions"].as_array().map(Vec::len), Some(1));
    assert_eq!(rest["versions"][0]["version"], 1);
    assert_eq!(rest["next_cursor"], Value::Null);
}

pub async fn doc_etag(app: &TestApp, token: &str, id: &str) -> String {
    etag(&app.get_as(&format!("/api/v1/documents/{id}"), token).await)
}
