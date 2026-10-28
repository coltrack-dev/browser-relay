use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use browser_runtime::BrowserRuntimeConfig;
use relay_server::router;
use tower::ServiceExt;

#[tokio::test]
async fn api_reports_initial_state_and_rejects_unsafe_navigation() {
    let app = router(BrowserRuntimeConfig {
        chromium_binary: "/definitely/missing/chromium".into(),
        user_data_dir: "/tmp/browser-relay-test-profile".into(),
        display: ":99".into(),
        proxy: None,
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/session/navigate")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"url":"file:///etc/passwd"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn api_start_failure_is_reported_as_failed_session() {
    let app = router(BrowserRuntimeConfig {
        chromium_binary: "/definitely/missing/chromium".into(),
        user_data_dir: "/tmp/browser-relay-test-profile".into(),
        display: ":99".into(),
        proxy: None,
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn api_mock_runtime_supports_start_and_stop_without_chromium() {
    let app = router(BrowserRuntimeConfig {
        chromium_binary: "/bin/true".into(),
        user_data_dir: "/tmp/browser-relay-test-profile".into(),
        display: ":99".into(),
        proxy: None,
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/session/stop")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn video_diagnostics_requires_an_existing_running_session() {
    let app = router(BrowserRuntimeConfig {
        chromium_binary: "/definitely/missing/chromium".into(),
        user_data_dir: "/tmp/browser-relay-test-profile".into(),
        display: ":99".into(),
        proxy: None,
    });
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/session/video-diagnostics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn video_diagnostics_reports_closed_cdp_when_chromium_exits() {
    let app = router(BrowserRuntimeConfig {
        chromium_binary: "/bin/true".into(),
        user_data_dir: "/tmp/browser-relay-test-profile".into(),
        display: ":99".into(),
        proxy: None,
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/session/video-diagnostics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains("CDP transport closed"));
    assert!(body.contains("\"site_accessible\":false"));
    assert!(body.contains("\"running\":false"));
    assert!(body.contains("\"exit_code\":0"));
}
