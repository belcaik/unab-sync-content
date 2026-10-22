//! Extraction of links and media references from Canvas-authored HTML.
//!
//! Canvas stores announcement and page bodies as HTML fragments. This module
//! turns one of those fragments into the set of URLs it references, classifying
//! each as ordinary link, inline media, or a Canvas-hosted file (which carries a
//! numeric id the API can resolve).

use regex::Regex;
use serde::Serialize;
use std::collections::HashSet;
use std::sync::LazyLock;

#[derive(Debug, Serialize, Clone)]
pub struct MediaRef {
    pub url: String,
    pub kind: MediaKind,
    pub file_id: Option<u64>,
    pub local_path: Option<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Image,
    Video,
    Audio,
    CanvasFile,
    Link,
}

#[derive(Debug, Default)]
pub struct ExtractedLinks {
    pub all: Vec<String>,
    pub media: Vec<MediaRef>,
    pub zoom: Vec<String>,
}

/// Compiled once: `extract_links` runs per announcement, inside a loop.
static URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"https?://[A-Za-z0-9\-._~:/?#\[\]@!$&'()*+,;=%]+"#).unwrap());
static IMG_SRC_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)<img[^>]+src=["']([^"']+)["']"#).unwrap());
static A_HREF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)<a[^>]+href=["']([^"']+)["']"#).unwrap());
static MEDIA_SRC_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)<(?:video|audio|source|iframe)[^>]+src=["']([^"']+)["']"#).unwrap()
});
static CANVAS_FILE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)/(?:api/v1/)?files/(\d+)"#).unwrap());
static COURSE_FILE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)/courses/\d+/files/(\d+)"#).unwrap());
static ZOOM_URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"https?://[A-Za-z0-9-]+\.zoom\.(us|com\.cn)/[A-Za-z0-9_/\-?&=%#\.]+"#).unwrap()
});

/// Trailing characters stripped from a bare URL found in prose.
const URL_TRAILERS: &[char] = &[',', ';', ')', ']', '}', '.', '!', '?'];

/// Resolves the Canvas file id a URL points at, if any.
///
/// Course-scoped paths are matched first: `/courses/N/files/M` must yield `M`,
/// not `N`.
fn canvas_file_id(url: &str) -> Option<u64> {
    COURSE_FILE_RE
        .captures(url)
        .or_else(|| CANVAS_FILE_RE.captures(url))
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse::<u64>().ok())
}

/// Every distinct Canvas file id referenced anywhere in a body of HTML.
///
/// `CANVAS_FILE_RE` alone covers every shape [`canvas_file_id`] accepts: a
/// course-scoped `/courses/N/files/M` contains `/files/M` as a substring, so
/// the scan captures the file id, never the course id.
pub fn canvas_file_ids(html: &str) -> HashSet<u64> {
    CANVAS_FILE_RE
        .captures_iter(html)
        .filter_map(|c| c.get(1)?.as_str().parse::<u64>().ok())
        .collect()
}

pub fn extract_links(html: &str) -> ExtractedLinks {
    let mut all: Vec<String> = Vec::new();
    let mut seen_all = HashSet::new();
    let mut media: Vec<MediaRef> = Vec::new();
    let mut seen_media = HashSet::new();

    let push_all = |s: String, all: &mut Vec<String>, seen: &mut HashSet<String>| {
        let trimmed = s.trim_end_matches(URL_TRAILERS).to_string();
        if !trimmed.is_empty() && seen.insert(trimmed.clone()) {
            all.push(trimmed);
        }
    };

    for m in URL_RE.find_iter(html) {
        push_all(m.as_str().to_string(), &mut all, &mut seen_all);
    }
    for cap in IMG_SRC_RE.captures_iter(html) {
        if let Some(src) = cap.get(1) {
            let url = src.as_str().to_string();
            push_all(url.clone(), &mut all, &mut seen_all);
            let file_id = canvas_file_id(&url);
            let media_key = url.clone();
            if seen_media.insert(media_key) {
                media.push(MediaRef {
                    url,
                    kind: if file_id.is_some() {
                        MediaKind::CanvasFile
                    } else {
                        MediaKind::Image
                    },
                    file_id,
                    local_path: None,
                });
            }
        }
    }
    for cap in MEDIA_SRC_RE.captures_iter(html) {
        if let Some(src) = cap.get(1) {
            let url = src.as_str().to_string();
            push_all(url.clone(), &mut all, &mut seen_all);
            let kind = classify_by_filename(&url);
            if seen_media.insert(url.clone()) {
                let file_id = canvas_file_id(&url);
                media.push(MediaRef {
                    url,
                    kind: if file_id.is_some() {
                        MediaKind::CanvasFile
                    } else {
                        kind
                    },
                    file_id,
                    local_path: None,
                });
            }
        }
    }
    for cap in A_HREF_RE.captures_iter(html) {
        if let Some(href) = cap.get(1) {
            let url = href.as_str().to_string();
            push_all(url.clone(), &mut all, &mut seen_all);
            let file_id = canvas_file_id(&url);
            if let Some(fid) = file_id {
                if seen_media.insert(url.clone()) {
                    media.push(MediaRef {
                        url,
                        kind: MediaKind::CanvasFile,
                        file_id: Some(fid),
                        local_path: None,
                    });
                }
            }
        }
    }

    let zoom = extract_zoom_urls(html);
    for z in &zoom {
        if seen_all.insert(z.clone()) {
            all.push(z.clone());
        }
    }

    ExtractedLinks { all, media, zoom }
}

/// Extracts the distinct Zoom meeting/recording URLs referenced by `input`.
///
/// Operates on raw text rather than parsed HTML: Zoom links are frequently
/// pasted into announcement prose without an anchor tag.
pub fn extract_zoom_urls(input: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for cap in ZOOM_URL_RE.captures_iter(input) {
        if let Some(m) = cap.get(0) {
            let url = m.as_str().trim_end_matches(URL_TRAILERS).to_string();
            if seen.insert(url.clone()) {
                out.push(url);
            }
        }
    }
    out
}

pub fn classify_by_filename(s: &str) -> MediaKind {
    let lower = s.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    match ext {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" => MediaKind::Image,
        "mp4" | "mov" | "webm" | "mkv" | "avi" => MediaKind::Video,
        "mp3" | "wav" | "ogg" | "m4a" | "flac" => MediaKind::Audio,
        _ => MediaKind::Link,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canvas_file_ids_matches_plain_api_and_course_paths() {
        // Every shape either of the two pre-merge regexes accepted: a bare
        // `/files/N`, `/api/v1/files/N`, and a course-scoped
        // `/courses/N/files/M` (the file id wins, never the course id).
        let html = r#"<a href="/courses/1/files/22">a</a><img src="/api/v1/files/33"/>"#;
        let ids = canvas_file_ids(html);
        assert!(ids.contains(&22));
        assert!(ids.contains(&33));
        assert!(!ids.contains(&1), "course id must not be captured");
    }

    #[test]
    fn canvas_file_ids_matches_bare_and_case_insensitive_paths() {
        let html = r#"<a href="/files/44">a</a><img src="/API/V1/FILES/55"/>"#;
        let ids = canvas_file_ids(html);
        assert!(ids.contains(&44));
        assert!(ids.contains(&55));
    }

    #[test]
    fn canvas_file_ids_dedupes_and_ignores_non_file_urls() {
        let html = r#"<a href="https://canvas.example.com/files/7?verifier=x">a</a>
                     <a href="https://canvas.example.com/files/7/download">b</a>
                     <a href="https://example.com/page">c</a>"#;
        let ids = canvas_file_ids(html);
        assert_eq!(ids.len(), 1);
        assert!(ids.contains(&7));
    }

    #[test]
    fn extracts_image_anchor_and_canvas_file() {
        let html = r#"
            <p>See <a href="https://example.com/courses/42/files/9001/download">slides</a>.</p>
            <img src="https://canvas.example.com/courses/42/files/9002/preview"/>
            <video src="https://cdn.example.com/clip.mp4"></video>
            <a href="https://other.example.com/page">other</a>
            <p>Join: https://abc-d.zoom.us/j/123456789?pwd=foo</p>
        "#;
        let out = extract_links(html);
        assert!(out.all.iter().any(|u| u.contains("zoom.us")));
        assert_eq!(out.zoom.len(), 1);
        let canvas_files: Vec<_> = out
            .media
            .iter()
            .filter(|m| matches!(m.kind, MediaKind::CanvasFile))
            .collect();
        assert!(canvas_files.iter().any(|m| m.file_id == Some(9001)));
        assert!(canvas_files.iter().any(|m| m.file_id == Some(9002)));
        assert!(out
            .media
            .iter()
            .any(|m| matches!(m.kind, MediaKind::Video) && m.url.ends_with(".mp4")));
    }

    #[test]
    fn extracts_real_unab_ipynb_link() {
        let html = r#"<p>Descarga el cuaderno: <a class="instructure_file_link instructure_scribd_file inline_disabled" title="taller.ipynb" href="https://canvas.unab.cl/courses/212693/files/20411861?verifier=A1DyJn5PDzVeKNBhRAkChfpYPq5U2NKGMxHZVYPH&amp;wrap=1" target="_blank" rel="noopener" data-api-endpoint="https://canvas.unab.cl/api/v1/courses/212693/files/20411861" data-api-returntype="File">taller.ipynb</a></p>"#;
        let out = extract_links(html);
        let canvas_files: Vec<_> = out
            .media
            .iter()
            .filter(|m| matches!(m.kind, MediaKind::CanvasFile))
            .collect();
        assert!(
            canvas_files.iter().any(|m| m.file_id == Some(20411861)),
            "expected file_id 20411861 in media; got {:?}",
            out.media
        );
    }

    #[test]
    fn classify_extensions() {
        assert_eq!(classify_by_filename("a.PNG"), MediaKind::Image);
        assert_eq!(classify_by_filename("a.mp4"), MediaKind::Video);
        assert_eq!(classify_by_filename("a.mp3"), MediaKind::Audio);
        assert_eq!(classify_by_filename("a.pdf"), MediaKind::Link);
    }
}
