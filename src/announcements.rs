use crate::canvas::{CanvasClient, FileObj};
use crate::config::Config;
use crate::fsutil::{
    atomic_rename, atomic_write, ensure_dir, sanitize_component, sanitize_filename_preserve_ext,
};
use crate::http::{build_http_client, HttpCtx};
use crate::progress::{progress_bar, spinner};
use crate::state::{ItemState, State};
use html2md::parse_html;
use regex::Regex;
use reqwest::header;
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;
use tracing::{info, warn};

#[derive(Debug, Serialize)]
pub struct AnnouncementRecord {
    pub id: u64,
    pub title: String,
    pub posted_at: Option<String>,
    pub html_url: Option<String>,
    pub author: Option<String>,
    pub body_md_path: Option<String>,
    pub links: Vec<String>,
    pub media: Vec<MediaRef>,
    pub zoom_links: Vec<String>,
}

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

pub async fn run_discovery(
    filter_course_id: Option<u64>,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::load_or_init()?;
    let http = build_http_client(&cfg);
    let httpctx = HttpCtx::new(&cfg, http);
    let canvas = CanvasClient::from_config().await?;

    let courses = canvas.list_courses().await?;
    let ignored: HashSet<String> = cfg.canvas.ignored_courses.iter().cloned().collect();
    let selected: Vec<crate::canvas::Course> = if let Some(cid) = filter_course_id {
        courses.into_iter().filter(|c| c.id == cid).collect()
    } else {
        courses
            .into_iter()
            .filter(|c| !ignored.contains(&c.id.to_string()))
            .collect()
    };

    if selected.is_empty() {
        println!("No matching courses.");
        return Ok(());
    }

    let course_pb = progress_bar(selected.len() as u64, "Scanning announcements");
    let mut totals = (0usize, 0usize, 0usize); // announcements, links, media
    for course in selected {
        course_pb.inc(1);
        course_pb.set_message(format!("Course {}", course.id));
        let course_dir = course_dir_for(&cfg, &course);
        if !dry_run {
            ensure_dir(&course_dir).await?;
        }
        let state_path = course_dir.join("state.json");
        let mut state = State::load(&state_path).await;

        let summary = run_for_course(
            &cfg,
            &canvas,
            &httpctx,
            &course,
            &course_dir,
            &mut state,
            dry_run,
            false,
        )
        .await
        .unwrap_or_else(|e| {
            warn!(course_id = course.id, error = %e, "announcements failed for course");
            CourseSummary::default()
        });
        totals.0 += summary.announcements;
        totals.1 += summary.links;
        totals.2 += summary.media;

        if !dry_run {
            state.save(&state_path).await?;
        }
    }
    course_pb.finish_and_clear();

    println!(
        "{}Announcements: {} | links: {} | media refs: {}",
        if dry_run { "DRY-RUN: " } else { "" },
        totals.0,
        totals.1,
        totals.2,
    );
    Ok(())
}

#[derive(Debug, Default)]
pub struct CourseSummary {
    pub announcements: usize,
    pub links: usize,
    pub media: usize,
}

fn course_dir_for(cfg: &Config, course: &crate::canvas::Course) -> PathBuf {
    let code = course.course_code.clone().unwrap_or_default();
    PathBuf::from(&cfg.download_root).join(if code.is_empty() {
        sanitize_component(&course.name)
    } else {
        format!(
            "{}_{}",
            sanitize_component(&course.name),
            sanitize_component(code)
        )
    })
}

#[allow(clippy::too_many_arguments)]
pub async fn run_for_course(
    cfg: &Config,
    canvas: &CanvasClient,
    httpctx: &HttpCtx,
    course: &crate::canvas::Course,
    course_dir: &Path,
    state: &mut State,
    dry_run: bool,
    verbose: bool,
) -> Result<CourseSummary, Box<dyn std::error::Error>> {
    let ann_dir = course_dir.join("announcements");
    let media_dir = ann_dir.join("media");
    if !dry_run {
        ensure_dir(&ann_dir).await?;
    }

    let sp = spinner(&format!("Loading announcements for {}", course.name));
    let announcements = match canvas.list_announcements(course.id).await {
        Ok(v) => v,
        Err(e) => {
            sp.finish_and_clear();
            warn!(course_id = course.id, error = %e, "list_announcements failed; skipping");
            return Ok(CourseSummary::default());
        }
    };
    sp.finish_and_clear();

    let mut summary = CourseSummary::default();
    let mut records: Vec<AnnouncementRecord> = Vec::with_capacity(announcements.len());

    let pb = progress_bar(
        announcements.len() as u64,
        &format!("Announcements in {}", course.name),
    );
    for ann in announcements {
        pb.inc(1);
        let title = ann
            .title
            .clone()
            .unwrap_or_else(|| format!("announcement_{}", ann.id));
        pb.set_message(title.clone());
        let html = ann.message.clone().unwrap_or_default();
        let extracted = extract_links(&html);
        summary.announcements += 1;
        summary.links += extracted.all.len();
        summary.media += extracted.media.len();

        let date_prefix = ann
            .posted_at
            .as_deref()
            .and_then(|s| s.get(0..10))
            .map(|d| format!("{}_", d))
            .unwrap_or_default();
        let slug = sanitize_component(&title);
        let slug_short: String = slug.chars().take(60).collect();
        let body_filename = format!("{}{}_{}.md", date_prefix, slug_short, ann.id);
        let body_path = ann_dir.join(&body_filename);

        let key = format!("announcement:{}", ann.id);
        let prev = state.get(&key).cloned();
        let unchanged = match (&prev, ann.posted_at.as_deref()) {
            (Some(p), Some(posted)) => p.updated_at.as_deref() == Some(posted),
            _ => false,
        };

        let md = parse_html(&html);
        let body_md_path_rel = format!("announcements/{}", body_filename);

        if dry_run {
            info!(
                course_id = course.id,
                announcement_id = ann.id,
                links = extracted.all.len(),
                media = extracted.media.len(),
                "dry-run announcement"
            );
        } else if unchanged && body_path.exists() {
            if verbose {
                info!(
                    course_id = course.id,
                    announcement_id = ann.id,
                    "announcement unchanged"
                );
            }
        } else {
            atomic_write(&body_path, md.as_bytes()).await?;
            state.set(
                key.clone(),
                ItemState {
                    etag: None,
                    updated_at: ann.posted_at.clone(),
                    size: Some(md.len() as u64),
                    content_hash: None,
                    last_error: None,
                    error_count: None,
                },
            );
        }

        // Download media (Canvas-hosted files) and inline attachments
        let mut media_with_paths: Vec<MediaRef> = Vec::with_capacity(extracted.media.len());
        if cfg.announcements.download_media && !dry_run {
            ensure_dir(&media_dir).await?;
        }
        for m in extracted.media.iter().cloned() {
            let mut m = m;
            if cfg.announcements.download_media && !dry_run {
                if let Some(fid) = m.file_id {
                    info!(course_id = course.id, announcement_id = ann.id, file_id = fid, url = %m.url, "discovered canvas media in announcement body");
                    match canvas.get_file(fid).await {
                        Ok(f) => match download_file_to(httpctx, &f, &media_dir, state).await {
                            Ok(path) => {
                                m.local_path = Some(format!(
                                    "announcements/media/{}",
                                    path.file_name().and_then(|s| s.to_str()).unwrap_or("")
                                ));
                            }
                            Err(e) => {
                                warn!(course_id = course.id, file_id = fid, error = %e, "media download failed");
                            }
                        },
                        Err(e) => {
                            warn!(course_id = course.id, file_id = fid, error = %e, "media metadata fetch failed");
                        }
                    }
                }
            }
            media_with_paths.push(m);
        }

        // Inline attachments returned by the API
        for att in &ann.attachments {
            if cfg.announcements.download_media && !dry_run {
                if let Err(e) = ensure_dir(&media_dir).await {
                    warn!(error = %e, "ensure media_dir");
                }
                match download_file_to(httpctx, att, &media_dir, state).await {
                    Ok(path) => {
                        media_with_paths.push(MediaRef {
                            url: att
                                .url
                                .clone()
                                .or(att.download_url.clone())
                                .unwrap_or_default(),
                            kind: classify_by_filename(
                                att.display_name
                                    .as_deref()
                                    .or(att.filename.as_deref())
                                    .unwrap_or(""),
                            ),
                            file_id: Some(att.id),
                            local_path: Some(format!(
                                "announcements/media/{}",
                                path.file_name().and_then(|s| s.to_str()).unwrap_or("")
                            )),
                        });
                        summary.media += 1;
                    }
                    Err(e) => {
                        warn!(course_id = course.id, file_id = att.id, error = %e, "attachment download failed");
                    }
                }
            } else {
                media_with_paths.push(MediaRef {
                    url: att
                        .url
                        .clone()
                        .or(att.download_url.clone())
                        .unwrap_or_default(),
                    kind: classify_by_filename(
                        att.display_name
                            .as_deref()
                            .or(att.filename.as_deref())
                            .unwrap_or(""),
                    ),
                    file_id: Some(att.id),
                    local_path: None,
                });
                summary.media += 1;
            }
        }

        records.push(AnnouncementRecord {
            id: ann.id,
            title,
            posted_at: ann.posted_at.clone(),
            html_url: ann.html_url.clone(),
            author: ann.author.as_ref().and_then(|a| a.display_name.clone()),
            body_md_path: if dry_run {
                None
            } else {
                Some(body_md_path_rel)
            },
            links: extracted.all,
            media: media_with_paths,
            zoom_links: extracted.zoom,
        });
    }
    pb.finish_and_clear();

    if !dry_run {
        let index_path = ann_dir.join("index.json");
        let json = serde_json::to_vec_pretty(&records)?;
        atomic_write(&index_path, &json).await?;
        info!(course_id = course.id, count = records.len(), path = %index_path.display(), "wrote announcements index");
    }

    Ok(summary)
}

async fn download_file_to(
    httpctx: &HttpCtx,
    f: &FileObj,
    out_dir: &Path,
    state: &mut State,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let raw_fname = f
        .display_name
        .clone()
        .or(f.filename.clone())
        .unwrap_or_else(|| format!("file_{}", f.id));
    let url = f
        .download_url
        .as_ref()
        .or(f.url.as_ref())
        .ok_or("missing file url")?;

    let key = format!("announcement_media:{}", f.id);
    let head = httpctx.send(httpctx.client.head(url)).await?;
    let etag = head
        .headers()
        .get(header::ETAG)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.trim_matches('"').to_string());

    let mut fname = raw_fname.clone();
    if std::path::Path::new(&fname).extension().is_none() {
        if let Some(ext) = head
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .and_then(content_type_to_ext)
        {
            fname = format!("{}.{}", fname, ext);
            info!(
                file_id = f.id,
                derived_ext = ext,
                "filename had no extension; derived from content-type"
            );
        }
    }
    let dest = out_dir.join(sanitize_filename_preserve_ext(&fname));
    info!(file_id = f.id, name = %raw_fname, path = %dest.display(), "announcement media -> downloading");

    if let (Some(prev), Some(et)) = (state.get(&key), etag.as_ref()) {
        if prev.etag.as_deref() == Some(et.as_str()) && dest.exists() {
            return Ok(dest);
        }
    }

    let part = dest.with_extension("part");
    let resp = httpctx.send(httpctx.client.get(url)).await?;
    if !resp.status().is_success() {
        return Err(format!("GET failed: {}", resp.status()).into());
    }
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&part)
        .await?;
    let mut stream = resp.bytes_stream();
    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let bytes = chunk?;
        file.write_all(&bytes).await?;
    }
    file.flush().await?;
    atomic_rename(&part, &dest).await?;

    let size = tokio::fs::metadata(&dest).await.ok().map(|m| m.len());
    state.set(
        key,
        ItemState {
            etag,
            updated_at: f.updated_at.clone(),
            size,
            content_hash: None,
            last_error: None,
            error_count: None,
        },
    );
    Ok(dest)
}

pub fn extract_links(html: &str) -> ExtractedLinks {
    static URL_RE: &str = r#"https?://[A-Za-z0-9\-._~:/?#\[\]@!$&'()*+,;=%]+"#;
    static IMG_SRC_RE: &str = r#"(?i)<img[^>]+src=["']([^"']+)["']"#;
    static A_HREF_RE: &str = r#"(?i)<a[^>]+href=["']([^"']+)["']"#;
    static SRC_RE: &str = r#"(?i)<(?:video|audio|source|iframe)[^>]+src=["']([^"']+)["']"#;
    static CANVAS_FILE_RE: &str = r#"(?i)/(?:api/v1/)?files/(\d+)"#;
    static COURSE_FILE_RE: &str = r#"(?i)/courses/\d+/files/(\d+)"#;

    let url_re = Regex::new(URL_RE).expect("url regex");
    let img_re = Regex::new(IMG_SRC_RE).expect("img regex");
    let a_re = Regex::new(A_HREF_RE).expect("a regex");
    let media_src_re = Regex::new(SRC_RE).expect("media-src regex");
    let canvas_file_re = Regex::new(CANVAS_FILE_RE).expect("canvas file regex");
    let course_file_re = Regex::new(COURSE_FILE_RE).expect("course file regex");

    let mut all: Vec<String> = Vec::new();
    let mut seen_all = HashSet::new();
    let mut media: Vec<MediaRef> = Vec::new();
    let mut seen_media = HashSet::new();

    let push_all = |s: String, all: &mut Vec<String>, seen: &mut HashSet<String>| {
        let trimmed = s
            .trim_end_matches(&[',', ';', ')', ']', '}', '.', '!', '?'][..])
            .to_string();
        if !trimmed.is_empty() && seen.insert(trimmed.clone()) {
            all.push(trimmed);
        }
    };

    for m in url_re.find_iter(html) {
        push_all(m.as_str().to_string(), &mut all, &mut seen_all);
    }
    for cap in img_re.captures_iter(html) {
        if let Some(src) = cap.get(1) {
            let url = src.as_str().to_string();
            push_all(url.clone(), &mut all, &mut seen_all);
            let file_id = course_file_re
                .captures(&url)
                .or_else(|| canvas_file_re.captures(&url))
                .and_then(|c| c.get(1))
                .and_then(|m| m.as_str().parse::<u64>().ok());
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
    for cap in media_src_re.captures_iter(html) {
        if let Some(src) = cap.get(1) {
            let url = src.as_str().to_string();
            push_all(url.clone(), &mut all, &mut seen_all);
            let kind = classify_by_filename(&url);
            if seen_media.insert(url.clone()) {
                let file_id = course_file_re
                    .captures(&url)
                    .or_else(|| canvas_file_re.captures(&url))
                    .and_then(|c| c.get(1))
                    .and_then(|m| m.as_str().parse::<u64>().ok());
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
    for cap in a_re.captures_iter(html) {
        if let Some(href) = cap.get(1) {
            let url = href.as_str().to_string();
            push_all(url.clone(), &mut all, &mut seen_all);
            let file_id = course_file_re
                .captures(&url)
                .or_else(|| canvas_file_re.captures(&url))
                .and_then(|c| c.get(1))
                .and_then(|m| m.as_str().parse::<u64>().ok());
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

pub fn extract_zoom_urls(input: &str) -> Vec<String> {
    static PATTERN: &str = r#"https?://[A-Za-z0-9-]+\.zoom\.(us|com\.cn)/[A-Za-z0-9_/\-?&=%#\.]+"#;
    let regex = Regex::new(PATTERN).expect("valid regex");
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for cap in regex.captures_iter(input) {
        if let Some(m) = cap.get(0) {
            let url = m
                .as_str()
                .trim_end_matches(&[',', ';', ')', ']', '}'][..])
                .to_string();
            if seen.insert(url.clone()) {
                out.push(url);
            }
        }
    }
    out
}

fn content_type_to_ext(ct: &str) -> Option<&'static str> {
    let mime = ct.split(';').next()?.trim().to_ascii_lowercase();
    Some(match mime.as_str() {
        "application/x-ipynb+json" | "application/x-jupyter-notebook" => "ipynb",
        "application/json" => "json",
        "application/pdf" => "pdf",
        "application/zip" => "zip",
        "application/x-tar" => "tar",
        "application/gzip" | "application/x-gzip" => "gz",
        "text/csv" => "csv",
        "text/plain" => "txt",
        "text/html" => "html",
        "text/markdown" => "md",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "audio/mpeg" => "mp3",
        "audio/wav" => "wav",
        "application/msword" => "doc",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/vnd.ms-excel" => "xls",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.ms-powerpoint" => "ppt",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
        _ => return None,
    })
}

fn classify_by_filename(s: &str) -> MediaKind {
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
