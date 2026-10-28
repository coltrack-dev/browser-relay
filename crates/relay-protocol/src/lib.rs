use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed,
}
#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub state: SessionState,
    pub url: Option<String>,
}
#[derive(Debug, Deserialize)]
pub struct NavigateRequest {
    pub url: String,
}
