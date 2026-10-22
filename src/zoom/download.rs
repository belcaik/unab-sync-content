//! Building blocks for fetching a captured replay asset over plain HTTP.
//!
//! The transfer itself lives in [`crate::core::transfer::fetch_resumable`];
//! what is here is the media HTTP client and the request headers that make
//! Zoom serve the asset.

use crate::zoom::models::{ReplayHeader, ZoomCookie};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, COOKIE};
use reqwest::Client;
use std::time::Duration;
use tracing::warn;

/// The client used to fetch recording media.
///
/// Deliberately not [`crate::core::http::build_http_client`]: that one's 60 s
/// *total* request timeout would kill a large recording mid-stream, and its
/// `Accept: application/json` default is wrong for media. This one has a
/// connect timeout, the crate's usual bounded redirect policy, and no total
/// timeout.
pub fn build_media_client() -> reqwest::Result<Client> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::limited(5))
        .connect_timeout(Duration::from_secs(15))
        .build()
}

/// Builds the header set for re-requesting a captured replay asset: the
/// browser-captured headers, minus any `Range` (resume offsets are decided
/// locally from the `.part` file), plus a `Cookie` header assembled from the
/// stored session cookies that match the download host.
pub fn replay_request_headers(
    asset: &ReplayHeader,
    cookies: &[ZoomCookie],
    download_url: &str,
) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in &asset.headers {
        if name.eq_ignore_ascii_case("range") {
            continue;
        }
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value).or_else(|_| HeaderValue::from_bytes(value.as_bytes())),
        ) {
            headers.insert(name, value);
        }
    }

    let host = url::Url::parse(download_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default();

    let cookie_values: Vec<String> = cookies
        .iter()
        .filter(|c| cookie_matches_host(&host, &c.domain))
        .map(|c| format!("{}={}", c.name, c.value))
        .collect();

    if cookie_values.is_empty() {
        warn!(host, "no cookies found for download host");
    } else if let Ok(value) = HeaderValue::from_str(&cookie_values.join("; ")) {
        headers.insert(COOKIE, value);
    }

    headers
}

/// A cookie matches when the host is the cookie domain itself or a subdomain
/// of it. The cookie domain's leading dot is ignored, and the boundary check
/// keeps `evilzoom.us` from matching a `zoom.us` cookie.
fn cookie_matches_host(host: &str, cookie_domain: &str) -> bool {
    let host = host.to_ascii_lowercase();
    let domain = cookie_domain
        .strip_prefix('.')
        .unwrap_or(cookie_domain)
        .to_ascii_lowercase();
    if host == domain {
        return true;
    }
    host.strip_suffix(&domain)
        .is_some_and(|prefix| prefix.ends_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie(domain: &str, name: &str, value: &str) -> ZoomCookie {
        ZoomCookie {
            domain: domain.to_string(),
            name: name.to_string(),
            value: value.to_string(),
            path: "/".to_string(),
            expires: None,
            secure: true,
            http_only: true,
        }
    }

    fn asset(headers: &[(&str, &str)]) -> ReplayHeader {
        ReplayHeader {
            download_url: String::new(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn a_captured_range_header_is_dropped() {
        let asset = asset(&[("Range", "bytes=99-"), ("x-zoom", "1")]);
        let headers = replay_request_headers(&asset, &[], "https://ssrweb.zoom.us/r/x.mp4");
        assert!(!headers.contains_key(reqwest::header::RANGE));
        assert_eq!(headers.get("x-zoom").unwrap(), "1");
    }

    #[test]
    fn cookies_matching_the_exact_host_are_sent() {
        let headers = replay_request_headers(
            &asset(&[]),
            &[cookie("zoom.us", "zm", "s3cr3t")],
            "https://zoom.us/r/x.mp4",
        );
        assert_eq!(headers.get(COOKIE).unwrap(), "zm=s3cr3t");
    }

    #[test]
    fn cookies_match_subdomains_with_or_without_a_leading_dot() {
        for domain in ["zoom.us", ".zoom.us"] {
            let headers = replay_request_headers(
                &asset(&[]),
                &[cookie(domain, "zm", "s3cr3t")],
                "https://ssrweb.zoom.us/r/x.mp4",
            );
            assert_eq!(headers.get(COOKIE).unwrap(), "zm=s3cr3t", "domain {domain}");
        }
    }

    #[test]
    fn a_lookalike_host_gets_no_cookies() {
        let headers = replay_request_headers(
            &asset(&[]),
            &[cookie(".zoom.us", "zm", "s3cr3t")],
            "https://evilzoom.us/r/x.mp4",
        );
        assert!(!headers.contains_key(COOKIE));
    }

    #[test]
    fn matching_cookies_replace_a_captured_cookie_header() {
        let headers = replay_request_headers(
            &asset(&[("cookie", "stale=1")]),
            &[cookie(".zoom.us", "zm", "s3cr3t")],
            "https://ssrweb.zoom.us/r/x.mp4",
        );
        assert_eq!(headers.get(COOKIE).unwrap(), "zm=s3cr3t");
    }
}
