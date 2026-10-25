//! Retroactive migration of a layout v1 library to layout v2.
//!
//! See `docs/specs/layout-v2.md` ("Reglas del migrador"). The pieces:
//!
//! - [`scan`] walks a library (read-only) into an [`Inventory`]: every file
//!   with its size, the Canvas ids recovered from `announcements/index.json`,
//!   the v2 course directories already present (`curso.json`) and the course
//!   directories under `calendar.caldir_root`.
//! - [`detect::detect`] tells a v1 library from a v2 one, from the inventory alone.
//! - [`plan::plan`] turns the inventory, plus optional Canvas metadata, into a
//!   [`plan::Plan`]: moves, markdown edits, destination collisions and what
//!   it could not place. It is pure and deterministic.
//!
//! Nothing here deletes, overwrites or even opens a file for writing. The
//! executor that applies a plan (journal, `rename(2)`, BLAKE3 for collisions)
//! is a separate step.

pub mod detect;
pub mod plan;

use crate::layout::{self, CourseMeta, ExistingCourseDir};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;

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
    /// its `announcements/index.json`.
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
    }
    let curso: CursoJson = serde_json::from_str(text).ok()?;
    Some(CourseMeta {
        id: curso.canvas_id,
        name: curso.name,
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
