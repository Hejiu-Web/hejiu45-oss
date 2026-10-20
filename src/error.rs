//! Error types shared across the storage stack.
//!
//! Two faces are needed:
//! * [`S3Error`] renders an AWS-compatible XML error document with the right
//!   HTTP status, so stock SDKs and `aws s3` CLI behave correctly.
//! * [`ApiError`] renders a small JSON envelope for the console's `/api`.
//!
//! Both are deliberately exhaustive about *what leaked*: internal errors are
//! logged with detail but returned to clients as a generic code so filesystem
//! paths and database internals never reach the wire.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::json;

/// An S3 protocol error.
#[derive(Debug, Clone)]
pub struct S3Error {
    pub code: &'static str,
    pub message: String,
    pub status: StatusCode,
}

impl S3Error {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            status,
        }
    }

    pub fn no_such_bucket(bucket: &str) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "NoSuchBucket",
            format!("The specified bucket does not exist: {bucket}"),
        )
    }

    pub fn no_such_key(key: &str) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "NoSuchKey",
            format!("The specified key does not exist: {key}"),
        )
    }

    pub fn bucket_not_empty() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "BucketNotEmpty",
            "The bucket you tried to delete is not empty.",
        )
    }

    pub fn bucket_already_exists(bucket: &str) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "BucketAlreadyOwnedByYou",
            format!("Your previous request to create the named bucket succeeded: {bucket}"),
        )
    }

    pub fn invalid_bucket_name(bucket: &str) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "InvalidBucketName",
            format!("The specified bucket is not valid: {bucket}"),
        )
    }

    pub fn signature_mismatch() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "SignatureDoesNotMatch",
            "The request signature we calculated does not match the signature you provided.",
        )
    }

    pub fn missing_signature() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "AccessDenied",
            "Anonymous access is not allowed. Sign the request with AWS Signature Version 4.",
        )
    }

    pub fn invalid_access_key(key: &str) -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "InvalidAccessKeyId",
            format!("The access key {key} does not exist."),
        )
    }

    pub fn access_denied(reason: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "AccessDenied", reason)
    }

    pub fn expired_presigned() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "AccessDenied",
            "Request has expired.",
        )
    }

    pub fn invalid_request(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "InvalidRequest", msg)
    }

    pub fn malformed_xml(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "MalformedXML", msg)
    }

    pub fn invalid_argument(name: &str, value: &str) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "InvalidArgument",
            format!("Invalid argument: {name}={value}"),
        )
    }

    pub fn internal(detail: impl std::fmt::Display) -> Self {
        tracing::error!(error = %detail, "internal S3 error");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "InternalError",
            "We encountered an internal error. Please try again.",
        )
    }

    pub fn too_large() -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "EntityTooLarge",
            "Your proposed upload exceeds the maximum allowed object size.",
        )
    }

    pub fn not_implemented(op: &str) -> Self {
        Self::new(
            StatusCode::NOT_IMPLEMENTED,
            "NotImplemented",
            format!("A header or operation you provided implies functionality that is not implemented: {op}"),
        )
    }

    /// Serialize the AWS error document.
    pub fn to_xml(&self, request_id: &str, resource: &str) -> String {
        let esc = |s: &str| xml_escape(s);
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <Error><Code>{}</Code><Message>{}</Message><Resource>{}</Resource><RequestId>{}</RequestId></Error>",
            esc(self.code),
            esc(&self.message),
            esc(resource),
            esc(request_id)
        )
    }
}

impl IntoResponse for S3Error {
    fn into_response(self) -> Response {
        let request_id = uuid::Uuid::new_v4().to_string();
        if self.status.is_server_error() {
            tracing::error!(code = self.code, msg = %self.message, "s3 request failed");
        } else {
            tracing::debug!(code = self.code, msg = %self.message, "s3 request rejected");
        }
        let body = self.to_xml(&request_id, "");
        (
            self.status,
            [
                ("content-type", "application/xml"),
                ("x-amz-request-id", request_id.as_str()),
            ],
            body,
        )
            .into_response()
    }
}

/// Public XML escaping for response bodies built elsewhere (e.g. S3 responses
/// that embed user-controlled keys, prefixes and metadata).
pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // XML 1.0 forbids most control characters outright. Object keys are
            // arbitrary bytes as far as S3 is concerned; replace them rather
            // than emit an unparseable document.
            c if (c as u32) < 0x20 && c != '\n' && c != '\r' && c != '\t' => out.push('\u{FFFD}'),
            c => out.push(c),
        }
    }
    out
}

/// Error envelope for the console `/api`.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

#[derive(Serialize)]
struct ApiErrorBody<'a> {
    error: ApiErrorDetail<'a>,
}

#[derive(Serialize)]
struct ApiErrorDetail<'a> {
    code: &'a str,
    message: &'a str,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "BadRequest", msg)
    }

    pub fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "Unauthorized",
            "Authentication required.",
        )
    }

    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "Forbidden", msg)
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "NotFound", msg)
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "Conflict", msg)
    }

    pub fn too_many_requests(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::TOO_MANY_REQUESTS, "TooManyRequests", msg)
    }

    pub fn internal(detail: impl std::fmt::Display) -> Self {
        tracing::error!(error = %detail, "internal API error");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "InternalError",
            "Internal server error.",
        )
    }
}

impl From<S3Error> for ApiError {
    fn from(e: S3Error) -> Self {
        let code = match e.code {
            "NoSuchBucket" | "NoSuchKey" => "NotFound",
            "AccessDenied" => "Forbidden",
            "BucketNotEmpty" | "BucketAlreadyOwnedByYou" => "Conflict",
            _ => "BadRequest",
        };
        ApiError::new(e.status, code, e.message)
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({}): {}", self.status.as_u16(), self.code, self.message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ApiErrorBody {
            error: ApiErrorDetail {
                code: self.code,
                message: &self.message,
            },
        };
        (self.status, Json(body)).into_response()
    }
}

/// Convenience for handlers: keep a JSON error usable as a response body.
pub fn json_error(status: StatusCode, code: &str, message: &str) -> serde_json::Value {
    json!({ "error": { "code": code, "message": message }, "status": status.as_u16() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_escapes_hostile_keys() {
        assert_eq!(xml_escape("a<b>&\"c'"), "a&lt;b&gt;&amp;&quot;c&apos;");
        assert_eq!(xml_escape("a\u{1}b"), "a\u{FFFD}b");
    }

    #[test]
    fn s3_error_document_shape() {
        let e = S3Error::no_such_key("a&b");
        let xml = e.to_xml("req1", "/bucket/a&b");
        assert!(xml.contains("<Code>NoSuchKey</Code>"));
        assert!(xml.contains("<Message>The specified key does not exist: a&amp;b</Message>"));
        assert!(xml.contains("<RequestId>req1</RequestId>"));
    }
}
