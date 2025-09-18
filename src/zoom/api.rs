use crate::config::Config;
use crate::zoom::db::ZoomDb;
use crate::zoom::models::{
    RecordingFileResponse, RecordingListResponse, RecordingSummary, RecordingsResult, ZoomCookie,
    ZoomRecordingFile,
};
use reqwest::cookie::Jar;
use reqwest::header::{HeaderName, HeaderValue};
use reqwest::{Client, Url};
use std::sync::Arc;
use thiserror::Error;

const ZOOM_BASE: &str = "https://applications.zoom.us";
const RECORDING_LIST_PATH: &str = "/api/v1/lti/rich/recording/COURSE";
const RECORDING_FILE_PATH: &str = "/api/v1/lti/rich/recording/file";

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
    request_headers: Vec<(String, String)>,
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
        let mut request_headers = std::collections::HashMap::new();
        let headers = db
            .get_all_request_headers(course_id)
            .map_err(ZoomApiError::Db)?;
        println!("Loaded {} headers for course {}", headers.len(), course_id);
        for (name, value) in headers {
            request_headers.insert(name.to_ascii_lowercase(), value);
        }
        Ok(Self {
            client,
            scid,
            base_url: Url::parse(ZOOM_BASE)?,
            request_headers: request_headers
                .into_iter()
                .map(|(name, value)| (name, value))
                .collect(),
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
                // println!("Query params: {:?}", qp);
            }
            println!("Fetching recordings page {}: {}", page, url);

            let mut attempt = 0;
            let resp = loop {
                attempt += 1;
                let request = self.with_request_headers(self.client.get(url.clone()));
                let response = match request.send().await {
                    Ok(resp) => resp,
                    Err(err) => {
                        println!("Zoom recordings request failed: {:#?}", err);
                        return Err(ZoomApiError::from(err));
                    }
                };

                if matches!(response.status().as_u16(), 401 | 403) {
                    if attempt == 1 {
                        println!(
                            "Zoom devolvió {}; reintentando con cookies actualizadas",
                            response.status()
                        );
                        continue;
                    }
                    return Err(ZoomApiError::MissingState);
                }
                break response;
            };
            println!("Response: {:?}", resp);
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
        let mut url = self.base_url.join(RECORDING_FILE_PATH)?;
        {
            let mut qp = url.query_pairs_mut();
            qp.append_pair("meetingId", &meeting.meeting_id);
            qp.append_pair("lti_scid", &self.scid);
        }

        let mut attempt = 0;
        let resp = loop {
            attempt += 1;
            let request = self.with_request_headers(self.client.get(url.clone()));
            let response = match request.send().await {
                Ok(resp) => resp,
                Err(err) => {
                    println!("Zoom recording files request failed: {:#?}", err);
                    return Err(ZoomApiError::from(err));
                }
            };
            if matches!(response.status().as_u16(), 401 | 403) {
                if attempt == 1 {
                    println!(
                        "Zoom devolvió {}; reintentando fetch_recording_files",
                        response.status()
                    );
                    continue;
                }
                return Err(ZoomApiError::MissingState);
            }
            break response;
        };
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

    fn with_request_headers(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.request_headers.is_empty() {
            builder
        } else {
            apply_stored_headers(builder, &self.request_headers)
        }
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

fn apply_stored_headers(
    mut builder: reqwest::RequestBuilder,
    headers: &[(String, String)],
) -> reqwest::RequestBuilder {
    println!("Applying {} stored headers", headers.len());
    for (name, value) in headers {
        if name.starts_with(':') {
            continue;
        }
        let header_name = match HeaderName::from_bytes(name.as_bytes()) {
            Ok(name) => name,
            Err(_) => continue,
        };
        let header_value = match HeaderValue::from_str(value) {
            Ok(value) => value,
            Err(_) => continue,
        };
        builder = builder.header(header_name, header_value);
    }
    println!("Applied headers, building request");
    builder
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_stored_headers_skips_invalid_entries() {
        let client = reqwest::Client::builder()
            .user_agent("test-agent")
            .build()
            .expect("client");

        let headers = vec![
            (":authority".to_string(), "ignored".to_string()),
            ("X-Test".to_string(), "value".to_string()),
            ("Invalid Header".to_string(), "value".to_string()),
        ];

        let request = apply_stored_headers(client.get("https://example.com"), &headers)
            .build()
            .expect("request build");

        let header_map = request.headers();
        assert_eq!(
            header_map
                .get("X-Test")
                .expect("x-test header present")
                .to_str()
                .unwrap(),
            "value"
        );
        assert!(header_map.get(":authority").is_none());
        assert!(header_map.get("Invalid Header").is_none());
    }
}
