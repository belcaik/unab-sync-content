use crate::config::Config;
use reqwest::{header, Client, ClientBuilder, RequestBuilder, Response, Url};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Semaphore};
use tokio::time::sleep;
use tracing::warn;

pub fn build_http_client(cfg: &Config) -> Client {
    let mut headers = header::HeaderMap::new();
    headers.insert(header::ACCEPT, header::HeaderValue::from_static("application/json"));

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
        if !part.starts_with('<') { continue; }
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
        let min_interval = if cfg.max_rps == 0 { Duration::from_millis(0) } else { Duration::from_millis((1000 / cfg.max_rps) as u64) };
        Self {
            client,
            limiter: Arc::new(Semaphore::new(cfg.concurrency as usize)),
            last: Arc::new(Mutex::new(Instant::now() - min_interval)),
            min_interval,
            max_retries: 5,
        }
    }

    pub async fn send(&self, rb: RequestBuilder) -> reqwest::Result<Response> {
        let _permit = self.limiter.acquire().await.expect("semaphore");
        // RPS pacing
        {
            let mut last = self.last.lock().await;
            let elapsed = last.elapsed();
            if elapsed < self.min_interval {
                sleep(self.min_interval - elapsed).await;
            }
            *last = Instant::now();
        }

        let mut attempt = 0;
        loop {
            let resp = rb.try_clone().expect("clone request").send().await?;
            if resp.status().as_u16() == 429 {
                let wait = resp
                    .headers()
                    .get(header::RETRY_AFTER)
                    .and_then(|h| h.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(Duration::from_secs)
                    .unwrap_or_else(|| Duration::from_millis(500 * (attempt + 1) as u64));
                warn!(attempt, wait_ms = %wait.as_millis(), "rate limited (429), backing off");
                sleep(wait).await;
            } else if resp.status().is_server_error() && attempt < self.max_retries {
                let back = Duration::from_millis(300 * (1 << attempt));
                warn!(attempt, status = %resp.status().as_u16(), backoff_ms = %back.as_millis(), "server error, retrying");
                sleep(back).await;
            } else {
                return Ok(resp);
            }
            attempt += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
