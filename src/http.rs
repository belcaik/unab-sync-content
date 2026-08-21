use crate::config::Config;
use reqwest::{header, Client, ClientBuilder, RequestBuilder, Response, Url};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Semaphore};
use tokio::time::sleep;
use tracing::warn;

pub fn build_http_client(cfg: &Config) -> Client {
    let mut headers = header::HeaderMap::new();
    headers.insert(
        header::ACCEPT,
        header::HeaderValue::from_static("application/json"),
    );

    let builder = ClientBuilder::new()
        .user_agent(if cfg.user_agent.is_empty() {
            format!("u_crawler/{}", env!("CARGO_PKG_VERSION"))
        } else {
            cfg.user_agent.clone()
        })
        .default_headers(headers)
        .gzip(true)
        .brotli(true)
        .deflate(true)
        .connect_timeout(Duration::from_secs(15))
        .pool_idle_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(60));

    builder.build().expect("http client build")
}

/// Extract the rel="next" link from an RFC5988 Link header, if present.
pub fn parse_next_link(link_header: &str) -> Option<Url> {
    // Simple stateful parser to avoid false positives in quoted params
    // Example: <https://example.com?a=1>; rel="next", <...>; rel="prev"
    for part in link_header.split(',') {
        let part = part.trim();
        if !part.starts_with('<') {
            continue;
        }
        let end = part.find('>')?;
        let url_str = &part[1..end];
        let params = &part[end + 1..];
        // search for rel="next" token
        let mut is_next = false;
        for p in params.split(';').map(|s| s.trim()) {
            if let Some((k, v)) = p.split_once('=') {
                if k.eq_ignore_ascii_case("rel") {
                    let v = v.trim_matches('"');
                    if v.eq_ignore_ascii_case("next") {
                        is_next = true;
                        break;
                    }
                }
            }
        }
        if is_next {
            if let Ok(url) = Url::parse(url_str) {
                return Some(url);
            }
        }
    }
    None
}

/// Upper bound on a server-supplied `Retry-After`.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct HttpCtx {
    pub client: Client,
    limiter: Arc<Semaphore>,
    last: Arc<Mutex<Instant>>, // crude RPS cap
    min_interval: Duration,
    max_retries: usize,
}

impl HttpCtx {
    pub fn new(cfg: &Config, client: Client) -> Self {
        let min_interval = if cfg.max_rps == 0 {
            Duration::from_millis(0)
        } else {
            Duration::from_millis((1000 / cfg.max_rps) as u64)
        };
        Self {
            client,
            limiter: Arc::new(Semaphore::new(cfg.concurrency as usize)),
            last: Arc::new(Mutex::new(Instant::now() - min_interval)),
            min_interval,
            max_retries: 5,
        }
    }

    pub async fn send(&self, rb: RequestBuilder) -> reqwest::Result<Response> {
        let _permit = self.limiter.acquire().await.ok();

        // RPS pacing: reserve this request's slot, then release the lock *before*
        // sleeping. Holding the guard across the await would serialise every request
        // at this gate and make the concurrency semaphore above it meaningless.
        let wake_at = {
            let mut last = self.last.lock().await;
            let slot = (*last + self.min_interval).max(Instant::now());
            *last = slot;
            slot
        };
        let now = Instant::now();
        if wake_at > now {
            sleep(wake_at - now).await;
        }

        // A streaming body cannot be replayed, so such a request gets one attempt.
        if rb.try_clone().is_none() {
            return rb.send().await;
        }

        for attempt in 0..=self.max_retries {
            let resp = rb
                .try_clone()
                .expect("checked clonable above")
                .send()
                .await?;

            let backoff = match Self::retry_after(&resp, attempt) {
                Some(d) => d,
                None => return Ok(resp),
            };
            if attempt == self.max_retries {
                warn!(
                    attempt,
                    status = %resp.status().as_u16(),
                    "retries exhausted, returning last response"
                );
                return Ok(resp);
            }
            warn!(attempt, status = %resp.status().as_u16(), backoff_ms = %backoff.as_millis(), "retrying");
            sleep(backoff).await;
        }

        // `max_retries` is a `usize`, so the loop above always runs at least once and
        // every path out of it returns.
        unreachable!("retry loop always returns")
    }

    /// Returns how long to wait before retrying, or `None` if the response is final.
    fn retry_after(resp: &Response, attempt: usize) -> Option<Duration> {
        let status = resp.status();
        if status.as_u16() == 429 {
            let hinted = resp
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|h| h.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(Duration::from_secs);
            // `Retry-After` is server-controlled; cap it so a hostile or buggy value
            // cannot park the process for hours.
            return Some(match hinted {
                Some(d) => d.min(MAX_RETRY_AFTER),
                None => Duration::from_millis(500 * (attempt as u64 + 1)),
            });
        }
        if status.is_server_error() {
            return Some(Duration::from_millis(300 * (1 << attempt.min(6))));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: u16, retry_after: Option<&str>) -> Response {
        let mut b = http::Response::builder().status(status);
        if let Some(v) = retry_after {
            b = b.header(header::RETRY_AFTER, v);
        }
        Response::from(b.body("").unwrap())
    }

    #[test]
    fn retry_after_is_none_for_success() {
        assert!(HttpCtx::retry_after(&response(200, None), 0).is_none());
    }

    #[test]
    fn retry_after_is_none_for_client_errors_other_than_429() {
        assert!(HttpCtx::retry_after(&response(404, None), 0).is_none());
    }

    #[test]
    fn retry_after_honours_the_server_hint_on_429() {
        let d = HttpCtx::retry_after(&response(429, Some("7")), 0).unwrap();
        assert_eq!(d, Duration::from_secs(7));
    }

    #[test]
    fn retry_after_caps_a_hostile_server_hint() {
        let d = HttpCtx::retry_after(&response(429, Some("999999")), 0).unwrap();
        assert_eq!(d, MAX_RETRY_AFTER);
    }

    #[test]
    fn retry_after_backs_off_on_server_errors() {
        assert!(HttpCtx::retry_after(&response(503, None), 0).is_some());
    }

    #[test]
    fn link_header_parses_next() {
        let h = "<https://api.example.com/courses?page=2>; rel=\"next\", <https://api.example.com/courses?page=5>; rel=\"last\"";
        let u = parse_next_link(h).unwrap();
        assert_eq!(u.as_str(), "https://api.example.com/courses?page=2");
    }

    #[test]
    fn link_header_none_when_missing() {
        let h = "<https://api.example.com/courses?page=5>; rel=\"last\"";
        assert!(parse_next_link(h).is_none());
    }

    #[test]
    fn link_header_ignores_other_rels() {
        let h = "<https://api.example.com/courses?page=2>; rel=\"prev\", <https://api.example.com/courses?page=3>; rel=\"first\"";
        assert!(parse_next_link(h).is_none());
    }
}
