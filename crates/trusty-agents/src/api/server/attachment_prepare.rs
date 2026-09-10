//! Authenticated, side-effect-free attachment normalization (#7370).
use axum::{Json, http::StatusCode};
use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};
use trusty_common::chat_attachments::{
    self as model, Attachment, AttachmentError, ImageBytes, InputAttachment,
};

pub(super) const MAX_BODY_BYTES: usize = 15 * 1024 * 1024;
type Error = (StatusCode, Json<Value>);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    items: Vec<Item>,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Item {
    File {
        name: String,
        mime_type: String,
        data_base64: String,
    },
    Clipboard {
        name: String,
        format: String,
        text: String,
    },
}
pub(super) fn error(error: AttachmentError) -> Error {
    let status = match error {
        AttachmentError::Malformed(_) => StatusCode::BAD_REQUEST,
        AttachmentError::TooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
        AttachmentError::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
    };
    (status, Json(json!({"error":error.to_string()})))
}

pub(super) fn json_error(rejection: axum::extract::rejection::JsonRejection) -> Error {
    let status = if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
        StatusCode::PAYLOAD_TOO_LARGE
    } else {
        StatusCode::BAD_REQUEST
    };
    (status, Json(json!({"error":rejection.body_text()})))
}

/// Why: headless API callers must prepare the same bounded input as the GUI.
/// What: validate raw content, parse with the document owner, return canonical attachments without writes.
/// Test: `prepare_route_accepts_csv_and_rejects_invalid_images`.
pub(super) async fn prepare(
    request: Result<Json<Request>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, Error> {
    let Json(request) = request.map_err(json_error)?;
    let prepared = tokio::task::spawn_blocking(move || normalize(request))
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":"Attachment preparation failed"})),
            )
        })?
        .map_err(error)?;
    Ok(Json(json!({"attachments":prepared})))
}

fn normalize(request: Request) -> Result<Vec<InputAttachment>, AttachmentError> {
    let invalid = |text: &str| AttachmentError::Invalid(text.into());
    if request.items.is_empty() {
        return Err(invalid("Provide at least one attachment"));
    }
    if request.items.len() > model::MAX_ATTACHMENTS {
        return Err(AttachmentError::TooLarge(
            "At most four attachments are allowed".into(),
        ));
    }
    let mut result = Vec::new();
    let mut total: usize = 0;
    for item in request.items {
        let (name, format, bytes, image) = match item {
            Item::Clipboard { name, format, text } => {
                let format = match format.as_str() {
                    "html" => "clipboard-html",
                    "tsv" => "clipboard-tsv",
                    _ => return Err(invalid("Unsupported clipboard format")),
                };
                (name, format, text.into_bytes(), None)
            }
            Item::File {
                name,
                mime_type,
                data_base64,
            } => {
                if data_base64.len() > model::MAX_FILE_BYTES.div_ceil(3) * 4 {
                    return Err(AttachmentError::TooLarge("Attachment exceeds 5 MiB".into()));
                }
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(&data_base64)
                    .map_err(|_| AttachmentError::Malformed("Invalid attachment base64".into()))?;
                let format = match mime_type.as_str() {
                    "image/png" | "image/jpeg" | "image/webp" => "image",
                    "text/csv" if name.to_ascii_lowercase().ends_with(".csv") => "csv",
                    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
                        if name.to_ascii_lowercase().ends_with(".xlsx") =>
                    {
                        "xlsx"
                    }
                    _ => return Err(invalid("Unsupported file MIME type or extension")),
                };
                let image = (format == "image").then_some((mime_type, data_base64));
                (name, format, bytes, image)
            }
        };
        if bytes.len() > model::MAX_FILE_BYTES {
            return Err(AttachmentError::TooLarge("Attachment exceeds 5 MiB".into()));
        }
        total = total.saturating_add(bytes.len());
        if total > model::MAX_TOTAL_BYTES {
            return Err(AttachmentError::TooLarge(
                "Attachments exceed 10 MiB total".into(),
            ));
        }
        result.push(if let Some((mime_type, data_base64)) = image {
            model::decode_image(&mime_type, &data_base64)?;
            Attachment::Image {
                name,
                mime_type,
                image: ImageBytes { data_base64 },
            }
        } else {
            trusty_search::core::extract::chat_tables::prepare(&name, format, &bytes)?
        });
    }
    model::validate_inputs(&result)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Body, http::Request};
    use tower::ServiceExt;
    #[tokio::test]
    async fn attachment_errors_are_structured() {
        let router = Router::new()
            .route("/prepare", axum::routing::post(prepare))
            .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY_BYTES));
        let cases = [
            (json!({"items":[],"unknown":true}).to_string(), StatusCode::BAD_REQUEST),
            (json!({"items":[{"kind":"file","name":"a.png","mime_type":"image/png","data_base64":"%%%"}]}).to_string(), StatusCode::BAD_REQUEST),
            (json!({"items":[{"kind":"clipboard","name":"a","format":"html","text":"no table"}]}).to_string(), StatusCode::UNPROCESSABLE_ENTITY),
            (json!({"items":[{"kind":"clipboard","name":"a","format":"tsv","text":"x".repeat(model::MAX_FILE_BYTES+1)}]}).to_string(), StatusCode::PAYLOAD_TOO_LARGE),
            (" ".repeat(MAX_BODY_BYTES+1), StatusCode::PAYLOAD_TOO_LARGE),
        ];
        for (body, expected) in cases {
            let response = router
                .clone()
                .oneshot(
                    Request::post("/prepare")
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
            let value: Value = serde_json::from_slice(
                &axum::body::to_bytes(response.into_body(), 4096)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert!(value["error"].is_string());
        }
    }
    #[tokio::test]
    async fn prepare_route_accepts_csv_and_rejects_invalid_images() {
        let router = Router::new().route("/prepare", axum::routing::post(prepare));
        for (mime, name, content, expected) in [
            ("text/csv", "a.csv", "Name,Role\nMaya,Lead", StatusCode::OK),
            (
                "image/png",
                "a.png",
                "not an image",
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
        ] {
            let value = json!({"items":[{"kind":"file","name":name,"mime_type":mime,"data_base64":base64::engine::general_purpose::STANDARD.encode(content)}]});
            let response = router
                .clone()
                .oneshot(
                    Request::post("/prepare")
                        .header("content-type", "application/json")
                        .body(Body::from(value.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
            if expected == StatusCode::OK {
                let bytes = axum::body::to_bytes(response.into_body(), MAX_BODY_BYTES)
                    .await
                    .unwrap();
                let value: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(
                    value["attachments"][0]["sheets"][0]["rows"][1],
                    json!(["Maya", "Lead"])
                );
            }
        }
    }
}
