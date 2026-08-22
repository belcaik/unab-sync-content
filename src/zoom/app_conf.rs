//! Parsing of Zoom's `window.appConf` bootstrap blob.
//!
//! Zoom's LTI launch page embeds the session identifiers the recordings API
//! needs — the LTI `scid` and a set of `x-zm-*` / `x-xsrf-token` request headers
//! — inside an inline `window.appConf = { ... }` script. There is no API that
//! returns them, so they are scraped out of the intercepted response body.
//!
//! This module is deliberately pure: it takes the body text and returns what it
//! found, so the scraping can be tested without a browser.

use regex::Regex;
use std::collections::HashMap;
use std::sync::LazyLock;

/// How much of the body to scan past `window.appConf`.
///
/// `ajaxHeaders` can sit well below the `scid`, so the window is generous.
const SCAN_CHARS: usize = 20_000;

static SCID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"scid\s*:\s*['"]([^'"]+)['"]"#).unwrap());
static AJAX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?s)ajaxHeaders\s*:\s*\[(.*?)\]"#).unwrap());
static KV_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\{\s*key\s*:\s*['"]([^'"]+)['"]\s*,\s*value\s*:\s*['"]([^'"]+)['"]\s*\}"#)
        .unwrap()
});
static XSRF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)['"]?x-xsrf-token['"]?\s*:\s*['"]([^'"]+)['"]"#).unwrap());

/// What one interception yielded. Either field may be absent: the identifiers
/// arrive across several responses.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AppConf {
    pub scid: Option<String>,
    pub headers: HashMap<String, String>,
}

impl AppConf {
    pub fn is_empty(&self) -> bool {
        self.scid.is_none() && self.headers.is_empty()
    }
}

/// Whether a header from `ajaxHeaders` is one the recordings API requires.
fn is_session_header(key: &str) -> bool {
    let k = key.to_lowercase();
    k.starts_with("x-zm-") || k == "x-xsrf-token"
}

/// Extracts the session identifiers from a response body, if it carries any.
pub fn parse(body: &str) -> AppConf {
    let Some(idx) = body.find("window.appConf") else {
        return AppConf::default();
    };

    // Take SCAN_CHARS *characters*, not bytes: slicing a decoded body at a fixed
    // byte offset would panic mid-codepoint.
    let chunk: String = body[idx..].chars().take(SCAN_CHARS).collect();

    let mut out = AppConf {
        scid: SCID_RE
            .captures(&chunk)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string()),
        headers: HashMap::new(),
    };

    if let Some(body) = AJAX_RE.captures(&chunk).and_then(|c| c.get(1)) {
        for cap in KV_RE.captures_iter(body.as_str()) {
            if let (Some(k), Some(v)) = (cap.get(1), cap.get(2)) {
                if is_session_header(k.as_str()) {
                    out.headers
                        .insert(k.as_str().to_string(), v.as_str().to_string());
                }
            }
        }
    }

    // The XSRF token is sometimes written outside the ajaxHeaders array.
    if !out
        .headers
        .keys()
        .any(|k| k.eq_ignore_ascii_case("x-xsrf-token"))
    {
        if let Some(v) = XSRF_RE.captures(&chunk).and_then(|c| c.get(1)) {
            out.headers
                .insert("x-xsrf-token".to_string(), v.as_str().to_string());
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = r#"
        <script>
        window.appConf = {
            scid: "abc123scid",
            ajaxHeaders: [
                {key: "x-zm-aid", value: "AID1"},
                {key: "x-zm-cluster-id", value: "us06"},
                {key: "x-xsrf-token", value: "XSRF1"},
                {key: "content-type", value: "application/json"}
            ]
        };
        </script>
    "#;

    #[test]
    fn extracts_the_scid() {
        assert_eq!(parse(BODY).scid.as_deref(), Some("abc123scid"));
    }

    #[test]
    fn keeps_only_session_headers() {
        let got = parse(BODY).headers;
        assert_eq!(got.get("x-zm-aid").map(String::as_str), Some("AID1"));
        assert_eq!(got.get("x-xsrf-token").map(String::as_str), Some("XSRF1"));
        assert!(
            !got.contains_key("content-type"),
            "content-type is not a session header"
        );
    }

    #[test]
    fn finds_a_standalone_xsrf_token_outside_ajax_headers() {
        let body = r#"window.appConf = { scid: "s1", "x-xsrf-token": "LOOSE" }"#;
        assert_eq!(
            parse(body).headers.get("x-xsrf-token").map(String::as_str),
            Some("LOOSE")
        );
    }

    #[test]
    fn a_body_without_app_conf_yields_nothing() {
        assert!(parse("<html>no bootstrap here</html>").is_empty());
    }

    #[test]
    fn a_multibyte_body_does_not_panic_at_the_scan_boundary() {
        // The scan window used to be a byte slice; an accented page truncated
        // mid-codepoint would panic.
        let padding = "á".repeat(SCAN_CHARS);
        let body = format!(r#"window.appConf = {{ scid: "ok" }}; // {padding}"#);
        assert_eq!(parse(&body).scid.as_deref(), Some("ok"));
    }
}
