//! Retroactive migration of a layout v1 library to layout v2.
//!
//! See `docs/specs/layout-v2.md` ("Reglas del migrador"). The pieces:
//!
//! - [`scan`] walks a library (read-only) into an [`Inventory`]: every file
//!   with its size, the Canvas ids recovered from `announcements/index.json`
//!   (else from the course's own links, [`course_id_from_links`]),
//!   the v2 course directories already present (`curso.json`) and the course
//!   directories under `calendar.caldir_root`.
//! - [`detect::detect`] tells a v1 library from a v2 one, from the inventory alone.
//! - [`plan::plan`] turns the inventory, plus optional Canvas metadata, into a
//!   [`plan::Plan`]: moves, markdown edits, destination collisions and what
//!   it could not place. It is pure and deterministic.
//! - [`rewrite`] copies a generated markdown note under `.u_crawler/backup/`
//!   and then refreshes its YAML frontmatter and Canvas file links.
//!
//! - [`apply::apply`] carries a plan out: a write-ahead journal under
//!   `.u_crawler/migrations/`, `rename(2)` without replacement, BLAKE3 for
//!   collisions; [`apply::undo`] reverses a journal.
//! - [`ensure`](crate::migrate::ensure) chains them: resume, plan, apply, rewrite, index files and
//!   `.u_crawler/library.json`. It is `u_crawler migrate`, and
//!   [`ensure_v2`](crate::migrate::ensure::ensure_v2) runs it at the start of every writer command.
//!
//! [`scan`], [`detect`] and [`plan`] never open a file for writing. The
//! writers are [`apply`], which renames and creates directories and
//! `curso.json` (nothing deleted, copied or overwritten), [`rewrite`],
//! which only replaces a note [`rewrite::generated_kind`] accepts, after
//! the backup exists, and [`ensure`](crate::migrate::ensure), which adds the root index files and
//! `library.json`.

pub mod apply;
pub mod detect;
pub mod ensure;
pub mod plan;
pub mod relink;
pub mod rewrite;

pub use ensure::{ensure_v2, migrate, undo, MigrateError, Outcome, Target};

use crate::layout::{self, CourseMeta, ExistingCourseDir};
use crate::state::State;
use regex::Regex;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;
use std::sync::LazyLock;

/// v1 directory holding one `Zoom/<course id>/` folder per course.
pub const V1_ZOOM_DIR: &str = "Zoom";
/// v1 course subdirectory holding one `<module id>_<Name>/` per module.
pub const V1_MODULES_DIR: &str = "Modules";
/// v1 module subdirectory holding downloaded files.
pub const V1_ATTACHMENTS_DIR: &str = "Attachments";
/// v1 course subdirectory holding announcements.
pub const V1_ANNOUNCEMENTS_DIR: &str = "announcements";
/// v1 announcements subdirectory holding announcement attachments.
pub const V1_MEDIA_DIR: &str = "media";
/// v1 per-course state file, at the top of the course directory.
pub const V1_STATE_FILE: &str = "state.json";
/// Announcement index file name (same in v1 and v2).
pub const INDEX_JSON: &str = "index.json";
/// Per-course metadata file of a v2 library.
pub const CURSO_JSON: &str = "curso.json";
/// Library marker file of a v2 library, under [`layout::META_DIR`].
pub const LIBRARY_JSON: &str = "library.json";

/// Directory never entered, listed or moved (a git repository's internals).
const GIT_DIR: &str = ".git";

/// What the planner needs to know about a library, read once from disk.
///
/// Every path is relative to its root and `/`-separated.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inventory {
    /// Every regular file under the library root, with its size in bytes.
    /// Nothing under a `.git` directory is listed; symlinks and other
    /// non-regular entries are not listed either, so they stay where they are.
    pub files: BTreeMap<String, u64>,
    /// Canvas course id per top-level directory, from the `courses/<id>` in
    /// its `announcements/index.json`, else, for a v1 course, from the links
    /// in its notes that its `state.json` backs ([`course_id_from_links`]).
    pub course_ids: BTreeMap<String, u64>,
    /// v2 course directories already in the library, from their `curso.json`.
    pub existing: Vec<ExistingCourseDir>,
    /// Directory names directly under `calendar.caldir_root`.
    pub caldir_dirs: BTreeSet<String>,
}

impl Inventory {
    /// The top-level directories that look like a v1 course: a `state.json`,
    /// a `Modules/` or an `announcements/` directly inside. Hidden entries
    /// and `Zoom/` are never courses.
    pub fn v1_course_dirs(&self) -> BTreeSet<&str> {
        self.files
            .keys()
            .filter_map(|path| {
                let (top, rest) = path.split_once('/')?;
                if top.starts_with('.') || top == V1_ZOOM_DIR {
                    return None;
                }
                let first = rest.split('/').next().unwrap_or(rest);
                let v1 = rest == V1_STATE_FILE
                    || (rest.contains('/')
                        && (first == V1_MODULES_DIR || first == V1_ANNOUNCEMENTS_DIR));
                v1.then_some(top)
            })
            .collect()
    }

    /// The names directly under the library root, files and directories alike.
    pub fn root_names(&self) -> BTreeSet<&str> {
        self.files
            .keys()
            .map(|path| path.split_once('/').map_or(path.as_str(), |(top, _)| top))
            .collect()
    }
}

/// Read a library into an [`Inventory`]. Read-only: nothing is created,
/// written, moved or deleted.
///
/// `caldir_root` is `calendar.caldir_root`; `None`, or a directory that does
/// not exist, leaves [`Inventory::caldir_dirs`] empty.
///
/// # Errors
///
/// Fails on any I/O error and on a file or directory name that is not UTF-8:
/// the planner cannot name a path it cannot represent, so it refuses to
/// plan rather than skip part of the library.
pub fn scan(root: &Path, caldir_root: Option<&Path>) -> io::Result<Inventory> {
    let mut inv = Inventory::default();
    walk(root, "", &mut inv.files)?;

    let tops: Vec<String> = inv
        .files
        .keys()
        .filter_map(|p| p.split_once('/').map(|(top, _)| top.to_string()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    for top in tops {
        let index = format!("{top}/{V1_ANNOUNCEMENTS_DIR}/{INDEX_JSON}");
        if inv.files.contains_key(&index) {
            let text =
                fs::read_to_string(root.join(&top).join(V1_ANNOUNCEMENTS_DIR).join(INDEX_JSON))?;
            match course_id_from_index(&text) {
                Some(id) => {
                    inv.course_ids.insert(top.clone(), id);
                }
                None => tracing::warn!(path = %index, "index.json names no single Canvas course"),
            }
        }
        if inv.files.contains_key(&format!("{top}/{CURSO_JSON}")) {
            let text = fs::read_to_string(root.join(&top).join(CURSO_JSON))?;
            match course_from_curso_json(&text) {
                Some(course) => inv.existing.push(ExistingCourseDir { dir: top, course }),
                None => tracing::warn!(path = %top, "unreadable curso.json"),
            }
        }
    }
    let unidentified: Vec<String> = inv
        .v1_course_dirs()
        .into_iter()
        .filter(|dir| !inv.course_ids.contains_key(*dir))
        .map(str::to_string)
        .collect();
    for dir in unidentified {
        if let Some(id) = course_id_from_notes(root, &inv, &dir)? {
            tracing::info!(path = %dir, course_id = id, "Canvas id read from the course's own links");
            inv.course_ids.insert(dir, id);
        }
    }

    if let Some(caldir) = caldir_root {
        match fs::read_dir(caldir) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry?;
                    if entry.file_type()?.is_dir() {
                        inv.caldir_dirs.insert(utf8_name(&entry)?);
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    tracing::debug!(
        files = inv.files.len(),
        course_ids = inv.course_ids.len(),
        existing = inv.existing.len(),
        caldir_dirs = inv.caldir_dirs.len(),
        "library scanned"
    );
    Ok(inv)
}

/// The Canvas id of the v1 course `dir` that has no `index.json`, from its
/// notes and its `state.json` ([`course_id_from_links`]). `None` without a
/// readable `state.json`.
fn course_id_from_notes(root: &Path, inv: &Inventory, dir: &str) -> io::Result<Option<u64>> {
    let state_rel = format!("{dir}/{V1_STATE_FILE}");
    if !inv.files.contains_key(&state_rel) {
        return Ok(None);
    }
    let state: State = match serde_json::from_slice(&fs::read(root.join(&state_rel))?) {
        Ok(state) => state,
        Err(e) => {
            tracing::warn!(path = %state_rel, error = %e, "unreadable state.json");
            return Ok(None);
        }
    };
    let keys: BTreeSet<String> = state.items.into_keys().collect();
    let prefix = format!("{dir}/");
    let mut notes = Vec::new();
    for rel in inv.files.keys() {
        if rel.starts_with(&prefix) && rel.ends_with(".md") {
            notes.push(String::from_utf8_lossy(&fs::read(root.join(rel))?).into_owned());
        }
    }
    Ok(course_id_from_links(
        &keys,
        notes.iter().map(String::as_str),
    ))
}

/// `courses/<id>/<kind>/<item>` in a note, `<kind>` being a resource
/// `state.json` keys by its Canvas id.
static COURSE_LINK_RE: LazyLock<Regex> = LazyLock::new(|| {
    // A literal pattern, so compiling it cannot fail.
    Regex::new(r#"/courses/(\d+)/(files|assignments|discussion_topics)/([^/?#\s)"'<>\]]+)"#)
        .unwrap()
});

/// The Canvas course id the links in a course's `notes` prove: a link
/// `courses/<n>/<kind>/<item>` counts only when the course's own
/// `state.json` (`state_keys`) recorded that item, so a link to another
/// course's resource that was never fetched proves nothing.
///
/// Assignments and announcements are listed per course and their ids are
/// global, so a single `<n>` among them is the answer. Files are weaker (v1
/// downloads the files a page embeds, a template course's included): they
/// decide only when no such item links, and only when they name a single
/// `<n>` too. Pages never count: `state.json` keys them by slug, and a slug
/// such as `informacion-general` is in every course built from one template.
/// Anything else is `None`.
pub fn course_id_from_links<'a>(
    state_keys: &BTreeSet<String>,
    notes: impl IntoIterator<Item = &'a str>,
) -> Option<u64> {
    let mut listed = BTreeSet::new();
    let mut files = BTreeSet::new();
    for note in notes {
        for cap in COURSE_LINK_RE.captures_iter(note) {
            let Ok(course) = cap[1].parse::<u64>() else {
                continue;
            };
            let (key, set) = match &cap[2] {
                "files" => (format!("file:{}", &cap[3]), &mut files),
                "assignments" => (format!("assignment:{}", &cap[3]), &mut listed),
                _ => (format!("announcement:{}", &cap[3]), &mut listed),
            };
            if state_keys.contains(&key) {
                set.insert(course);
            }
        }
    }
    let single = |set: &BTreeSet<u64>| match set.len() {
        1 => set.first().copied(),
        _ => None,
    };
    if listed.is_empty() {
        single(&files)
    } else {
        single(&listed)
    }
}

/// The Canvas course id an announcements `index.json` points at: the
/// `courses/<id>` in each record's `html_url`. `None` when the file is not
/// a JSON array, no record has such a URL, or two records disagree.
pub fn course_id_from_index(text: &str) -> Option<u64> {
    let records: Vec<serde_json::Value> = serde_json::from_str(text).ok()?;
    let mut ids = records.iter().filter_map(|r| {
        let url = r.get("html_url")?.as_str()?;
        let (_, rest) = url.split_once("/courses/")?;
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits.parse::<u64>().ok()
    });
    let first = ids.next()?;
    ids.all(|id| id == first).then_some(first)
}

/// The course recorded in a v2 `curso.json` (`index::render_curso_json`).
pub fn course_from_curso_json(text: &str) -> Option<CourseMeta> {
    #[derive(Deserialize)]
    struct CursoJson {
        canvas_id: Option<u64>,
        course_code: Option<String>,
        name: String,
        #[serde(default)]
        canvas_name: Option<String>,
    }
    let curso: CursoJson = serde_json::from_str(text).ok()?;
    Some(CourseMeta {
        id: curso.canvas_id,
        // `name` is the readable one; older files have only `name`.
        name: curso.canvas_name.unwrap_or(curso.name),
        code: curso.course_code,
    })
}

/// `<root>/.u_crawler/library.json`, relative to the library root.
pub fn library_json_rel() -> String {
    format!("{}/{LIBRARY_JSON}", layout::META_DIR)
}

fn walk(dir: &Path, prefix: &str, out: &mut BTreeMap<String, u64>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = utf8_name(&entry)?;
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        // `DirEntry::file_type` does not follow symlinks.
        let ft = entry.file_type()?;
        if ft.is_dir() {
            if name != GIT_DIR {
                walk(&entry.path(), &rel, out)?;
            }
        } else if ft.is_file() {
            out.insert(rel, entry.metadata()?.len());
        } else {
            tracing::debug!(path = %rel, "not a regular file; left out of the inventory");
        }
    }
    Ok(())
}

fn utf8_name(entry: &fs::DirEntry) -> io::Result<String> {
    entry.file_name().into_string().map_err(|name| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("file name is not UTF-8: {}", name.to_string_lossy()),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn course_id_from_index_reads_the_course_of_the_html_url() {
        let text = r#"[{"id":1,"html_url":"https://x.edu/courses/9001/discussion_topics/1"},
                       {"id":2,"html_url":"https://x.edu/courses/9001/discussion_topics/2"}]"#;
        assert_eq!(course_id_from_index(text), Some(9001));
    }

    #[test]
    fn course_id_from_index_refuses_disagreement_and_garbage() {
        let two = r#"[{"html_url":"https://x.edu/courses/1/d/1"},{"html_url":"https://x.edu/courses/2/d/1"}]"#;
        assert_eq!(course_id_from_index(two), None);
        assert_eq!(course_id_from_index("[]"), None);
        assert_eq!(course_id_from_index(r#"[{"html_url":null}]"#), None);
        assert_eq!(course_id_from_index("{"), None);
    }

    fn keys(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|k| k.to_string()).collect()
    }

    #[test]
    fn course_id_from_links_needs_the_link_backed_by_the_state() {
        let state = keys(&["file:11", "file:12"]);
        let note = "![a](https://x.edu/courses/77/files/11/preview?verifier=v) \
                    [b](/courses/77/files/12?wrap=1) [c](https://x.edu/courses/5/files/99)";
        assert_eq!(course_id_from_links(&state, [note]), Some(77));
        // A link to a file the course never downloaded proves nothing.
        assert_eq!(
            course_id_from_links(&state, ["[c](https://x.edu/courses/5/files/99)"]),
            None
        );
        assert_eq!(course_id_from_links(&state, [""]), None);
    }

    #[test]
    fn course_id_from_links_refuses_two_courses_of_one_kind() {
        // v1 also downloads a template course's files a page embeds.
        let state = keys(&["file:11", "file:21"]);
        let notes = [
            "/courses/77/files/11/preview",
            "/courses/93/files/21/download",
        ];
        assert_eq!(course_id_from_links(&state, notes), None);
    }

    #[test]
    fn course_id_from_links_trusts_items_canvas_lists_per_course_over_files() {
        // The real shape: most downloaded files are a template course's, but
        // assignments and announcements are listed per course.
        let state = keys(&["file:11", "file:21", "file:22", "announcement:7"]);
        let notes = [
            "/courses/93/files/21/preview /courses/93/files/22/preview",
            "/courses/77/files/11/preview",
            "https://x.edu/courses/77/discussion_topics/7",
        ];
        assert_eq!(course_id_from_links(&state, notes), Some(77));
        let split = keys(&["assignment:1", "announcement:2"]);
        let notes = [
            "/courses/77/assignments/1",
            "/courses/78/discussion_topics/2",
        ];
        assert_eq!(course_id_from_links(&split, notes), None);
    }

    #[test]
    fn course_id_from_links_never_trusts_a_page_slug() {
        // Another course built from the same template has the same slug.
        let state = keys(&["page:informacion-general", "file:11"]);
        let notes = [
            "/courses/7777/pages/informacion-general",
            "/courses/77/files/11/preview",
        ];
        assert_eq!(course_id_from_links(&state, notes), Some(77));
        assert_eq!(course_id_from_links(&state, [notes[0]]), None);
    }

    #[test]
    fn scan_reads_the_id_of_a_course_without_index_json_from_its_notes() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join("lib");
        let course = root.join("202525_1149_MECANICA_CFSA3220_202525_1149_EL_ON");
        fs::create_dir_all(course.join("Modules/5_Unidad"))?;
        fs::write(
            course.join("state.json"),
            br#"{"items":{"file:17317335":{"size":3},"page:sesion":{}}}"#,
        )?;
        fs::write(
            course.join("Modules/5_Unidad/01-Sesion.md"),
            b"<img src=\"https://canvas.x.cl/courses/188678/files/17317335/preview?verifier=a\">\n\
              [otro](https://canvas.x.cl/courses/92458/files/1/download)\n",
        )?;
        // A v2 course directory is not a v1 course: its id comes from curso.json.
        let v2 = root.join("v2-curso/.u_crawler");
        fs::create_dir_all(&v2)?;
        fs::write(v2.join("state.json"), br#"{"items":{"file:1":{}}}"#)?;
        fs::write(
            root.join("v2-curso/n.md"),
            b"/courses/92458/files/1/download",
        )?;

        let inv = scan(&root, None)?;
        let ids: Vec<(&str, u64)> = inv
            .course_ids
            .iter()
            .map(|(d, id)| (d.as_str(), *id))
            .collect();
        assert_eq!(
            ids,
            [("202525_1149_MECANICA_CFSA3220_202525_1149_EL_ON", 188678)]
        );
        Ok(())
    }

    #[test]
    fn course_from_curso_json_accepts_an_offline_null_id() {
        let text = r#"{"canvas_id":null,"course_code":"X_1","name":"N","term":null,"section":null,"zoom_ids":[]}"#;
        let meta = course_from_curso_json(text);
        assert_eq!(
            meta,
            Some(CourseMeta {
                id: None,
                name: "N".into(),
                code: Some("X_1".into())
            })
        );
    }

    #[test]
    fn v1_course_dirs_needs_a_v1_marker_inside() {
        let mut inv = Inventory::default();
        for p in [
            "A_X/state.json",
            "B_X/Modules/1_M/01-P.md",
            "C_X/announcements/a.md",
            "Notas/state.json/raro.md",
            "atdf105-x/.u_crawler/state.json",
            ".obsidian/state.json",
            "Zoom/1/a.mp4",
            "suelto.md",
        ] {
            inv.files.insert(p.into(), 1);
        }
        let dirs: Vec<&str> = inv.v1_course_dirs().into_iter().collect();
        assert_eq!(dirs, ["A_X", "B_X", "C_X"]);
    }

    #[test]
    fn scan_skips_git_and_reads_ids() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join("lib");
        let course = root.join("N_C");
        fs::create_dir_all(course.join("announcements"))?;
        fs::create_dir_all(root.join(".git/objects"))?;
        fs::write(root.join(".git/objects/x"), b"x")?;
        fs::write(course.join("state.json"), b"{}")?;
        fs::write(
            course.join("announcements/index.json"),
            br#"[{"html_url":"https://x.edu/courses/77/discussion_topics/1"}]"#,
        )?;
        let caldir = dir.path().join("cal");
        fs::create_dir_all(caldir.join("N_C/deadlines"))?;
        fs::write(caldir.join("loose.ics"), b"x")?;

        let inv = scan(&root, Some(&caldir))?;
        let files: Vec<&str> = inv.files.keys().map(String::as_str).collect();
        assert_eq!(files, ["N_C/announcements/index.json", "N_C/state.json"]);
        assert_eq!(inv.files["N_C/state.json"], 2);
        assert_eq!(inv.course_ids.get("N_C"), Some(&77));
        assert_eq!(inv.caldir_dirs.iter().collect::<Vec<_>>(), ["N_C"]);

        let missing = scan(&root, Some(&dir.path().join("nope")))?;
        assert!(missing.caldir_dirs.is_empty());
        Ok(())
    }
}
