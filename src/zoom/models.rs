use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoomCookie {
    pub domain: String,
    pub name: String,
    pub value: String,
    pub path: String,
    pub expires: Option<i64>,
    pub secure: bool,
    pub http_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ZoomRecordingFile {
    pub meeting_id: String,
    pub play_url: String,
    pub download_url: Option<String>,
    pub file_type: Option<String>,
    pub recording_start: Option<String>,
    pub topic: Option<String>,
    pub start_time: Option<String>,
    pub timezone: Option<String>,
    pub meeting_number: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayHeader {
    pub download_url: String,
    pub headers: HashMap<String, String>,
}

impl ZoomRecordingFile {
    pub fn filename_hint(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(start) = &self.start_time {
            parts.push(start.split(' ').next().unwrap_or(start).to_string());
        }
        if let Some(topic) = &self.topic {
            parts.push(topic.clone());
        }
        if parts.is_empty() {
            format!("zoom-{}", self.meeting_id.replace('/', "_"))
        } else {
            parts.join(" - ")
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingListResponse {
    pub status: Option<bool>,
    pub code: Option<i32>,
    pub result: Option<RecordingsResult>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingsResult {
    pub page_num: Option<i32>,
    pub page_size: Option<i32>,
    pub total: Option<i64>,
    pub list: Option<Vec<RecordingSummary>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingSummary {
    #[serde(default)]
    pub meeting_id: String,
    pub meeting_number: Option<String>,
    pub topic: Option<String>,
    pub start_time: Option<String>,
    pub timezone: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingFileResponse {
    pub status: Option<bool>,
    pub code: Option<i32>,
    pub result: Option<RecordingFileResult>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingFileResult {
    #[serde(default)]
    pub recording_files: Option<Vec<RecordingFileEntry>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RecordingFileEntry {
    #[serde(rename = "playUrl")]
    pub play_url: Option<String>,
    #[serde(rename = "downloadUrl")]
    pub download_url: Option<String>,
    #[serde(rename = "fileType")]
    pub file_type: Option<String>,
    #[serde(rename = "recordingStart")]
    pub recording_start: Option<String>,
}
