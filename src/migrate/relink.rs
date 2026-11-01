//! Where every Canvas resource is in the library, so the notes' Canvas links
//! can point at the local copies ([`crate::links::rewrite_canvas_links`]).
//!
//! [`read_sources`] is the only disk read: every v2 course (a `curso.json`),
//! its files and sizes, its notes' frontmatter, its `state.json`, its
//! `anuncios/index.json` media, and the renames of the finished migration
//! journals. [`build`] turns that into a [`LinkMap`] without touching the
//! filesystem, by these rules, most reliable first:
//!
//! - a note's own frontmatter: `url` (page, assignment, announcement) and,
//!   for `tarea` / `anuncio`, `canvas_id`;
//! - `state.json`: an entry's `path` (`ItemState::path`, written by `sync`
//!   and by a migration with Canvas), when that file is there;
//! - announcement media: `index.json`'s `local_path`, as is (v2) or through
//!   the journal's rename of the v1 file;
//! - a file whose `state.json` entry has an `etag` and a `size` (even with
//!   a `last_error` from a later refresh): the attachment(s) of the course with that size whose
//!   bytes have that S3 ETag — the MD5 of the content, or for a multipart
//!   upload (`<md5>-<N>`) the MD5 of its parts' MD5s, 5 MiB parts first.
//!   Proof of content, so it holds when Canvas no longer has the course;
//!   [`proven`] has it written into `state.json` as the entry's `path`;
//! - a file whose `state.json` entry has a `size` (v1 recorded no name):
//!   the one attachment of the course with that size, or with that size and
//!   the recorded `file_name`. Several of the same size are
//!   [`FileTarget::Ambiguous`], settled per link by its text or left remote;
//! - wherever a file was found, every other attachment of the course with
//!   its name and its bytes is a copy of it ([`FileTarget::Copies`]): v1
//!   downloaded one file into each module that showed it, and each note
//!   links the copy of its own module;
//! - a page `state.json` lists (`page:<slug>`): the one page note of the
//!   course named after that slug (`NN-<slug>.md`). Needs the course's
//!   Canvas id, since a page link names its course.
//!
//! Two different notes claiming one resource at the same level is not a
//! guess either way: the resource stays remote.
//!
//! With a Canvas client, [`learn`] adds what only Canvas knows (module
//! items, assignment ids, file names). That and what [`proven`] found are
//! written into each course's `state.json` ([`state_updates`], with a
//! backup), so later runs find it without hashing or asking.

use super::apply::{self, Entry, JournalState};
use super::plan::Root;
use super::rewrite;
use super::INDEX_JSON;
use crate::canvas::CanvasClient;
use crate::frontmatter::{self, DocKind, Frontmatter};
use crate::layout;
use crate::library;
use crate::links::{self, CanvasRef, FileTarget, LinkMap};
use crate::state::{ItemState, State};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

/// Version of the link rules a library was relinked with, recorded in
/// `library.json` as `links_version`. A library below it is relinked once by
/// the next writer command (`migrate::ensure::ensure_v2`).
///
/// 2: links inside a link's text (`[![x](…)](…)`, `[<img src=…>](…)`),
/// Canvas file URLs in other attributes (`data-api-endpoint`), and identical
/// copies of one file ([`FileTarget::Copies`]), and files found offline by
/// the S3 ETag `state.json` recorded for them (the MD5 of their bytes).
pub const LINKS_VERSION: u32 = 2;

/// State key of a module item, whose `path` is what the item shows.
pub fn module_item_key(id: u64) -> String {
    format!("module-item:{id}")
}

/// Everything [`build`] reads, from one library.
#[derive(Clone, Debug, Default)]
pub struct Sources {
    /// The v2 courses (directories with a `curso.json`), by directory name.
    pub courses: Vec<CourseSource>,
    /// v1 path → v2 path, for every rename a finished (not undone) journal
    /// made in the library, relative to the library root.
    pub renames: BTreeMap<String, String>,
}

/// One course directory, as [`build`] needs it. Paths are relative to the
/// course directory, `/`-separated.
#[derive(Clone, Debug, Default)]
pub struct CourseSource {
    /// Directory name under the library root.
    pub dir: String,
    /// Canvas id from `curso.json`.
    pub canvas_id: Option<u64>,
    /// `.u_crawler/state.json`, or empty when there is none.
    pub state: State,
    /// `.u_crawler/state.json` exists and was read.
    pub has_state: bool,
    /// Every regular file of the course outside `.u_crawler/`, with its size.
    pub files: BTreeMap<String, u64>,
    /// SHA-1 (hex) of each attachment whose size another attachment of the
    /// course shares: same bytes are the same file, whichever copy a link
    /// points at.
    pub digests: BTreeMap<String, String>,
    /// The S3 ETags ([`normalize_etag`]) of the bytes of each attachment a
    /// `file:` entry's `etag` is checked against: its MD5, and the multipart
    /// forms (`<md5>-<parts>`) those entries ask for. Only attachments with
    /// the `size` of such an entry are hashed ([`attachment_etags`]).
    pub etags: BTreeMap<String, BTreeSet<String>>,
    /// Every generated note ([`rewrite::generated_kind`]) with its frontmatter.
    pub notes: BTreeMap<String, Note>,
    /// `(file id, local_path)` of the media `anuncios/index.json` lists.
    pub index_media: Vec<(u64, String)>,
    /// `(announcement id, body_md_path)` of the records `anuncios/index.json`
    /// lists (v2: relative to the course; v1: `announcements/<name>`).
    pub index_notes: Vec<(u64, String)>,
    /// Hosts of the Canvas URLs the course records (notes' `url`,
    /// `index.json`'s `html_url`): where its Canvas lives.
    pub hosts: BTreeSet<String>,
}

/// A generated note.
#[derive(Clone, Debug)]
pub struct Note {
    /// What kind of note its path says it is.
    pub kind: DocKind,
    /// Its frontmatter, when it has ours.
    pub frontmatter: Option<Frontmatter>,
    /// Canvas file ids its text mentions ([`links::canvas_file_ids`]).
    pub file_ids: BTreeSet<u64>,
    /// SHA-1 (hex) of its bytes and of each backup of it under
    /// `.u_crawler/backup/<ts>/`: what v1 recorded as an item's
    /// `content_hash` is the SHA-1 of the note it wrote.
    pub hashes: BTreeSet<String>,
}

impl CourseSource {
    /// Files under an `adjuntos/` directory: `modulos/<m>/adjuntos/<f>` and
    /// `anuncios/adjuntos/<f>`.
    fn attachments(&self) -> impl Iterator<Item = (&str, u64)> {
        self.files.iter().filter_map(|(rel, size)| {
            let parts: Vec<&str> = rel.split('/').collect();
            let ok = match parts.as_slice() {
                [layout::MODULES_DIR, _, layout::ATTACHMENTS_DIR, name] => !name.ends_with(".part"),
                [layout::ANNOUNCEMENTS_DIR, layout::ATTACHMENTS_DIR, name] => {
                    !name.ends_with(".part")
                }
                _ => false,
            };
            ok.then_some((rel.as_str(), *size))
        })
    }

    fn join(&self, rel: &str) -> PathBuf {
        Path::new(&self.dir).join(rel)
    }
}

#[derive(Debug, Default, Deserialize)]
struct IndexRecord {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    body_md_path: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    media: Vec<IndexMedia>,
}

#[derive(Debug, Default, Deserialize)]
struct IndexMedia {
    #[serde(default)]
    file_id: Option<u64>,
    #[serde(default)]
    local_path: Option<String>,
}

/// Read what [`build`] needs from the library at `root`. Read-only.
///
/// # Errors
///
/// A directory or journal that cannot be listed or read. An unreadable
/// `state.json`, `index.json` or note is logged and treated as empty.
pub fn read_sources(root: &Path) -> io::Result<Sources> {
    let lib = library::read(root)?;
    let mut sources = Sources::default();
    let backups = backup_batches(root)?;
    for (dir, record) in &lib.records {
        let course_dir = root.join(dir);
        let mut course = CourseSource {
            dir: dir.clone(),
            canvas_id: record.canvas_id,
            ..CourseSource::default()
        };
        list_files(&course_dir, "", &mut course.files)?;
        let mut by_size: BTreeMap<u64, Vec<String>> = BTreeMap::new();
        for (rel, size) in course.attachments() {
            by_size.entry(size).or_default().push(rel.to_string());
        }
        for rel in by_size.into_values().filter(|r| r.len() > 1).flatten() {
            let bytes = std::fs::read(course_dir.join(&rel))?;
            course.digests.insert(rel, sha1_hex(&bytes));
        }
        let state_path = library::state_path(&course_dir);
        match std::fs::read(&state_path) {
            Ok(bytes) => match serde_json::from_slice::<State>(&bytes) {
                Ok(state) => {
                    course.state = state;
                    course.has_state = true;
                }
                Err(e) => {
                    tracing::warn!(path = %state_path.display(), error = %e, "unreadable state.json; links resolved without it")
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let index = course_dir.join(layout::ANNOUNCEMENTS_DIR).join(INDEX_JSON);
        if let Ok(text) = std::fs::read_to_string(&index) {
            match serde_json::from_str::<Vec<IndexRecord>>(&text) {
                Ok(records) => {
                    for record in records {
                        course
                            .hosts
                            .extend(record.html_url.as_deref().and_then(links::url_host));
                        if let (Some(id), Some(body)) = (record.id, record.body_md_path) {
                            course.index_notes.push((id, body));
                        }
                        course.index_media.extend(
                            record
                                .media
                                .into_iter()
                                .filter_map(|m| Some((m.file_id?, m.local_path?))),
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(path = %index.display(), error = %e, "unreadable index.json; its media are matched by size only")
                }
            }
        }
        course.etags = attachment_etags(&course_dir, &course);
        for rel in course.files.keys() {
            let Some(kind) = rewrite::generated_kind(&Path::new(dir).join(rel)) else {
                continue;
            };
            let text = match std::fs::read_to_string(course_dir.join(rel)) {
                Ok(text) => text,
                Err(e) => {
                    tracing::warn!(path = %course_dir.join(rel).display(), error = %e, "unreadable note; not relinked");
                    continue;
                }
            };
            let (_, body) = frontmatter::strip_bom(&text);
            let fm = frontmatter::parse(body).ok().flatten();
            course.hosts.extend(
                fm.as_ref()
                    .and_then(|f| f.url.as_deref())
                    .and_then(links::url_host),
            );
            let mut hashes = BTreeSet::from([sha1_hex(text.as_bytes())]);
            for batch in &backups {
                if let Ok(original) = std::fs::read(batch.join(dir).join(rel)) {
                    hashes.insert(sha1_hex(&original));
                }
            }
            course.notes.insert(
                rel.clone(),
                Note {
                    kind,
                    frontmatter: fm,
                    file_ids: links::canvas_file_ids(&text).collect(),
                    hashes,
                },
            );
        }
        sources.courses.push(course);
    }
    for (ts, state) in apply::journals(root)? {
        if !matches!(state, JournalState::Done | JournalState::Open) {
            continue;
        }
        for entry in apply::read_journal(&apply::journal_path(root, &ts))? {
            if let Entry::Rename {
                root: Root::Library,
                from,
                to,
            } = entry
            {
                sources.renames.insert(from, to);
            }
        }
    }
    Ok(sources)
}

/// Every `.u_crawler/backup/<ts>/` directory of the library.
fn backup_batches(root: &Path) -> io::Result<Vec<PathBuf>> {
    let dir = root.join(layout::META_DIR).join("backup");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            out.push(entry.path());
        }
    }
    out.sort();
    Ok(out)
}

fn sha1_hex(bytes: &[u8]) -> String {
    use sha1::{Digest, Sha1};
    hex::encode(Sha1::digest(bytes))
}

const MIB: u64 = 1024 * 1024;

/// Most part sizes a multipart ETag is tried with: each is one more MD5 pass
/// over the file.
const MAX_PART_SIZES: usize = 8;

/// The `file:<id>` entries of a course's `state.json`.
fn file_entries(course: &CourseSource) -> impl Iterator<Item = (u64, &ItemState)> {
    course.state.items.iter().filter_map(|(key, item)| {
        let id = key.strip_prefix("file:")?.parse().ok()?;
        Some((id, item))
    })
}

/// An S3 ETag as `state.json` records it, normalized: the lowercase hex MD5
/// (`"…"` quotes dropped), with `-<parts>` kept for a multipart upload, and
/// the part count. Anything else (a weak `W/` tag, not 32 hex digits, zero
/// parts) is no proof of content: `None`.
fn normalize_etag(raw: &str) -> Option<(String, Option<u32>)> {
    let tag = raw.trim();
    let tag = tag
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(tag);
    let (md5, parts) = match tag.split_once('-') {
        Some((md5, n)) => (md5, Some(n.parse::<u32>().ok().filter(|n| *n > 0)?)),
        None => (tag, None),
    };
    if md5.len() != 32 || !md5.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let md5 = md5.to_ascii_lowercase();
    let tag = match parts {
        Some(n) => format!("{md5}-{n}"),
        None => md5,
    };
    Some((tag, parts))
}

/// The normalized etag of a `file:` entry, which proves a local copy. A
/// `last_error` does not void it: a later refresh that failed leaves the
/// copy an earlier download wrote, and bytes with that ETag are that file
/// whatever happened since.
fn usable_etag(item: &ItemState) -> Option<(String, Option<u32>)> {
    normalize_etag(item.etag.as_deref()?)
}

/// The part sizes, whole MiB, that cut `size` bytes into exactly `parts`
/// parts: 5 MiB first (every multipart ETag observed from Canvas), then the
/// others ascending, at most [`MAX_PART_SIZES`]. One part is one size: any
/// part at least as large as the file gives the same ETag.
fn part_sizes(size: u64, parts: u32) -> Vec<u64> {
    let n = u64::from(parts);
    if n == 0 || size == 0 {
        return Vec::new();
    }
    let smallest = size.div_ceil(n.saturating_mul(MIB)).max(1);
    if n == 1 {
        return vec![smallest * MIB];
    }
    let fits = |mib: u64| {
        let part = mib.saturating_mul(MIB);
        (n - 1).saturating_mul(part) < size && size <= n.saturating_mul(part)
    };
    let mut out = Vec::new();
    if fits(5) {
        out.push(5 * MIB);
    }
    for mib in (smallest..).take_while(|m| fits(*m)) {
        if out.len() >= MAX_PART_SIZES {
            break;
        }
        if mib != 5 {
            out.push(mib * MIB);
        }
    }
    out
}

/// The S3 ETags `bytes` can have: its MD5, and for each part size, the MD5
/// of the concatenated binary MD5s of its parts, `-<number of parts>`. One
/// pass over `bytes`, in 1 MiB reads (every part size is whole MiB).
fn s3_etags(mut bytes: impl io::Read, part_sizes: &BTreeSet<u64>) -> io::Result<BTreeSet<String>> {
    use md5::{Digest, Md5};
    struct Multipart {
        part: u64,
        filled: u64,
        current: Md5,
        digests: Vec<u8>,
        count: usize,
    }
    impl Multipart {
        fn finish_part(&mut self) {
            self.digests.extend(self.current.finalize_reset());
            self.count += 1;
            self.filled = 0;
        }
    }
    let mut whole = Md5::new();
    let mut multis: Vec<Multipart> = part_sizes
        .iter()
        .map(|&part| Multipart {
            part,
            filled: 0,
            current: Md5::new(),
            digests: Vec::new(),
            count: 0,
        })
        .collect();
    let mut buf = vec![0u8; MIB as usize];
    loop {
        let mut got = 0;
        while got < buf.len() {
            match bytes.read(&mut buf[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        let chunk = &buf[..got];
        whole.update(chunk);
        for m in &mut multis {
            m.current.update(chunk);
            m.filled += got as u64;
            if m.filled == m.part {
                m.finish_part();
            }
        }
        if got < buf.len() {
            break;
        }
    }
    let mut out = BTreeSet::from([hex::encode(whole.finalize())]);
    for mut m in multis {
        if m.filled > 0 {
            m.finish_part();
        }
        out.insert(format!(
            "{}-{}",
            hex::encode(Md5::digest(&m.digests)),
            m.count
        ));
    }
    Ok(out)
}

/// [`CourseSource::etags`]: the attachments of `course` that have the size
/// of a `file:` entry with a usable etag and no sure copy yet (a `path` or
/// `index.json` media on disk), each read once. Nothing else is hashed, and
/// an entry without a `size` hashes nothing. An unreadable attachment is
/// logged and left out.
fn attachment_etags(
    course_dir: &Path,
    course: &CourseSource,
) -> BTreeMap<String, BTreeSet<String>> {
    let indexed: BTreeSet<u64> = course
        .index_media
        .iter()
        .filter(|(_, local)| course.files.contains_key(local))
        .map(|(id, _)| *id)
        .collect();
    let mut wanted: BTreeMap<u64, BTreeSet<u64>> = BTreeMap::new();
    for (id, item) in file_entries(course) {
        let placed = item
            .path
            .as_deref()
            .is_some_and(|p| course.files.contains_key(p));
        let (Some(size), Some((_, parts)), false) = (
            item.size,
            usable_etag(item),
            placed || indexed.contains(&id),
        ) else {
            continue;
        };
        let sizes = wanted.entry(size).or_default();
        if let Some(n) = parts {
            sizes.extend(part_sizes(size, n));
        }
    }
    let mut out = BTreeMap::new();
    for (rel, size) in course.attachments() {
        let Some(parts) = wanted.get(&size) else {
            continue;
        };
        let path = course_dir.join(rel);
        match std::fs::File::open(&path).and_then(|f| s3_etags(f, parts)) {
            Ok(tags) => {
                out.insert(rel.to_string(), tags);
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "attachment not readable; not matched by etag")
            }
        }
    }
    out
}

/// The attachments of `course` whose bytes have the S3 ETag `item` recorded
/// when it downloaded them (same `size`, same MD5): proof of content, not a
/// guess. Several are copies with the same bytes. `None` when the entry has
/// no usable etag or size, or nothing matches.
fn by_etag(course: &CourseSource, item: &ItemState) -> Option<FileTarget> {
    let (tag, _) = usable_etag(item)?;
    let size = item.size?;
    let mut hits: Vec<PathBuf> = course
        .attachments()
        .filter(|(rel, s)| *s == size && course.etags.get(*rel).is_some_and(|t| t.contains(&tag)))
        .map(|(rel, _)| course.join(rel))
        .collect();
    match hits.len() {
        0 => None,
        1 => hits.pop().map(FileTarget::Path),
        _ => Some(FileTarget::Copies(hits)),
    }
}

/// [`build`] over [`read_sources`]: where everything already on disk is.
///
/// # Errors
///
/// As [`read_sources`].
pub fn disk_map(root: &Path) -> io::Result<LinkMap> {
    Ok(build(&read_sources(root)?))
}

/// Every regular file under `dir` outside `.u_crawler`, `.git` and
/// `.obsidian`, with its size, relative to it. Symlinks and names that are
/// not UTF-8 are skipped.
fn list_files(dir: &Path, prefix: &str, out: &mut BTreeMap<String, u64>) -> io::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if matches!(name.as_str(), layout::META_DIR | ".git" | ".obsidian") {
            continue;
        }
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let ft = entry.file_type()?;
        if ft.is_dir() {
            list_files(&entry.path(), &rel, out)?;
        } else if ft.is_file() {
            out.insert(rel, entry.metadata()?.len());
        }
    }
    Ok(())
}

/// Candidates for one key, by how reliable their source is (lower wins).
struct Votes<K> {
    votes: BTreeMap<K, BTreeMap<u8, BTreeSet<PathBuf>>>,
}

impl<K: Ord + Clone + std::fmt::Debug> Votes<K> {
    fn new() -> Self {
        Votes {
            votes: BTreeMap::new(),
        }
    }

    fn add(&mut self, key: K, level: u8, path: PathBuf) {
        self.votes
            .entry(key)
            .or_default()
            .entry(level)
            .or_default()
            .insert(path);
    }

    /// The one path of each key's most reliable level; a key whose best
    /// level has two different paths is left out (and logged).
    fn settle(self, what: &str) -> BTreeMap<K, PathBuf> {
        let mut out = BTreeMap::new();
        for (key, levels) in self.votes {
            let Some((_, paths)) = levels.into_iter().next() else {
                continue;
            };
            if paths.len() == 1 {
                if let Some(path) = paths.into_iter().next() {
                    out.insert(key, path);
                }
            } else {
                tracing::info!(what, key = ?key, candidates = ?paths, "two notes claim one Canvas resource; its links stay remote");
            }
        }
        out
    }
}

const LEVEL_EXACT: u8 = 0;
const LEVEL_GUESSED: u8 = 1;

/// The [`LinkMap`] of a library, from what is on disk (see the module docs).
pub fn build(sources: &Sources) -> LinkMap {
    let mut map = LinkMap::default();
    let mut pages = Votes::new();
    let mut assignments = Votes::new();
    let mut announcements = Votes::new();
    let mut module_items = Votes::new();
    for course in &sources.courses {
        map.hosts.extend(course.hosts.iter().cloned());
        if let Some(id) = course.canvas_id {
            map.courses.insert(id, PathBuf::from(&course.dir));
        }
        for (rel, note) in &course.notes {
            let path = course.join(rel);
            if let Some(fm) = &note.frontmatter {
                match fm.url.as_deref().and_then(links::canvas_ref) {
                    Some(CanvasRef::Page { course, slug }) => {
                        pages.add((course, slug), LEVEL_EXACT, path.clone())
                    }
                    Some(CanvasRef::Assignment(id)) => {
                        assignments.add(id, LEVEL_EXACT, path.clone())
                    }
                    Some(CanvasRef::Announcement(id)) => {
                        announcements.add(id, LEVEL_EXACT, path.clone())
                    }
                    _ => {}
                }
                match (fm.tipo, fm.canvas_id) {
                    (DocKind::Assignment, Some(id)) => {
                        assignments.add(id, LEVEL_EXACT, path.clone())
                    }
                    (DocKind::Announcement, Some(id)) => {
                        announcements.add(id, LEVEL_EXACT, path.clone())
                    }
                    _ => {}
                }
            }
        }
        let prefix = format!("{}/", course.dir);
        for (id, body) in &course.index_notes {
            if course.notes.contains_key(body) {
                announcements.add(*id, LEVEL_EXACT, course.join(body));
                continue;
            }
            // v1 `announcements/<name>`, renamed by a migration journal.
            let suffix = format!("/{body}");
            for (_, to) in sources
                .renames
                .iter()
                .filter(|(from, to)| from.ends_with(&suffix) && to.starts_with(&prefix))
            {
                if let Some(rel) = to
                    .strip_prefix(&prefix)
                    .filter(|r| course.notes.contains_key(*r))
                {
                    announcements.add(*id, LEVEL_EXACT, course.join(rel));
                }
            }
        }
        for (key, item) in &course.state.items {
            let Some((kind, id)) = key.split_once(':') else {
                continue;
            };
            let recorded = item
                .path
                .as_deref()
                .filter(|p| course.files.contains_key(*p))
                .map(|p| course.join(p));
            // The note whose bytes (or whose backup's) v1 hashed.
            let hashed = |want: DocKind| -> Vec<PathBuf> {
                let Some(hash) = item.content_hash.as_deref() else {
                    return Vec::new();
                };
                course
                    .notes
                    .iter()
                    .filter(|(_, note)| note.kind == want && note.hashes.contains(hash))
                    .map(|(rel, _)| course.join(rel))
                    .collect()
            };
            match (kind, recorded) {
                ("page", recorded) => {
                    if let Some(course_id) = course.canvas_id {
                        let key = (course_id, id.to_ascii_lowercase());
                        if let Some(path) = recorded {
                            pages.add(key.clone(), LEVEL_EXACT, path);
                        }
                        for path in hashed(DocKind::Page) {
                            pages.add(key.clone(), LEVEL_EXACT, path);
                        }
                        for path in page_notes_for_slug(course, course_id, id) {
                            pages.add(key.clone(), LEVEL_GUESSED, path);
                        }
                    }
                }
                ("assignment", recorded) => {
                    if let Ok(id) = id.parse() {
                        for path in recorded.into_iter().chain(hashed(DocKind::Assignment)) {
                            assignments.add(id, LEVEL_EXACT, path);
                        }
                    }
                }
                ("announcement", Some(path)) => {
                    if let Ok(id) = id.parse() {
                        announcements.add(id, LEVEL_EXACT, path);
                    }
                }
                ("module-item", Some(path)) => {
                    if let Ok(id) = id.parse() {
                        module_items.add(id, LEVEL_EXACT, path);
                    }
                }
                _ => {}
            }
        }
        for (id, target) in course_files(course, &sources.renames) {
            // File ids are global: a copy in any course is the file. The
            // first sure one wins over a later guess.
            match (map.files.get(&id), &target) {
                (Some(FileTarget::Path(_) | FileTarget::Copies(_)), _) => {}
                (Some(FileTarget::Ambiguous(_)), FileTarget::Ambiguous(_)) => {}
                _ => {
                    map.files.insert(id, target);
                }
            }
        }
    }
    // A v1 announcement name ends in its id (`…_<id>.md`): the migrator's
    // own source for `canvas_id`, readable before the note has frontmatter.
    for (from, to) in &sources.renames {
        let from_kind = rewrite::generated_kind(Path::new(from));
        let to_kind = rewrite::generated_kind(Path::new(to));
        if from_kind != Some(DocKind::Announcement) || to_kind != Some(DocKind::Announcement) {
            continue;
        }
        let Some((dir, rel)) = to.split_once('/') else {
            continue;
        };
        let known = sources
            .courses
            .iter()
            .any(|c| c.dir == dir && c.notes.contains_key(rel));
        if let (true, Some(id)) = (known, v1_announcement_id(from)) {
            announcements.add(id, LEVEL_EXACT, PathBuf::from(to));
        }
    }
    map.pages = pages.settle("page");
    map.assignments = assignments.settle("assignment");
    map.announcements = announcements.settle("announcement");
    map.module_items = module_items.settle("module item");
    map
}

/// The announcement id at the end of a v1 body name: `…_<id>.md`.
pub fn v1_announcement_id(path: &str) -> Option<u64> {
    let stem = path.rsplit('/').next()?.strip_suffix(".md")?;
    stem.rsplit_once('_')?.1.parse().ok()
}

/// The page notes of `course` named after `slug` (`modulos/<m>/NN-<slug>.md`)
/// whose frontmatter does not name another page.
fn page_notes_for_slug(course: &CourseSource, course_id: u64, slug: &str) -> Vec<PathBuf> {
    let wanted = layout::page_file(1, slug);
    let Some((_, wanted)) = wanted.split_once('-') else {
        return Vec::new();
    };
    let slug = slug.to_ascii_lowercase();
    course
        .notes
        .iter()
        .filter(|(rel, note)| {
            let parts: Vec<&str> = rel.split('/').collect();
            let named = match parts.as_slice() {
                [layout::MODULES_DIR, _, file] => file.split_once('-').is_some_and(|(nn, rest)| {
                    nn.bytes().all(|b| b.is_ascii_digit()) && rest == wanted
                }),
                _ => false,
            };
            let other = note
                .frontmatter
                .as_ref()
                .and_then(|fm| fm.url.as_deref())
                .and_then(links::canvas_ref)
                .is_some_and(|r| {
                    r != CanvasRef::Page {
                        course: course_id,
                        slug: slug.clone(),
                    }
                });
            note.kind == DocKind::Page && named && !other
        })
        .map(|(rel, _)| course.join(rel))
        .collect()
}

/// Where each Canvas file of `course` is: `state.json`'s `path`, then the
/// announcement media `index.json` lists, then the content its recorded
/// etag proves ([`by_etag`]), then size (and recorded name).
fn course_files(
    course: &CourseSource,
    renames: &BTreeMap<String, String>,
) -> Vec<(u64, FileTarget)> {
    let attachments: Vec<(&str, u64)> = course.attachments().collect();
    let mut out: BTreeMap<u64, FileTarget> = BTreeMap::new();
    let sure = |id: u64, rel: &str, out: &mut BTreeMap<u64, FileTarget>| {
        if out.get(&id).and_then(FileTarget::sure_path).is_none() {
            out.insert(id, copies_of(course, rel));
        }
    };
    for (id, item) in file_entries(course) {
        if let Some(path) = item
            .path
            .as_deref()
            .filter(|p| course.files.contains_key(*p))
        {
            sure(id, path, &mut out);
        }
    }
    let prefix = format!("{}/", course.dir);
    for (id, local) in &course.index_media {
        if attachments.iter().any(|(rel, _)| rel == local) {
            sure(*id, local, &mut out);
            continue;
        }
        let suffix = format!("/{local}");
        let moved: BTreeSet<&str> = renames
            .iter()
            .filter(|(from, to)| from.ends_with(&suffix) && to.starts_with(&prefix))
            .filter_map(|(_, to)| to.strip_prefix(&prefix))
            .filter(|rel| course.files.contains_key(*rel))
            .collect();
        if moved.len() == 1 {
            if let Some(rel) = moved.into_iter().next() {
                sure(*id, rel, &mut out);
            }
        }
    }
    for (id, item) in file_entries(course) {
        if out.contains_key(&id) {
            continue;
        }
        if let Some(target) = by_etag(course, item) {
            out.insert(id, target);
        }
    }
    for (id, item) in file_entries(course) {
        if out.contains_key(&id) {
            continue;
        }
        if let Some(target) = by_size(&attachments, &course.digests, item) {
            out.insert(
                id,
                match target {
                    BySize::One(rel) => copies_of(course, rel),
                    BySize::Candidates(rels) => FileTarget::Ambiguous(
                        rels.iter()
                            .map(|rel| Path::new(&course.dir).join(rel))
                            .collect(),
                    ),
                },
            );
        }
    }
    out.into_iter().collect()
}

/// What [`by_size`] found, relative to the course directory.
enum BySize<'a> {
    /// The file (its copies are found by [`copies_of`]).
    One(&'a str),
    /// One of these, if the link's text names it.
    Candidates(Vec<&'a str>),
}

/// `rel` (relative to `course`) and every other attachment of the course
/// with its file name and its bytes, as library paths: the copies of one
/// file. A file with no such twin is a [`FileTarget::Path`].
fn copies_of(course: &CourseSource, rel: &str) -> FileTarget {
    let name = rel.rsplit('/').next();
    let copies: Vec<PathBuf> = match course.digests.get(rel) {
        Some(digest) => course
            .attachments()
            .filter(|(other, _)| {
                *other == rel
                    || (other.rsplit('/').next() == name
                        && course.digests.get(*other) == Some(digest))
            })
            .map(|(other, _)| course.join(other))
            .collect(),
        None => Vec::new(),
    };
    if copies.len() > 1 {
        FileTarget::Copies(copies)
    } else {
        FileTarget::Path(course.join(rel))
    }
}

/// The attachment(s) a file entry can be, by its recorded name and size.
///
/// With a recorded `file_name` (v2) only attachments with that name (and
/// size) count: none means the copy is not on disk, and another file that
/// happens to have the size is never taken for it. Several with the same
/// bytes are copies of one file ([`BySize::One`] names the first;
/// [`copies_of`] finds the rest). Without a name (v1), the size alone
/// proves nothing: every attachment of that size is a candidate (even a
/// single one), and a link reaches one only when its own text names it
/// ([`LinkMap::resolve`]).
fn by_size<'a>(
    attachments: &[(&'a str, u64)],
    digests: &BTreeMap<String, String>,
    item: &ItemState,
) -> Option<BySize<'a>> {
    let candidates: Vec<&'a str> = match item.file_name.as_deref() {
        Some(name) => attachments
            .iter()
            .filter(|(rel, size)| {
                rel.rsplit('/').next() == Some(name) && item.size.is_none_or(|s| s == *size)
            })
            .map(|(rel, _)| *rel)
            .collect(),
        None => {
            let size = item.size?;
            attachments
                .iter()
                .filter(|(_, s)| *s == size)
                .map(|(rel, _)| *rel)
                .collect()
        }
    };
    // Same bytes, same file: keep one copy of each content.
    let mut seen = BTreeSet::new();
    let distinct: Vec<&'a str> = candidates
        .into_iter()
        .filter(|rel| match digests.get(*rel) {
            Some(digest) => seen.insert(digest.clone()),
            None => true,
        })
        .collect();
    match (item.file_name.is_some(), distinct.as_slice()) {
        (_, []) => None,
        (true, [one]) => Some(BySize::One(one)),
        _ => Some(BySize::Candidates(distinct)),
    }
}

/// What Canvas told [`learn`]: per course directory, state key → path
/// relative to the course directory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Learned {
    /// Course directory → state key (`page:`, `assignment:`, `file:`,
    /// `module-item:`) → path relative to it.
    pub paths: BTreeMap<String, BTreeMap<String, String>>,
}

impl Learned {
    /// Put what Canvas said into `map`, over what the disk suggested.
    pub fn apply(&self, sources: &Sources, map: &mut LinkMap) {
        for course in &sources.courses {
            let Some(paths) = self.paths.get(&course.dir) else {
                continue;
            };
            for (key, rel) in paths {
                let path = course.join(rel);
                let Some((kind, id)) = key.split_once(':') else {
                    continue;
                };
                match kind {
                    "page" => {
                        if let Some(course_id) = course.canvas_id {
                            map.pages.insert((course_id, id.to_string()), path);
                        }
                    }
                    "assignment" => {
                        if let Ok(id) = id.parse() {
                            map.assignments.insert(id, path);
                        }
                    }
                    "file" => {
                        if let Ok(id) = id.parse() {
                            map.files.insert(id, copies_of(course, rel));
                        }
                    }
                    "module-item" => {
                        if let Ok(id) = id.parse() {
                            map.module_items.insert(id, path);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    /// Add `other`'s paths, over this one's where both name a key.
    pub fn extend(&mut self, other: Learned) {
        for (dir, paths) in other.paths {
            self.paths.entry(dir).or_default().extend(paths);
        }
    }
}

/// What the disk proves but `state.json` does not record yet: the copy of
/// each `file:<id>` [`build`] found by its etag ([`by_etag`]), the first of
/// its copies, so [`state_updates`] writes it down and later runs (and a
/// course Canvas no longer answers for) find it by its `path`. Already in
/// [`build`]'s map; not to be [`Learned::apply`]'d.
pub fn proven(sources: &Sources) -> Learned {
    let mut learned = Learned::default();
    for course in &sources.courses {
        let found: BTreeMap<u64, FileTarget> =
            course_files(course, &sources.renames).into_iter().collect();
        let mut paths = BTreeMap::new();
        for (id, item) in file_entries(course) {
            let Some(target) = by_etag(course, item) else {
                continue;
            };
            if found.get(&id) != Some(&target) {
                continue;
            }
            let rel = target
                .sure_path()
                .and_then(|p| p.strip_prefix(&course.dir).ok())
                .and_then(Path::to_str)
                .map(|p| p.replace('\\', "/"));
            if let Some(rel) = rel.filter(|rel| item.path.as_deref() != Some(rel.as_str())) {
                paths.insert(format!("file:{id}"), rel);
            }
        }
        if !paths.is_empty() {
            learned.paths.insert(course.dir.clone(), paths);
        }
    }
    learned
}

/// Ask Canvas what the disk cannot say, for every course with a Canvas id:
/// module items (and through them pages and assignments) and the name of
/// each file a note links to that `map` has no single copy of. Paths are
/// only learned when the file they name is on disk. A course Canvas will
/// not answer for is skipped with a warning: the offline map still holds.
pub async fn learn(client: &CanvasClient, sources: &Sources, map: &LinkMap) -> Learned {
    let mut learned = Learned::default();
    for course in &sources.courses {
        let Some(course_id) = course.canvas_id else {
            continue;
        };
        let mut paths: BTreeMap<String, String> = BTreeMap::new();
        let assignment_files = match client.list_assignments(course_id).await {
            Ok(list) => crate::syncer::assignment_files(&list),
            Err(e) => {
                tracing::warn!(course_id, error = %e, "assignments not readable; their links resolved offline");
                Default::default()
            }
        };
        let modules = match client.list_modules_with_items(course_id).await {
            Ok(modules) => modules,
            Err(e) => {
                tracing::warn!(course_id, error = %e, "modules not readable; their links resolved offline");
                Vec::new()
            }
        };
        let mut file_items: Vec<(u64, u64)> = Vec::new();
        for (i, module) in modules.iter().enumerate() {
            let position = module
                .position
                .unwrap_or_else(|| u32::try_from(i + 1).unwrap_or(u32::MAX));
            let module_dir = layout::module_dir(position, &module.name);
            for (idx, item) in module.items.iter().enumerate() {
                let item_position = u32::try_from(idx + 1).unwrap_or(u32::MAX);
                if let Some(slug) = item.page_url.as_deref() {
                    let title = item.title.as_deref().unwrap_or(slug);
                    if let Some(rel) = page_note(
                        course,
                        &module_dir,
                        &layout::page_file(item_position, title),
                    ) {
                        paths.insert(format!("page:{}", slug.to_ascii_lowercase()), rel.clone());
                        paths.insert(module_item_key(item.id), rel);
                    }
                    continue;
                }
                match (item.kind.as_deref(), item.content_id) {
                    (Some("Assignment"), Some(aid)) => {
                        let Some(file) = assignment_files.get(&aid) else {
                            continue;
                        };
                        let rel = format!("{}/{file}", layout::ASSIGNMENTS_DIR);
                        if course.files.contains_key(&rel) {
                            paths.insert(format!("assignment:{aid}"), rel.clone());
                            paths.insert(module_item_key(item.id), rel);
                        }
                    }
                    (Some("File"), Some(fid)) => file_items.push((item.id, fid)),
                    _ => {}
                }
            }
        }
        let mut wanted: BTreeSet<u64> = course
            .notes
            .values()
            .flat_map(|n| n.file_ids.iter().copied())
            .collect();
        wanted.extend(file_items.iter().map(|(_, fid)| *fid));
        let attachments: Vec<(&str, u64)> = course.attachments().collect();
        for fid in wanted {
            let known = map
                .files
                .get(&fid)
                .and_then(FileTarget::sure_path)
                .and_then(|path| path.strip_prefix(&course.dir).ok())
                .and_then(|p| p.to_str())
                .map(str::to_string);
            let rel = match known {
                Some(rel) => Some(rel),
                None => match client.get_file(fid).await {
                    Ok(f) => file_by_name(&attachments, &course.digests, &f),
                    Err(e) => {
                        tracing::info!(course_id, file_id = fid, error = %e, "file not readable from Canvas; its links stay as they are");
                        None
                    }
                },
            };
            if let Some(rel) = rel {
                paths.insert(format!("file:{fid}"), rel);
            }
        }
        for (item_id, fid) in file_items {
            if let Some(rel) = paths.get(&format!("file:{fid}")).cloned() {
                paths.insert(module_item_key(item_id), rel);
            }
        }
        if !paths.is_empty() {
            learned.paths.insert(course.dir.clone(), paths);
        }
    }
    learned
}

/// The page note `file` of a module: under `module_dir` when it is there,
/// else the one module that has it.
fn page_note(course: &CourseSource, module_dir: &str, file: &str) -> Option<String> {
    let exact = format!("{}/{module_dir}/{file}", layout::MODULES_DIR);
    if course.notes.contains_key(&exact) {
        return Some(exact);
    }
    let mut found = course.notes.keys().filter(|rel| {
        let parts: Vec<&str> = rel.split('/').collect();
        matches!(parts.as_slice(), [layout::MODULES_DIR, _, f] if *f == file)
    });
    let first = found.next()?;
    found.next().is_none().then(|| first.clone())
}

/// The attachment named for `f` (`layout::attachment_file`, or its `-<id>`
/// form `download::Claims` gives a second file of that name) with its size,
/// when Canvas gives one.
///
/// Several hits are one file when they all have the same bytes (one
/// download copied into several modules): the first, in path order, is
/// recorded, and [`Learned::apply`] maps all of them as its copies. Hits
/// whose bytes differ are different files, and none is taken.
fn file_by_name(
    attachments: &[(&str, u64)],
    digests: &BTreeMap<String, String>,
    f: &crate::canvas::FileObj,
) -> Option<String> {
    let name = f.display_name.as_deref().or(f.filename.as_deref())?;
    let plain = layout::attachment_file(name);
    let suffixed = layout::with_suffix(&plain, &f.id.to_string());
    let hits: Vec<&str> = attachments
        .iter()
        .filter(|(rel, size)| {
            let base = rel.rsplit('/').next().unwrap_or(rel);
            (base == plain || base == suffixed) && f.size.is_none_or(|s| s == *size)
        })
        .map(|(rel, _)| *rel)
        .collect();
    let (first, rest) = hits.split_first()?;
    let digest = digests.get(*first);
    let same = |rel: &&str| digest.is_some() && digests.get(*rel) == digest;
    rest.iter().all(same).then(|| (*first).to_string())
}

/// The `state.json` each course gets once what Canvas taught (or the disk
/// proved, [`proven`]) is in it: the `path` of entries that are there (and
/// a `file:` entry's `file_name`, when it had none), and new
/// `module-item:<id>` entries. Only courses that have a `state.json` and whose entries change
/// are listed, as (path relative to the library root, new text); the caller
/// writes them with [`rewrite::rewrite_state`], so `--undo` restores them.
///
/// # Errors
///
/// Serializing a state fails (it cannot, for these types).
pub fn state_updates(
    sources: &Sources,
    learned: &Learned,
) -> serde_json::Result<Vec<(PathBuf, String)>> {
    let mut out = Vec::new();
    for course in &sources.courses {
        let Some(paths) = learned.paths.get(&course.dir) else {
            continue;
        };
        if !course.has_state {
            continue;
        }
        let mut state = course.state.clone();
        for (key, rel) in paths {
            match state.items.get_mut(key) {
                Some(item) => {
                    item.path = Some(rel.clone());
                    // The name the copy has, so a later `sync` keeps it
                    // (`download::Claims`) rather than download it again.
                    if key.starts_with("file:") && item.file_name.is_none() {
                        item.file_name = rel.rsplit('/').next().map(str::to_string);
                    }
                }
                None if key.starts_with("module-item:") => {
                    state.items.insert(
                        key.clone(),
                        ItemState {
                            path: Some(rel.clone()),
                            ..ItemState::default()
                        },
                    );
                }
                None => {}
            }
        }
        if state.items == course.state.items {
            continue;
        }
        let rel = Path::new(&course.dir)
            .join(layout::META_DIR)
            .join(library::STATE_FILE);
        out.push((rel, serde_json::to_string_pretty(&state)?));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fm(tipo: DocKind, canvas_id: Option<u64>, url: Option<&str>) -> Option<Frontmatter> {
        Some(Frontmatter {
            ramo: "ramo".into(),
            tipo,
            canvas_id,
            url: url.map(str::to_string),
            publicado: None,
            vence: None,
            disponible: None,
        })
    }

    fn note(kind: DocKind, frontmatter: Option<Frontmatter>) -> Note {
        Note {
            kind,
            frontmatter,
            file_ids: BTreeSet::new(),
            hashes: BTreeSet::new(),
        }
    }

    fn entry(size: Option<u64>, file_name: Option<&str>, path: Option<&str>) -> ItemState {
        ItemState {
            size,
            file_name: file_name.map(str::to_string),
            path: path.map(str::to_string),
            ..ItemState::default()
        }
    }

    fn course() -> CourseSource {
        let mut c = CourseSource {
            dir: "ramo".into(),
            canvas_id: Some(9),
            has_state: true,
            ..CourseSource::default()
        };
        for (rel, size) in [
            ("modulos/01-intro/adjuntos/guia-1.pdf", 30),
            ("modulos/02-semana/adjuntos/guia-2.pdf", 30),
            ("modulos/02-semana/adjuntos/rubrica.docx", 41),
            ("anuncios/adjuntos/coder-s3.zip", 50),
            ("modulos/01-intro/01-intro.md", 10),
            ("modulos/02-semana/01-fase-1.md", 10),
            ("modulos/02-semana/02-fase-2.md", 10),
            ("tareas/tarea.md", 10),
            ("anuncios/2026-06-17_hola.md", 10),
        ] {
            c.files.insert(rel.into(), size);
        }
        c.notes.insert(
            "modulos/01-intro/01-intro.md".into(),
            note(DocKind::Page, None),
        );
        c.notes.insert(
            "modulos/02-semana/01-fase-1.md".into(),
            note(DocKind::Page, None),
        );
        c.notes.insert(
            "modulos/02-semana/02-fase-2.md".into(),
            note(
                DocKind::Page,
                fm(
                    DocKind::Page,
                    Some(3),
                    Some("https://canvas.example.edu/courses/9/pages/fase-2"),
                ),
            ),
        );
        c.notes.insert(
            "tareas/tarea.md".into(),
            note(DocKind::Assignment, fm(DocKind::Assignment, Some(77), None)),
        );
        c.notes.insert(
            "anuncios/2026-06-17_hola.md".into(),
            note(
                DocKind::Announcement,
                fm(DocKind::Announcement, Some(501), None),
            ),
        );
        c
    }

    #[test]
    fn files_by_path_size_name_and_index_media() {
        let mut c = course();
        c.state
            .items
            .insert("file:1".into(), entry(Some(30), None, None));
        c.state
            .items
            .insert("file:2".into(), entry(Some(30), Some("guia-2.pdf"), None));
        c.state
            .items
            .insert("file:3".into(), entry(Some(41), None, None));
        c.state.items.insert(
            "file:4".into(),
            entry(Some(30), None, Some("modulos/01-intro/adjuntos/guia-1.pdf")),
        );
        c.state
            .items
            .insert("file:5".into(), entry(Some(999), None, None));
        c.index_media
            .push((4242, "announcements/media/CodeR_S3.zip".into()));
        let mut renames = BTreeMap::new();
        renames.insert(
            "V1_DIR/announcements/media/CodeR_S3.zip".to_string(),
            "ramo/anuncios/adjuntos/coder-s3.zip".to_string(),
        );
        let files: BTreeMap<u64, FileTarget> = course_files(&c, &renames).into_iter().collect();
        let p = |s: &str| PathBuf::from(format!("ramo/{s}"));
        assert_eq!(
            files[&1],
            FileTarget::Ambiguous(vec![
                p("modulos/01-intro/adjuntos/guia-1.pdf"),
                p("modulos/02-semana/adjuntos/guia-2.pdf")
            ])
        );
        assert_eq!(
            files[&2],
            FileTarget::Path(p("modulos/02-semana/adjuntos/guia-2.pdf"))
        );
        // A size alone (v1, no name) is only a candidate, even when unique.
        assert_eq!(
            files[&3],
            FileTarget::Ambiguous(vec![p("modulos/02-semana/adjuntos/rubrica.docx")])
        );
        assert_eq!(
            files[&4],
            FileTarget::Path(p("modulos/01-intro/adjuntos/guia-1.pdf"))
        );
        assert_eq!(
            files[&4242],
            FileTarget::Path(p("anuncios/adjuntos/coder-s3.zip"))
        );
        assert!(!files.contains_key(&5));
    }

    #[test]
    fn a_recorded_name_not_on_disk_is_never_matched_by_size() {
        let mut c = course();
        c.files
            .insert("modulos/01-intro/adjuntos/otro-archivo.pdf".into(), 1234);
        c.state.items.insert(
            "file:500".into(),
            entry(Some(1234), Some("guia-3.pdf"), None),
        );
        // v1, no name: the one file of that size is a candidate, not an answer.
        c.state
            .items
            .insert("file:501".into(), entry(Some(1234), None, None));
        let files: BTreeMap<u64, FileTarget> =
            course_files(&c, &BTreeMap::new()).into_iter().collect();
        assert!(!files.contains_key(&500));
        let mut map = LinkMap::default();
        map.files.insert(501, files[&501].clone());
        assert_eq!(
            map.resolve(
                &CanvasRef::File(501),
                "Guía 3",
                Path::new("ramo/tareas/tarea.md")
            ),
            links::Resolution::Ambiguous
        );
        assert_eq!(
            map.resolve(
                &CanvasRef::File(501),
                "otro archivo.pdf",
                Path::new("ramo/tareas/tarea.md")
            ),
            links::Resolution::Local(PathBuf::from(
                "ramo/modulos/01-intro/adjuntos/otro-archivo.pdf"
            ))
        );
    }

    #[test]
    fn copies_with_the_same_bytes_are_one_file() {
        let mut c = course();
        for rel in [
            "modulos/01-intro/adjuntos/guia.pdf",
            "modulos/02-semana/adjuntos/guia.pdf",
        ] {
            c.files.insert(rel.into(), 777);
            c.digests.insert(rel.into(), "same".into());
        }
        c.state
            .items
            .insert("file:600".into(), entry(Some(777), Some("guia.pdf"), None));
        c.state
            .items
            .insert("file:601".into(), entry(Some(777), None, None));
        c.state.items.insert(
            "file:602".into(),
            entry(None, None, Some("modulos/02-semana/adjuntos/guia.pdf")),
        );
        let files: BTreeMap<u64, FileTarget> =
            course_files(&c, &BTreeMap::new()).into_iter().collect();
        let first = PathBuf::from("ramo/modulos/01-intro/adjuntos/guia.pdf");
        let second = PathBuf::from("ramo/modulos/02-semana/adjuntos/guia.pdf");
        let copies = FileTarget::Copies(vec![first.clone(), second.clone()]);
        assert_eq!(files[&600], copies);
        // A recorded path has the same copies.
        assert_eq!(files[&602], copies);
        // Without a name, still one candidate: the link text must name it.
        assert_eq!(files[&601], FileTarget::Ambiguous(vec![first.clone()]));
        // Each note gets the copy of its own module, else the first.
        let mut map = LinkMap::default();
        map.files.insert(600, copies);
        let at = |note: &str| map.resolve(&CanvasRef::File(600), "", Path::new(note));
        assert_eq!(
            at("ramo/modulos/02-semana/01-fase-1.md"),
            links::Resolution::Local(second)
        );
        assert_eq!(at("ramo/tareas/tarea.md"), links::Resolution::Local(first));
        // Different bytes with the recorded name: two files, ambiguous.
        c.digests
            .insert("modulos/02-semana/adjuntos/guia.pdf".into(), "other".into());
        let files: BTreeMap<u64, FileTarget> =
            course_files(&c, &BTreeMap::new()).into_iter().collect();
        assert!(matches!(&files[&600], FileTarget::Ambiguous(v) if v.len() == 2));
        assert_eq!(
            files[&602],
            FileTarget::Path(PathBuf::from("ramo/modulos/02-semana/adjuntos/guia.pdf"))
        );
    }

    fn md5_hex(bytes: &[u8]) -> String {
        use md5::{Digest, Md5};
        hex::encode(Md5::digest(bytes))
    }

    fn etagged(size: usize, etag: &str) -> ItemState {
        ItemState {
            etag: Some(etag.into()),
            size: Some(size as u64),
            ..ItemState::default()
        }
    }

    /// [`course`] plus `files` written under a temp dir, `entries` in its
    /// state, and the etags [`read_sources`] would hash for it.
    fn on_disk(
        files: &[(&str, &[u8])],
        entries: Vec<(&str, ItemState)>,
    ) -> (tempfile::TempDir, CourseSource) {
        let tmp = tempfile::tempdir().expect("temp dir");
        let mut c = course();
        for (rel, bytes) in files {
            let path = tmp.path().join(rel);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(&path, bytes).expect("write");
            c.files.insert((*rel).into(), bytes.len() as u64);
        }
        for (key, item) in entries {
            c.state.items.insert(key.into(), item);
        }
        c.etags = attachment_etags(tmp.path(), &c);
        (tmp, c)
    }

    fn resolve(c: &CourseSource, id: u64, label: &str) -> links::Resolution {
        let mut map = LinkMap::default();
        for (fid, target) in course_files(c, &BTreeMap::new()) {
            map.files.insert(fid, target);
        }
        map.resolve(
            &CanvasRef::File(id),
            label,
            Path::new("ramo/tareas/tarea.md"),
        )
    }

    #[test]
    fn etags_are_normalized_and_part_sizes_try_5_mib_first() {
        let md5 = "47E6DD4FD0037C28A7C3E84018E7ACF7";
        let lower = md5.to_ascii_lowercase();
        assert_eq!(normalize_etag(md5), Some((lower.clone(), None)));
        assert_eq!(
            normalize_etag(&format!("\"{md5}-2\"")),
            Some((format!("{lower}-2"), Some(2)))
        );
        assert_eq!(normalize_etag(&format!("W/\"{md5}\"")), None);
        assert_eq!(normalize_etag("abc"), None);
        assert_eq!(normalize_etag(&format!("{md5}-0")), None);
        // 6025972 bytes in 2 parts: 3, 4 or 5 MiB each.
        assert_eq!(part_sizes(6_025_972, 2), vec![5 * MIB, 3 * MIB, 4 * MIB]);
        assert_eq!(part_sizes(6_025_972, 1), vec![6 * MIB]);
        assert!(part_sizes(6_025_972, 9).is_empty());
    }

    #[test]
    fn an_md5_etag_makes_a_link_local_whatever_its_text() {
        let (a, b) = (b"contenido de la guia".as_slice(), b"otra cosa".as_slice());
        let quoted = format!("\"{}\"", md5_hex(b).to_ascii_uppercase());
        let (_tmp, c) = on_disk(
            &[
                ("modulos/01-intro/adjuntos/guia.pdf", a),
                ("anuncios/adjuntos/notas.txt", b),
            ],
            vec![
                ("file:700", etagged(a.len(), &md5_hex(a))),
                ("file:701", etagged(b.len(), &quoted)),
            ],
        );
        assert_eq!(
            resolve(&c, 700, "Material de clase"),
            links::Resolution::Local(PathBuf::from("ramo/modulos/01-intro/adjuntos/guia.pdf"))
        );
        assert_eq!(
            resolve(&c, 701, "aquí"),
            links::Resolution::Local(PathBuf::from("ramo/anuncios/adjuntos/notas.txt"))
        );
    }

    #[test]
    fn a_multipart_etag_is_checked_in_5_mib_parts() {
        let bytes: Vec<u8> = (0..6 * MIB as usize + 12_345)
            .map(|i| (i % 251) as u8)
            .collect();
        let parts: Vec<u8> = bytes
            .chunks(5 * MIB as usize)
            .flat_map(|part| {
                use md5::{Digest, Md5};
                Md5::digest(part).to_vec()
            })
            .collect();
        let etag = format!("\"{}-2\"", md5_hex(&parts));
        // The plain MD5 with `-2` is not the multipart ETag.
        let wrong = format!("{}-2", md5_hex(&bytes));
        let (_tmp, c) = on_disk(
            &[("modulos/02-semana/adjuntos/apunte.pdf", &bytes)],
            vec![
                ("file:710", etagged(bytes.len(), &etag)),
                ("file:711", etagged(bytes.len(), &wrong)),
            ],
        );
        assert_eq!(
            resolve(&c, 710, "Material"),
            links::Resolution::Local(PathBuf::from("ramo/modulos/02-semana/adjuntos/apunte.pdf"))
        );
        assert_eq!(resolve(&c, 711, "Material"), links::Resolution::Ambiguous);
    }

    #[test]
    fn an_etag_that_matches_nothing_leaves_the_size_rule() {
        let a = b"contenido".as_slice();
        let (_tmp, c) = on_disk(
            &[("modulos/01-intro/adjuntos/guia.pdf", a)],
            vec![("file:720", etagged(a.len(), &md5_hex(b"otro contenido")))],
        );
        assert_eq!(resolve(&c, 720, "Material"), links::Resolution::Ambiguous);
        assert_eq!(
            resolve(&c, 720, "guia.pdf"),
            links::Resolution::Local(PathBuf::from("ramo/modulos/01-intro/adjuntos/guia.pdf"))
        );
    }

    #[test]
    fn of_two_files_of_one_size_the_etag_picks_its_own() {
        let (a, b) = (b"aaaa".as_slice(), b"bbbb".as_slice());
        let (_tmp, c) = on_disk(
            &[
                ("modulos/01-intro/adjuntos/uno.pdf", a),
                ("modulos/02-semana/adjuntos/dos.pdf", b),
            ],
            vec![("file:730", etagged(b.len(), &md5_hex(b)))],
        );
        let files: BTreeMap<u64, FileTarget> =
            course_files(&c, &BTreeMap::new()).into_iter().collect();
        assert_eq!(
            files[&730],
            FileTarget::Path(PathBuf::from("ramo/modulos/02-semana/adjuntos/dos.pdf"))
        );
    }

    #[test]
    fn identical_copies_matched_by_etag_are_copies() {
        let a = b"la misma guia".as_slice();
        let (_tmp, c) = on_disk(
            &[
                ("modulos/01-intro/adjuntos/guia.pdf", a),
                ("modulos/02-semana/adjuntos/guia.pdf", a),
            ],
            vec![("file:740", etagged(a.len(), &md5_hex(a)))],
        );
        let files: BTreeMap<u64, FileTarget> =
            course_files(&c, &BTreeMap::new()).into_iter().collect();
        assert_eq!(
            files[&740],
            FileTarget::Copies(vec![
                PathBuf::from("ramo/modulos/01-intro/adjuntos/guia.pdf"),
                PathBuf::from("ramo/modulos/02-semana/adjuntos/guia.pdf"),
            ])
        );
    }

    #[test]
    fn a_failed_refresh_keeps_the_proof_and_a_sizeless_entry_hashes_nothing() {
        let a = b"contenido".as_slice();
        let failed = ItemState {
            last_error: Some("refresh failed".into()),
            ..etagged(a.len(), &md5_hex(a))
        };
        let sizeless = ItemState {
            size: None,
            ..etagged(a.len(), &md5_hex(a))
        };
        let (_tmp, c) = on_disk(
            &[("modulos/01-intro/adjuntos/guia.pdf", a)],
            vec![("file:750", failed), ("file:751", sizeless)],
        );
        assert_eq!(
            resolve(&c, 750, "Material"),
            links::Resolution::Local(PathBuf::from("ramo/modulos/01-intro/adjuntos/guia.pdf"))
        );
        assert_eq!(resolve(&c, 751, "Material"), links::Resolution::Remote);
    }

    #[test]
    fn what_an_etag_proves_is_written_to_state() {
        let a = b"contenido de la guia".as_slice();
        let (_tmp, c) = on_disk(
            &[("modulos/01-intro/adjuntos/guia.pdf", a)],
            vec![("file:760", etagged(a.len(), &md5_hex(a)))],
        );
        let sources = Sources {
            courses: vec![c],
            renames: BTreeMap::new(),
        };
        let learned = proven(&sources);
        let updates = state_updates(&sources, &learned).expect("serialize");
        let [(rel, text)] = updates.as_slice() else {
            panic!("one state.json: {updates:?}");
        };
        assert_eq!(rel, &PathBuf::from("ramo/.u_crawler/state.json"));
        let state: State = serde_json::from_str(text).expect("state");
        let item = &state.items["file:760"];
        assert_eq!(
            item.path.as_deref(),
            Some("modulos/01-intro/adjuntos/guia.pdf")
        );
        assert_eq!(item.file_name.as_deref(), Some("guia.pdf"));
        // Recorded, it is found by its path: nothing left to prove.
        let mut sources = sources;
        sources.courses[0].state = state;
        assert!(proven(&sources).paths.is_empty());
    }

    #[test]
    fn canvas_names_identical_copies_as_one_file() {
        let mut c = course();
        for (rel, digest) in [
            ("modulos/01-intro/adjuntos/banner-teoria.png", "same"),
            ("modulos/02-semana/adjuntos/banner-teoria.png", "same"),
            ("modulos/01-intro/adjuntos/pauta.pdf", "one"),
            ("modulos/02-semana/adjuntos/pauta.pdf", "two"),
        ] {
            c.files.insert(rel.into(), 900);
            c.digests.insert(rel.into(), digest.into());
        }
        let attachments: Vec<(&str, u64)> = c.attachments().collect();
        let file = |id, name: &str| crate::canvas::FileObj {
            id,
            display_name: Some(name.to_string()),
            filename: None,
            size: Some(900),
            updated_at: None,
            url: None,
            download_url: None,
        };
        let banner = file_by_name(&attachments, &c.digests, &file(70, "Banner Teoria.png"));
        assert_eq!(
            banner.as_deref(),
            Some("modulos/01-intro/adjuntos/banner-teoria.png")
        );
        // Same name and size, other bytes: two files, neither taken.
        assert_eq!(
            file_by_name(&attachments, &c.digests, &file(71, "Pauta.pdf")),
            None
        );
        // What Canvas taught maps every copy, and each module links its own.
        let sources = Sources {
            courses: vec![c],
            renames: BTreeMap::new(),
        };
        let mut map = build(&sources);
        let mut learned = Learned::default();
        learned.paths.insert(
            "ramo".into(),
            BTreeMap::from([("file:70".to_string(), banner.unwrap_or_default())]),
        );
        learned.apply(&sources, &mut map);
        assert_eq!(
            map.resolve(
                &CanvasRef::File(70),
                "",
                Path::new("ramo/modulos/02-semana/02-fase-2.md")
            ),
            links::Resolution::Local(PathBuf::from(
                "ramo/modulos/02-semana/adjuntos/banner-teoria.png"
            ))
        );
    }

    #[test]
    fn pages_from_frontmatter_state_path_and_slug() {
        let mut c = course();
        c.state
            .items
            .insert("page:intro".into(), entry(None, None, None));
        c.state
            .items
            .insert("page:fase-1".into(), entry(None, None, None));
        // The note named fase-2 says it is fase-2: no other slug gets it.
        c.state.items.insert(
            "page:otra".into(),
            entry(None, None, Some("modulos/02-semana/02-fase-2.md")),
        );
        c.state
            .items
            .insert("page:no-descargada".into(), entry(None, None, None));
        let map = build(&Sources {
            courses: vec![c],
            renames: BTreeMap::new(),
        });
        let page = |slug: &str| map.pages.get(&(9, slug.to_string())).cloned();
        assert_eq!(
            page("intro"),
            Some(PathBuf::from("ramo/modulos/01-intro/01-intro.md"))
        );
        assert_eq!(
            page("fase-1"),
            Some(PathBuf::from("ramo/modulos/02-semana/01-fase-1.md"))
        );
        assert_eq!(
            page("fase-2"),
            Some(PathBuf::from("ramo/modulos/02-semana/02-fase-2.md"))
        );
        assert_eq!(
            page("otra"),
            Some(PathBuf::from("ramo/modulos/02-semana/02-fase-2.md"))
        );
        assert_eq!(page("no-descargada"), None);
        assert_eq!(map.assignments[&77], PathBuf::from("ramo/tareas/tarea.md"));
        assert_eq!(
            map.announcements[&501],
            PathBuf::from("ramo/anuncios/2026-06-17_hola.md")
        );
        assert_eq!(map.courses[&9], PathBuf::from("ramo"));
    }

    #[test]
    fn a_page_slug_two_notes_answer_to_stays_remote() {
        let mut c = course();
        c.files.insert("modulos/03-otra/01-fase-1.md".into(), 10);
        c.notes.insert(
            "modulos/03-otra/01-fase-1.md".into(),
            note(DocKind::Page, None),
        );
        c.state
            .items
            .insert("page:fase-1".into(), entry(None, None, None));
        let map = build(&Sources {
            courses: vec![c],
            renames: BTreeMap::new(),
        });
        assert!(!map.pages.contains_key(&(9, "fase-1".to_string())));
    }

    #[test]
    fn a_course_without_canvas_id_maps_no_pages() {
        let mut c = course();
        c.canvas_id = None;
        c.state
            .items
            .insert("page:intro".into(), entry(None, None, None));
        let map = build(&Sources {
            courses: vec![c],
            renames: BTreeMap::new(),
        });
        assert!(!map.pages.contains_key(&(9, "intro".to_string())));
        assert!(map.courses.is_empty());
    }

    #[test]
    fn learned_paths_win_over_the_disk() {
        let c = course();
        let sources = Sources {
            courses: vec![c],
            renames: BTreeMap::new(),
        };
        let mut map = build(&sources);
        let mut learned = Learned::default();
        learned.paths.insert(
            "ramo".into(),
            BTreeMap::from([
                ("module-item:7".to_string(), "tareas/tarea.md".to_string()),
                (
                    "file:1".to_string(),
                    "modulos/01-intro/adjuntos/guia-1.pdf".to_string(),
                ),
            ]),
        );
        learned.apply(&sources, &mut map);
        assert_eq!(map.module_items[&7], PathBuf::from("ramo/tareas/tarea.md"));
        assert_eq!(
            map.files[&1],
            FileTarget::Path(PathBuf::from("ramo/modulos/01-intro/adjuntos/guia-1.pdf"))
        );
    }

    #[test]
    fn canvas_names_a_page_note_by_module_then_alone() {
        let c = course();
        assert_eq!(
            page_note(&c, "02-semana", "01-fase-1.md").as_deref(),
            Some("modulos/02-semana/01-fase-1.md")
        );
        // Numbered offline in another order: the one note with that name.
        assert_eq!(
            page_note(&c, "07-semana", "01-fase-1.md").as_deref(),
            Some("modulos/02-semana/01-fase-1.md")
        );
        assert_eq!(page_note(&c, "02-semana", "09-nada.md"), None);
    }

    #[test]
    fn canvas_names_a_file_by_name_and_size() {
        let c = course();
        let attachments: Vec<(&str, u64)> = c.attachments().collect();
        let file = |id, name: &str, size| crate::canvas::FileObj {
            id,
            display_name: Some(name.to_string()),
            filename: None,
            size,
            updated_at: None,
            url: None,
            download_url: None,
        };
        let by_name = |f| file_by_name(&attachments, &c.digests, &f);
        assert_eq!(
            by_name(file(1, "Guía 2.PDF", Some(30))).as_deref(),
            Some("modulos/02-semana/adjuntos/guia-2.pdf")
        );
        assert_eq!(by_name(file(1, "Guía 2.PDF", Some(31))), None);
        assert_eq!(
            by_name(file(4242, "CodeR_S3.zip", None)).as_deref(),
            Some("anuncios/adjuntos/coder-s3.zip")
        );
    }

    #[test]
    fn state_updates_add_paths_and_module_items_only() {
        let mut c = course();
        c.state
            .items
            .insert("page:fase-1".into(), entry(None, None, None));
        let sources = Sources {
            courses: vec![c],
            renames: BTreeMap::new(),
        };
        let mut learned = Learned::default();
        learned.paths.insert(
            "ramo".into(),
            BTreeMap::from([
                (
                    "page:fase-1".to_string(),
                    "modulos/02-semana/01-fase-1.md".to_string(),
                ),
                ("module-item:7".to_string(), "tareas/tarea.md".to_string()),
                ("assignment:77".to_string(), "tareas/tarea.md".to_string()),
            ]),
        );
        let updates = state_updates(&sources, &learned).unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].0, PathBuf::from("ramo/.u_crawler/state.json"));
        let state: State = serde_json::from_str(&updates[0].1).unwrap();
        assert_eq!(
            state.items["page:fase-1"].path.as_deref(),
            Some("modulos/02-semana/01-fase-1.md")
        );
        assert_eq!(
            state.items["module-item:7"].path.as_deref(),
            Some("tareas/tarea.md")
        );
        // No entry to add a path to: Canvas's word is not a sync record.
        assert!(!state.items.contains_key("assignment:77"));
        // Nothing new: nothing to write.
        let mut done = sources;
        done.courses[0].state = state;
        assert!(state_updates(&done, &learned).unwrap().is_empty());
    }

    #[test]
    fn v1_content_hashes_name_pages_and_assignments() {
        let mut c = course();
        let hash = |s: &str| sha1_hex(s.as_bytes());
        if let Some(note) = c.notes.get_mut("tareas/tarea.md") {
            note.frontmatter = None;
            note.hashes.insert(hash("v1 tarea"));
        }
        if let Some(note) = c.notes.get_mut("modulos/01-intro/01-intro.md") {
            note.hashes.insert(hash("v1 intro"));
        }
        let hashed = |h: &str| ItemState {
            content_hash: Some(hash(h)),
            ..ItemState::default()
        };
        c.state
            .items
            .insert("assignment:30".into(), hashed("v1 tarea"));
        // A slug that names no file, found by its hash.
        c.state
            .items
            .insert("page:programa".into(), hashed("v1 intro"));
        c.state
            .items
            .insert("assignment:31".into(), hashed("otra cosa"));
        let map = build(&Sources {
            courses: vec![c],
            renames: BTreeMap::new(),
        });
        assert_eq!(map.assignments[&30], PathBuf::from("ramo/tareas/tarea.md"));
        assert!(!map.assignments.contains_key(&31));
        assert_eq!(
            map.pages[&(9, "programa".to_string())],
            PathBuf::from("ramo/modulos/01-intro/01-intro.md")
        );
    }
}
