//! Loopback web surface behaviour: one-time code, HttpOnly session, CSRF,
//! host and origin checks, and server-side authorization before a task or
//! an artifact is reached.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use layeragent_web::{WebConfig, build};
use std::path::PathBuf;
use tower::ServiceExt;

/// A stand-in daemon that answers on the profile socket.
async fn fake_daemon(home: PathBuf, profile: String) {
    let runtime = home.join("runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    let socket = runtime.join(format!("{profile}.sock"));
    let _ = std::fs::remove_file(&socket);
    let app = Router::new()
        .route("/health", axum::routing::get(|| async { "ok" }))
        .route(
            "/v1/sessions",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!([{"id":"s1","principal":"owner","workspace":"/tmp/project","created_at":0}]))
            }),
        )
        .route(
            "/v1/tasks",
            axum::routing::post(|| async {
                axum::Json(serde_json::json!({"id":"t1","state":"prepared","revision":1}))
            }),
        )
        .route(
            "/v1/tasks/t1/correct",
            axum::routing::post(|| async {
                (
                    StatusCode::CONFLICT,
                    axum::Json(serde_json::json!({"error":"task revision changed; reload before correcting"})),
                )
            }),
        )
        .route(
            "/v1/artifacts/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            axum::routing::get(|| async { axum::Json(serde_json::json!({"offset":0,"text":"artifact"})) }),
        );
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    axum::serve(listener, app).await.unwrap();
}

fn request(method: &str, path: &str, host: &str, origin: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, host);
    if let Some(origin) = origin {
        builder = builder.header(header::ORIGIN, origin);
    }
    builder.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn a_browser_needs_the_one_time_code_and_then_a_csrf_token() {
    let home = tempfile::tempdir().unwrap();
    let profile = "default".to_string();
    tokio::spawn(fake_daemon(home.path().to_path_buf(), profile.clone()));
    // The daemon socket must exist before the client connects.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let config = WebConfig::new(
        "127.0.0.1:9473".parse().unwrap(),
        home.path().to_path_buf(),
        profile,
        home.path().join("dist"),
    );
    let (app, code) = build(&config).unwrap();

    // An unauthenticated read is refused.
    let response = app
        .clone()
        .oneshot(request("GET", "/api/bootstrap", "127.0.0.1:9473", None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // The one-time code creates a session and returns a CSRF token.
    let body = serde_json::json!({"code": code});
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/login")
                .header(header::HOST, "127.0.0.1:9473")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .unwrap()
        .to_string();
    assert!(
        cookie.contains("HttpOnly"),
        "cookie must be HttpOnly: {cookie}"
    );
    assert!(cookie.contains("SameSite=Strict"), "cookie must be strict");
    let session_id = cookie
        .split(';')
        .next()
        .unwrap()
        .split_once('=')
        .unwrap()
        .1
        .to_string();
    let csrf = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let csrf = serde_json::from_slice::<serde_json::Value>(&csrf).unwrap()["csrf_token"]
        .as_str()
        .unwrap()
        .to_string();

    // The same code cannot be used twice.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/login")
                .header(header::HOST, "127.0.0.1:9473")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // A mutation without the CSRF token is refused.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/tasks")
                .header(header::HOST, "127.0.0.1:9473")
                .header(header::COOKIE, format!("layeragent_session={session_id}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"workspace":"/tmp/project","objective":"do it"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // A foreign host is refused even with a valid session.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/bootstrap")
                .header(header::HOST, "evil.example")
                .header(header::COOKIE, format!("layeragent_session={session_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // A foreign origin is refused.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/bootstrap")
                .header(header::HOST, "127.0.0.1:9473")
                .header(header::ORIGIN, "http://evil.example")
                .header(header::COOKIE, format!("layeragent_session={session_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // An authenticated read succeeds and the security headers are present.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/bootstrap")
                .header(header::HOST, "127.0.0.1:9473")
                .header(header::COOKIE, format!("layeragent_session={session_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .is_some()
    );

    // A stale correction keeps its conflict status.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/tasks/t1/correct")
                .header(header::HOST, "127.0.0.1:9473")
                .header(header::COOKIE, format!("layeragent_session={session_id}"))
                .header("x-layeragent-csrf", &csrf)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"instruction":"different","expected_revision":1})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);

    // Logging out invalidates the interface session only.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/logout")
                .header(header::HOST, "127.0.0.1:9473")
                .header(header::COOKIE, format!("layeragent_session={session_id}"))
                .header("x-layeragent-csrf", &csrf)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/bootstrap")
                .header(header::HOST, "127.0.0.1:9473")
                .header(header::COOKIE, format!("layeragent_session={session_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
