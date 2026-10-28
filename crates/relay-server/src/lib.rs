use axum::{
    Json, Router,
    extract::State,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::StatusCode,
    routing::{get, post},
};
use browser_control::{VideoDiagnosticsSample, validate_navigation_url};
use browser_runtime::{BrowserRuntime, BrowserRuntimeConfig, BrowserRuntimeDiagnostics};
use futures_util::StreamExt;
use media_streaming::{MediaSession, SignalMessage};
use relay_protocol::{NavigateRequest, SessionState, StatusResponse};
use session_manager::Session;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{Duration, sleep};

struct AppState {
    session: Session,
    runtime: Option<BrowserRuntime>,
    config: BrowserRuntimeConfig,
    media: MediaSession,
}
type SharedState = Arc<Mutex<AppState>>;

pub fn router(config: BrowserRuntimeConfig) -> Router {
    let state = Arc::new(Mutex::new(AppState {
        session: Session::default(),
        runtime: None,
        config,
        media: MediaSession::default(),
    }));
    Router::new()
        .route("/health", get(health))
        .route("/api/session", get(status).post(start))
        .route("/api/session/stop", post(stop))
        .route("/api/session/navigate", post(navigate))
        .route("/api/session/reload", post(reload))
        .route("/api/session/video-diagnostics", get(video_diagnostics))
        .route("/api/media/ws", get(media_ws))
        .with_state(state)
}

async fn media_ws(
    ws: WebSocketUpgrade,
    State(state): State<SharedState>,
) -> impl axum::response::IntoResponse {
    ws.on_upgrade(move |socket| media_socket(socket, state))
}

async fn media_socket(mut socket: WebSocket, state: SharedState) {
    let media = { state.lock().await.media.clone() };
    if media.connect_viewer().await.is_err() {
        let _ = send_signal(
            &mut socket,
            SignalMessage::Error {
                message: "another viewer is already connected".into(),
            },
        )
        .await;
        return;
    }
    let _ = send_signal(&mut socket, SignalMessage::Ready).await;
    let _ = send_signal(
        &mut socket,
        SignalMessage::State {
            state: media.state().await,
        },
    )
    .await;
    #[cfg(feature = "gstreamer")]
    let mut video = None;
    while let Some(Ok(message)) = socket.next().await {
        let Message::Text(text) = message else {
            continue;
        };
        let Ok(signal) = serde_json::from_str::<SignalMessage>(&text) else {
            let _ = send_signal(
                &mut socket,
                SignalMessage::Error {
                    message: "invalid signaling message".into(),
                },
            )
            .await;
            continue;
        };
        match signal {
            SignalMessage::Offer { sdp } => {
                #[cfg(feature = "gstreamer")]
                {
                    if video.is_none() {
                        match media_streaming::GstreamerVideoSession::start(
                            &media_streaming::VideoConfig::from_env(),
                        ) {
                            Ok(pipeline) => video = Some(pipeline),
                            Err(error) => {
                                eprintln!("GStreamer video pipeline start failed: {error}");
                                media.fail().await;
                                let _ = send_signal(
                                    &mut socket,
                                    SignalMessage::Error {
                                        message: "failed to start GStreamer video pipeline".into(),
                                    },
                                )
                                .await;
                                continue;
                            }
                        }
                    }
                    let negotiation = match video.as_ref() {
                        Some(pipeline) => pipeline.set_remote_offer(&sdp),
                        None => Err(media_streaming::MediaError::Backend(
                            "GStreamer video pipeline is unavailable".into(),
                        )),
                    };
                    match negotiation {
                        Ok(answer) => {
                            let _ = send_signal(&mut socket, answer).await;
                            media.mark_streaming().await;
                            let _ = send_signal(
                                &mut socket,
                                SignalMessage::State {
                                    state: media.state().await,
                                },
                            )
                            .await;
                        }
                        Err(error) => {
                            if let Some(pipeline) = video.take() {
                                let _ = pipeline.stop();
                            }
                            media.fail().await;
                            let _ = send_signal(
                                &mut socket,
                                SignalMessage::Error {
                                    message: error.to_string(),
                                },
                            )
                            .await;
                        }
                    }
                }
                #[cfg(not(feature = "gstreamer"))]
                {
                    let _ = sdp;
                    media.fail().await;
                    let _ = send_signal(
                        &mut socket,
                        SignalMessage::Error {
                            message: "media backend is not built; use the gstreamer feature".into(),
                        },
                    )
                    .await;
                }
            }
            SignalMessage::IceCandidate {
                candidate,
                sdp_m_line_index,
            } => {
                #[cfg(feature = "gstreamer")]
                if let Some(ref pipeline) = video {
                    let _ = pipeline.add_ice_candidate(&SignalMessage::IceCandidate {
                        candidate,
                        sdp_m_line_index,
                    });
                }
                #[cfg(not(feature = "gstreamer"))]
                let _ = (candidate, sdp_m_line_index);
            }
            _ => {}
        }
    }
    #[cfg(feature = "gstreamer")]
    if let Some(video) = video {
        let _ = video.stop();
    }
    let _ = media.disconnect_viewer().await;
}

async fn send_signal(socket: &mut WebSocket, message: SignalMessage) -> Result<(), String> {
    let text = serde_json::to_string(&message).map_err(|error| error.to_string())?;
    socket
        .send(Message::Text(text.into()))
        .await
        .map_err(|error| error.to_string())
}

async fn health() -> &'static str {
    "ok"
}
async fn status(State(state): State<SharedState>) -> Json<StatusResponse> {
    let s = state.lock().await;
    Json(StatusResponse {
        state: s.session.state.clone(),
        url: s.session.url.clone(),
    })
}
async fn start(State(state): State<SharedState>) -> (StatusCode, Json<StatusResponse>) {
    let mut s = state.lock().await;
    if s.session.begin_start().is_err() {
        return (StatusCode::CONFLICT, status_response(&s));
    }
    let code = match BrowserRuntime::launch(&s.config).await {
        Ok(runtime) => {
            s.runtime = Some(runtime);
            let _ = s.session.started();
            StatusCode::OK
        }
        Err(_) => {
            s.session.start_failed();
            StatusCode::SERVICE_UNAVAILABLE
        }
    };
    (code, status_response(&s))
}
async fn stop(State(state): State<SharedState>) -> (StatusCode, Json<StatusResponse>) {
    let mut s = state.lock().await;
    if s.session.begin_stop().is_err() {
        return (StatusCode::CONFLICT, status_response(&s));
    }
    let result = if let Some(mut runtime) = s.runtime.take() {
        runtime.stop().await
    } else {
        Ok(())
    };
    let code = if result.is_ok() {
        let _ = s.session.stopped();
        StatusCode::OK
    } else {
        s.session.stop_failed();
        StatusCode::INTERNAL_SERVER_ERROR
    };
    (code, status_response(&s))
}
async fn navigate(
    State(state): State<SharedState>,
    Json(req): Json<NavigateRequest>,
) -> (StatusCode, Json<StatusResponse>) {
    let Ok(url) = validate_navigation_url(&req.url) else {
        let s = state.lock().await;
        return (StatusCode::BAD_REQUEST, status_response(&s));
    };
    let mut s = state.lock().await;
    let Some(cdp) = s.runtime.as_ref().map(BrowserRuntime::cdp) else {
        return (StatusCode::CONFLICT, status_response(&s));
    };
    if cdp.navigate(url.as_str()).await.is_err() {
        s.session.stop_failed();
        return (StatusCode::BAD_GATEWAY, status_response(&s));
    }
    let code = if s.session.set_url(url.to_string()).is_ok() {
        StatusCode::OK
    } else {
        StatusCode::CONFLICT
    };
    (code, status_response(&s))
}
async fn reload(State(state): State<SharedState>) -> (StatusCode, Json<StatusResponse>) {
    let s = state.lock().await;
    let Some(cdp) = s.runtime.as_ref().map(BrowserRuntime::cdp) else {
        return (StatusCode::CONFLICT, status_response(&s));
    };
    let code = if matches!(s.session.state, SessionState::Running) && cdp.reload().await.is_ok() {
        StatusCode::OK
    } else {
        StatusCode::BAD_GATEWAY
    };
    (code, status_response(&s))
}

#[derive(Debug, serde::Serialize)]
struct VideoDiagnosticsResponse {
    samples: Vec<browser_control::VideoDiagnosticsSnapshot>,
    site_accessible: bool,
    video_data_loaded: bool,
    playback_started: bool,
    current_time_increased: bool,
    decoded_frames_increased: bool,
    playback_quality_available: bool,
    success: bool,
    error: Option<String>,
    chromium: Option<BrowserRuntimeDiagnostics>,
}

async fn video_diagnostics(
    State(state): State<SharedState>,
) -> (StatusCode, Json<VideoDiagnosticsResponse>) {
    let cdp = {
        let s = state.lock().await;
        if !matches!(s.session.state, SessionState::Running) {
            return (
                StatusCode::CONFLICT,
                Json(empty_video_diagnostics("Chromium session is not running")),
            );
        }
        match s.runtime.as_ref() {
            Some(runtime) => runtime.cdp(),
            None => {
                return (
                    StatusCode::CONFLICT,
                    Json(empty_video_diagnostics("Chromium runtime is unavailable")),
                );
            }
        }
    };

    let mut snapshots = Vec::with_capacity(3);
    let mut error = None;
    for index in 0..3 {
        match cdp.video_diagnostics().await {
            Ok(snapshot) => snapshots.push(snapshot),
            Err(cause) => {
                error = Some(cause.to_string());
                break;
            }
        }
        if index < 2 {
            sleep(Duration::from_secs(2)).await;
        }
    }

    let chromium = {
        let mut s = state.lock().await;
        s.runtime.as_mut().map(BrowserRuntime::diagnostics)
    };

    let samples: Vec<&VideoDiagnosticsSample> = snapshots
        .iter()
        .filter_map(|snapshot| snapshot.sample.as_ref())
        .collect();
    let first = samples.first().copied();
    let last = samples.last().copied();
    let site_accessible = error.is_none()
        && !snapshots.is_empty()
        && snapshots.iter().all(|snapshot| {
            snapshot.document_ready_state == "complete" && !snapshot.location_hostname.is_empty()
        });
    let video_data_loaded = samples
        .iter()
        .any(|sample| sample.ready_state >= 2 && sample.video_width > 0 && sample.video_height > 0);
    let playback_started = samples.iter().any(|sample| !sample.paused);
    let current_time_increased = first
        .zip(last)
        .and_then(|(first, last)| first.current_time.zip(last.current_time))
        .is_some_and(|(first, last)| last > first);
    let decoded_frames_increased = first.zip(last).is_some_and(|(first, last)| {
        match (first.total_video_frames, last.total_video_frames) {
            (Some(first), Some(last)) => last > first,
            _ => last.presented_frame_callbacks > first.presented_frame_callbacks,
        }
    });
    let playback_quality_available = samples
        .iter()
        .any(|sample| sample.playback_quality_available);
    let success = error.is_none()
        && snapshots.len() == 3
        && video_data_loaded
        && playback_started
        && current_time_increased
        && decoded_frames_increased;

    (
        if error.is_some() {
            StatusCode::BAD_GATEWAY
        } else {
            StatusCode::OK
        },
        Json(VideoDiagnosticsResponse {
            samples: snapshots,
            site_accessible,
            video_data_loaded,
            playback_started,
            current_time_increased,
            decoded_frames_increased,
            playback_quality_available,
            success,
            error,
            chromium,
        }),
    )
}

fn empty_video_diagnostics(error: &str) -> VideoDiagnosticsResponse {
    VideoDiagnosticsResponse {
        samples: Vec::new(),
        site_accessible: false,
        video_data_loaded: false,
        playback_started: false,
        current_time_increased: false,
        decoded_frames_increased: false,
        playback_quality_available: false,
        success: false,
        error: Some(error.into()),
        chromium: None,
    }
}

fn status_response(s: &AppState) -> Json<StatusResponse> {
    Json(StatusResponse {
        state: s.session.state.clone(),
        url: s.session.url.clone(),
    })
}
