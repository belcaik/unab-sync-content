use crate::config::Config;
use crate::zoom::db::ZoomDb;
use crate::zoom::models::{
    RecordingFileResponse, RecordingListResponse, RecordingSummary, RecordingsResult,
    ZoomRecordingFile,
};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, Url};
use reqwest_cookie_store::CookieStoreMutex;
use std::sync::Arc;
use thiserror::Error;
use tracing::{debug, info, trace, warn};

const ZOOM_BASE: &str = "https://applications.zoom.us";
const RECORDING_LIST_PATH: &str = "/api/v1/lti/rich/recording/COURSE";
const RECORDING_FILE_PATH: &str = "/api/v1/lti/rich/recording/file";

#[derive(Debug, Error)]
pub enum ZoomApiError {
    #[error("run `u_crawler zoom flow` first to capture lti_scid and cookies")]
    MissingState,
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Url(#[from] url::ParseError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Db(#[from] Box<dyn std::error::Error>),
    #[error(transparent)]
    Cookie(#[from] cookie::ParseError),
    #[error(transparent)]
    CookieStore(#[from] cookie_store::Error),
    #[error("{0}")]
    Message(String),
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
            .map_err(ZoomApiError::Db)?;
        
        if let Some(ref s) = scid {
            info!("Loaded scid from DB: {}", s);
        } else {
            warn!("No scid found in DB for course {}", course_id);
            return Err(ZoomApiError::MissingState);
        }
        let scid = scid.unwrap();

        let cookies = db.load_cookies().map_err(ZoomApiError::Db)?;
        info!("Loaded {} cookies from DB", cookies.len());
        if cookies.is_empty() {
            warn!("No cookies found in DB");
            return Err(ZoomApiError::MissingState);
        }

        // Build cookie store
        let cookie_store = cookie_store::CookieStore::default();
        let mut cookie_store = cookie_store;
        for c in &cookies {
            let mut cookie = cookie::Cookie::new(c.name.clone(), c.value.clone());
            cookie.set_domain(c.domain.clone());
            cookie.set_path(c.path.clone());
            cookie.set_secure(c.secure);
            cookie.set_http_only(c.http_only);
            // We need to parse the domain to a URL for insert_raw, or just use the domain string if it's a domain match
            // insert_raw takes &Cookie and &Url.
            // We can construct a dummy URL from the domain.
            let url_str = format!("https://{}{}", c.domain.trim_start_matches('.'), c.path);
            if let Ok(url) = Url::parse(&url_str) {
                 let _ = cookie_store.insert_raw(&cookie, &url);
            }
        }
        let cookie_store = Arc::new(CookieStoreMutex::new(cookie_store));

        // Build headers
        let mut headers = HeaderMap::new();
        headers.insert("User-Agent", HeaderValue::from_str(&effective_user_agent(cfg)).unwrap());
        headers.insert("Referer", HeaderValue::from_static("https://canvas.unab.cl/"));

        // Load ajaxHeaders
        let stored_headers = db
            .get_all_request_headers(course_id)
            .map_err(ZoomApiError::Db)?;
        
        debug!(
            course_id,
            count = stored_headers.len(),
            "loaded stored request headers"
        );

        for (name, value) in stored_headers {
            if let Ok(hname) = HeaderName::from_bytes(name.as_bytes()) {
                if let Ok(hval) = HeaderValue::from_str(&value) {
                    headers.insert(hname, hval);
                }
            }
        }

        let client = Client::builder()
            .cookie_provider(cookie_store)
            .default_headers(headers)
            .build()?;

        Ok(Self {
            client,
            scid,
            base_url: Url::parse(ZOOM_BASE)?,
        })
    }

    pub async fn validate_cookies(&self) -> bool {
        let mut url = match self.base_url.join(RECORDING_LIST_PATH) {
            Ok(u) => u,
            Err(_) => return false,
        };
        
        {
            let mut qp = url.query_pairs_mut();
            qp.append_pair("startTime", "");
            qp.append_pair("endTime", "");
            qp.append_pair("keyWord", "");
            qp.append_pair("searchType", "1");
            qp.append_pair("status", "");
            qp.append_pair("page", "1");
            qp.append_pair("total", "0");
            qp.append_pair("lti_scid", &self.scid);
        }

        debug!(url = %url, "validating Zoom cookies");

        // We use a separate client or the existing one? Existing one has cookies.
        // We need to ensure we don't follow redirects to detect 302 easily, 
        // OR we check if the final URL is still the API URL.
        // But `self.client` is already built.
        // Let's just check status 200.
        match self.client.get(url).send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.as_u16() == 200 {
                    // Extra check: ensure it's JSON, not a login page HTML
                    if let Some(ct) = resp.headers().get(reqwest::header::CONTENT_TYPE) {
                        if let Ok(ct_str) = ct.to_str() {
                            if ct_str.contains("application/json") {
                                info!("Zoom cookies are valid (HTTP 200 JSON)");
                                return true;
                            }
                        }
                    }
                    // If content-type check fails or is missing, but status is 200, 
                    // it might still be valid or it might be a 200 OK login page.
                    // Let's assume if it's not JSON it's suspicious for an API call.
                    warn!("Zoom cookies validation: HTTP 200 but Content-Type not JSON");
                    false
                } else {
                    warn!("Zoom cookies validation failed: HTTP {}", status);
                    false
                }
            }
            Err(e) => {
                warn!("Zoom cookie validation request failed: {}", e);
                false
            }
        }
    }

    pub async fn list_recordings(
        &self,
        since: Option<&str>,
    ) -> Result<RecordingListResponse, ZoomApiError> {
        let mut page = 1;
        let mut all = Vec::new();
        let mut total_expected: Option<i64> = None;

        loop {
            let mut url = self.base_url.join(RECORDING_LIST_PATH)?;
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
            info!(page, url = %url, "fetching Zoom recordings page");

            let resp = self.client.get(url.clone()).send().await?;

            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                warn!(status = %status, body = %text, "Zoom recordings request failed");
                if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
                     return Err(ZoomApiError::MissingState);
                }
                return Err(ZoomApiError::Message(format!("HTTP {} - {}", status, text)));
            }


            let payload: RecordingListResponse = resp.json().await?;
            
            if let Some(result) = &payload.result {
                total_expected = total_expected.or(result.total);
                if let Some(list) = &result.list {
                    if list.is_empty() {
                        break;
                    }
                    all.extend(list.clone());
                    
                    // Check if we have all
                    if let Some(total) = total_expected {
                        if all.len() as i64 >= total {
                            break;
                        }
                    }
                    
                    // Check if page is full
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
            // Safety break
            if page > 100 {
                warn!("Too many pages, stopping");
                break;
            }
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
        let mut url = self.base_url.join(RECORDING_FILE_PATH)?;
        {
            let mut qp = url.query_pairs_mut();
            qp.append_pair("meetingId", &meeting.meeting_id);
            qp.append_pair("lti_scid", &self.scid);
        }

        let resp = self.client.get(url.clone()).send().await?;
        
        if !resp.status().is_success() {
             let status = resp.status();
             if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
                 return Err(ZoomApiError::MissingState);
             }
             return Err(ZoomApiError::Http(resp.error_for_status().unwrap_err()));
        }

        trace!(status = %resp.status(), meeting_id = %meeting.meeting_id, "Zoom recording files response received");
        let payload: RecordingFileResponse = resp.json().await?;
        let mut out = Vec::new();
        if let Some(result) = payload.result {
            if let Some(entries) = result.recording_files {
                for entry in entries.into_iter().filter(|e| e.play_url.is_some()) {
                    out.push(ZoomRecordingFile {
                        meeting_id: meeting.meeting_id.clone(),
                        play_url: entry.play_url.unwrap(),
                        download_url: entry.download_url.clone(),
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

pub(crate) fn effective_user_agent(cfg: &Config) -> String {
    if !cfg.zoom.user_agent.trim().is_empty() {
        cfg.zoom.user_agent.clone()
    } else if !cfg.user_agent.trim().is_empty() {
        cfg.user_agent.clone()
    } else {
        format!("u_crawler/{}", env!("CARGO_PKG_VERSION"))
    }
}
