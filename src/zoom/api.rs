use crate::config::Config;
use crate::zoom::db::ZoomDb;
use crate::zoom::models::{
    RecordingFileResponse, RecordingListResponse, RecordingSummary, RecordingsResult,
    ZoomCookie, ZoomRecordingFile,
};
use reqwest::cookie::Jar;
use reqwest::{Client, Url};
use std::sync::Arc;
use thiserror::Error;

const ZOOM_BASE: &str = "https://applications.zoom.us";

#[derive(Debug, Error)]
pub enum ZoomApiError {
    #[error("ejecuta 'u_crawler zoom sniff-cdp' primero para obtener lti_scid y cookies")]
    MissingState,
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Url(#[from] url::ParseError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Db(#[from] Box<dyn std::error::Error>),
}

pub struct ZoomClient {
    client: Client,
    scid: String,
    base_url: Url,
}

impl ZoomClient {
    pub async fn new(cfg: &Config, db: &ZoomDb, course_id: u64) -> Result<Self, ZoomApiError> {
        let scid = db
            .get_scid(course_id)
            .map_err(ZoomApiError::Db)?
            .ok_or(ZoomApiError::MissingState)?;
        let cookies = db.load_cookies().map_err(ZoomApiError::Db)?;
        if cookies.is_empty() {
            return Err(ZoomApiError::MissingState);
        }

        let jar = build_cookie_jar(&cookies)?;
        let client = reqwest::Client::builder()
            .cookie_provider(jar)
            .user_agent(effective_user_agent(cfg))
            .build()?;

        Ok(Self {
            client,
            scid,
            base_url: Url::parse("https://applications.zoom.us")?,
        })
    }

    pub async fn list_recordings(
        &self,
        since: Option<&str>,
    ) -> Result<RecordingListResponse, ZoomApiError> {
        let mut page = 1;
        let mut all = Vec::new();
        let mut total_expected: Option<i64> = None;

        loop {
            let mut url = self
                .base_url
                .join("/api/v1/lti/rich/recording/COURSE")?;
            let end = chrono::Utc::now().format("%Y-%m-%d").to_string();
            {
                let mut qp = url.query_pairs_mut();
                qp.append_pair("startTime", since.unwrap_or(""));
                qp.append_pair("endTime", &end);
                qp.append_pair("keyWord", "");
                qp.append_pair("searchType", "1");
                qp.append_pair("status", "");
                qp.append_pair("page", &page.to_string());
                qp.append_pair("total", "0");
                qp.append_pair("lti_scid", &self.scid);
            }

            let resp = self.client.get(url).send().await?;
            if resp.status().as_u16() == 401 || resp.status().as_u16() == 403 {
                return Err(ZoomApiError::MissingState);
            }
            let payload: RecordingListResponse = resp.json().await?;
            if let Some(result) = &payload.result {
                total_expected = total_expected.or(result.total);
                if let Some(list) = &result.list {
                    if list.is_empty() {
                        break;
                    }
                    all.extend(list.clone());
                    if let Some(total) = total_expected {
                        if all.len() as i64 >= total {
                            break;
                        }
                    }
                    if result.page_size.unwrap_or_default() as usize > list.len() {
                        break;
                    }
                } else {
                    break;
                }
            } else {
                break;
            }

            page += 1;
        }

        Ok(RecordingListResponse {
            status: Some(true),
            code: Some(200),
            result: Some(RecordingsResult {
                page_num: None,
                page_size: Some(all.len() as i32),
                total: Some(all.len() as i64),
                list: Some(all),
            }),
        })
    }

    pub async fn fetch_recording_files(
        &self,
        meeting: &RecordingSummary,
    ) -> Result<Vec<ZoomRecordingFile>, ZoomApiError> {
        let mut url = self
            .base_url
            .join("/api/v1/lti/rich/recording/file")?;
        {
            let mut qp = url.query_pairs_mut();
            qp.append_pair("meetingId", &meeting.meeting_id);
            qp.append_pair("lti_scid", &self.scid);
        }

        let resp = self.client.get(url).send().await?;
        if resp.status().as_u16() == 401 || resp.status().as_u16() == 403 {
            return Err(ZoomApiError::MissingState);
        }
        let payload: RecordingFileResponse = resp.json().await?;
        let mut out = Vec::new();
        if let Some(result) = payload.result {
            if let Some(entries) = result.recording_files {
                for entry in entries.into_iter().filter(|e| e.play_url.is_some()) {
                    out.push(ZoomRecordingFile {
                        meeting_id: meeting.meeting_id.clone(),
                        play_url: entry.play_url.unwrap(),
                        file_type: entry.file_type.clone(),
                        recording_start: entry.recording_start.clone(),
                        topic: meeting.topic.clone(),
                        start_time: meeting.start_time.clone(),
                        timezone: meeting.timezone.clone(),
                        meeting_number: meeting.meeting_number.clone(),
                    });
                }
            }
        }
        Ok(out)
    }
}

fn build_cookie_jar(cookies: &[ZoomCookie]) -> Result<Arc<Jar>, ZoomApiError> {
    let jar = Arc::new(Jar::default());
    for cookie in cookies {
        let mut cookie_str = format!(
            "{}={}; Domain={}; Path={}",
            cookie.name, cookie.value, cookie.domain, cookie.path
        );
        if cookie.secure {
            cookie_str.push_str("; Secure");
        }
        if cookie.http_only {
            cookie_str.push_str("; HttpOnly");
        }
        let url = Url::parse(&format!("{}{}", ZOOM_BASE, cookie.path))
            .unwrap_or_else(|_| Url::parse(ZOOM_BASE).unwrap());
        jar.add_cookie_str(&cookie_str, &url);
    }
    Ok(jar)
}

pub(crate) fn effective_user_agent(cfg: &Config) -> String {
    if !cfg.zoom.user_agent.trim().is_empty() {
        cfg.zoom.user_agent.clone()
    } else if !cfg.user_agent.trim().is_empty() {
        cfg.user_agent.clone()
    } else {
        format!("u_crawler/{}", env!("CARGO_PKG_VERSION"))
    }
}
