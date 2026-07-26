//! JSON body extractor whose rejections stay inside the API error envelope.

use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, Request};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::de::DeserializeOwned;

use crate::error::error_envelope;

/// `axum::Json`, with the refusal rendered like every other API failure.
///
/// An endpoint that refuses a body — because it carries a field the endpoint
/// does not implement, for instance — only helps the caller if the refusal is
/// readable. `axum::Json` answers its own rejections in `text/plain`, so a
/// client parsing the documented envelope gets a parse failure instead of a
/// reason. This wrapper changes the body only: the status code stays the one
/// `axum` chose for that rejection, so no existing failure mode moves.
pub struct ContractJson<T>(pub T);

impl<T, S> FromRequest<S> for ContractJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ContractJsonRejection;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let axum::Json(value) = axum::Json::<T>::from_request(req, state).await?;
        Ok(Self(value))
    }
}

/// A [`JsonRejection`] answered in the API error envelope.
#[derive(Debug)]
pub struct ContractJsonRejection(JsonRejection);

impl From<JsonRejection> for ContractJsonRejection {
    fn from(rejection: JsonRejection) -> Self {
        Self(rejection)
    }
}

impl ContractJsonRejection {
    /// The error code carried in the envelope, derived from the status `axum`
    /// already assigns to the rejection. Deriving it from the status rather
    /// than matching the rejection variants keeps this mapping total: a variant
    /// added by a future `axum` still produces a coherent envelope instead of
    /// failing to compile or falling through to a wrong code.
    fn code(status: StatusCode) -> &'static str {
        match status {
            StatusCode::UNPROCESSABLE_ENTITY => "VALIDATION_ERROR",
            StatusCode::UNSUPPORTED_MEDIA_TYPE => "UNSUPPORTED_MEDIA_TYPE",
            StatusCode::PAYLOAD_TOO_LARGE => "PAYLOAD_TOO_LARGE",
            _ => "BAD_REQUEST",
        }
    }
}

impl IntoResponse for ContractJsonRejection {
    fn into_response(self) -> Response {
        let status = self.0.status();
        error_envelope(status, Self::code(status), self.0.body_text())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::Body;
    use axum::http::{header, Request as HttpRequest};
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Strict {
        kept: Option<bool>,
    }

    async fn reject(
        body: &'static str,
        content_type: Option<&'static str>,
    ) -> (StatusCode, String) {
        let mut request = HttpRequest::builder().method("PUT").uri("/probe");
        if let Some(content_type) = content_type {
            request = request.header(header::CONTENT_TYPE, content_type);
        }
        let request = request
            .body(Body::from(body))
            .expect("probe request must build");

        let rejection = ContractJson::<Strict>::from_request(request, &())
            .await
            .err()
            .expect("the probe body must be refused");

        let response = rejection.into_response();
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("rejection body must read");

        assert!(
            content_type.starts_with("application/json"),
            "a refusal must be machine-readable, got {content_type}"
        );
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tokio::test]
    async fn an_unknown_field_is_refused_in_the_error_envelope() {
        let (status, body) =
            reject(r#"{"kept":true,"dropped":["x"]}"#, Some("application/json")).await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let body: serde_json::Value = serde_json::from_str(&body).expect("envelope must be JSON");
        assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("dropped"),
            "the refusal names the offending field: {body}"
        );
    }

    #[tokio::test]
    async fn malformed_json_keeps_the_status_axum_assigns() {
        let (status, body) = reject("{not json", Some("application/json")).await;

        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "a syntax error stays a 400, only its body shape changes"
        );
        let body: serde_json::Value = serde_json::from_str(&body).expect("envelope must be JSON");
        assert_eq!(body["error"]["code"], "BAD_REQUEST");
    }

    #[tokio::test]
    async fn a_missing_content_type_keeps_its_own_status() {
        let (status, body) = reject(r#"{"kept":true}"#, None).await;

        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        let body: serde_json::Value = serde_json::from_str(&body).expect("envelope must be JSON");
        assert_eq!(body["error"]["code"], "UNSUPPORTED_MEDIA_TYPE");
    }
}
