use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;

use crate::types::ApiError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessLevel {
    Full,
    ReadOnly,
}

#[derive(Clone)]
pub struct HttpAuth {
    pub full_password: String,
    pub read_only_password: String,
}

pub fn extract_basic_password(req: &Request) -> Option<String> {
    let header = req.headers().get(AUTHORIZATION)?.to_str().ok()?;
    let encoded = header.strip_prefix("Basic ")?;
    let decoded = BASE64.decode(encoded).ok()?;
    let credentials = String::from_utf8(decoded).ok()?;
    let (_, password) = credentials.split_once(':')?;
    Some(password.to_string())
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(WWW_AUTHENTICATE, "Basic realm=\"mdk\"")],
        Json(ApiError {
            error: "Invalid credentials".into(),
            code: "unauthorized".into(),
        }),
    )
        .into_response()
}

pub async fn auth_middleware(
    State(http_auth): State<HttpAuth>,
    mut req: Request,
    next: Next,
) -> Response {
    let access_level = match extract_basic_password(&req) {
        Some(ref pw) if pw == &http_auth.full_password => AccessLevel::Full,
        Some(ref pw) if pw == &http_auth.read_only_password => AccessLevel::ReadOnly,
        _ => return unauthorized(),
    };

    req.extensions_mut().insert(access_level);
    next.run(req).await
}

pub async fn require_full_access(req: Request, next: Next) -> Response {
    match req.extensions().get::<AccessLevel>() {
        Some(AccessLevel::Full) => next.run(req).await,
        _ => (
            StatusCode::FORBIDDEN,
            Json(ApiError {
                error: "Full access required".into(),
                code: "forbidden".into(),
            }),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::middleware;
    use axum::routing::{get, post};
    use axum::Router;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn test_auth() -> HttpAuth {
        HttpAuth {
            full_password: "full_secret".into(),
            read_only_password: "readonly_secret".into(),
        }
    }

    fn encode_basic(user: &str, pass: &str) -> String {
        format!("Basic {}", BASE64.encode(format!("{user}:{pass}")))
    }

    fn test_router() -> Router {
        let auth = test_auth();

        let read_only_routes = Router::new().route("/readonly", get(|| async { "ok" }));

        let full_routes = Router::new()
            .route("/full", post(|| async { "ok" }))
            .layer(middleware::from_fn(require_full_access));

        read_only_routes
            .merge(full_routes)
            .layer(middleware::from_fn_with_state(
                auth.clone(),
                auth_middleware,
            ))
            .with_state(auth)
    }

    // -- extract_basic_password tests --

    #[test]
    fn extract_empty_username() {
        let req = Request::builder()
            .header(AUTHORIZATION, encode_basic("", "mypass"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(extract_basic_password(&req).as_deref(), Some("mypass"));
    }

    #[test]
    fn extract_with_username() {
        let req = Request::builder()
            .header(AUTHORIZATION, encode_basic("phoenix", "secret"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(extract_basic_password(&req).as_deref(), Some("secret"));
    }

    #[test]
    fn extract_no_auth_header() {
        let req = Request::builder().body(Body::empty()).unwrap();
        assert_eq!(extract_basic_password(&req), None);
    }

    #[test]
    fn extract_bearer_returns_none() {
        let req = Request::builder()
            .header(AUTHORIZATION, "Bearer some_token")
            .body(Body::empty())
            .unwrap();
        assert_eq!(extract_basic_password(&req), None);
    }

    #[test]
    fn extract_malformed_base64() {
        let req = Request::builder()
            .header(AUTHORIZATION, "Basic !!!not-base64!!!")
            .body(Body::empty())
            .unwrap();
        assert_eq!(extract_basic_password(&req), None);
    }

    #[test]
    fn extract_no_colon_in_decoded() {
        let req = Request::builder()
            .header(AUTHORIZATION, format!("Basic {}", BASE64.encode("nocolon")))
            .body(Body::empty())
            .unwrap();
        assert_eq!(extract_basic_password(&req), None);
    }

    #[test]
    fn extract_password_with_colon() {
        let req = Request::builder()
            .header(AUTHORIZATION, encode_basic("user", "pass:with:colons"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            extract_basic_password(&req).as_deref(),
            Some("pass:with:colons")
        );
    }

    // -- integration tests against the test router --

    #[tokio::test]
    async fn no_auth_returns_401() {
        let app = test_router();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/readonly")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            resp.headers().get(WWW_AUTHENTICATE).unwrap(),
            "Basic realm=\"mdk\""
        );
    }

    #[tokio::test]
    async fn wrong_password_returns_401() {
        let app = test_router();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/readonly")
                    .header(AUTHORIZATION, encode_basic("", "wrong"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn full_password_accesses_readonly_route() {
        let app = test_router();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/readonly")
                    .header(AUTHORIZATION, encode_basic("", "full_secret"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"ok");
    }

    #[tokio::test]
    async fn readonly_password_accesses_readonly_route() {
        let app = test_router();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/readonly")
                    .header(AUTHORIZATION, encode_basic("", "readonly_secret"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn full_password_accesses_full_route() {
        let app = test_router();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/full")
                    .header(AUTHORIZATION, encode_basic("", "full_secret"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn readonly_password_rejected_from_full_route() {
        let app = test_router();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/full")
                    .header(AUTHORIZATION, encode_basic("", "readonly_secret"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn username_is_ignored() {
        let app = test_router();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/readonly")
                    .header(AUTHORIZATION, encode_basic("anything", "full_secret"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
    }
}
