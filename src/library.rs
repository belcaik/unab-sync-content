//! Which directory each course owns in a layout v2 library.
//!
//! The writers (`sync`, `announcements`, `calendar`, `zoom flow`) all need
//! the same answer, and it must stay the same across runs: a course keeps
//! the directory its `curso.json` claims, and only a newcomer is named by
//! [`CourseNaming::course_dirs`]. [`read`] is the one read of the library
//! root; [`course_dirs`] is pure; [`claim`] writes a minimal `curso.json`
//! into a directory the moment a writer creates it, so the name is reserved
//! even if the run stops right after.

use crate::canvas::Course;
use crate::fsutil::atomic_write;
use crate::index::{self, CourseIndex, LibraryIndex, RecordingIndex};
use crate::layout::{self, CourseMeta, CourseNaming, ExistingCourseDir};
use chrono::{NaiveDate, Utc};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

/// Per-course metadata file of a v2 library (`index::render_curso_json`).
pub const CURSO_JSON: &str = "curso.json";
/// Per-course sync state, under [`layout::META_DIR`].
pub const STATE_FILE: &str = "state.json";

/// What a `curso.json` records, as read back from disk.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct CourseRecord {
    /// Canvas course id; `None` after an offline migration.
    #[serde(default)]
    pub canvas_id: Option<u64>,
    /// Canvas `course_code`.
    #[serde(default)]
    pub course_code: Option<String>,
    /// Canvas `name`.
    pub name: String,
    /// Academic term, when recovered.
    #[serde(default)]
    pub term: Option<String>,
    /// Section / NRC, when recovered.
    #[serde(default)]
    pub section: Option<String>,
    /// v1 `Zoom/<id>` folders migrated into the course.
    #[serde(default)]
    pub zoom_ids: Vec<u64>,
}

impl CourseRecord {
    fn meta(&self) -> CourseMeta {
        CourseMeta {
            id: self.canvas_id,
            name: self.name.clone(),
            code: self.course_code.clone(),
        }
    }
}

/// The top level of a library, as the writers need it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Library {
    /// Every directory with a readable `curso.json`, by directory name.
    pub records: BTreeMap<String, CourseRecord>,
    /// Directory names no course may be given: anything at the root that is
    /// neither a v2 course nor an interrupted one (a `.u_crawler/state.json`
    /// without `curso.json`, which its course is allowed to resume).
    pub reserved: Vec<String>,
}

/// Read the top level of the library at `root`. A missing root is an empty
/// library. A name that is not UTF-8 is reserved by being skipped: no slug
/// can collide with it, since slugs are ASCII.
///
/// # Errors
///
/// Any I/O error other than the root not existing.
pub fn read(root: &Path) -> io::Result<Library> {
    let mut lib = Library::default();
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(lib),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let curso = entry.path().join(CURSO_JSON);
        match std::fs::read_to_string(&curso) {
            Ok(text) => match serde_json::from_str::<CourseRecord>(&text) {
                Ok(record) => {
                    lib.records.insert(name, record);
                }
                Err(e) => {
                    tracing::warn!(path = %curso.display(), error = %e, "unreadable curso.json; directory reserved");
                    lib.reserved.push(name);
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if !state_path(&entry.path()).exists() {
                    lib.reserved.push(name);
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(lib)
}

/// The directory name of each course in `courses`, by Canvas id.
///
/// Pass every course the run knows about (the whole active list, not just
/// the selected ones), so two sections that collide are told apart the same
/// way whichever one a `--course-id` run picks. Courses are named in id
/// order, which is what keeps [`layout::disambiguate`] stable.
pub fn course_dirs(
    naming: &CourseNaming,
    lib: &Library,
    courses: &[Course],
) -> BTreeMap<u64, String> {
    let mut existing: Vec<ExistingCourseDir> = lib
        .records
        .iter()
        .map(|(dir, record)| ExistingCourseDir {
            dir: dir.clone(),
            course: record.meta(),
        })
        .collect();
    existing.extend(lib.reserved.iter().map(|dir| ExistingCourseDir {
        dir: dir.clone(),
        course: CourseMeta::default(),
    }));
    let mut sorted: Vec<&Course> = courses.iter().collect();
    sorted.sort_by_key(|c| c.id);
    sorted.dedup_by_key(|c| c.id);
    let metas: Vec<CourseMeta> = sorted.iter().map(|c| CourseMeta::from(*c)).collect();
    let dirs = naming.course_dirs(&existing, &metas);
    sorted.iter().map(|c| c.id).zip(dirs).collect()
}

/// `<course_dir>/.u_crawler/state.json`.
pub fn state_path(course_dir: &Path) -> PathBuf {
    course_dir.join(layout::META_DIR).join(STATE_FILE)
}

/// The [`CourseIndex`] skeleton of a course: its directory, its Canvas
/// identity and its section, merged with what `curso.json` already records
/// (`zoom_ids`, and `term` / `section` when this run has none).
pub fn course_index(
    naming: &CourseNaming,
    dir: &str,
    course: &Course,
    previous: Option<&CourseRecord>,
) -> CourseIndex {
    let meta = CourseMeta::from(course);
    CourseIndex {
        dir: dir.to_string(),
        section: naming
            .section(&meta)
            .or_else(|| previous.and_then(|p| p.section.clone())),
        term: previous.and_then(|p| p.term.clone()),
        zoom_ids: previous.map(|p| p.zoom_ids.clone()).unwrap_or_default(),
        course: meta,
        ..Default::default()
    }
}

/// Write `curso.json` into `course_dir` unless it already has one, so the
/// directory is the course's from the moment it exists.
///
/// # Errors
///
/// Any I/O error writing the file.
pub async fn claim(course_dir: &Path, skeleton: &CourseIndex) -> io::Result<()> {
    let path = course_dir.join(CURSO_JSON);
    if tokio::fs::try_exists(&path).await? {
        return Ok(());
    }
    atomic_write(&path, index::render_curso_json(skeleton).as_bytes()).await
}

/// Write `contents` to `path` unless it already holds exactly that, so a
/// regenerated index that did not change keeps its inode and mtime.
async fn write_if_changed(path: &Path, contents: &str) -> io::Result<()> {
    match tokio::fs::read(path).await {
        Ok(current) if current == contents.as_bytes() => Ok(()),
        Ok(_) => atomic_write(path, contents.as_bytes()).await,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            atomic_write(path, contents.as_bytes()).await
        }
        Err(e) => Err(e),
    }
}

/// [`write_if_changed`] for a file at the library root, which is not ours
/// alone: a vault may already hold an `AGENTS.md` or `INDEX.md` of its own.
/// One whose first line is not the first line of `contents` is left alone,
/// with a warning. Returns whether the file now holds `contents`.
async fn write_root_file(path: &Path, contents: &str) -> io::Result<bool> {
    let header = contents.lines().next().unwrap_or_default();
    match tokio::fs::read_to_string(path).await {
        Ok(current) if current.lines().next() != Some(header) => {
            tracing::warn!(path = %path.display(), "not written by u_crawler; left alone");
            return Ok(false);
        }
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        // Not UTF-8 is not ours either.
        Err(e) if e.kind() == io::ErrorKind::InvalidData => {
            tracing::warn!(path = %path.display(), "not written by u_crawler; left alone");
            return Ok(false);
        }
        Err(e) => return Err(e),
    }
    write_if_changed(path, contents).await?;
    Ok(true)
}

/// The recordings under `<course_dir>/grabaciones/`, read from disk: what
/// the course README lists, whichever run downloaded them.
///
/// # Errors
///
/// Any I/O error other than the directory not existing.
pub async fn list_recordings(course_dir: &Path) -> io::Result<Vec<RecordingIndex>> {
    let dir = course_dir.join(layout::RECORDINGS_DIR);
    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_file() {
            continue;
        }
        if let Some(name) = entry.file_name().to_str() {
            if let Some(recording) = recording_index(name) {
                out.push(recording);
            }
        }
    }
    out.sort_by(|a, b| a.file.cmp(&b.file));
    Ok(out)
}

/// A v2 recording file name read back: `YYYY-MM-DD_<topic>.mp4`, the topic
/// shown with spaces. Anything that is not an `.mp4` is not a recording.
fn recording_index(name: &str) -> Option<RecordingIndex> {
    let stem = name.strip_suffix(".mp4")?;
    let (date, topic) = match stem.split_once('_') {
        Some((d, t)) => match NaiveDate::parse_from_str(d, "%Y-%m-%d") {
            Ok(date) => (Some(date), t),
            Err(_) => (None, stem),
        },
        None => (None, stem),
    };
    Some(RecordingIndex {
        file: name.to_string(),
        date,
        topic: topic.replace('-', " "),
    })
}

/// Write a course's `README.md`, `curso.json` and one `README.md` per module.
///
/// # Errors
///
/// Any I/O error writing them.
pub async fn write_course_files(course_dir: &Path, course: &CourseIndex) -> io::Result<()> {
    write_if_changed(
        &course_dir.join("README.md"),
        &index::render_course_readme(course),
    )
    .await?;
    write_if_changed(
        &course_dir.join(CURSO_JSON),
        &index::render_curso_json(course),
    )
    .await?;
    for module in &course.modules {
        let path = course_dir
            .join(layout::MODULES_DIR)
            .join(&module.dir)
            .join("README.md");
        write_if_changed(&path, &index::render_module_readme(module)).await?;
    }
    Ok(())
}

/// Write the library's `AGENTS.md` and, when `with_index` is set, its
/// `INDEX.md`. The index lists `synced` plus every other course `lib` knows
/// from its `curso.json`; only `synced` brings deadlines, which is why a run
/// that did not see every course passes `with_index = false`. Either file is
/// left alone when one already there does not start like ours.
///
/// # Errors
///
/// Any I/O error writing them.
pub async fn write_library_files(
    root: &Path,
    lib: &Library,
    synced: Vec<CourseIndex>,
    with_index: bool,
) -> io::Result<()> {
    write_root_file(&root.join("AGENTS.md"), &index::render_agents_md()).await?;
    if !with_index {
        return Ok(());
    }
    let mut courses = synced;
    for (dir, record) in &lib.records {
        if courses.iter().any(|c| &c.dir == dir) {
            continue;
        }
        courses.push(CourseIndex {
            dir: dir.clone(),
            course: record.meta(),
            term: record.term.clone(),
            section: record.section.clone(),
            zoom_ids: record.zoom_ids.clone(),
            ..Default::default()
        });
    }
    let library = LibraryIndex { courses };
    write_root_file(
        &root.join("INDEX.md"),
        &index::render_index_md(&library, Utc::now().date_naive()),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;

    fn naming() -> CourseNaming {
        CourseNaming::new(&config::Layout::default()).unwrap()
    }

    fn course(id: u64, name: &str, code: &str) -> Course {
        Course {
            id,
            name: name.into(),
            course_code: Some(code.into()),
        }
    }

    #[test]
    fn a_new_course_gets_its_slug() {
        let dirs = course_dirs(
            &naming(),
            &Library::default(),
            &[course(7, "Minería de Datos", "ATDF105")],
        );
        assert_eq!(dirs[&7], "atdf105-mineria-de-datos");
    }

    #[test]
    fn a_course_keeps_the_directory_its_curso_json_claims() {
        let mut lib = Library::default();
        lib.records.insert(
            "mi-carpeta".into(),
            CourseRecord {
                canvas_id: Some(7),
                name: "Otro nombre".into(),
                ..Default::default()
            },
        );
        let dirs = course_dirs(&naming(), &lib, &[course(7, "Minería", "ATDF105")]);
        assert_eq!(dirs[&7], "mi-carpeta");
    }

    #[test]
    fn a_foreign_directory_is_never_given_to_a_course() {
        let lib = Library {
            reserved: vec!["atdf105-mineria".into()],
            ..Default::default()
        };
        let dirs = course_dirs(&naming(), &lib, &[course(7, "Minería", "ATDF105")]);
        assert_eq!(dirs[&7], "atdf105-mineria-c7");
    }

    #[test]
    fn naming_does_not_depend_on_input_order() {
        let a = course(2, "Electivo", "ATDF106");
        let b = course(1, "Electivo", "ATDF106");
        let lib = Library::default();
        let one = course_dirs(&naming(), &lib, &[a.clone(), b.clone()]);
        let two = course_dirs(&naming(), &lib, &[b, a]);
        assert_eq!(one, two);
        assert_ne!(one[&1], one[&2]);
    }

    #[test]
    fn read_reserves_foreign_dirs_but_not_interrupted_courses() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("ajena")).unwrap();
        std::fs::create_dir_all(root.join("cortada/.u_crawler")).unwrap();
        std::fs::write(root.join("cortada/.u_crawler/state.json"), "{}").unwrap();
        std::fs::create_dir_all(root.join("ramo")).unwrap();
        std::fs::write(
            root.join("ramo/curso.json"),
            r#"{"canvas_id": 3, "course_code": null, "name": "Ramo", "zoom_ids": [9]}"#,
        )
        .unwrap();
        std::fs::write(root.join("INDEX.md"), "").unwrap();

        let lib = read(root).unwrap();
        assert_eq!(lib.reserved, vec!["ajena".to_string()]);
        assert_eq!(lib.records["ramo"].canvas_id, Some(3));
        assert_eq!(lib.records["ramo"].zoom_ids, vec![9]);
    }

    #[test]
    fn read_of_a_missing_root_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(read(&tmp.path().join("nope")).unwrap(), Library::default());
    }

    #[test]
    fn course_index_keeps_what_curso_json_already_records() {
        let previous = CourseRecord {
            canvas_id: Some(7),
            name: "x".into(),
            term: Some("202615".into()),
            zoom_ids: vec![157107],
            ..Default::default()
        };
        let got = course_index(
            &naming(),
            "dir",
            &course(7, "Minería", "ATDF105"),
            Some(&previous),
        );
        assert_eq!(got.zoom_ids, vec![157107]);
        assert_eq!(got.term.as_deref(), Some("202615"));
        assert_eq!(got.course.id, Some(7));
    }

    #[test]
    fn recording_index_reads_a_v2_name_back() {
        let got = recording_index("2026-06-18_sesion-sincronica-1.mp4").unwrap();
        assert_eq!(got.date, NaiveDate::from_ymd_opt(2026, 6, 18));
        assert_eq!(got.topic, "sesion sincronica 1");
        assert!(recording_index("notas.md").is_none());
        assert_eq!(recording_index("zoom-abc.mp4").unwrap().date, None);
    }

    #[tokio::test]
    async fn course_files_are_written_and_left_alone_when_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let mut course = course_index(&naming(), "ramo", &course(1, "Ramo", "R1"), None);
        course.modules.push(index::ModuleIndex::new(1, "Intro"));
        write_course_files(tmp.path(), &course).await.unwrap();
        let readme = tmp.path().join("README.md");
        let module_readme = tmp.path().join("modulos/01-intro/README.md");
        assert!(module_readme.exists());
        let before = std::fs::metadata(&readme).unwrap().modified().unwrap();
        write_course_files(tmp.path(), &course).await.unwrap();
        assert_eq!(
            std::fs::metadata(&readme).unwrap().modified().unwrap(),
            before
        );
        let curso = std::fs::read_to_string(tmp.path().join(CURSO_JSON)).unwrap();
        assert_eq!(
            serde_json::from_str::<CourseRecord>(&curso)
                .unwrap()
                .canvas_id,
            Some(1)
        );
    }

    #[tokio::test]
    async fn a_filtered_run_writes_agents_md_but_not_index_md() {
        let tmp = tempfile::tempdir().unwrap();
        write_library_files(tmp.path(), &Library::default(), Vec::new(), false)
            .await
            .unwrap();
        assert!(tmp.path().join("AGENTS.md").exists());
        assert!(!tmp.path().join("INDEX.md").exists());
    }

    #[tokio::test]
    async fn index_md_lists_courses_this_run_did_not_sync() {
        let tmp = tempfile::tempdir().unwrap();
        let mut lib = Library::default();
        lib.records.insert(
            "archivado".into(),
            CourseRecord {
                canvas_id: Some(5),
                name: "Ramo Archivado".into(),
                ..Default::default()
            },
        );
        write_library_files(tmp.path(), &lib, Vec::new(), true)
            .await
            .unwrap();
        let index = std::fs::read_to_string(tmp.path().join("INDEX.md")).unwrap();
        assert!(
            index.contains("[Ramo Archivado](archivado/README.md)"),
            "{index}"
        );
    }

    #[tokio::test]
    async fn a_root_file_of_the_vault_is_never_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("AGENTS.md"), "# Mis reglas\n").unwrap();
        write_library_files(tmp.path(), &Library::default(), Vec::new(), true)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("AGENTS.md")).unwrap(),
            "# Mis reglas\n"
        );
        // Ours, and stale: refreshed.
        std::fs::write(
            tmp.path().join("INDEX.md"),
            "# Índice de la biblioteca\nviejo\n",
        )
        .unwrap();
        write_library_files(tmp.path(), &Library::default(), Vec::new(), true)
            .await
            .unwrap();
        let index = std::fs::read_to_string(tmp.path().join("INDEX.md")).unwrap();
        assert!(!index.contains("viejo"), "{index}");
    }

    #[tokio::test]
    async fn claim_never_overwrites_an_existing_curso_json() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(CURSO_JSON), "mine").unwrap();
        let skeleton = course_index(&naming(), "d", &course(1, "A", "B"), None);
        claim(tmp.path(), &skeleton).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(CURSO_JSON)).unwrap(),
            "mine"
        );
    }
}
