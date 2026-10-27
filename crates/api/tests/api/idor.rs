//! T18: another actor's folders and documents look exactly like ids that do not exist.
use reqwest::{Method, RequestBuilder};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::support::{TestApp, docx, multipart, new_key, ok_json};

/// Status and body with the per-request id removed, so two answers can be compared.
async fn answer(request: RequestBuilder) -> (u16, Value) {
    let response = request.send().await.expect("request");
    let status = response.status().as_u16();
    let mut body: Value = response.json().await.expect("JSON body");
    body.as_object_mut().expect("object").remove("request_id");
    (status, body)
}

#[tokio::test]
async fn another_actors_items_answer_404_like_a_random_id() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let owner = app.token().await;
    let folder = app.create_folder(&owner, None, "Bí mật").await;
    let doc = ok_json(
        app.upload(
            &owner,
            "lương.docx",
            &docx("salary"),
            &[("folder_id", folder["id"].as_str().expect("id"))],
        )
        .await,
        201,
    )
    .await;
    let trashed = app.create_blank(&owner, "word", Some("Đã xoá"), None).await;
    let trashed_id = trashed["id"].as_str().expect("id").to_owned();
    app.request(
        Method::DELETE,
        &format!("/api/v1/documents/{trashed_id}"),
        &owner,
    )
    .send()
    .await
    .expect("trash");
    let owner_etag =
        crate::documents::doc_etag(&app, &owner, doc["id"].as_str().expect("id")).await;

    let intruder = app.token_of("usr_mock_binh", "Trần Thị Bình").await;
    let own_folder = app.create_folder(&intruder, None, "Của tôi").await;
    let own_doc = app.create_blank(&intruder, "word", None, None).await;
    let (own_folder_id, own_doc_id) = (
        own_folder["id"].as_str().expect("id").to_owned(),
        own_doc["id"].as_str().expect("id").to_owned(),
    );

    let folder_id = folder["id"].as_str().expect("id").to_owned();
    let doc_id = doc["id"].as_str().expect("id").to_owned();
    let random = Uuid::new_v4().to_string();
    let file = docx("intruder");
    let requests = |f: &str, d: &str| -> Vec<(String, RequestBuilder)> {
        let t = intruder.as_str();
        let (form_type, form) = multipart(&[("folder_id", f)], Some(("x.docx", &file)));
        vec![
            (
                "children".into(),
                app.request(Method::GET, &format!("/api/v1/folders/{f}/children"), t),
            ),
            (
                "rename folder".into(),
                app.request(Method::PATCH, &format!("/api/v1/folders/{f}"), t)
                    .json(&json!({ "name": "x" })),
            ),
            (
                "move own folder into it".into(),
                app.request(
                    Method::PATCH,
                    &format!("/api/v1/folders/{own_folder_id}"),
                    t,
                )
                .json(&json!({ "parent_id": f })),
            ),
            (
                "trash folder".into(),
                app.request(Method::DELETE, &format!("/api/v1/folders/{f}"), t),
            ),
            (
                "restore folder".into(),
                app.request(Method::POST, &format!("/api/v1/folders/{f}/restore"), t),
            ),
            (
                "create folder in it".into(),
                app.request(Method::POST, "/api/v1/folders", t)
                    .header("idempotency-key", new_key())
                    .json(&json!({ "parent_id": f, "name": "x" })),
            ),
            (
                "create blank in it".into(),
                app.request(Method::POST, "/api/v1/documents", t)
                    .header("idempotency-key", new_key())
                    .json(&json!({ "kind": "word", "folder_id": f })),
            ),
            (
                "upload into it".into(),
                app.request(Method::POST, "/api/v1/documents", t)
                    .header("idempotency-key", new_key())
                    .header("content-type", form_type)
                    .body(form),
            ),
            (
                "get document".into(),
                app.request(Method::GET, &format!("/api/v1/documents/{d}"), t),
            ),
            (
                "rename document".into(),
                app.request(Method::PATCH, &format!("/api/v1/documents/{d}"), t)
                    .json(&json!({ "title": "x" })),
            ),
            (
                "move own document into the folder".into(),
                app.request(Method::PATCH, &format!("/api/v1/documents/{own_doc_id}"), t)
                    .json(&json!({ "folder_id": f })),
            ),
            (
                "trash document".into(),
                app.request(Method::DELETE, &format!("/api/v1/documents/{d}"), t),
            ),
            (
                "restore document".into(),
                app.request(Method::POST, &format!("/api/v1/documents/{d}/restore"), t),
            ),
            (
                "content".into(),
                app.request(Method::GET, &format!("/api/v1/documents/{d}/content"), t),
            ),
            (
                "save".into(),
                app.request(Method::PUT, &format!("/api/v1/documents/{d}/content"), t)
                    .header("if-match", owner_etag.as_str())
                    .body(docx("overwrite")),
            ),
            (
                "versions".into(),
                app.request(Method::GET, &format!("/api/v1/documents/{d}/versions"), t),
            ),
        ]
    };
    let foreign = requests(&folder_id, &doc_id);
    let missing = requests(&random, &random);
    for ((label, theirs), (_, nothing)) in foreign.into_iter().zip(missing) {
        let (status, body) = answer(theirs).await;
        assert_eq!(status, 404, "{label}: {body}");
        assert_eq!(body["code"], "NOT_FOUND", "{label}");
        assert_eq!(answer(nothing).await, (status, body), "{label}");
    }
    for (method, path) in [
        (Method::GET, format!("/api/v1/documents/{trashed_id}")),
        (
            Method::POST,
            format!("/api/v1/documents/{trashed_id}/restore"),
        ),
    ] {
        let (status, _) = answer(app.request(method, &path, &intruder)).await;
        assert_eq!(status, 404, "{path}");
    }

    let trash = ok_json(app.get_as("/api/v1/trash", &intruder).await, 200).await;
    assert_eq!(trash["documents"], json!([]));
    let root = ok_json(
        app.get_as("/api/v1/folders/root/children", &intruder).await,
        200,
    )
    .await;
    assert_eq!(root["folders"].as_array().map(Vec::len), Some(1));
    assert_eq!(root["folders"][0]["id"], own_folder["id"]);

    // Nothing of the owner's changed.
    let after = ok_json(
        app.get_as(&format!("/api/v1/documents/{doc_id}"), &owner)
            .await,
        200,
    )
    .await;
    assert_eq!(after, doc);
    let folder_after = ok_json(
        app.get_as(&format!("/api/v1/folders/{folder_id}/children"), &owner)
            .await,
        200,
    )
    .await;
    assert_eq!(folder_after["folders"], json!([]));
    assert_eq!(folder_after["documents"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        app.count("SELECT count(*) FROM document_versions").await,
        3,
        "no version was added"
    );
}
