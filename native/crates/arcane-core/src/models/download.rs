use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadStatus {
    Queued,
    Downloading,
    Paused,
    Verifying,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadTask {
    pub id: String,
    pub name: String,
    pub instance_id: Option<String>,
    pub url: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub speed_bps: u64,
    pub status: DownloadStatus,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadProgressEvent {
    pub task_id: String,
    pub name: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub speed_bps: u64,
    pub status: DownloadStatus,
    pub error: Option<String>,
}
