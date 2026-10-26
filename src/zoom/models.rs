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
    /// Zoom's id for this recording file, when the listing reports one.
    #[serde(default)]
    pub file_id: Option<String>,
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
    /// The `state.json` key of this recording: `zoom:<file id>`.
    ///
    /// The file id is Zoom's own when the listing reports one; otherwise the
    /// meeting id plus the segment's start (or, lacking that, its play URL),
    /// which tells the parts of one meeting apart.
    pub fn state_key(&self) -> String {
        match (&self.file_id, &self.recording_start) {
            (Some(id), _) if !id.is_empty() => format!("zoom:{id}"),
            (_, Some(start)) => format!("zoom:{}:{start}", self.meeting_id),
            _ => format!("zoom:{}:{}", self.meeting_id, self.play_url),
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
    /// Kept as a raw value: whether Zoom sends it as a string or a number,
    /// the listing must still parse. [`RecordingFileEntry::file_id`] reads it.
    #[serde(default)]
    pub id: Option<serde_json::Value>,
    #[serde(rename = "playUrl")]
    pub play_url: Option<String>,
    #[serde(rename = "downloadUrl")]
    pub download_url: Option<String>,
    #[serde(rename = "fileType")]
    pub file_type: Option<String>,
    #[serde(rename = "recordingStart")]
    pub recording_start: Option<String>,
}

impl RecordingFileEntry {
    /// The entry's id as text, whether Zoom sent a string or a number.
    pub fn file_id(&self) -> Option<String> {
        match self.id.as_ref()? {
            serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(file_id: Option<&str>, start: Option<&str>) -> ZoomRecordingFile {
        ZoomRecordingFile {
            meeting_id: "m1".into(),
            file_id: file_id.map(str::to_string),
            play_url: "https://zoom.us/rec/play/x".into(),
            download_url: None,
            file_type: None,
            recording_start: start.map(str::to_string),
            topic: None,
            start_time: None,
            timezone: None,
            meeting_number: None,
        }
    }

    #[test]
    fn state_key_prefers_zooms_file_id() {
        assert_eq!(file(Some("abc"), Some("t")).state_key(), "zoom:abc");
        assert_eq!(file(None, Some("t0")).state_key(), "zoom:m1:t0");
        assert_eq!(
            file(None, None).state_key(),
            "zoom:m1:https://zoom.us/rec/play/x"
        );
    }

    #[test]
    fn file_id_accepts_a_string_or_a_number() {
        let entry: RecordingFileEntry =
            serde_json::from_str(r#"{"id": 42, "playUrl": "u"}"#).unwrap();
        assert_eq!(entry.file_id().as_deref(), Some("42"));
        let entry: RecordingFileEntry =
            serde_json::from_str(r#"{"id": "f-1", "playUrl": "u"}"#).unwrap();
        assert_eq!(entry.file_id().as_deref(), Some("f-1"));
        let entry: RecordingFileEntry = serde_json::from_str(r#"{"playUrl": "u"}"#).unwrap();
        assert_eq!(entry.file_id(), None);
    }
}
