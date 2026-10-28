use reqwest::Method;
use serde_json::{Value, json};

use crate::support::{TestApp, expect_error, ok_json};

fn id(v: &Value) -> &str {
    v["id"].as_str().expect("id")
}

fn names(listing: &Value, key: &str, field: &str) -> Vec<String> {
    listing[key]
        .as_array()
        .expect("array")
        .iter()
        .map(|e| e[field].as_str().expect("name").to_owned())
        .collect()
}

async fn children(app: &TestApp, token: &str, folder: &str, query: &str) -> Value {
    let path = format!("/api/v1/folders/{folder}/children{query}");
    ok_json(app.get_as(&path, token).await, 200).await
}

#[tokio::test]
async fn created_folders_are_listed_folders_first_by_name_across_pages() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let created = app.create_folder(&token, None, "beta").await;
    assert_eq!(created["name"], "beta");
    assert_eq!(created["parent_id"], Value::Null);
    assert_eq!(created["state"], "ACTIVE");
    assert_eq!(created["trashed_at"], Value::Null);
    app.create_folder(&token, None, "Alpha").await;
    app.create_folder(&token, None, "gamma").await;
    app.create_blank(&token, "word", Some("zeta"), None).await;
    app.create_blank(&token, "sheet", Some("Delta"), None).await;

    let mut folders = Vec::new();
    let mut documents = Vec::new();
    let mut query = "?limit=2".to_owned();
    let mut pages = 0;
    loop {
        let page = children(&app, &token, "root", &query).await;
        pages += 1;
        let (f, d) = (
            names(&page, "folders", "name"),
            names(&page, "documents", "title"),
        );
        assert!(f.len() + d.len() <= 2, "{page}");
        assert!(
            f.is_empty() || documents.is_empty(),
            "folders come before documents"
        );
        folders.extend(f);
        documents.extend(d);
        match page["next_cursor"].as_str() {
            Some(cursor) => query = format!("?limit=2&cursor={cursor}"),
            None => break,
        }
    }
    assert_eq!(folders, ["Alpha", "beta", "gamma"]);
    assert_eq!(documents, ["Delta", "zeta"]);
    assert_eq!(pages, 3);
}

#[tokio::test]
async fn nested_folders_have_their_own_children() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let parent = app.create_folder(&token, None, "Dự án").await;
    let child = app.create_folder(&token, Some(id(&parent)), "Hồ sơ").await;
    assert_eq!(child["parent_id"], parent["id"]);
    let listing = children(&app, &token, id(&parent), "").await;
    assert_eq!(names(&listing, "folders", "name"), ["Hồ sơ"]);
    assert_eq!(listing["next_cursor"], Value::Null);
    let root = children(&app, &token, "root", "").await;
    assert_eq!(names(&root, "folders", "name"), ["Dự án"]);

    let missing = format!("/api/v1/folders/{}/children", uuid::Uuid::new_v4());
    expect_error(app.get_as(&missing, &token).await, 404, "NOT_FOUND", false).await;
    let body = json!({ "parent_id": uuid::Uuid::new_v4(), "name": "x" });
    expect_error(
        app.post_json("/api/v1/folders", &token, &body).await,
        404,
        "NOT_FOUND",
        false,
    )
    .await;
}

#[tokio::test]
async fn names_are_unique_per_parent_ignoring_case_and_normalization() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let first = app.create_folder(&token, None, "Hợp đồng").await;
    let decomposed = "Ho\u{031B}\u{0323}p đo\u{0302}\u{0300}ng";
    for name in ["hợp đồng", "HỢP ĐỒNG", decomposed, "  Hợp đồng  "] {
        let response = app
            .post_json("/api/v1/folders", &token, &json!({ "name": name }))
            .await;
        expect_error(response, 409, "NAME_TAKEN", false).await;
    }
    // Stored NFC and trimmed.
    let other = app
        .create_folder(&token, None, &format!(" {decomposed}2 "))
        .await;
    assert_eq!(other["name"], "Hợp đồng2");
    let rename = app
        .send_json(
            Method::PATCH,
            &format!("/api/v1/folders/{}", id(&other)),
            &token,
            &json!({ "name": "HỢP ĐỒNG" }),
        )
        .await;
    expect_error(rename, 409, "NAME_TAKEN", false).await;

    let inside = app
        .create_folder(&token, Some(id(&first)), "hợp đồng")
        .await;
    assert_eq!(inside["parent_id"], first["id"], "another parent is fine");

    let trash = format!("/api/v1/folders/{}", id(&other));
    assert_eq!(
        app.request(Method::DELETE, &trash, &token)
            .send()
            .await
            .expect("trash")
            .status(),
        204
    );
    app.create_folder(&token, None, "Hợp đồng2").await;

    for bad in [
        "",
        "   ",
        "a/b",
        "a\\b",
        "x\u{202E}y",
        "tab\there",
        "nul\u{0}",
    ] {
        let response = app
            .post_json("/api/v1/folders", &token, &json!({ "name": bad }))
            .await;
        expect_error(response, 422, "INVALID_REQUEST", false).await;
    }
}

#[tokio::test]
async fn folders_rename_and_move_but_never_into_themselves() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let a = app.create_folder(&token, None, "A").await;
    let b = app.create_folder(&token, Some(id(&a)), "B").await;
    let c = app.create_folder(&token, Some(id(&b)), "C").await;
    let other = app.create_folder(&token, None, "Other").await;

    let patch = |folder: &Value, body: Value| {
        let path = format!("/api/v1/folders/{}", id(folder));
        let token = token.clone();
        let app = &app;
        async move { app.send_json(Method::PATCH, &path, &token, &body).await }
    };

    let renamed = ok_json(patch(&b, json!({ "name": "B2" })).await, 200).await;
    assert_eq!(renamed["name"], "B2");
    assert_eq!(renamed["parent_id"], a["id"], "a rename keeps the parent");

    for target in [&a, &b, &c] {
        let cycle = patch(&a, json!({ "parent_id": target["id"] })).await;
        expect_error(cycle, 422, "FOLDER_CYCLE", false).await;
    }

    let moved = ok_json(patch(&b, json!({ "parent_id": other["id"] })).await, 200).await;
    assert_eq!(moved["parent_id"], other["id"]);
    let listing = children(&app, &token, id(&other), "").await;
    assert_eq!(names(&listing, "folders", "name"), ["B2"]);

    let to_root = ok_json(patch(&b, json!({ "parent_id": null })).await, 200).await;
    assert_eq!(to_root["parent_id"], Value::Null);
    let untouched = ok_json(patch(&b, json!({ "name": "B3" })).await, 200).await;
    assert_eq!(
        untouched["parent_id"],
        Value::Null,
        "an absent parent_id is no move"
    );
    let empty = patch(&b, json!({})).await;
    expect_error(empty, 422, "INVALID_REQUEST", false).await;

    let nowhere = patch(&b, json!({ "parent_id": uuid::Uuid::new_v4() })).await;
    expect_error(nowhere, 404, "NOT_FOUND", false).await;
}

#[tokio::test]
async fn trash_is_recursive_and_restore_brings_the_tree_back() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let a = app.create_folder(&token, None, "A").await;
    let b = app.create_folder(&token, Some(id(&a)), "B").await;
    let doc = app
        .create_blank(&token, "word", Some("Báo cáo"), Some(id(&b)))
        .await;
    let doc_path = format!("/api/v1/documents/{}", id(&doc));

    let response = app
        .request(
            Method::DELETE,
            &format!("/api/v1/folders/{}", id(&a)),
            &token,
        )
        .send()
        .await
        .expect("trash");
    assert_eq!(response.status(), 204);
    let trashed_doc = ok_json(app.get_as(&doc_path, &token).await, 200).await;
    assert_eq!(trashed_doc["state"], "TRASHED");
    let children_of_b = format!("/api/v1/folders/{}/children", id(&b));
    expect_error(
        app.get_as(&children_of_b, &token).await,
        404,
        "NOT_FOUND",
        false,
    )
    .await;
    let root = children(&app, &token, "root", "").await;
    assert_eq!(names(&root, "folders", "name"), Vec::<String>::new());

    let trash = ok_json(app.get_as("/api/v1/trash", &token).await, 200).await;
    assert_eq!(names(&trash, "folders", "name"), ["A"], "only the top item");
    assert_eq!(names(&trash, "documents", "title"), Vec::<String>::new());
    assert_eq!(trash["folders"][0]["state"], "TRASHED");
    assert!(trash["folders"][0]["trashed_at"].is_string());

    let restore = format!("/api/v1/folders/{}/restore", id(&a));
    let restored = ok_json(
        app.request(Method::POST, &restore, &token)
            .send()
            .await
            .expect("restore"),
        200,
    )
    .await;
    assert_eq!(restored["state"], "ACTIVE");
    assert_eq!(restored["parent_id"], Value::Null);
    let doc_again = ok_json(app.get_as(&doc_path, &token).await, 200).await;
    assert_eq!(doc_again["state"], "ACTIVE");
    assert_eq!(doc_again["folder_id"], b["id"]);
    let listing = children(&app, &token, id(&b), "").await;
    assert_eq!(names(&listing, "documents", "title"), ["Báo cáo"]);
    let trash = ok_json(app.get_as("/api/v1/trash", &token).await, 200).await;
    assert_eq!(trash["folders"], json!([]));
}

#[tokio::test]
async fn an_item_trashed_on_its_own_stays_trashed_when_its_parent_is_restored() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let a = app.create_folder(&token, None, "A").await;
    let b = app.create_folder(&token, Some(id(&a)), "B").await;
    let trash = |folder: &Value| {
        app.request(
            Method::DELETE,
            &format!("/api/v1/folders/{}", id(folder)),
            &token,
        )
        .send()
    };
    assert_eq!(trash(&b).await.expect("trash b").status(), 204);
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    assert_eq!(trash(&a).await.expect("trash a").status(), 204);
    let listed = ok_json(app.get_as("/api/v1/trash", &token).await, 200).await;
    assert_eq!(names(&listed, "folders", "name"), ["A", "B"]);

    let restore = |folder: &Value| {
        app.request(
            Method::POST,
            &format!("/api/v1/folders/{}/restore", id(folder)),
            &token,
        )
        .send()
    };
    assert_eq!(restore(&a).await.expect("restore a").status(), 200);
    let inside = children(&app, &token, id(&a), "").await;
    assert_eq!(inside["folders"], json!([]), "B was trashed before A");
    let b_back = ok_json(restore(&b).await.expect("restore b"), 200).await;
    assert_eq!(b_back["parent_id"], a["id"]);
}

#[tokio::test]
async fn restore_goes_to_the_root_when_the_parent_is_trashed() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let p = app.create_folder(&token, None, "P").await;
    let c = app.create_folder(&token, Some(id(&p)), "C").await;
    for folder in [&c, &p] {
        let response = app
            .request(
                Method::DELETE,
                &format!("/api/v1/folders/{}", id(folder)),
                &token,
            )
            .send()
            .await
            .expect("trash");
        assert_eq!(response.status(), 204);
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let restore = format!("/api/v1/folders/{}/restore", id(&c));
    let back = ok_json(
        app.request(Method::POST, &restore, &token)
            .send()
            .await
            .expect("restore"),
        200,
    )
    .await;
    assert_eq!(back["parent_id"], Value::Null);
    let root = children(&app, &token, "root", "").await;
    assert_eq!(names(&root, "folders", "name"), ["C"]);

    // A root folder of the same name blocks the next restore into the root.
    let q = app.create_folder(&token, None, "Q").await;
    let q2 = app.create_folder(&token, Some(id(&q)), "C2").await;
    for folder in [&q2, &q] {
        app.request(
            Method::DELETE,
            &format!("/api/v1/folders/{}", id(folder)),
            &token,
        )
        .send()
        .await
        .expect("trash");
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    app.create_folder(&token, None, "c2").await;
    let blocked = format!("/api/v1/folders/{}/restore", id(&q2));
    let response = app
        .request(Method::POST, &blocked, &token)
        .send()
        .await
        .expect("restore");
    expect_error(response, 409, "NAME_TAKEN", false).await;
}

#[tokio::test]
async fn repeated_trash_and_restore_are_harmless() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let f = app.create_folder(&token, None, "F").await;
    let path = format!("/api/v1/folders/{}", id(&f));
    for _ in 0..2 {
        let response = app
            .request(Method::DELETE, &path, &token)
            .send()
            .await
            .expect("trash");
        assert_eq!(response.status(), 204);
    }
    for _ in 0..2 {
        let response = app
            .request(Method::POST, &format!("{path}/restore"), &token)
            .send()
            .await
            .expect("restore");
        assert_eq!(ok_json(response, 200).await["state"], "ACTIVE");
    }
}

#[tokio::test]
async fn folder_events_are_audited_with_the_folder_id() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let f = app.create_folder(&token, None, "F").await;
    app.create_folder(&token, None, "G").await;
    let path = format!("/api/v1/folders/{}", id(&f));
    let rename = |name: &'static str| {
        let (app, path, token) = (&app, &path, &token);
        async move {
            app.send_json(Method::PATCH, path, token, &json!({ "name": name }))
                .await
        }
    };
    assert_eq!(rename("F2").await.status(), 200);
    assert_eq!(rename("g").await.status(), 409);
    let trash = app.request(Method::DELETE, &path, &token).send();
    assert_eq!(trash.await.expect("trash").status(), 204);
    let restore = app
        .request(Method::POST, &format!("{path}/restore"), &token)
        .send();
    assert_eq!(restore.await.expect("restore").status(), 200);

    let rows: Vec<(String, String, Option<uuid::Uuid>)> = sqlx::query_as(
        "SELECT action, result, document_id FROM audit_events
         WHERE folder_id = $1::uuid ORDER BY at, id",
    )
    .bind(id(&f))
    .fetch_all(app.pool())
    .await
    .expect("audit rows");
    let expected = [
        ("folder.create", "success"),
        ("folder.update", "success"),
        ("folder.update", "error"),
        ("folder.trash", "success"),
        ("folder.restore", "success"),
    ];
    assert_eq!(rows.len(), expected.len(), "{rows:?}");
    for ((action, result, document), (want_action, want_result)) in rows.iter().zip(expected) {
        assert_eq!(
            (action.as_str(), result.as_str()),
            (want_action, want_result)
        );
        assert_eq!(*document, None);
    }
    let unlabelled = app
        .count(
            "SELECT count(*) FROM audit_events WHERE action LIKE 'folder.%' AND folder_id IS NULL",
        )
        .await;
    assert_eq!(unlabelled, 0);
}

#[tokio::test]
async fn a_folder_comes_with_its_ancestors_from_the_top() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let a = app.create_folder(&token, None, "A").await;
    let b = app.create_folder(&token, Some(id(&a)), "B").await;
    let c = app.create_folder(&token, Some(id(&b)), "C").await;
    let get = |folder: &str| {
        let (app, token) = (&app, &token);
        let path = format!("/api/v1/folders/{folder}");
        async move { app.get_as(&path, token).await }
    };

    let path = ok_json(get(id(&c)).await, 200).await;
    assert_eq!(path["folder"], c);
    let ancestors: Vec<&str> = path["ancestors"]
        .as_array()
        .expect("ancestors")
        .iter()
        .map(|f| f["name"].as_str().expect("name"))
        .collect();
    assert_eq!(ancestors, ["A", "B"]);
    assert_eq!(path["ancestors"][0], a);
    let top = ok_json(get(id(&a)).await, 200).await;
    assert_eq!(top["ancestors"], json!([]), "the root is not listed");

    expect_error(get("root").await, 422, "INVALID_REQUEST", false).await;
    let random = uuid::Uuid::new_v4().to_string();
    expect_error(get(&random).await, 404, "NOT_FOUND", false).await;
    app.trash(&token, &format!("/api/v1/folders/{}", id(&b)))
        .await;
    for trashed in [id(&b), id(&c)] {
        expect_error(get(trashed).await, 404, "NOT_FOUND", false).await;
    }
}
