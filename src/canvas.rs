use crate::config::Config;
use crate::http::{build_http_client, parse_next_link, HttpCtx};
use reqwest::{header, Url};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::io;
use thiserror::Error;
use tracing::{debug, error, warn};

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
    Decode(#[from] serde_json::Error),
    #[error("missing canvas token; run `auth canvas` first")]
    MissingToken,
    #[error("invalid url: {0}")]
    Url(#[from] url::ParseError),
}

/// Maximum body text retained for diagnostics on a failed Canvas response.
const ERROR_SNIPPET_CHARS: usize = 1000;

/// Safety valve: Canvas pagination is server-driven, so a malformed `Link` chain
/// could otherwise loop indefinitely.
const MAX_PAGES: usize = 200;

pub struct CanvasClient {
    pub base: Url,
    pub http: HttpCtx,
    pub token: String,
}

impl CanvasClient {
    pub async fn from_config() -> Result<Self, CanvasError> {
        let cfg = Config::load_or_init()?;
        let http = HttpCtx::new(&cfg, build_http_client(&cfg));
        let base = Url::parse(&cfg.canvas.base_url)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid base_url"))?;
        let token = resolve_token(&cfg).await.ok_or(CanvasError::MissingToken)?;
        Ok(CanvasClient { base, http, token })
    }

    fn auth_header_val(&self) -> Result<header::HeaderValue, CanvasError> {
        // `token_cmd` output routinely carries a trailing newline, which is not a
        // legal header value.
        let v = format!("Bearer {}", self.token.trim());
        header::HeaderValue::from_str(&v).map_err(|_| CanvasError::MissingToken)
    }

    /// Performs one authenticated GET and decodes the body.
    ///
    /// Returns the decoded payload alongside the raw `Link` header, which the
    /// paginated variant uses to find the next page.
    async fn get_json<T: DeserializeOwned>(
        &self,
        url: Url,
        ctx: &str,
    ) -> Result<(T, Option<String>), CanvasError> {
        debug!(method = "GET", url = %url, ctx, "canvas request");
        let resp = self
            .http
            .send(
                self.http
                    .client
                    .get(url)
                    .header(header::AUTHORIZATION, self.auth_header_val()?),
            )
            .await?;

        let status = resp.status();
        // The `Link` header must be read before the body consumes the response.
        let link = resp
            .headers()
            .get(header::LINK)
            .and_then(|h| h.to_str().ok())
            .map(|s| s.to_string());
        let text = resp.text().await?;

        if !status.is_success() {
            let snippet = text.chars().take(ERROR_SNIPPET_CHARS).collect::<String>();
            error!(status = %status.as_u16(), body = %snippet, ctx, "canvas non-success response");
            return Err(CanvasError::Status(status.as_u16(), snippet));
        }

        match serde_json::from_str::<T>(&text) {
            Ok(v) => Ok((v, link)),
            Err(e) => {
                let snippet = text.chars().take(ERROR_SNIPPET_CHARS).collect::<String>();
                error!(error = %e, body = %snippet, ctx, "canvas decode failure");
                Err(CanvasError::Decode(e))
            }
        }
    }

    /// Walks a `Link`-header-paginated Canvas collection to exhaustion.
    async fn list_paginated<T: DeserializeOwned>(
        &self,
        path: &str,
        ctx: &str,
    ) -> Result<Vec<T>, CanvasError> {
        let mut out = Vec::new();
        let mut next = Some(self.base.join(path)?);
        for _ in 0..MAX_PAGES {
            let Some(url) = next.take() else {
                return Ok(out);
            };
            let (mut page, link) = self.get_json::<Vec<T>>(url, ctx).await?;
            out.append(&mut page);
            next = link.as_deref().and_then(parse_next_link);
        }
        warn!(ctx, pages = MAX_PAGES, "pagination cap reached; truncating");
        Ok(out)
    }

    pub async fn list_courses(&self) -> Result<Vec<Course>, CanvasError> {
        self.list_paginated(
            "/api/v1/courses?enrollment_state=active&per_page=100",
            "courses",
        )
        .await
    }

    pub async fn list_modules_with_items(
        &self,
        course_id: u64,
    ) -> Result<Vec<Module>, CanvasError> {
        self.list_paginated(
            &format!("/api/v1/courses/{course_id}/modules?include=items&per_page=100"),
            "modules",
        )
        .await
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

#[derive(Debug, Clone, Deserialize)]
pub struct Assignment {
    pub id: u64,
    pub name: Option<String>,
    pub description: Option<String>,
    pub updated_at: Option<String>,
    #[serde(default)]
    pub due_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub unlock_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub lock_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub points_possible: Option<f64>,
    #[serde(default)]
    pub omit_from_final_grade: Option<bool>,
    #[serde(default)]
    pub html_url: Option<String>,
    #[serde(default)]
    pub assignment_group_id: Option<u64>,
    #[serde(default)]
    pub submission_types: Option<Vec<String>>,
    #[serde(default)]
    pub published: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct Announcement {
    pub id: u64,
    pub title: Option<String>,
    pub message: Option<String>,
    pub posted_at: Option<String>,
    pub html_url: Option<String>,
    #[serde(default)]
    pub author: Option<AnnouncementAuthor>,
    #[serde(default)]
    pub attachments: Vec<FileObj>,
}

#[derive(Debug, Deserialize)]
pub struct AnnouncementAuthor {
    pub display_name: Option<String>,
}

impl CanvasClient {
    pub async fn get_page(&self, course_id: u64, page_url: &str) -> Result<PageObj, CanvasError> {
        let url = self.base.join(&format!(
            "/api/v1/courses/{course_id}/pages/{}",
            urlencoding::encode(page_url)
        ))?;
        Ok(self.get_json::<PageObj>(url, "page").await?.0)
    }
    pub async fn get_file(&self, file_id: u64) -> Result<FileObj, CanvasError> {
        let url = self.base.join(&format!("/api/v1/files/{file_id}"))?;
        Ok(self.get_json::<FileObj>(url, "file").await?.0)
    }

    pub async fn list_announcements(
        &self,
        course_id: u64,
    ) -> Result<Vec<Announcement>, CanvasError> {
        self.list_paginated(
            &format!(
                "/api/v1/courses/{course_id}/discussion_topics\
                 ?only_announcements=true&per_page=100"
            ),
            "announcements",
        )
        .await
    }

    pub async fn list_assignments(&self, course_id: u64) -> Result<Vec<Assignment>, CanvasError> {
        self.list_paginated(
            &format!("/api/v1/courses/{course_id}/assignments?per_page=100"),
            "assignments",
        )
        .await
    }

    /// Fetches the caller's own submissions for every assignment in a course,
    /// in one bulk request rather than one per assignment (calendar-sync
    /// spec D1, ticket 09). `student_ids[]=self` is Canvas's shorthand for
    /// "the authenticated user" and needs no id lookup.
    ///
    /// `list_paginated` joins a plain `&str` onto `base` and `CanvasClient`
    /// has no query builder (spec D1's "nota de encaje"), so the repeated
    /// `student_ids[]` param is pre-encoded here (`%5B%5D`) rather than
    /// passed as literal `[]` — the same approach `get_page` already uses for
    /// `page_url` via `urlencoding::encode`.
    pub async fn list_submissions(&self, course_id: u64) -> Result<Vec<Submission>, CanvasError> {
        self.list_paginated(
            &format!(
                "/api/v1/courses/{course_id}/students/submissions\
                 ?student_ids%5B%5D=self&per_page=100"
            ),
            "submissions",
        )
        .await
    }
}

/// One student's submission record for one assignment, as returned by the
/// bulk `students/submissions` endpoint.
///
/// Minimal by design: the calendar-sync flow (spec D7) only needs to decide
/// "done or not", which `submitted_at` and `workflow_state` answer between
/// them. Other fields Canvas returns (`grade`, `score`, `attempt`, …) have no
/// consumer yet and are left off rather than carried speculatively.
#[derive(Debug, Clone, Deserialize)]
pub struct Submission {
    pub assignment_id: u64,
    #[serde(default)]
    pub submitted_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub workflow_state: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://canvas.example.com").unwrap()
    }

    #[test]
    fn announcements_path_survives_the_line_continuation() {
        let course_id = 42u64;
        let path = format!(
            "/api/v1/courses/{course_id}/discussion_topics\
             ?only_announcements=true&per_page=100"
        );
        assert_eq!(
            base().join(&path).unwrap().as_str(),
            "https://canvas.example.com/api/v1/courses/42/discussion_topics\
             ?only_announcements=true&per_page=100"
        );
    }

    #[test]
    fn page_url_is_percent_encoded() {
        let url = base()
            .join(&format!(
                "/api/v1/courses/1/pages/{}",
                urlencoding::encode("semana 1/intro")
            ))
            .unwrap();
        assert!(url.as_str().ends_with("/pages/semana%201%2Fintro"));
    }

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

    #[test]
    fn submissions_path_pre_encodes_the_repeated_bracket_param() {
        let course_id = 42u64;
        let path = format!(
            "/api/v1/courses/{course_id}/students/submissions\
             ?student_ids%5B%5D=self&per_page=100"
        );
        assert_eq!(
            base().join(&path).unwrap().as_str(),
            "https://canvas.example.com/api/v1/courses/42/students/submissions\
             ?student_ids%5B%5D=self&per_page=100"
        );
    }

    #[test]
    fn submission_deserializes_submitted_at_and_workflow_state() {
        let body = r#"
        {
            "assignment_id": 4501,
            "submitted_at": "2026-02-18T10:00:00Z",
            "workflow_state": "submitted"
        }
        "#;
        let submission: Submission = serde_json::from_str(body).unwrap();
        assert_eq!(submission.assignment_id, 4501);
        assert!(submission.submitted_at.is_some());
        assert_eq!(submission.workflow_state.as_deref(), Some("submitted"));
    }

    #[test]
    fn submission_without_submission_still_deserializes() {
        let body = r#"
        {
            "assignment_id": 9001,
            "submitted_at": null,
            "workflow_state": "unsubmitted"
        }
        "#;
        let submission: Submission = serde_json::from_str(body).unwrap();
        assert_eq!(submission.assignment_id, 9001);
        assert_eq!(submission.submitted_at, None);
        assert_eq!(submission.workflow_state.as_deref(), Some("unsubmitted"));
    }

    #[test]
    fn assignment_deserializes_dates_and_grading_metadata_from_a_full_response() {
        let body = r#"
        {
            "id": 4501,
            "name": "Problem Set 3",
            "description": "<p>Solve the attached problems.</p>",
            "updated_at": "2026-02-10T18:04:00Z",
            "due_at": "2026-02-20T23:59:00Z",
            "unlock_at": "2026-02-13T00:00:00Z",
            "lock_at": "2026-02-21T06:00:00Z",
            "points_possible": 25.0,
            "omit_from_final_grade": false,
            "html_url": "https://canvas.example.com/courses/1/assignments/4501",
            "assignment_group_id": 77,
            "submission_types": ["online_upload", "online_text_entry"],
            "published": true
        }
        "#;

        let assignment: Assignment = serde_json::from_str(body).unwrap();

        assert_eq!(assignment.id, 4501);
        assert_eq!(assignment.name.as_deref(), Some("Problem Set 3"));
        assert_eq!(
            assignment.due_at,
            Some(
                chrono::DateTime::parse_from_rfc3339("2026-02-20T23:59:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc)
            )
        );
        assert_eq!(
            assignment.unlock_at,
            Some(
                chrono::DateTime::parse_from_rfc3339("2026-02-13T00:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc)
            )
        );
        assert_eq!(
            assignment.lock_at,
            Some(
                chrono::DateTime::parse_from_rfc3339("2026-02-21T06:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc)
            )
        );
        assert_eq!(assignment.points_possible, Some(25.0));
        assert_eq!(assignment.omit_from_final_grade, Some(false));
        assert_eq!(
            assignment.html_url.as_deref(),
            Some("https://canvas.example.com/courses/1/assignments/4501")
        );
        assert_eq!(assignment.assignment_group_id, Some(77));
        assert_eq!(
            assignment.submission_types,
            Some(vec![
                "online_upload".to_string(),
                "online_text_entry".to_string()
            ])
        );
        assert_eq!(assignment.published, Some(true));
    }

    #[test]
    fn assignment_without_optional_fields_still_deserializes() {
        let body = r#"
        {
            "id": 9001,
            "name": "Reading Reflection",
            "description": null,
            "updated_at": "2026-01-05T09:00:00Z"
        }
        "#;

        let assignment: Assignment = serde_json::from_str(body).unwrap();

        assert_eq!(assignment.id, 9001);
        assert_eq!(assignment.due_at, None);
        assert_eq!(assignment.unlock_at, None);
        assert_eq!(assignment.lock_at, None);
        assert_eq!(assignment.points_possible, None);
        assert_eq!(assignment.omit_from_final_grade, None);
        assert_eq!(assignment.html_url, None);
        assert_eq!(assignment.assignment_group_id, None);
        assert_eq!(assignment.submission_types, None);
        assert_eq!(assignment.published, None);
    }
}
