//! Links Canvas puts in HTML, and the markdown links the migrator rewrites.
//!
//! Canvas stores announcement and page bodies as HTML fragments. [`extract_links`]
//! turns one fragment into the URLs it references, classifying each as an
//! ordinary link, inline media, or a Canvas-hosted file (which carries a numeric
//! id the API can resolve).
//!
//! [`rewrite_canvas_links`] is the pure half of pointing notes at the local
//! copies: a markdown link [`canvas_ref`] reads as a Canvas file, page,
//! assignment, announcement, module item or course becomes a path relative to
//! the note, when the caller's [`LinkMap`] has it. Anything not downloaded,
//! anything ambiguous, and anything inside code, stays as it was.

use crate::layout;
use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path, PathBuf};
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
/// `/files/<id>` and `/api/v1/files/<id>`. Literal pattern, so `Regex::new` only
/// fails if this string is edited into something the parser rejects.
static CANVAS_FILE_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:/api/v1)?/files/(\d+)").unwrap());
static ZOOM_URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"https?://[A-Za-z0-9-]+\.zoom\.(us|com\.cn)/[A-Za-z0-9_/\-?&=%#\.]+"#).unwrap()
});

/// Trailing characters stripped from a bare URL found in prose.
const URL_TRAILERS: &[char] = &[',', ';', ')', ']', '}', '.', '!', '?'];

/// Every Canvas file id in `text`: `/files/<id>` or `/api/v1/files/<id>`.
///
/// This is the pattern `syncer` uses to decide which files to download. A course
/// id in `/courses/<id>/files/<file>` is not captured; the file id is.
pub fn canvas_file_ids(text: &str) -> impl Iterator<Item = u64> + '_ {
    CANVAS_FILE_ID_RE
        .captures_iter(text)
        .filter_map(|cap| cap.get(1)?.as_str().parse::<u64>().ok())
}

/// The first Canvas file id in `text`, if any.
fn canvas_file_id(url: &str) -> Option<u64> {
    canvas_file_ids(url).next()
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

/// A Canvas resource a link points at, read from its URL by [`canvas_ref`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CanvasRef {
    /// `/files/<id>`, `/courses/<n>/files/<id>[/preview|/download]`,
    /// `/api/v1[/courses/<n>]/files/<id>`.
    File(u64),
    /// `/courses/<course>/pages/<slug>`.
    Page {
        /// Course id in the URL.
        course: u64,
        /// The page's `url` slug, lowercased.
        slug: String,
    },
    /// `/courses/<n>/assignments/<id>`.
    Assignment(u64),
    /// `/courses/<n>/discussion_topics/<id>` (announcements are discussion topics).
    Announcement(u64),
    /// `/courses/<n>/modules/items/<id>`.
    ModuleItem(u64),
    /// `/courses/<id>` itself.
    Course(u64),
    /// `https://<canvas>/mailto:x@y`, a `mailto:` Canvas made relative to its
    /// own host. The value is the `mailto:` URL it should have been.
    Mailto(String),
}

/// Where a Canvas resource is in the library: what [`rewrite_canvas_links`]
/// points links at. Every path is relative to the library root.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkMap {
    /// Canvas course id → course directory. A link to the course itself goes
    /// to its `README.md`.
    pub courses: BTreeMap<u64, PathBuf>,
    /// Canvas file id → its local copy, or the copies it might be.
    pub files: BTreeMap<u64, FileTarget>,
    /// (course id, page slug) → page note.
    pub pages: BTreeMap<(u64, String), PathBuf>,
    /// Assignment id → note under `tareas/`.
    pub assignments: BTreeMap<u64, PathBuf>,
    /// Announcement (discussion topic) id → note under `anuncios/`.
    pub announcements: BTreeMap<u64, PathBuf>,
    /// Module item id → the page, assignment or file it shows.
    pub module_items: BTreeMap<u64, PathBuf>,
}

/// Where a Canvas file is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileTarget {
    /// This file.
    Path(PathBuf),
    /// One of these files, all of the right size: the link's own text has to
    /// name exactly one of them ([`LinkMap::resolve`]), or the link stays remote.
    Ambiguous(Vec<PathBuf>),
}

/// How [`LinkMap::resolve`] answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// The local copy, relative to the library root.
    Local(PathBuf),
    /// The link should become this URL (`mailto:`).
    Url(String),
    /// More than one local file could be it, and the link text names none.
    Ambiguous,
    /// No local copy.
    Remote,
}

impl LinkMap {
    /// Where `target` is, if anywhere. `label` is the link's text, the only
    /// tie-break between same-size copies ([`FileTarget::Ambiguous`]): it
    /// must name exactly one of them, as its original file name
    /// ([`layout::attachment_file`]) or as its slug.
    pub fn resolve(&self, target: &CanvasRef, label: &str) -> Resolution {
        let found = match target {
            CanvasRef::File(id) => match self.files.get(id) {
                Some(FileTarget::Path(path)) => Some(path.clone()),
                Some(FileTarget::Ambiguous(paths)) => {
                    return match pick_by_label(paths, label) {
                        Some(path) => Resolution::Local(path.clone()),
                        None => Resolution::Ambiguous,
                    }
                }
                None => None,
            },
            CanvasRef::Page { course, slug } => self.pages.get(&(*course, slug.clone())).cloned(),
            CanvasRef::Assignment(id) => self.assignments.get(id).cloned(),
            CanvasRef::Announcement(id) => self.announcements.get(id).cloned(),
            CanvasRef::ModuleItem(id) => self.module_items.get(id).cloned(),
            CanvasRef::Course(id) => self.courses.get(id).map(|dir| dir.join(COURSE_README)),
            CanvasRef::Mailto(url) => return Resolution::Url(url.clone()),
        };
        match found {
            Some(path) => Resolution::Local(path),
            None => Resolution::Remote,
        }
    }
}

/// The course index every v2 course directory has.
const COURSE_README: &str = "README.md";

/// The one candidate `label` names, by original file name or by slug.
fn pick_by_label<'a>(candidates: &'a [PathBuf], label: &str) -> Option<&'a PathBuf> {
    let label = unescape_markdown(label);
    let label = label.trim();
    if label.is_empty() {
        return None;
    }
    let as_file = layout::attachment_file(label);
    let as_slug = layout::slugify(label);
    let mut hits = candidates.iter().filter(|path| {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
        name == as_file || (!as_slug.is_empty() && stem == as_slug)
    });
    let first = hits.next()?;
    hits.next().is_none().then_some(first)
}

/// `label` without markdown backslash escapes (html2md writes `Guia\_1.PDF`).
fn unescape_markdown(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut chars = label.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// The Canvas resource `url` points at, or `None` for anything without a
/// local copy (quizzes, `media_attachments_iframe`, `external_tools`,
/// `equation_images`, other sites…).
///
/// Absolute `http(s)` URLs on any host and root-relative paths are read the
/// same way; the query and fragment are ignored. A `/files/<id>` anywhere in
/// the path is a file, as [`canvas_file_ids`] reads it.
pub fn canvas_ref(url: &str) -> Option<CanvasRef> {
    let url = destination_body(url);
    let path_and_rest = if starts_with_http(url) {
        let after_scheme = &url[url.find("//")? + 2..];
        &after_scheme[after_scheme.find('/')?..]
    } else if url.starts_with('/') && !url.starts_with("//") {
        url
    } else {
        return None;
    };
    if let Some(mail) = path_and_rest.strip_prefix("/mailto:") {
        return (!mail.is_empty()).then(|| CanvasRef::Mailto(format!("mailto:{mail}")));
    }
    let path = path_and_rest
        .split(['?', '#'])
        .next()
        .unwrap_or(path_and_rest);
    let segments: Vec<&str> = path
        .trim_start_matches('/')
        .trim_end_matches('/')
        .split('/')
        .collect();
    let id = |s: &str| -> Option<u64> {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse().ok())
            .flatten()
    };
    if let ["courses", course, rest @ ..] = segments.as_slice() {
        if let Some(course) = id(course) {
            match rest {
                [] => return Some(CanvasRef::Course(course)),
                ["pages", slug] if !slug.is_empty() => {
                    return Some(CanvasRef::Page {
                        course,
                        slug: slug.to_ascii_lowercase(),
                    })
                }
                ["assignments", item] => return id(item).map(CanvasRef::Assignment),
                ["discussion_topics", item] => return id(item).map(CanvasRef::Announcement),
                ["modules", "items", item] => return id(item).map(CanvasRef::ModuleItem),
                _ => {}
            }
        }
    }
    canvas_file_ids(path).next().map(CanvasRef::File)
}

/// What one [`rewrite_canvas_links`] call did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkStats {
    /// Links now pointing at a local copy.
    pub local: usize,
    /// `https://<canvas>/mailto:` links turned into `mailto:`.
    pub mailto: usize,
    /// Canvas file ids left remote because more than one local file could be them.
    pub ambiguous: Vec<u64>,
}

/// Rewrite Canvas links in markdown to paths relative to `md_path`.
///
/// `md_path` and every path in `map` are relative to the same root (the
/// library root). A link is rewritten when [`canvas_ref`] reads a resource
/// from it that `map` has a local copy of ([`LinkMap::resolve`]); the
/// replacement is the path from the note's directory to that copy, with `/`
/// separators, and Canvas's query and fragment are dropped. A misplaced
/// `https://<canvas>/mailto:` becomes the `mailto:` it was. Everything else
/// stays exactly as it was.
///
/// Fenced code and inline code are not rewritten. Inline links, images,
/// autolinks, reference definitions and bare `http(s)` URLs are. A destination
/// is read on the same line as its opening `[`.
pub fn rewrite_canvas_links(markdown: &str, md_path: &Path, map: &LinkMap) -> (String, LinkStats) {
    let mut cx = Cx {
        md_path,
        map,
        stats: LinkStats::default(),
    };
    let mut out = String::with_capacity(markdown.len());
    let mut rest = markdown;
    let mut code: Option<usize> = None;
    while !rest.is_empty() {
        if code.is_none() {
            if let Some((fence, len)) = open_fence(rest) {
                let (line_len, nl_len) = line_bounds(rest);
                let opener = line_len + nl_len;
                out.push_str(&rest[..opener]);
                rest = &rest[opener..];
                loop {
                    if rest.is_empty() {
                        break;
                    }
                    let (line_len, nl_len) = line_bounds(rest);
                    let line = &rest[..line_len];
                    let take = line_len + nl_len;
                    out.push_str(&rest[..take]);
                    rest = &rest[take..];
                    if close_fence(line, fence, len) {
                        break;
                    }
                }
                continue;
            }
        }
        let (line_len, nl_len) = line_bounds(rest);
        out.push_str(&rewrite_line(&rest[..line_len], &mut code, &mut cx));
        out.push_str(&rest[line_len..line_len + nl_len]);
        rest = &rest[line_len + nl_len..];
    }
    cx.stats.ambiguous.sort_unstable();
    cx.stats.ambiguous.dedup();
    (out, cx.stats)
}

/// What the rewrite of one note carries along.
struct Cx<'a> {
    md_path: &'a Path,
    map: &'a LinkMap,
    stats: LinkStats,
}

fn line_bounds(s: &str) -> (usize, usize) {
    match s.find('\n') {
        Some(n) if n > 0 && s.as_bytes()[n - 1] == b'\r' => (n - 1, 2),
        Some(n) => (n, 1),
        None => (s.len(), 0),
    }
}

fn open_fence(s: &str) -> Option<(char, usize)> {
    let (line_len, _) = line_bounds(s);
    let line = &s[..line_len];
    let indent = line.bytes().take_while(|b| *b == b' ').count();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let ch = rest.chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    let n = rest.bytes().take_while(|b| *b == ch as u8).count();
    if n < 3 {
        return None;
    }
    let after = &rest[n..];
    if ch == '`' && after.contains('`') {
        return None;
    }
    Some((ch, n))
}

fn close_fence(line: &str, ch: char, n: usize) -> bool {
    let indent = line.bytes().take_while(|b| *b == b' ').count();
    if indent > 3 {
        return false;
    }
    let rest = line[indent..].trim_end();
    rest.len() >= n && rest.bytes().all(|b| b == ch as u8)
}

fn rewrite_line(line: &str, code: &mut Option<usize>, cx: &mut Cx<'_>) -> String {
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    if let Some(n) = *code {
        match find_backtick_run(line, n) {
            Some(end) => {
                out.push_str(&line[..end]);
                i = end;
                *code = None;
            }
            None => return line.to_string(),
        }
    }
    while i < line.len() {
        let byte = line.as_bytes()[i];
        if byte == b'`' {
            let n = backtick_run(&line[i..]);
            match find_backtick_run(&line[i + n..], n) {
                Some(rel) => {
                    let end = i + n + rel;
                    out.push_str(&line[i..end]);
                    i = end;
                }
                None => {
                    *code = Some(n);
                    out.push_str(&line[i..]);
                    break;
                }
            }
            continue;
        }
        let interesting = matches!(byte, b'[' | b'!' | b'<' | b'h' | b'H');
        let bare_ok = (byte != b'h' && byte != b'H') || url_boundary(line, i);
        if interesting && bare_ok {
            if let Some(found) = match_link(&line[i..], ref_def_slot(line, i), cx) {
                match found.replacement {
                    Some(text) => out.push_str(&text),
                    None => out.push_str(&line[i..i + found.len]),
                }
                i += found.len;
                continue;
            }
        }
        let Some(ch) = line[i..].chars().next() else {
            break;
        };
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

struct Found {
    len: usize,
    replacement: Option<String>,
}

fn match_link(s: &str, allow_ref: bool, cx: &mut Cx<'_>) -> Option<Found> {
    if s.starts_with('<') {
        if let Some(found) = match_autolink(s, cx) {
            return Some(found);
        }
    }
    if starts_with_http(s) {
        if let Some(found) = match_bare_url(s, cx) {
            return Some(found);
        }
    }
    let bang = s.starts_with('!');
    let bracket = if bang { 1 } else { 0 };
    if !s[bracket..].starts_with('[') {
        return None;
    }
    let label_end = parse_label(s, bracket)?;
    if s[label_end..].starts_with('(') {
        return finish_inline(s, bang, bracket, label_end, cx);
    }
    if allow_ref && !bang && s[label_end..].starts_with(':') {
        return finish_ref(s, label_end, cx);
    }
    None
}

fn finish_inline(
    s: &str,
    bang: bool,
    bracket: usize,
    label_end: usize,
    cx: &mut Cx<'_>,
) -> Option<Found> {
    let dest_at = label_end + 1;
    let (dest_len, dest_raw) = parse_destination(&s[dest_at..])?;
    let after_dest = dest_at + dest_len;
    let (title, close_len) = parse_title_and_close(&s[after_dest..])?;
    let end = after_dest + close_len;
    let label = &s[bracket + 1..label_end - 1];
    let replacement =
        local_link(dest_raw, label, cx).map(|link| render_inline(bang, label, &link.dest, title));
    Some(Found {
        len: end,
        replacement,
    })
}

fn finish_ref(s: &str, label_end: usize, cx: &mut Cx<'_>) -> Option<Found> {
    let after_colon = &s[label_end + 1..];
    let lead = leading_ws(after_colon);
    if lead == 0 {
        return None;
    }
    let (dest_len, dest_raw) = parse_destination(&after_colon[lead..])?;
    let after_dest = label_end + 1 + lead + dest_len;
    let rest = s[after_dest..].trim_start();
    let title = if rest.is_empty() {
        None
    } else {
        let title_len = title_len(rest)?;
        if !rest[title_len..].trim().is_empty() {
            return None;
        }
        Some(&rest[..title_len])
    };
    let label = &s[1..label_end - 1];
    let replacement = local_link(dest_raw, label, cx).map(|link| match title {
        Some(title) => format!("[{label}]: {} {title}", link.dest),
        None => format!("[{label}]: {}", link.dest),
    });
    Some(Found {
        len: s.len(),
        replacement,
    })
}

fn match_autolink(s: &str, cx: &mut Cx<'_>) -> Option<Found> {
    let rest = s.strip_prefix('<')?;
    let close = rest.find('>')?;
    let inner = &rest[..close];
    if inner.chars().any(char::is_whitespace) || !starts_with_http(inner) {
        return None;
    }
    let replacement =
        local_link(inner, "", cx).map(|link| format!("[{}]({})", link.label, link.dest));
    Some(Found {
        len: close + 2,
        replacement,
    })
}

fn match_bare_url(s: &str, cx: &mut Cx<'_>) -> Option<Found> {
    if !starts_with_http(s) {
        return None;
    }
    let len = bare_url_len(s);
    if len == 0 {
        return None;
    }
    let url = &s[..len];
    let replacement =
        local_link(url, "", cx).map(|link| format!("[{}]({})", link.label, link.dest));
    Some(Found { len, replacement })
}

fn render_inline(bang: bool, label: &str, dest: &str, title: Option<&str>) -> String {
    let bang = if bang { "!" } else { "" };
    match title {
        Some(title) => format!("{bang}[{label}]({dest} {title})"),
        None => format!("{bang}[{label}]({dest})"),
    }
}

struct LocalLink {
    dest: String,
    label: String,
}

/// The new destination of the link `url` with text `label`, counted in
/// `cx.stats`, or `None` to leave it alone.
fn local_link(url: &str, label: &str, cx: &mut Cx<'_>) -> Option<LocalLink> {
    let target = canvas_ref(url)?;
    let link = match cx.map.resolve(&target, label) {
        Resolution::Local(path) => {
            let rel = relative_link(cx.md_path, &path)?;
            if rel.chars().any(char::is_control) {
                return None;
            }
            let label = match target {
                CanvasRef::Course(_) => path.parent().map_or_else(String::new, file_label),
                _ => file_label(&path),
            };
            LocalLink {
                dest: markdown_destination(&rel),
                label,
            }
        }
        Resolution::Url(url) => {
            if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
                return None;
            }
            LocalLink {
                label: escape_label(url.trim_start_matches("mailto:")),
                dest: url,
            }
        }
        Resolution::Ambiguous => {
            if let CanvasRef::File(id) = target {
                cx.stats.ambiguous.push(id);
            }
            return None;
        }
        Resolution::Remote => return None,
    };
    if link.dest == url.trim() {
        return None;
    }
    match target {
        CanvasRef::Mailto(_) => cx.stats.mailto += 1,
        _ => cx.stats.local += 1,
    }
    Some(link)
}

fn destination_body(raw: &str) -> &str {
    let trimmed = raw.trim();
    trimmed
        .strip_prefix('<')
        .and_then(|inner| inner.strip_suffix('>'))
        .unwrap_or(trimmed)
}

fn relative_link(from_md: &Path, to_file: &Path) -> Option<String> {
    let from_dir = from_md.parent().unwrap_or(Path::new(""));
    let from = path_parts(from_dir)?;
    let to = path_parts(to_file)?;
    if to.is_empty() {
        return None;
    }
    let mut shared = 0;
    while shared < from.len() && shared < to.len() && from[shared] == to[shared] {
        shared += 1;
    }
    let mut parts = Vec::new();
    parts.extend(std::iter::repeat_n("..", from.len() - shared));
    for part in &to[shared..] {
        parts.push(part);
    }
    if parts.is_empty() {
        to.last().cloned()
    } else {
        Some(parts.join("/"))
    }
}

fn path_parts(path: &Path) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part.to_str()?.to_string()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(out)
}

/// `rel` as a link destination: percent-encoded (UTF-8) outside
/// `[A-Za-z0-9._~/-]`, which every markdown viewer, Obsidian included, decodes.
fn markdown_destination(rel: &str) -> String {
    let mut out = String::with_capacity(rel.len());
    for byte in rel.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'~' | b'/' | b'-') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn file_label(path: &Path) -> String {
    match path.file_name().and_then(|name| name.to_str()) {
        Some(name) if !name.is_empty() => escape_label(name),
        _ => "archivo".to_string(),
    }
}

fn escape_label(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c == '[' || c == ']' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn parse_label(s: &str, open: usize) -> Option<usize> {
    let mut depth = 0;
    let mut chars = s[open..].char_indices();
    while let Some((off, c)) = chars.next() {
        if c == '\\' {
            chars.next()?;
            continue;
        }
        if c == '[' {
            depth += 1;
            continue;
        }
        if c == ']' {
            depth -= 1;
            if depth == 0 {
                return Some(open + off + c.len_utf8());
            }
        }
    }
    None
}

fn parse_destination(s: &str) -> Option<(usize, &str)> {
    let lead = leading_ws(s);
    let body = &s[lead..];
    if body.is_empty() {
        return None;
    }
    if let Some(stripped) = body.strip_prefix('<') {
        let mut escaped = false;
        for (i, c) in stripped.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            if c == '\\' {
                escaped = true;
                continue;
            }
            if c == '>' {
                // `i` is relative to `stripped`, which starts one byte after `<`.
                let end = 1 + i + c.len_utf8();
                return Some((lead + end, &body[..end]));
            }
        }
        return None;
    }
    let mut depth = 0;
    let mut escaped = false;
    for (i, c) in body.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if c.is_whitespace() && depth == 0 {
            if i == 0 {
                return None;
            }
            return Some((lead + i, &body[..i]));
        }
        if c == '(' {
            depth += 1;
            continue;
        }
        if c == ')' {
            if depth == 0 {
                if i == 0 {
                    return None;
                }
                return Some((lead + i, &body[..i]));
            }
            depth -= 1;
        }
    }
    if depth == 0 {
        Some((lead + body.len(), body))
    } else {
        None
    }
}

fn parse_title_and_close(s: &str) -> Option<(Option<&str>, usize)> {
    let lead = leading_ws(s);
    let trimmed = &s[lead..];
    if trimmed.starts_with(')') {
        return Some((None, lead + 1));
    }
    let title_len = title_len(trimmed)?;
    let after = &trimmed[title_len..];
    let mid = leading_ws(after);
    if after[mid..].starts_with(')') {
        Some((Some(&trimmed[..title_len]), lead + title_len + mid + 1))
    } else {
        None
    }
}

fn title_len(s: &str) -> Option<usize> {
    match s.chars().next()? {
        '"' => find_quoted(s, '"'),
        '\'' => find_quoted(s, '\''),
        '(' => find_paren_title(s),
        _ => None,
    }
}

fn find_quoted(s: &str, quote: char) -> Option<usize> {
    let mut escaped = false;
    for (i, c) in s.char_indices().skip(1) {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if c == quote {
            return Some(i + c.len_utf8());
        }
    }
    None
}

fn find_paren_title(s: &str) -> Option<usize> {
    let mut depth = 0;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if c == '(' {
            depth += 1;
            continue;
        }
        if c == ')' {
            depth -= 1;
            if depth == 0 {
                return Some(i + 1);
            }
        }
    }
    None
}

fn leading_ws(s: &str) -> usize {
    s.chars()
        .take_while(|c| c.is_whitespace())
        .map(char::len_utf8)
        .sum()
}

fn find_backtick_run(s: &str, n: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let start = i;
            while i < bytes.len() && bytes[i] == b'`' {
                i += 1;
            }
            if i - start == n {
                return Some(i);
            }
        } else {
            i += 1;
        }
    }
    None
}

fn backtick_run(s: &str) -> usize {
    s.bytes().take_while(|b| *b == b'`').count()
}

fn ref_def_slot(line: &str, i: usize) -> bool {
    i <= 3 && line.as_bytes()[..i].iter().all(|b| *b == b' ')
}

fn url_boundary(line: &str, i: usize) -> bool {
    if i == 0 {
        return true;
    }
    match line[..i].chars().next_back() {
        Some(c) => !c.is_ascii_alphanumeric() && c != '_',
        None => true,
    }
}

fn starts_with_http(s: &str) -> bool {
    starts_with_ignore_ascii(s, "https://") || starts_with_ignore_ascii(s, "http://")
}

fn starts_with_ignore_ascii(s: &str, prefix: &str) -> bool {
    let mut chars = s.chars();
    for expected in prefix.chars() {
        match chars.next() {
            Some(c) if c.eq_ignore_ascii_case(&expected) => {}
            _ => return false,
        }
    }
    true
}

fn bare_url_len(s: &str) -> usize {
    let mut len = 0;
    for (i, c) in s.char_indices() {
        if is_url_char(c) {
            len = i + c.len_utf8();
        } else {
            break;
        }
    }
    trim_url_tail(&s[..len]).len()
}

fn is_url_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            '-' | '.'
                | '_'
                | '~'
                | ':'
                | '/'
                | '?'
                | '#'
                | '['
                | ']'
                | '@'
                | '!'
                | '$'
                | '&'
                | '\''
                | '('
                | ')'
                | '*'
                | '+'
                | ','
                | ';'
                | '='
                | '%'
        )
}

fn trim_url_tail(s: &str) -> &str {
    let mut s = s.trim_end_matches([',', ';', '.', '!', '?', ':']);
    loop {
        let open = s.chars().filter(|c| *c == '(').count();
        let close = s.chars().filter(|c| *c == ')').count();
        if close <= open {
            break;
        }
        let Some(trimmed) = s.strip_suffix(')') else {
            break;
        };
        s = trimmed;
    }
    s
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

    #[test]
    fn canvas_file_ids_capture_the_file_not_the_course() {
        let text =
            "https://canvas.example.com/courses/42/files/9001/download /api/v1/files/33 /FILES/7";
        let ids: Vec<u64> = canvas_file_ids(text).collect();
        assert_eq!(ids, vec![9001, 33, 7]);
    }

    /// [`rewrite_canvas_links`] over a map with only `files`.
    fn rw(input: &str, md: &Path, files: &BTreeMap<u64, PathBuf>) -> String {
        let map = LinkMap {
            files: files
                .iter()
                .map(|(id, path)| (*id, FileTarget::Path(path.clone())))
                .collect(),
            ..LinkMap::default()
        };
        rewrite_canvas_links(input, md, &map).0
    }

    fn guia() -> BTreeMap<u64, PathBuf> {
        let mut files = BTreeMap::new();
        files.insert(
            22,
            PathBuf::from("course/modulos/01-intro/adjuntos/guia.pdf"),
        );
        files
    }

    fn page() -> &'static Path {
        Path::new("course/modulos/01-intro/01-intro.md")
    }

    #[test]
    fn rewrites_markdown_links_and_leaves_missing_ids() {
        let input = "\
[guía](https://canvas.unab.cl/courses/212693/files/22?verifier=SECRET&wrap=1)
[otra](https://canvas.example.com/courses/1/files/99/download)
![fig](https://canvas.example.com/api/v1/files/22/preview)
[pagina](https://canvas.example.com/courses/22/pages/intro)
";
        let out = rw(input, page(), &guia());
        assert!(out.contains("[guía](adjuntos/guia.pdf)\n"), "{out}");
        assert!(!out.contains("SECRET"), "{out}");
        assert!(out.contains("[otra](https://canvas.example.com/courses/1/files/99/download)\n"));
        assert!(out.contains("![fig](adjuntos/guia.pdf)\n"));
        assert!(out.contains("[pagina](https://canvas.example.com/courses/22/pages/intro)\n"));
        assert_eq!(rw(&out, page(), &guia()), out);
    }

    #[test]
    fn rewrites_titles_autolinks_bare_urls_and_reference_definitions() {
        let input = "\
[slides](https://canvas.example.com/files/22 \"Diapos\")
<https://canvas.example.com/files/22/download>
Bajar https://canvas.example.com/FILES/22/download.
[guia]: https://canvas.example.com/courses/1/files/22/preview
";
        let out = rw(input, page(), &guia());
        assert!(
            out.contains("[slides](adjuntos/guia.pdf \"Diapos\")\n"),
            "{out}"
        );
        assert!(out.contains("[guia.pdf](adjuntos/guia.pdf)\n"), "{out}");
        assert!(
            out.contains("Bajar [guia.pdf](adjuntos/guia.pdf).\n"),
            "{out}"
        );
        assert!(out.contains("[guia]: adjuntos/guia.pdf\n"), "{out}");
        assert_eq!(rw(&out, page(), &guia()), out);
    }

    #[test]
    fn relative_link_climbs_out_of_the_note_directory() {
        let md = Path::new("course/anuncios/2026-06-17_hola.md");
        let input = "[g](https://canvas.example.com/files/22)\n";
        let out = rw(input, md, &guia());
        assert_eq!(out, "[g](../modulos/01-intro/adjuntos/guia.pdf)\n");
    }

    #[test]
    fn spaces_in_the_local_name_are_percent_encoded() {
        let mut files = BTreeMap::new();
        files.insert(
            22,
            PathBuf::from("course/modulos/01-intro/adjuntos/guia (1).pdf"),
        );
        let out = rw("[g](https://canvas.example.com/files/22)\n", page(), &files);
        assert_eq!(out, "[g](adjuntos/guia%20%281%29.pdf)\n");
        assert_eq!(rw(&out, page(), &files), out);
    }

    #[test]
    fn code_fences_and_inline_code_are_not_rewritten() {
        let input = "\
```
[a](https://canvas.example.com/files/22)
```
Usa `[a](https://canvas.example.com/files/22)` tal cual.
";
        assert_eq!(rw(input, page(), &guia()), input);
    }

    #[test]
    fn multiline_code_span_hides_the_link_until_it_closes() {
        let input = "``\n[a](https://canvas.example.com/files/22)\n``\n[b](https://canvas.example.com/files/22)\n";
        let out = rw(input, page(), &guia());
        assert!(
            out.contains("``\n[a](https://canvas.example.com/files/22)\n``\n"),
            "{out}"
        );
        assert!(out.contains("[b](adjuntos/guia.pdf)\n"), "{out}");
    }

    #[test]
    fn crlf_around_a_rewritten_link_stays() {
        let input = "Hola\r\n[a](https://canvas.example.com/files/22)\r\n";
        let out = rw(input, page(), &guia());
        assert_eq!(out, "Hola\r\n[a](adjuntos/guia.pdf)\r\n");
    }

    #[test]
    fn the_first_file_id_wins_and_an_unmapped_path_is_left_alone() {
        let input = "[g](https://canvas.example.com/files/1/files/22)\n";
        assert_eq!(rw(input, page(), &guia()), input);
        let mut files = guia();
        files.insert(1, PathBuf::from("course/modulos/01-intro/adjuntos/uno.pdf"));
        let out = rw(input, page(), &files);
        assert_eq!(out, "[g](adjuntos/uno.pdf)\n");
    }

    #[test]
    fn canvas_ref_reads_every_canvas_link_shape() {
        let host = "https://canvas.unab.cl";
        let page = |course, slug: &str| CanvasRef::Page {
            course,
            slug: slug.into(),
        };
        let cases: Vec<(String, Option<CanvasRef>)> = vec![
            (
                format!("{host}/courses/1/files/22/preview?verifier=x&wrap=1"),
                Some(CanvasRef::File(22)),
            ),
            (
                format!("{host}/api/v1/courses/1/files/22"),
                Some(CanvasRef::File(22)),
            ),
            (
                format!("{host}/courses/1/files/22?wrap=1"),
                Some(CanvasRef::File(22)),
            ),
            (
                "/courses/1/files/22/download?download_frd=1".into(),
                Some(CanvasRef::File(22)),
            ),
            (
                "/courses/1/files/22/preview".into(),
                Some(CanvasRef::File(22)),
            ),
            (
                format!("{host}/courses/223175/pages/fase-1"),
                Some(page(223175, "fase-1")),
            ),
            (
                "/courses/223175/pages/Fase-1?module_item_id=5#top".into(),
                Some(page(223175, "fase-1")),
            ),
            (
                "/courses/1/modules/items/77".into(),
                Some(CanvasRef::ModuleItem(77)),
            ),
            (
                "/courses/1/assignments/30".into(),
                Some(CanvasRef::Assignment(30)),
            ),
            (
                "/courses/1/discussion_topics/501".into(),
                Some(CanvasRef::Announcement(501)),
            ),
            (
                format!("{host}/courses/9002"),
                Some(CanvasRef::Course(9002)),
            ),
            (
                format!("{host}/courses/9002/"),
                Some(CanvasRef::Course(9002)),
            ),
            (
                format!("{host}/mailto:docente@example.edu"),
                Some(CanvasRef::Mailto("mailto:docente@example.edu".into())),
            ),
            (
                "<https://canvas.unab.cl/courses/1/pages/a>".into(),
                Some(page(1, "a")),
            ),
            (format!("{host}/courses/1/quizzes/12"), None),
            (format!("{host}/media_attachments_iframe/333"), None),
            (format!("{host}/courses/1/external_tools/187"), None),
            (format!("{host}/equation_images/x%2By"), None),
            (
                format!("{host}/courses/1/assignments/30/submissions/4"),
                None,
            ),
            (format!("{host}/courses/1/pages/a/edit"), None),
            ("mailto:a@b.c".into(), None),
            ("adjuntos/guia.pdf".into(), None),
            ("//cdn.example.com/files/3".into(), None),
        ];
        for (url, want) in cases {
            assert_eq!(canvas_ref(&url), want, "{url}");
        }
    }

    fn full_map() -> LinkMap {
        let mut map = LinkMap::default();
        map.courses.insert(9001, PathBuf::from("mineria"));
        map.courses.insert(9002, PathBuf::from("electivo"));
        map.files.insert(
            1,
            FileTarget::Path(PathBuf::from(
                "mineria/modulos/01-intro/adjuntos/rubrica.docx",
            )),
        );
        map.files.insert(
            2,
            FileTarget::Ambiguous(vec![
                PathBuf::from("mineria/modulos/01-intro/adjuntos/guia-1.pdf"),
                PathBuf::from("mineria/modulos/02-semana/adjuntos/guia-2.pdf"),
            ]),
        );
        map.pages.insert(
            (9001, "fase-1".into()),
            PathBuf::from("mineria/modulos/02-semana/01-fase-1.md"),
        );
        map.pages.insert(
            (9002, "intro".into()),
            PathBuf::from("electivo/modulos/01-avisos/01-intro.md"),
        );
        map.assignments
            .insert(30, PathBuf::from("mineria/tareas/semana-10.md"));
        map.announcements
            .insert(501, PathBuf::from("mineria/anuncios/2026-06-17_hola.md"));
        map.module_items
            .insert(77, PathBuf::from("mineria/modulos/02-semana/01-fase-1.md"));
        map
    }

    #[test]
    fn every_kind_of_link_points_at_its_local_copy() {
        let note = Path::new("mineria/modulos/02-semana/02-fase-2.md");
        let input = "\
[Fase 1](https://canvas.unab.cl/courses/9001/pages/fase-1) [Item](/courses/9001/modules/items/77)
![](https://canvas.unab.cl/courses/9001/files/1/preview?verifier=x)
[Tarea](/courses/9001/assignments/30) [Aviso](https://canvas.unab.cl/courses/9001/discussion_topics/501#x)
[Otro ramo](https://canvas.unab.cl/courses/9002/pages/intro) [Ramo](https://canvas.unab.cl/courses/9002)
[Guia\\_1.PDF](https://canvas.unab.cl/courses/9001/files/2/download) [descarga](/courses/9001/files/2)
[correo](https://canvas.unab.cl/mailto:docente@example.edu) [quiz](/courses/9001/quizzes/3)
";
        let (out, stats) = rewrite_canvas_links(input, note, &full_map());
        let want = "\
[Fase 1](01-fase-1.md) [Item](01-fase-1.md)
![](../01-intro/adjuntos/rubrica.docx)
[Tarea](../../tareas/semana-10.md) [Aviso](../../anuncios/2026-06-17_hola.md)
[Otro ramo](../../../electivo/modulos/01-avisos/01-intro.md) [Ramo](../../../electivo/README.md)
[Guia\\_1.PDF](../01-intro/adjuntos/guia-1.pdf) [descarga](/courses/9001/files/2)
[correo](mailto:docente@example.edu) [quiz](/courses/9001/quizzes/3)
";
        assert_eq!(out, want);
        assert_eq!(stats.local, 8);
        assert_eq!(stats.mailto, 1);
        assert_eq!(stats.ambiguous, vec![2]);
        let (again, stats) = rewrite_canvas_links(&out, note, &full_map());
        assert_eq!(again, out);
        assert_eq!(stats.local, 0);
    }

    #[test]
    fn bare_and_autolinked_canvas_urls_get_a_label() {
        let note = Path::new("mineria/anuncios/2026-06-17_hola.md");
        let input = "Ver https://canvas.unab.cl/courses/9001/pages/fase-1. O <https://canvas.unab.cl/courses/9002>\n";
        let (out, _) = rewrite_canvas_links(input, note, &full_map());
        assert_eq!(
            out,
            "Ver [01-fase-1.md](../modulos/02-semana/01-fase-1.md). O [electivo](../../electivo/README.md)\n"
        );
    }

    #[test]
    fn an_ambiguous_file_is_settled_only_by_a_label_naming_one_copy() {
        let map = full_map();
        let r = CanvasRef::File(2);
        let local = |p: &str| Resolution::Local(PathBuf::from(p));
        assert_eq!(
            map.resolve(&r, "Guía 2"),
            local("mineria/modulos/02-semana/adjuntos/guia-2.pdf")
        );
        assert_eq!(
            map.resolve(&r, "guia_1.PDF"),
            local("mineria/modulos/01-intro/adjuntos/guia-1.pdf")
        );
        assert_eq!(map.resolve(&r, "guia"), Resolution::Ambiguous);
        assert_eq!(map.resolve(&r, ""), Resolution::Ambiguous);
        assert_eq!(map.resolve(&CanvasRef::File(3), "x"), Resolution::Remote);
    }
}
