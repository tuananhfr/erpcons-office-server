//! `POST /documents`: a multipart upload, or `{kind, title}` for a blank document.
use std::io;

use axum::{
    Extension,
    body::Body,
    extract::{FromRequest, Request, State},
    http::{StatusCode, header::CONTENT_TYPE},
    response::Response,
};
use futures_util::TryStreamExt;
use multer::{Constraints, Multipart, SizeLimit};
use office_documents::{BodyTooLarge, Caller, CreateDocument, Format, Kind, Staged};
use serde_json::json;
use tokio_util::io::StreamReader;
use uuid::Uuid;

use super::respond::{caller, stored_document};
use crate::{
    ApiError, AppState, Authenticated, ErrorCode, RequestId,
    extract::{ApiJson, IdempotencyKey},
    idempotency,
    requests::CreateBlank,
};

const FILE: &str = "file";
const TITLE: &str = "title";
const FOLDER_ID: &str = "folder_id";
/// Room for a 255-character title in any script, and for a uuid.
const TEXT_FIELD_BYTES: u64 = 4096;

pub(crate) async fn create(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    key: IdempotencyKey,
    request: Request,
) -> Result<Response, ApiError> {
    let content_type = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let c = caller(&actor, &rid);
    match mime.as_str() {
        "application/json" => {
            let ApiJson(body) = ApiJson::<CreateBlank>::from_request(request, &state).await?;
            create_blank(&state, c, &key, body).await
        }
        "multipart/form-data" => {
            let boundary =
                multer::parse_boundary(&content_type).map_err(|_| ErrorCode::InvalidRequest)?;
            let upload = read_upload(&state, &c, request.into_body(), boundary).await?;
            create_uploaded(&state, c, &key, upload).await
        }
        _ => Err(ErrorCode::InvalidRequest.into()),
    }
}

async fn create_blank(
    state: &AppState,
    c: Caller<'_>,
    key: &IdempotencyKey,
    body: CreateBlank,
) -> Result<Response, ApiError> {
    let kind = Kind::parse(&body.kind).ok_or(ErrorCode::InvalidRequest)?;
    let hash = idempotency::request_hash("document.create.blank", &body);
    let docs = &state.docs;
    let CreateBlank {
        title, folder_id, ..
    } = body;
    idempotency::run(state, c.actor, key, &hash, move || async move {
        let staged = docs.stage_blank(&c, Format::for_kind(kind)).await?;
        let request = CreateDocument {
            folder_id,
            title,
            file_name: None,
            staged,
        };
        let doc = docs.create_document(&c, request).await?;
        Ok(stored_document(StatusCode::CREATED, &doc))
    })
    .await
}

struct Upload {
    staged: Staged,
    file_name: Option<String>,
    title: Option<String>,
    folder_id: Option<Uuid>,
}

async fn create_uploaded(
    state: &AppState,
    c: Caller<'_>,
    key: &IdempotencyKey,
    upload: Upload,
) -> Result<Response, ApiError> {
    // The bytes are stored by now, so the content is known by its digest.
    let hash = idempotency::request_hash(
        "document.create.upload",
        json!({
            "sha256": upload.staged.sha256.to_hex(),
            "size": upload.staged.size,
            "format": upload.staged.format.extension(),
            "file_name": upload.file_name,
            "title": upload.title,
            "folder_id": upload.folder_id,
        }),
    );
    let docs = &state.docs;
    idempotency::run(state, c.actor, key, &hash, move || async move {
        let request = CreateDocument {
            folder_id: upload.folder_id,
            title: upload.title,
            file_name: upload.file_name,
            staged: upload.staged,
        };
        let doc = docs.create_document(&c, request).await?;
        Ok(stored_document(StatusCode::CREATED, &doc))
    })
    .await
}

/// Reads the form, streaming `file` to storage as it arrives. The format is the file name's
/// extension, then checked against the content.
async fn read_upload(
    state: &AppState,
    c: &Caller<'_>,
    body: Body,
    boundary: String,
) -> Result<Upload, ApiError> {
    let limits = SizeLimit::new()
        .whole_stream(state.config.max_body_bytes)
        .for_field(TITLE, TEXT_FIELD_BYTES)
        .for_field(FOLDER_ID, TEXT_FIELD_BYTES);
    let constraints = Constraints::new()
        .allowed_fields(vec![FILE, TITLE, FOLDER_ID])
        .size_limit(limits);
    let mut form = Multipart::with_constraints(body.into_data_stream(), boundary, constraints);
    let (mut staged, mut file_name, mut title, mut folder_id) = (None, None, None, None);
    while let Some(field) = form.next_field().await.map_err(form_error)? {
        match field.name().unwrap_or_default() {
            FILE if staged.is_none() => {
                file_name = field.file_name().map(str::to_owned);
                let format = file_name
                    .as_deref()
                    .and_then(Format::from_file_name)
                    .ok_or(ErrorCode::UnsupportedType)?;
                let reader = StreamReader::new(field.map_err(body_error));
                staged = Some(state.docs.stage_upload(c, format, Box::pin(reader)).await?);
            }
            TITLE if title.is_none() => title = Some(field.text().await.map_err(form_error)?),
            FOLDER_ID if folder_id.is_none() => {
                let text = field.text().await.map_err(form_error)?;
                folder_id =
                    Some(Uuid::parse_str(text.trim()).map_err(|_| ErrorCode::InvalidRequest)?);
            }
            _ => return Err(ErrorCode::InvalidRequest.into()),
        }
    }
    Ok(Upload {
        staged: staged.ok_or(ErrorCode::InvalidRequest)?,
        file_name,
        title,
        folder_id,
    })
}

fn too_large(e: &multer::Error) -> bool {
    matches!(
        e,
        multer::Error::StreamSizeExceeded { .. } | multer::Error::FieldSizeExceeded { .. }
    )
}

fn form_error(e: multer::Error) -> ApiError {
    if too_large(&e) {
        ErrorCode::TooLarge.into()
    } else {
        ErrorCode::InvalidRequest.into()
    }
}

/// A read error of the file part; the document service recognizes [`BodyTooLarge`].
fn body_error(e: multer::Error) -> io::Error {
    if too_large(&e) {
        io::Error::other(BodyTooLarge)
    } else {
        io::Error::other(e)
    }
}
