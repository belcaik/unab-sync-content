use crate::config::{load_config_from_path, Config, ConfigPaths};
use crate::http::{build_http_client, parse_next_link};
use reqwest::{header, Client, Url};
use serde::Deserialize;
use std::io;
use thiserror::Error;
use tracing::{debug, error};

#[derive(Debug, Error)]
pub enum CanvasError {
    #[error("config error: {0}")]
    Config(#[from] crate::config::ConfigError),
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("http status {0}: {1}")]
    Status(u16, String),
    #[error("decode error: {0}")]
    Decode(String),
    #[error("missing canvas token; run `auth canvas` first")]
    MissingToken,
}

pub struct CanvasClient {
    pub base: Url,
    pub http: Client,
    pub token: String,
}

impl CanvasClient {
    pub async fn from_config() -> Result<Self, CanvasError> {
        let paths = ConfigPaths::default()?;
        let cfg = load_config_from_path(&paths.config_file)
            .await
            .unwrap_or_default();
        let http = build_http_client(&cfg);
        let base = Url::parse(&cfg.canvas.base_url)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid base_url"))?;
        let token = resolve_token(&cfg).await.ok_or(CanvasError::MissingToken)?;
        Ok(CanvasClient { base, http, token })
    }

    fn auth_header_val(&self) -> header::HeaderValue {
        let v = format!("Bearer {}", self.token);
        header::HeaderValue::from_str(&v).expect("valid header")
    }

    pub async fn list_courses(&self) -> Result<Vec<Course>, CanvasError> {
        let mut out = Vec::new();
        let mut next = Some(
            self.base
                .join("/api/v1/courses?enrollment_state=active&per_page=100")
                .unwrap(),
        );
        while let Some(url) = next.take() {
            debug!(method = "GET", url = %url, "canvas request");
            let resp = self
                .http
                .get(url.clone())
                .header(header::AUTHORIZATION, self.auth_header_val())
                .send()
                .await?;
            let status = resp.status();
            // Capture Link header before consuming body
            let link = resp
                .headers()
                .get(header::LINK)
                .and_then(|h| h.to_str().ok())
                .map(|s| s.to_string());

            // Body text for diagnostics, then decode
            let text = resp.text().await?;
            if !status.is_success() {
                let snippet = text.chars().take(500).collect::<String>();
                error!(status = %status.as_u16(), body = %snippet, "canvas non-success response");
                return Err(CanvasError::Status(status.as_u16(), snippet));
            }
            let mut page: Vec<Course> = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => {
                    let snippet = text.chars().take(500).collect::<String>();
                    error!(error = %e, body = %snippet, "canvas decode failure (courses)");
                    return Err(CanvasError::Decode(e.to_string()));
                }
            };
            out.append(&mut page);

            // Parse Link header for next
            next = link.as_deref().and_then(parse_next_link);
        }
        Ok(out)
    }

    pub async fn list_modules_with_items(
        &self,
        course_id: u64,
    ) -> Result<Vec<Module>, CanvasError> {
        let mut out = Vec::new();
        let mut next = Some(
            self.base
                .join(&format!(
                    "/api/v1/courses/{}/modules?include=items&per_page=100",
                    course_id
                ))
                .unwrap(),
        );
        while let Some(url) = next.take() {
            debug!(method = "GET", course_id = course_id, url = %url, "canvas request");
            let resp = self
                .http
                .get(url.clone())
                .header(header::AUTHORIZATION, self.auth_header_val())
                .send()
                .await?;
            let status = resp.status();
            let link = resp
                .headers()
                .get(header::LINK)
                .and_then(|h| h.to_str().ok())
                .map(|s| s.to_string());
            let text = resp.text().await?;
            if !status.is_success() {
                let snippet = text.chars().take(2000).collect::<String>();
                error!(status = %status.as_u16(), body = %snippet, course_id, "canvas non-success response (modules)");
                return Err(CanvasError::Status(status.as_u16(), snippet));
            }
            let mut page: Vec<Module> = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => {
                    let snippet = text.chars().take(2000).collect::<String>();
                    error!(error = %e, body = %snippet, course_id, "canvas decode failure (modules)");
                    return Err(CanvasError::Decode(e.to_string()));
                }
            };
            out.append(&mut page);
            next = link.as_deref().and_then(parse_next_link);
        }
        Ok(out)
    }

    #[allow(dead_code)]
    pub async fn list_files(&self, course_id: u64) -> Result<Vec<FileObj>, CanvasError> {
        let mut out = Vec::new();
        let mut next = Some(
            self.base
                .join(&format!(
                    "/api/v1/courses/{}/files?sort=updated_at&per_page=100",
                    course_id
                ))
                .unwrap(),
        );
        while let Some(url) = next.take() {
            debug!(method = "GET", course_id = course_id, url = %url, "canvas request");
            let resp = self
                .http
                .get(url.clone())
                .header(header::AUTHORIZATION, self.auth_header_val())
                .send()
                .await?;
            let status = resp.status();
            let link = resp
                .headers()
                .get(header::LINK)
                .and_then(|h| h.to_str().ok())
                .map(|s| s.to_string());
            let text = resp.text().await?;
            if !status.is_success() {
                let snippet = text.chars().take(1000).collect::<String>();
                error!(status = %status.as_u16(), body = %snippet, course_id, "canvas non-success response (files)");
                return Err(CanvasError::Status(status.as_u16(), snippet));
            }
            let mut page: Vec<FileObj> = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => {
                    let snippet = text.chars().take(1000).collect::<String>();
                    error!(error = %e, body = %snippet, course_id, "canvas decode failure (files)");
                    return Err(CanvasError::Decode(e.to_string()));
                }
            };
            out.append(&mut page);
            next = link.as_deref().and_then(parse_next_link);
        }
        Ok(out)
    }
}

async fn resolve_token(cfg: &Config) -> Option<String> {
    if let Some(t) = cfg.canvas.token.as_ref() {
        if !t.trim().is_empty() {
            return Some(t.clone());
        }
    }
    if let Some(cmd) = cfg.canvas.token_cmd.as_ref() {
        // Execute via sh -lc to support pipelines; trim output
        let output = tokio::process::Command::new("sh")
            .arg("-lc")
            .arg(cmd)
            .output()
            .await
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    } else {
        None
    }
}

#[derive(Debug, Deserialize)]
pub struct Course {
    pub id: u64,
    pub name: String,
    pub course_code: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Module {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub items: Vec<ModuleItem>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct ModuleItem {
    pub id: u64,
    pub title: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub html_url: Option<String>,
    pub page_url: Option<String>,
    pub external_url: Option<String>,
    pub content_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct FileObj {
    pub id: u64,
    pub display_name: Option<String>,
    pub filename: Option<String>,
    #[allow(dead_code)]
    pub size: Option<u64>,
    pub updated_at: Option<String>,
    pub url: Option<String>,
    pub download_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PageObj {
    pub title: Option<String>,
    pub body: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Assignment {
    pub id: u64,
    pub name: Option<String>,
    pub description: Option<String>,
    pub updated_at: Option<String>,
}

impl CanvasClient {
    pub async fn get_page(&self, course_id: u64, page_url: &str) -> Result<PageObj, CanvasError> {
        let url = self
            .base
            .join(&format!(
                "/api/v1/courses/{}/pages/{}",
                course_id,
                urlencoding::encode(page_url)
            ))
            .unwrap();
        tracing::debug!(method = "GET", url = %url, "canvas request");
        let resp = self
            .http
            .get(url)
            .header(header::AUTHORIZATION, self.auth_header_val())
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            let snippet = text.chars().take(2000).collect::<String>();
            tracing::error!(status = %status.as_u16(), body = %snippet, course_id, page_url, "canvas non-success response (page)");
            return Err(CanvasError::Status(status.as_u16(), snippet));
        }
        match serde_json::from_str::<PageObj>(&text) {
            Ok(p) => Ok(p),
            Err(e) => {
                let snippet = text.chars().take(2000).collect::<String>();
                tracing::error!(error = %e, body = %snippet, course_id, page_url, "canvas decode failure (page)");
                Err(CanvasError::Decode(e.to_string()))
            }
        }
    }
    pub async fn get_file(&self, file_id: u64) -> Result<FileObj, CanvasError> {
        let url = self
            .base
            .join(&format!("/api/v1/files/{}", file_id))
            .unwrap();
        debug!(method = "GET", file_id, url = %url, "canvas request (get_file)");
        let resp = self
            .http
            .get(url)
            .header(header::AUTHORIZATION, self.auth_header_val())
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            let snippet = text.chars().take(500).collect::<String>();
            error!(status = %status.as_u16(), body = %snippet, file_id, "get_file non-success");
            return Err(CanvasError::Status(status.as_u16(), snippet));
        }
        match serde_json::from_str::<FileObj>(&text) {
            Ok(v) => Ok(v),
            Err(e) => {
                let snippet = text.chars().take(500).collect::<String>();
                error!(error = %e, body = %snippet, file_id, "decode failure (file)");
                Err(CanvasError::Decode(e.to_string()))
            }
        }
    }

    pub async fn list_assignments(&self, course_id: u64) -> Result<Vec<Assignment>, CanvasError> {
        let mut out = Vec::new();
        let mut next = Some(
            self.base
                .join(&format!(
                    "/api/v1/courses/{}/assignments?per_page=100",
                    course_id
                ))
                .unwrap(),
        );
        while let Some(url) = next.take() {
            debug!(method = "GET", course_id = course_id, url = %url, "canvas request (assignments)");
            let resp = self
                .http
                .get(url.clone())
                .header(header::AUTHORIZATION, self.auth_header_val())
                .send()
                .await?;
            let status = resp.status();
            let link = resp
                .headers()
                .get(header::LINK)
                .and_then(|h| h.to_str().ok())
                .map(|s| s.to_string());
            let text = resp.text().await?;
            if !status.is_success() {
                let snippet = text.chars().take(1000).collect::<String>();
                error!(status = %status.as_u16(), body = %snippet, course_id, "canvas non-success response (assignments)");
                return Err(CanvasError::Status(status.as_u16(), snippet));
            }
            let mut page: Vec<Assignment> = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => {
                    let snippet = text.chars().take(1000).collect::<String>();
                    error!(error = %e, body = %snippet, course_id, "canvas decode failure (assignments)");
                    return Err(CanvasError::Decode(e.to_string()));
                }
            };
            out.append(&mut page);
            next = link.as_deref().and_then(parse_next_link);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simulated_pagination_loop() {
        // Simulate a chain of Link headers and ensure loop follows 3 pages
        let headers = [
            Some("<https://x/api?page=2>; rel=\"next\"".to_string()),
            Some("<https://x/api?page=3>; rel=\"next\"".to_string()),
            None,
        ];

        let mut count = 0usize;
        let mut i = 0usize;
        let mut next = Some(Url::parse("https://x/api?page=1").unwrap());
        while let Some(_url) = next.take() {
            count += 1;
            let h = headers[i].as_deref();
            let parsed = h.and_then(parse_next_link);
            next = parsed;
            i = (i + 1).min(headers.len() - 1);
        }
        assert_eq!(count, 3);
    }
}
