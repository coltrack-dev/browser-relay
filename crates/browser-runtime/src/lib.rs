//! Owns the unprivileged Chromium process.

use browser_control::{CdpClient, CdpTransportState, ControlError};
use std::{
    os::unix::io::AsRawFd,
    process::Stdio,
    sync::{Arc, Mutex as StdMutex},
    time::Instant,
};
use thiserror::Error;
use tokio::{
    io::AsyncReadExt,
    net::UnixStream,
    process::{Child, Command},
    time::{Duration, timeout},
};
use url::Url;

const LOCAL_PROXY_BYPASS: &str = "localhost;127.0.0.1;[::1];fixture";
const XVFB_PID_FILE: &str = "/tmp/browserrelay-xvfb.pid";
const STDERR_LIMIT: usize = 8192;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Socks5Proxy {
    pub host: String,
    pub port: u16,
}

impl Socks5Proxy {
    pub fn parse(raw: &str) -> Result<Self, RuntimeError> {
        let url = Url::parse(raw).map_err(|_| RuntimeError::InvalidProxy(raw.into()))?;
        if url.scheme() != "socks5"
            || url.username() != ""
            || url.password().is_some()
            || !(url.path().is_empty() || url.path() == "/")
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(RuntimeError::InvalidProxy(raw.into()));
        }
        let host = url
            .host_str()
            .ok_or_else(|| RuntimeError::InvalidProxy(raw.into()))?
            .to_owned();
        let port = url
            .port()
            .ok_or_else(|| RuntimeError::InvalidProxy(raw.into()))?;
        Ok(Self { host, port })
    }

    fn chromium_value(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!("socks5://{host}:{}", self.port)
    }
}

#[derive(Debug, Clone)]
pub struct BrowserRuntimeConfig {
    pub chromium_binary: String,
    pub user_data_dir: String,
    pub display: String,
    pub proxy: Option<String>,
}
impl Default for BrowserRuntimeConfig {
    fn default() -> Self {
        Self {
            chromium_binary: std::env::var("BROWSER_RELAY_CHROMIUM")
                .unwrap_or_else(|_| "chromium".into()),
            user_data_dir: "/tmp/browser-relay-profile".into(),
            display: std::env::var("DISPLAY").unwrap_or_else(|_| ":99".into()),
            proxy: std::env::var("BROWSER_RELAY_PROXY")
                .ok()
                .filter(|value| !value.trim().is_empty()),
        }
    }
}

impl BrowserRuntimeConfig {
    pub fn chromium_args(&self) -> Result<Vec<String>, RuntimeError> {
        let mut args = vec![
            "--headless=new".into(),
            "--disable-gpu".into(),
            "--remote-debugging-pipe".into(),
            "--no-first-run".into(),
            "--no-default-browser-check".into(),
            format!("--user-data-dir={}", self.user_data_dir),
        ];
        if let Some(raw_proxy) = &self.proxy {
            let proxy = Socks5Proxy::parse(raw_proxy)?;
            args.push(format!("--proxy-server={}", proxy.chromium_value()));
            args.push(format!("--proxy-bypass-list={LOCAL_PROXY_BYPASS}"));
        }
        Ok(args)
    }
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("failed to spawn Chromium: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("CDP error: {0}")]
    Cdp(#[from] ControlError),
    #[error("invalid BROWSER_RELAY_PROXY value: {0}")]
    InvalidProxy(String),
}

pub struct BrowserRuntime {
    child: Option<Child>,
    cdp: CdpClient,
    stderr_tail: Arc<StdMutex<Vec<u8>>>,
    started_at: Instant,
    display: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct BrowserRuntimeDiagnostics {
    pub pid: Option<u32>,
    pub running: bool,
    pub exit_code: Option<i32>,
    pub exit_signal: Option<i32>,
    pub uptime_ms: u128,
    pub display: String,
    pub xvfb_running: bool,
    pub cdp_transport: CdpTransportState,
    pub stderr_tail: String,
}

impl BrowserRuntime {
    pub async fn launch(config: &BrowserRuntimeConfig) -> Result<Self, RuntimeError> {
        let (parent_to_child, child_read) = std::os::unix::net::UnixStream::pair()?;
        let (child_write, parent_from_child) = std::os::unix::net::UnixStream::pair()?;
        let child_read_fd = child_read.as_raw_fd();
        let child_write_fd = child_write.as_raw_fd();
        let mut command = Command::new(&config.chromium_binary);
        command
            .args(config.chromium_args()?)
            .env("DISPLAY", &config.display)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(child_read_fd, 3) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::dup2(child_write_fd, 4) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        drop(child_read);
        drop(child_write);
        parent_to_child.set_nonblocking(true)?;
        parent_from_child.set_nonblocking(true)?;
        let parent_to_child = UnixStream::from_std(parent_to_child)?;
        let parent_from_child = UnixStream::from_std(parent_from_child)?;
        let (_, input) = parent_to_child.into_split();
        let (output, _) = parent_from_child.into_split();
        let stderr_tail = Arc::new(StdMutex::new(Vec::new()));
        if let Some(mut stderr) = child.stderr.take() {
            let stderr_tail = stderr_tail.clone();
            tokio::spawn(async move {
                let mut chunk = [0u8; 1024];
                loop {
                    let count = match stderr.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(count) => count,
                    };
                    let Ok(mut tail) = stderr_tail.lock() else {
                        break;
                    };
                    tail.extend_from_slice(&chunk[..count]);
                    if tail.len() > STDERR_LIMIT {
                        let excess = tail.len() - STDERR_LIMIT;
                        tail.drain(..excess);
                    }
                }
            });
        }
        Ok(Self {
            child: Some(child),
            cdp: CdpClient::new(input, output),
            stderr_tail,
            started_at: Instant::now(),
            display: config.display.clone(),
        })
    }
    pub async fn stop(&mut self) -> Result<(), RuntimeError> {
        if let Some(mut child) = self.child.take() {
            let _ = timeout(Duration::from_secs(2), self.cdp.close()).await;
            if child.try_wait()?.is_none() {
                child.kill().await?;
                let _ = child.wait().await;
            }
        }
        Ok(())
    }
    pub fn cdp(&self) -> CdpClient {
        self.cdp.clone()
    }
    pub fn is_running(&mut self) -> bool {
        self.child
            .as_mut()
            .is_some_and(|child| child.try_wait().ok().flatten().is_none())
    }

    pub fn diagnostics(&mut self) -> BrowserRuntimeDiagnostics {
        let (pid, running, exit_code, exit_signal) = match self.child.as_mut() {
            Some(child) => match child.try_wait() {
                Ok(Some(status)) => {
                    #[cfg(unix)]
                    let signal = std::os::unix::process::ExitStatusExt::signal(&status);
                    #[cfg(not(unix))]
                    let signal = None;
                    (child.id(), false, status.code(), signal)
                }
                Ok(None) => (child.id(), true, None, None),
                Err(_) => (child.id(), false, None, None),
            },
            None => (None, false, None, None),
        };
        let stderr_tail = self
            .stderr_tail
            .lock()
            .map(|tail| sanitize_stderr(&tail))
            .unwrap_or_default();
        BrowserRuntimeDiagnostics {
            pid,
            running,
            exit_code,
            exit_signal,
            uptime_ms: self.started_at.elapsed().as_millis(),
            display: self.display.clone(),
            xvfb_running: xvfb_running(),
            cdp_transport: self.cdp.transport_state(),
            stderr_tail,
        }
    }
}

fn xvfb_running() -> bool {
    let Ok(pid) = std::fs::read_to_string(XVFB_PID_FILE)
        .ok()
        .and_then(|value| value.trim().parse::<libc::pid_t>().ok())
        .ok_or(())
    else {
        return false;
    };
    unsafe { libc::kill(pid, 0) == 0 }
}

fn sanitize_stderr(bytes: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    for scheme in ["http://", "https://", "file://"] {
        while let Some(start) = text.find(scheme) {
            let end = text[start..]
                .find(char::is_whitespace)
                .map(|offset| start + offset)
                .unwrap_or(text.len());
            text.replace_range(start..end, "<redacted-url>");
        }
    }
    text
}
impl Drop for BrowserRuntime {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.start_kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_explicit_socks5_proxy() {
        assert_eq!(
            Socks5Proxy::parse("socks5://192.168.13.128:4712").unwrap(),
            Socks5Proxy {
                host: "192.168.13.128".into(),
                port: 4712
            }
        );
        assert!(Socks5Proxy::parse("http://192.168.13.128:4712").is_err());
        assert!(Socks5Proxy::parse("socks5://192.168.13.128").is_err());
        assert!(Socks5Proxy::parse("socks5://user:pass@192.168.13.128:4712").is_err());
    }

    #[test]
    fn proxy_args_keep_cdp_pipe_and_bypass_only_local_resources() {
        let config = BrowserRuntimeConfig {
            chromium_binary: "chromium".into(),
            user_data_dir: "/tmp/profile".into(),
            display: ":99".into(),
            proxy: Some("socks5://192.168.13.128:4712".into()),
        };
        let args = config.chromium_args().unwrap();
        assert!(args.contains(&"--remote-debugging-pipe".into()));
        assert!(args.contains(&"--proxy-server=socks5://192.168.13.128:4712".into()));
        assert!(args.contains(&"--proxy-bypass-list=localhost;127.0.0.1;[::1];fixture".into()));
        assert!(!args.iter().any(|arg| arg == "--no-proxy-server"));
    }

    #[test]
    fn no_proxy_keeps_chromium_direct_configuration_unchanged() {
        let config = BrowserRuntimeConfig {
            chromium_binary: "chromium".into(),
            user_data_dir: "/tmp/profile".into(),
            display: ":99".into(),
            proxy: None,
        };
        let args = config.chromium_args().unwrap();
        assert!(!args.iter().any(|arg| arg.starts_with("--proxy-server=")));
        assert!(args.contains(&"--remote-debugging-pipe".into()));
    }

    #[tokio::test]
    async fn process_diagnostics_capture_exit_and_closed_transport() {
        let config = BrowserRuntimeConfig {
            chromium_binary: "/bin/true".into(),
            user_data_dir: "/tmp/browser-relay-test-profile".into(),
            display: ":99".into(),
            proxy: None,
        };
        let mut runtime = BrowserRuntime::launch(&config).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let diagnostics = runtime.diagnostics();
        assert!(!diagnostics.running);
        assert_eq!(diagnostics.exit_code, Some(0));
        assert_eq!(diagnostics.cdp_transport, CdpTransportState::Open);
        let error = runtime.cdp().video_diagnostics().await.unwrap_err();
        assert!(matches!(error, ControlError::TransportClosed { .. }));
        assert_eq!(
            runtime.diagnostics().cdp_transport,
            CdpTransportState::Closed
        );
    }
}
