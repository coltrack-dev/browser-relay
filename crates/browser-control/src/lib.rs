use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    net::unix::{OwnedReadHalf, OwnedWriteHalf},
    sync::Mutex,
};
use url::Url;

#[derive(Debug, Error)]
pub enum ControlError {
    #[error("URL must use http or https")]
    InvalidUrl,
    #[error("CDP I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("CDP JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("CDP response error: {0}")]
    Protocol(String),
    #[error("CDP transport closed during {operation}: {details}")]
    TransportClosed { operation: String, details: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum CdpTransportState {
    Open,
    Closed,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VideoDiagnosticsSample {
    pub ready_state: u64,
    pub network_state: u64,
    pub paused: bool,
    pub current_time: Option<f64>,
    pub duration: Option<f64>,
    pub video_width: u64,
    pub video_height: u64,
    pub buffered_ranges: u64,
    pub total_video_frames: Option<u64>,
    pub dropped_video_frames: Option<u64>,
    pub playback_quality_available: bool,
    pub presented_frame_callbacks: u64,
}

const VIDEO_DIAGNOSTICS_SCRIPT: &str = r#"(async () => {
  const video = document.querySelector('video');
  const quality = video && typeof video.getVideoPlaybackQuality === 'function'
    ? video.getVideoPlaybackQuality()
    : null;
  const finite = value => Number.isFinite(value) ? value : null;
  const presentedFrameCallbacks = video && 'requestVideoFrameCallback' in video
    ? await new Promise(resolve => {
        let finished = false;
        const finish = value => { if (!finished) { finished = true; resolve(value); } };
        const timer = setTimeout(() => finish(0), 500);
        video.requestVideoFrameCallback(() => { clearTimeout(timer); finish(1); });
      })
    : 0;
  return {
    document_ready_state: document.readyState,
    location_hostname: location.hostname,
    video_count: document.querySelectorAll('video').length,
    sample: video ? {
      ready_state: video.readyState,
      network_state: video.networkState,
      paused: video.paused,
      current_time: finite(video.currentTime),
      duration: finite(video.duration),
      video_width: video.videoWidth,
      video_height: video.videoHeight,
      buffered_ranges: video.buffered.length,
      total_video_frames: quality ? quality.totalVideoFrames : null,
      dropped_video_frames: quality ? quality.droppedVideoFrames : null,
      playback_quality_available: Boolean(quality),
      presented_frame_callbacks: presentedFrameCallbacks
    } : null
  };
})()"#;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VideoDiagnosticsSnapshot {
    pub document_ready_state: String,
    pub location_hostname: String,
    pub video_count: u64,
    pub sample: Option<VideoDiagnosticsSample>,
}
pub fn validate_navigation_url(raw: &str) -> Result<Url, ControlError> {
    let url = Url::parse(raw).map_err(|_| ControlError::InvalidUrl)?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        _ => Err(ControlError::InvalidUrl),
    }
}

/// Minimal CDP client for Chromium's `--remote-debugging-pipe` transport.
/// Messages are length-prefixed JSON objects; no public debugging port is used.
#[derive(Clone)]
pub struct CdpClient {
    input: Arc<Mutex<OwnedWriteHalf>>,
    output: Arc<Mutex<BufReader<OwnedReadHalf>>>,
    command_lock: Arc<Mutex<()>>,
    session_lock: Arc<Mutex<()>>,
    next_id: Arc<AtomicU64>,
    session_id: Arc<Mutex<Option<String>>>,
    transport_state: Arc<std::sync::atomic::AtomicU8>,
}

impl CdpClient {
    pub fn new(input: OwnedWriteHalf, output: OwnedReadHalf) -> Self {
        Self {
            input: Arc::new(Mutex::new(input)),
            output: Arc::new(Mutex::new(BufReader::new(output))),
            command_lock: Arc::new(Mutex::new(())),
            session_lock: Arc::new(Mutex::new(())),
            next_id: Arc::new(AtomicU64::new(1)),
            session_id: Arc::new(Mutex::new(None)),
            transport_state: Arc::new(std::sync::atomic::AtomicU8::new(0)),
        }
    }

    pub fn transport_state(&self) -> CdpTransportState {
        if self.transport_state.load(Ordering::Acquire) == 0 {
            CdpTransportState::Open
        } else {
            CdpTransportState::Closed
        }
    }

    fn closed_error(&self, operation: &str, details: impl ToString) -> ControlError {
        self.transport_state.store(1, Ordering::Release);
        ControlError::TransportClosed {
            operation: operation.into(),
            details: details.to_string(),
        }
    }

    async fn command(
        &self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> Result<Value, ControlError> {
        if self.transport_state() == CdpTransportState::Closed {
            return Err(self.closed_error(method, "pipe was already closed"));
        }
        let _command_guard = self.command_lock.lock().await;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut message = json!({"id": id, "method": method, "params": params});
        if let Some(session_id) = session_id {
            message["sessionId"] = json!(session_id);
        }
        let bytes = serde_json::to_vec(&message)?;
        let mut input = self.input.lock().await;
        if let Err(error) = input.write_all(&bytes).await {
            return if error.kind() == std::io::ErrorKind::BrokenPipe {
                Err(self.closed_error(method, error))
            } else {
                Err(ControlError::Io(error))
            };
        }
        if let Err(error) = input.write_all(&[0]).await {
            return if error.kind() == std::io::ErrorKind::BrokenPipe {
                Err(self.closed_error(method, error))
            } else {
                Err(ControlError::Io(error))
            };
        }
        if let Err(error) = input.flush().await {
            return if error.kind() == std::io::ErrorKind::BrokenPipe {
                Err(self.closed_error(method, error))
            } else {
                Err(ControlError::Io(error))
            };
        }
        drop(input);
        loop {
            let mut output = self.output.lock().await;
            let response = match read_message(&mut *output).await {
                Ok(response) => response,
                Err(ControlError::Io(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::BrokenPipe
                    ) =>
                {
                    return Err(self.closed_error(method, error));
                }
                Err(error) => return Err(error),
            };
            drop(output);
            if response.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = response.get("error") {
                return Err(ControlError::Protocol(error.to_string()));
            }
            return Ok(response.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    async fn session(&self) -> Result<String, ControlError> {
        let _session_guard = self.session_lock.lock().await;
        if let Some(id) = self.session_id.lock().await.clone() {
            return Ok(id);
        }
        let targets = self.command("Target.getTargets", json!({}), None).await?;
        let target_id = targets["targetInfos"]
            .as_array()
            .and_then(|targets| targets.iter().find(|target| target["type"] == "page"))
            .and_then(|target| target["targetId"].as_str())
            .ok_or_else(|| ControlError::Protocol("no page target".into()))?
            .to_owned();
        let attached = self
            .command(
                "Target.attachToTarget",
                json!({"targetId": target_id, "flatten": true}),
                None,
            )
            .await?;
        let session_id = attached["sessionId"]
            .as_str()
            .ok_or_else(|| ControlError::Protocol("missing sessionId".into()))?
            .to_owned();
        *self.session_id.lock().await = Some(session_id.clone());
        Ok(session_id)
    }

    pub async fn navigate(&self, raw_url: &str) -> Result<(), ControlError> {
        let url = validate_navigation_url(raw_url)?;
        let session = self.session().await?;
        self.command(
            "Page.navigate",
            json!({"url": url.as_str()}),
            Some(&session),
        )
        .await?;
        Ok(())
    }

    pub async fn reload(&self) -> Result<(), ControlError> {
        let session = self.session().await?;
        self.command("Page.reload", json!({"ignoreCache": false}), Some(&session))
            .await?;
        Ok(())
    }

    pub async fn video_diagnostics(&self) -> Result<VideoDiagnosticsSnapshot, ControlError> {
        let session = self.session().await?;
        let result = self
            .command(
                "Runtime.evaluate",
                json!({
                    "expression": VIDEO_DIAGNOSTICS_SCRIPT,
                    "returnByValue": true,
                    "awaitPromise": true
                }),
                Some(&session),
            )
            .await?;
        if let Some(exception) = result.get("exceptionDetails") {
            return Err(ControlError::Protocol(format!(
                "Runtime.evaluate exception: {exception}"
            )));
        }
        let value = result
            .get("result")
            .and_then(|result| result.get("value"))
            .cloned()
            .ok_or_else(|| ControlError::Protocol("Runtime.evaluate returned no value".into()))?;
        serde_json::from_value(value).map_err(|error| {
            ControlError::Protocol(format!("invalid video diagnostics result: {error}"))
        })
    }

    pub async fn close(&self) -> Result<(), ControlError> {
        self.command("Browser.close", json!({}), None)
            .await
            .map(|_| ())
    }
}

async fn read_message<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Value, ControlError> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        reader.read_exact(&mut byte).await?;
        if byte[0] == 0 {
            break;
        }
        bytes.push(byte[0]);
    }
    Ok(serde_json::from_slice(&bytes)?)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_non_web_urls() {
        assert!(validate_navigation_url("file:///etc/passwd").is_err());
    }

    #[tokio::test]
    async fn reads_nul_terminated_cdp_frame() {
        let (mut writer, mut reader) = tokio::io::duplex(128);
        let payload = br#"{"id":7,"result":{}}"#;
        writer.write_all(payload).await.unwrap();
        writer.write_all(&[0]).await.unwrap();
        let value = read_message(&mut reader).await.unwrap();
        assert_eq!(value["id"], 7);
        assert!(value["result"].is_object());
    }

    #[tokio::test]
    async fn reuses_one_transport_for_sequential_navigation_and_reload() {
        let (client_stream, mut server_stream) = tokio::net::UnixStream::pair().unwrap();
        let (client_read, client_write) = client_stream.into_split();
        let client = CdpClient::new(client_write, client_read);
        let server = tokio::spawn(async move {
            for _ in 0..5 {
                let request = read_message(&mut server_stream).await.unwrap();
                let result = match request["method"].as_str().unwrap() {
                    "Target.getTargets" => serde_json::json!({
                        "targetInfos": [{"type": "page", "targetId": "page-1"}]
                    }),
                    "Target.attachToTarget" => serde_json::json!({"sessionId": "session-1"}),
                    "Page.navigate" | "Page.reload" => serde_json::json!({}),
                    method => panic!("unexpected method {method}"),
                };
                let response = serde_json::json!({
                    "id": request["id"],
                    "result": result
                });
                server_stream
                    .write_all(response.to_string().as_bytes())
                    .await
                    .unwrap();
                server_stream.write_all(&[0]).await.unwrap();
            }
        });
        client.navigate("https://example.org").await.unwrap();
        client.reload().await.unwrap();
        client.navigate("https://example.org/next").await.unwrap();
        assert_eq!(client.transport_state(), CdpTransportState::Open);
        server.await.unwrap();
    }

    #[test]
    fn video_diagnostics_snapshot_contains_only_playback_metadata() {
        let snapshot: VideoDiagnosticsSnapshot = serde_json::from_value(serde_json::json!({
            "document_ready_state": "complete",
            "location_hostname": "www.youtube.com",
            "video_count": 1,
            "sample": {
                "ready_state": 4,
                "network_state": 1,
                "paused": false,
                "current_time": 12.5,
                "duration": 120.0,
                "video_width": 1280,
                "video_height": 720,
                "buffered_ranges": 1,
                "total_video_frames": 375,
                "dropped_video_frames": 2,
                "playback_quality_available": true,
                "presented_frame_callbacks": 1
            }
        }))
        .unwrap();
        assert_eq!(snapshot.location_hostname, "www.youtube.com");
        assert_eq!(snapshot.sample.unwrap().total_video_frames, Some(375));
    }
}
