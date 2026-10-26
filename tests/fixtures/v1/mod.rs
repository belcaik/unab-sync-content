//! Synthetic layout-v1 library for the migrator tests (spec T1).
//!
//! [`write_v1_library`] fills a caller-supplied directory with a deterministic
//! tree shaped like what `u_crawler` writes today. Nothing in it is real course
//! data: names are the examples already pinned in `docs/specs/layout-v2.md` and
//! `src/layout.rs`, ids are from a made-up range, and every "media" file is a
//! few text bytes.
//!
//! Integration tests in this crate include the module with:
//!
//! ```text
//! #[path = "fixtures/v1/mod.rs"]
//! mod v1;
//! ```
//!
//! v2 names published on the catalog come from [`u_crawler::layout`] compiled
//! with the commented UNAB `[layout]` example in `assets/config.toml`
//! ([`unab_naming`]). v1 directory and file names come from [`u_crawler::fsutil`],
//! the same helpers the writers use. The manifest ([`V1Library::manifest`],
//! recomputed by [`hash_tree`]) is the no-loss check: a migration may move
//! paths, and the multiset of SHA-1 values must stay the same. [`V1Library::foreign`]
//! paths must stay put, byte for byte, at the same relative path.
//!
//! # What the tree contains
//!
//! - Two sections of ATDF106. Section 2520 has an `announcements/` body and no
//!   `index.json`, so its Canvas id is unknown. Section 2524 has `index.json`.
//! - A same-day recording pair `…DISRUPCION_EMPRESARIAL.mp4` and `…_1.mp4`.
//! - `Sesion_sincronica_1.mp4`, a recording whose name does not contain the course.
//! - Four 2025-style courses with no `announcements/` directory at all, each
//!   with one of the recording names from the spec's library findings.
//! - A user note inside a module (`nota-del-usuario.md`).
//! - `CodeR_S3.zip` under `announcements/media/`, capitals intact.
//! - `.obsidian/`, a loose root file, and a synthetic `.git/HEAD` at the library root.
//! - `Zoom/424242/`, whose id matches no course and whose names match no course.
//! - Two distinct Canvas items per kind that want one v2 name: announcements
//!   801 and 802 of DISRUPCION, same day and title, identical bodies
//!   ([`TWIN_ANNOUNCEMENT_ID`]); and two MINERIA assignments with one title,
//!   items `04` and `06` of one module, different bodies ([`TWIN_ASSIGNMENT`]).
//! - Every shape of Canvas link the real notes carry (T10), in MINERIA's page
//!   [`LINKS_PAGE`] ([`LINKS_BODY`]): file previews, downloads and API URLs,
//!   pages, a module item, an assignment, an announcement, a page and the
//!   home of another course, quizzes, iframes, external tools, equation
//!   images, a `mailto:` and one Canvas mangled into
//!   `https://<canvas>/mailto:`. Two attachments have the same size
//!   ([`SAME_SIZE_FILES`]); `state.json` records each attachment's size
//!   (no name, as v1 did) and each page's slug.
//!
//! There is no `.u_crawler/library.json`. That absence, plus `Zoom/<digits>/`,
//! `Modules/<id>_` and `NN-ASSIGN-`, is the v1 signal.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;

use sha1::{Digest, Sha1};
use u_crawler::announcements::AnnouncementRecord;
use u_crawler::canvas::Course;
use u_crawler::fsutil::{self, course_dir};
use u_crawler::layout::{self, CourseMeta, CourseNaming};
use u_crawler::links::{MediaKind, MediaRef};
use u_crawler::state::{ItemState, State};

/// Relative path of the synthetic Obsidian config at the library root.
pub const OBSIDIAN_REL: &str = ".obsidian/app.json";
/// Relative path of the synthetic git HEAD at the library root.
pub const GIT_HEAD_REL: &str = ".git/HEAD";
/// Loose file at the library root. It is not a course and must not move.
pub const LOOSE_FILE_REL: &str = "Pasted image.png";
/// File name of the note a user dropped inside a module.
pub const USER_NOTE_FILE: &str = "nota-del-usuario.md";
/// Announcement attachment whose stem keeps its capitals, as v1 stores it.
pub const CAPITAL_MEDIA_FILE: &str = "CodeR_S3.zip";
/// `Zoom/<id>` that no course's `index.json` and no recording token points at.
pub const ORPHAN_ZOOM_ID: u64 = 424_242;
/// The second of two DISRUPCION announcements with one date and title.
pub const TWIN_ANNOUNCEMENT_ID: u64 = 802;
/// The title two MINERIA assignments share (items `04` and `06`).
pub const TWIN_ASSIGNMENT: &str = "Semana 11 Sumativa 3 Informe de caso 3";
/// Title of the MINERIA page whose body is [`LINKS_BODY`].
pub const LINKS_PAGE: &str = "Fase 4";
/// Canvas ids of two attachments with the same size (`Guia_1.PDF`,
/// `Guia_2.PDF`): only a link whose text names one can be pointed at it.
pub const SAME_SIZE_FILES: [u64; 2] = [1001, 1002];
/// The links of [`LINKS_PAGE`], one shape per line, as html2md wrote them.
pub const LINKS_BODY: &str = "\
- [Guia\\_1.PDF](https://canvas.example.edu/courses/9001/files/1001/preview?verifier=abc&wrap=1)
- [descarga](/courses/9001/files/1002/download?download_frd=1)
- ![Diagrama](https://canvas.example.edu/courses/9001/files/1003/preview)
- [Rubrica](https://canvas.example.edu/api/v1/courses/9001/files/1004)
- [rubrica](https://canvas.example.edu/courses/9001/files/1004?wrap=1)
- [CodeR](/courses/9001/files/4242/preview)
- [Fase 1](https://canvas.example.edu/courses/9001/pages/informacion-general)
- [Contenidos](/courses/9001/pages/contenidos?module_item_id=7001)
- [Item](https://canvas.example.edu/courses/9001/modules/items/7001)
- [Tarea](https://canvas.example.edu/courses/9001/assignments/6001)
- [Aviso](https://canvas.example.edu/courses/9001/discussion_topics/501)
- [Otro ramo](https://canvas.example.edu/courses/9003/pages/informacion-general)
- [Ramo](https://canvas.example.edu/courses/9002)
- [Quiz](https://canvas.example.edu/courses/9001/quizzes/12)
- [Video](https://canvas.example.edu/media_attachments_iframe/333)
- [Zoom](https://canvas.example.edu/courses/9001/external_tools/187)
- ![ecuacion](https://canvas.example.edu/equation_images/x%2By)
- [correo](https://canvas.example.edu/mailto:docente@example.edu)
- [correo bueno](mailto:docente@example.edu)

```
[en codigo](https://canvas.example.edu/courses/9001/pages/contenidos)
```
";

const CANVAS_HOST: &str = "https://canvas.example.edu";

/// A v1 library written by [`write_v1_library`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V1Library {
    /// Every file under the root, relative path (`/`-separated) → lowercase hex SHA-1.
    pub manifest: BTreeMap<String, String>,
    /// Courses, in the generator's fixed order.
    pub courses: Vec<V1Course>,
    /// `Zoom/<id>` with no course to migrate into.
    pub orphan_zoom: V1Zoom,
    /// Root-level files that are not part of any course (`.obsidian`, `.git`, loose files).
    pub foreign: Vec<String>,
}

/// One v1 course directory plus the v2 directory [`layout`] gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V1Course {
    /// Canvas id is `Some` only when `announcements/index.json` carries `courses/<id>`.
    pub meta: CourseMeta,
    /// Directory name under the library root (`fsutil::course_dir`).
    pub v1_dir: String,
    /// Directory name [`CourseNaming::course_dirs`] returns for the whole fixture
    /// with an empty `existing` set and [`unab_naming`].
    pub v2_dir: String,
    /// `announcements/` exists. Distinct from [`Self::has_index_json`]: section
    /// 2520 has the directory and no index.
    pub has_announcements_dir: bool,
    /// `announcements/index.json` exists.
    pub has_index_json: bool,
    /// Relative path of `announcements/index.json`, when present.
    pub index_rel: Option<String>,
    /// Recordings that belong to this course. `None` when the course has no `Zoom/` folder.
    ///
    /// When [`Self::meta`] has an id, [`V1Zoom::folder_id`] is that same id.
    /// A 2025-style course has a folder id that appears nowhere in an index;
    /// the recording file names carry the course tokens instead.
    pub zoom: Option<V1Zoom>,
    /// Relative path of [`USER_NOTE_FILE`], when this course has one.
    pub user_note_rel: Option<String>,
    /// Relative path of [`CAPITAL_MEDIA_FILE`] under `announcements/media/`.
    pub capital_media_rel: Option<String>,
    /// [`layout::attachment_file`] of [`CAPITAL_MEDIA_FILE`].
    pub capital_media_v2_name: Option<String>,
}

/// One `Zoom/<id>/` directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V1Zoom {
    /// The numeric directory name under `Zoom/`.
    pub folder_id: u64,
    /// `Zoom/<folder_id>`, relative to the library root.
    pub rel: String,
    pub recordings: Vec<V1RecordingFile>,
}

/// One recording file inside a [`V1Zoom`] directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V1RecordingFile {
    /// Path relative to the library root.
    pub rel: String,
    /// The file name only, as v1 wrote it.
    pub file_name: String,
    /// [`layout::recording_file`] after [`layout::parse_v1_recording`], using
    /// the course's Canvas name. `None` for the orphan folder (no course) and
    /// for names that are not a dated `.mp4`.
    pub v2_file_name: Option<String>,
}

impl V1Library {
    /// The course whose Canvas `course_code` is `code`.
    pub fn course_by_code(&self, code: &str) -> Option<&V1Course> {
        self.courses
            .iter()
            .find(|c| c.meta.code.as_deref() == Some(code))
    }
}

/// Lowercase hex SHA-1 of `bytes`. This is the hash stored in [`V1Library::manifest`].
pub fn sha1_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha1::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0xf) as usize] as char);
    }
    out
}

/// SHA-1 of every regular file under `root`, keyed by the `/`-separated path
/// relative to `root`. This is how a no-loss test re-reads a tree after a migration.
///
/// # Errors
///
/// Returns an error if a name is not UTF-8, or if the walk meets a symlink or
/// any other non-regular file.
pub fn hash_tree(root: &Path) -> io::Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    walk(root, "", &mut out)?;
    Ok(out)
}

/// The commented UNAB `[layout]` example in `assets/config.toml`, compiled.
///
/// The catalog's [`V1Course::v2_dir`] values were produced with this naming and
/// an empty `existing` set, so later tests do not re-parse the example.
///
/// # Errors
///
/// Returns an error if a commented pattern is missing or is not a valid regex.
pub fn unab_naming() -> io::Result<CourseNaming> {
    let mut code_pattern = String::new();
    let mut name_strip_pattern = String::new();
    let mut section_pattern = String::new();
    let mut term_pattern = String::new();
    for line in include_str!("../../../assets/config.toml").lines() {
        if let Some(value) = commented_pattern(line, "code_pattern") {
            code_pattern = unquote(value)?;
        } else if let Some(value) = commented_pattern(line, "name_strip_pattern") {
            name_strip_pattern = unquote(value)?;
        } else if let Some(value) = commented_pattern(line, "section_pattern") {
            section_pattern = unquote(value)?;
        } else if let Some(value) = commented_pattern(line, "term_pattern") {
            term_pattern = unquote(value)?;
        }
    }
    if code_pattern.is_empty()
        || name_strip_pattern.is_empty()
        || section_pattern.is_empty()
        || term_pattern.is_empty()
    {
        return Err(io::Error::other(
            "assets/config.toml is missing a commented UNAB layout pattern",
        ));
    }
    CourseNaming::new(&u_crawler::config::Layout {
        course_dir: "{code}-{name}".to_string(),
        code_pattern,
        name_strip_pattern,
        section_pattern,
        term_pattern,
    })
    .map_err(io::Error::other)
}

/// Write the synthetic v1 library under `root` and return its catalog.
///
/// `root` is created if needed and must be empty: the generator does not
/// overwrite or delete anything already there. Two calls on two empty
/// directories produce the same [`V1Library`], including the manifest.
///
/// # Errors
///
/// Returns an error if `root` is not empty, a path cannot be written, or the
/// blueprint is internally inconsistent (that last one is a bug in this module).
pub fn write_v1_library(root: &Path) -> io::Result<V1Library> {
    fs::create_dir_all(root)?;
    if fs::read_dir(root)?.next().transpose()?.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "v1 fixture root is not empty",
        ));
    }

    let naming = unab_naming()?;
    check_ids(PLANS)?;
    let metas: Vec<CourseMeta> = PLANS.iter().map(CoursePlan::meta).collect();
    let v2_dirs = naming.course_dirs(&[], &metas);
    if v2_dirs.len() != PLANS.len() {
        return Err(io::Error::other(
            "layout::course_dirs returned a different number of names than courses",
        ));
    }

    let mut manifest = BTreeMap::new();
    let mut courses = Vec::with_capacity(PLANS.len());
    for (plan, v2_dir) in PLANS.iter().zip(v2_dirs) {
        courses.push(write_course(root, &mut manifest, plan, v2_dir)?);
    }
    let orphan_zoom = write_orphan(root, &mut manifest)?;
    let foreign = write_foreign(root, &mut manifest)?;

    Ok(V1Library {
        manifest,
        courses,
        orphan_zoom,
        foreign,
    })
}

fn commented_pattern<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = line.strip_prefix("# ")?;
    let rest = rest.strip_prefix(key)?;
    rest.strip_prefix(" = ")
}

fn unquote(value: &str) -> io::Result<String> {
    let rest = value
        .trim()
        .strip_prefix('\'')
        .ok_or_else(|| io::Error::other("layout pattern is missing its opening quote"))?;
    let end = rest
        .find('\'')
        .ok_or_else(|| io::Error::other("layout pattern is missing its closing quote"))?;
    Ok(rest[..end].to_string())
}

fn walk(dir: &Path, prefix: &str, out: &mut BTreeMap<String, String>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| io::Error::other("non-utf8 file name in the v1 fixture"))?;
        let rel = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        let ft = entry.file_type()?;
        if ft.is_dir() {
            walk(&entry.path(), &rel, out)?;
        } else if ft.is_file() {
            let bytes = fs::read(entry.path())?;
            out.insert(rel, sha1_hex(&bytes));
        } else {
            return Err(io::Error::other(format!("unsupported file type at {rel}")));
        }
    }
    Ok(())
}

fn put(
    root: &Path,
    manifest: &mut BTreeMap<String, String>,
    rel: &str,
    bytes: &[u8],
) -> io::Result<()> {
    if rel.is_empty()
        || rel.starts_with('/')
        || rel
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(io::Error::other(format!("bad fixture relative path {rel}")));
    }
    let path = rel
        .split('/')
        .fold(root.to_path_buf(), |acc, part| acc.join(part));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, bytes)?;
    if manifest.insert(rel.to_string(), sha1_hex(bytes)).is_some() {
        return Err(io::Error::other(format!("duplicate fixture path {rel}")));
    }
    Ok(())
}

fn join_rel(parts: &[&str]) -> String {
    parts.join("/")
}

fn json_bytes(value: &impl serde::Serialize) -> io::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn v1_course_dirname(name: &str, code: &str) -> io::Result<String> {
    let path = course_dir(
        Path::new("."),
        &Course {
            id: 0,
            name: name.to_string(),
            course_code: Some(code.to_string()),
        },
    );
    path.file_name()
        .and_then(|s| s.to_str())
        .map(str::to_string)
        .ok_or_else(|| io::Error::other("course directory name is not utf-8"))
}

fn module_dirname(id: u64, name: &str) -> String {
    format!("{id}_{}", fsutil::sanitize_component(name))
}

/// Mirrors `AnnouncementSync::body_filename`: `YYYY-MM-DD_<slug>_<id>.md`.
fn announcement_filename(date: &str, title: &str, id: u64) -> String {
    let slug: String = fsutil::sanitize_component(title).chars().take(60).collect();
    format!("{date}_{slug}_{id}.md")
}

fn page_filename(position: u32, title: &str) -> String {
    format!("{:02}-{}.md", position, fsutil::sanitize_component(title))
}

fn assignment_filename(position: u32, title: &str) -> String {
    format!(
        "{:02}-ASSIGN-{}.md",
        position,
        fsutil::sanitize_component(title)
    )
}

fn markdown(title: &str, code: &str) -> Vec<u8> {
    format!("# {title}\n\nTexto sintetico de {code}.\n").into_bytes()
}

/// The item number is in the body, so two assignments with one title differ
/// in content as they do in Canvas.
fn assignment_markdown(assignment: &PagePlan, code: &str) -> Vec<u8> {
    let mut body = markdown(assignment.title, code);
    body.extend_from_slice(format!("Item {:02}.\n", assignment.position).as_bytes());
    body
}

fn page_markdown(page: &PagePlan, code: &str) -> Vec<u8> {
    let mut body = markdown(page.title, code);
    if page.links {
        body.push(b'\n');
        body.extend_from_slice(LINKS_BODY.as_bytes());
    }
    body
}

fn fake_bytes(kind: &str, name: &str) -> Vec<u8> {
    format!("synthetic-{kind}:{name}\n").into_bytes()
}

fn state_bytes(plan: &CoursePlan) -> io::Result<Vec<u8>> {
    let mut state = State::default();
    let entry = |size: Option<u64>, content_hash: Option<String>| ItemState {
        etag: size.map(|_| "etag".to_string()),
        updated_at: Some("2026-01-15T00:00:00Z".to_string()),
        size,
        content_hash,
        last_error: None,
        error_count: None,
        file_name: None,
        path: None,
    };
    state.set(format!("page:{}", plan.code), entry(Some(1), None));
    // v1 recorded the SHA-1 of each note it wrote, and no path.
    for module in plan.modules {
        for page in module.pages {
            let hash = sha1_hex(&page_markdown(page, plan.code));
            state.set(
                format!("page:{}", layout::slugify(page.title)),
                entry(None, Some(hash)),
            );
        }
        for assignment in module.assignments {
            let hash = sha1_hex(&assignment_markdown(assignment, plan.code));
            state.set(
                format!("assignment:{}", assignment.id),
                entry(None, Some(hash)),
            );
        }
        for (id, original) in module.attachments {
            let size = fake_bytes("file", original).len() as u64;
            state.set(format!("file:{id}"), entry(Some(size), None));
        }
    }
    if let AnnPlan::Indexed {
        media: Some(original),
        ..
    } = plan.announcements
    {
        let size = fake_bytes("zip", original).len() as u64;
        state.set(format!("file:{CODER_FILE_ID}"), entry(Some(size), None));
    }
    json_bytes(&state)
}

#[derive(Clone, Copy)]
struct PagePlan {
    position: u32,
    title: &'static str,
    /// The body ends with [`LINKS_BODY`].
    links: bool,
    /// Canvas id of an assignment (its `assignment:<id>` state key); `0`
    /// for a page, which v1 keyed by slug.
    id: u64,
}

#[derive(Clone, Copy)]
struct ModulePlan {
    id: u64,
    name: &'static str,
    pages: &'static [PagePlan],
    assignments: &'static [PagePlan],
    /// Canvas id and display name before `sanitize_filename_preserve_ext`,
    /// as Canvas would send it.
    attachments: &'static [(u64, &'static str)],
    user_note: bool,
}

#[derive(Clone, Copy)]
enum AnnPlan {
    /// No `announcements/` directory. This is the 2025 library shape.
    Absent,
    /// A markdown body and no `index.json`, so the course id cannot be recovered.
    Body {
        id: u64,
        date: &'static str,
        title: &'static str,
    },
    Indexed {
        id: u64,
        date: &'static str,
        title: &'static str,
        /// Original media file name, stored under `announcements/media/`.
        media: Option<&'static str>,
        /// A second announcement with the same date, title and body, and this id.
        twin: Option<u64>,
    },
}

#[derive(Clone, Copy)]
struct CoursePlan {
    /// Canvas id written into `index.json`. `None` unless [`AnnPlan::Indexed`].
    canvas_id: Option<u64>,
    name: &'static str,
    code: &'static str,
    /// `Zoom/<id>`. Equal to `canvas_id` when that id is known.
    zoom_folder_id: Option<u64>,
    announcements: AnnPlan,
    modules: &'static [ModulePlan],
    recordings: &'static [&'static str],
}

impl CoursePlan {
    fn meta(&self) -> CourseMeta {
        CourseMeta {
            id: self.canvas_id,
            name: self.name.to_string(),
            code: Some(self.code.to_string()),
        }
    }
}

fn check_ids(plans: &[CoursePlan]) -> io::Result<()> {
    let mut zoom_ids = BTreeSet::new();
    let mut canvas_ids = BTreeSet::new();
    for plan in plans {
        if let Some(id) = plan.zoom_folder_id {
            if id == ORPHAN_ZOOM_ID || !zoom_ids.insert(id) {
                return Err(io::Error::other(format!(
                    "zoom folder {id} collides with another course or the orphan"
                )));
            }
        }
        if let Some(id) = plan.canvas_id {
            if id == ORPHAN_ZOOM_ID || !canvas_ids.insert(id) {
                return Err(io::Error::other(format!("canvas id {id} is reused")));
            }
        }
    }
    Ok(())
}

fn write_course(
    root: &Path,
    manifest: &mut BTreeMap<String, String>,
    plan: &CoursePlan,
    v2_dir: String,
) -> io::Result<V1Course> {
    let indexed = matches!(plan.announcements, AnnPlan::Indexed { .. });
    if plan.canvas_id.is_some() != indexed {
        return Err(io::Error::other(format!(
            "course {} has a canvas id that does not match its index.json",
            plan.code
        )));
    }
    if let (Some(canvas_id), Some(zoom_id)) = (plan.canvas_id, plan.zoom_folder_id) {
        if canvas_id != zoom_id {
            return Err(io::Error::other(format!(
                "course {} zoom folder {zoom_id} is not its canvas id {canvas_id}",
                plan.code
            )));
        }
    }

    let v1_dir = v1_course_dirname(plan.name, plan.code)?;
    put(
        root,
        manifest,
        &join_rel(&[&v1_dir, "state.json"]),
        &state_bytes(plan)?,
    )?;

    let mut user_note_rel = None;
    for module in plan.modules {
        if let Some(rel) = write_module(root, manifest, &v1_dir, plan.code, module)? {
            if user_note_rel.is_some() {
                return Err(io::Error::other(format!(
                    "course {} has more than one user note",
                    plan.code
                )));
            }
            user_note_rel = Some(rel);
        }
    }

    let (has_announcements_dir, has_index_json, index_rel, capital_media_rel) =
        write_announcements(root, manifest, plan, &v1_dir)?;
    let capital_media_v2_name = capital_media_rel
        .as_ref()
        .map(|_| layout::attachment_file(CAPITAL_MEDIA_FILE));
    let zoom = write_course_zoom(root, manifest, plan)?;

    Ok(V1Course {
        meta: plan.meta(),
        v1_dir,
        v2_dir,
        has_announcements_dir,
        has_index_json,
        index_rel,
        zoom,
        user_note_rel,
        capital_media_rel,
        capital_media_v2_name,
    })
}

fn write_module(
    root: &Path,
    manifest: &mut BTreeMap<String, String>,
    v1_dir: &str,
    code: &str,
    module: &ModulePlan,
) -> io::Result<Option<String>> {
    let dirname = module_dirname(module.id, module.name);
    for page in module.pages {
        let name = page_filename(page.position, page.title);
        put(
            root,
            manifest,
            &join_rel(&[v1_dir, "Modules", &dirname, &name]),
            &page_markdown(page, code),
        )?;
    }
    for assignment in module.assignments {
        let name = assignment_filename(assignment.position, assignment.title);
        put(
            root,
            manifest,
            &join_rel(&[v1_dir, "Modules", &dirname, &name]),
            &assignment_markdown(assignment, code),
        )?;
    }
    for (_, original) in module.attachments {
        let name = fsutil::sanitize_filename_preserve_ext(original);
        put(
            root,
            manifest,
            &join_rel(&[v1_dir, "Modules", &dirname, "Attachments", &name]),
            &fake_bytes("file", original),
        )?;
    }
    if module.user_note {
        let rel = join_rel(&[v1_dir, "Modules", &dirname, USER_NOTE_FILE]);
        put(
            root,
            manifest,
            &rel,
            b"Nota personal. No la genero u_crawler.\n",
        )?;
        return Ok(Some(rel));
    }
    Ok(None)
}

fn write_announcements(
    root: &Path,
    manifest: &mut BTreeMap<String, String>,
    plan: &CoursePlan,
    v1_dir: &str,
) -> io::Result<(bool, bool, Option<String>, Option<String>)> {
    let (id, date, title, media, twin, indexed) = match plan.announcements {
        AnnPlan::Absent => return Ok((false, false, None, None)),
        AnnPlan::Body { id, date, title } => (id, date, title, None, None, false),
        AnnPlan::Indexed {
            id,
            date,
            title,
            media,
            twin,
        } => (id, date, title, media, twin, true),
    };
    require_date(date)?;
    let filename = announcement_filename(date, title, id);
    for name in
        std::iter::once(filename).chain(twin.map(|twin| announcement_filename(date, title, twin)))
    {
        put(
            root,
            manifest,
            &join_rel(&[v1_dir, "announcements", &name]),
            &markdown(title, plan.code),
        )?;
    }

    let mut capital_media_rel = None;
    let mut media_refs = Vec::new();
    if let Some(original) = media {
        let name = fsutil::sanitize_filename_preserve_ext(original);
        let rel = join_rel(&[v1_dir, "announcements", "media", &name]);
        put(root, manifest, &rel, &fake_bytes("zip", original))?;
        if original == CAPITAL_MEDIA_FILE {
            capital_media_rel = Some(rel);
        }
        media_refs.push(MediaRef {
            url: format!("{CANVAS_HOST}/files/{CODER_FILE_ID}"),
            kind: MediaKind::CanvasFile,
            file_id: Some(CODER_FILE_ID),
            local_path: Some(format!("announcements/media/{name}")),
        });
    }

    let index_rel = if indexed {
        let canvas_id = plan.canvas_id.ok_or_else(|| {
            io::Error::other(format!(
                "course {} has index.json but no canvas id",
                plan.code
            ))
        })?;
        let record = |id: u64, media: Vec<MediaRef>| AnnouncementRecord {
            id,
            title: title.to_string(),
            posted_at: Some(format!("{date}T12:00:00Z")),
            html_url: Some(format!(
                "{CANVAS_HOST}/courses/{canvas_id}/discussion_topics/{id}"
            )),
            author: Some("Docente".to_string()),
            body_md_path: Some(format!(
                "announcements/{}",
                announcement_filename(date, title, id)
            )),
            links: Vec::new(),
            media,
            zoom_links: Vec::new(),
        };
        let mut records = vec![record(id, media_refs)];
        records.extend(twin.map(|twin| record(twin, Vec::new())));
        let rel = join_rel(&[v1_dir, "announcements", "index.json"]);
        put(root, manifest, &rel, &json_bytes(&records)?)?;
        Some(rel)
    } else {
        None
    };
    Ok((true, indexed, index_rel, capital_media_rel))
}

fn write_course_zoom(
    root: &Path,
    manifest: &mut BTreeMap<String, String>,
    plan: &CoursePlan,
) -> io::Result<Option<V1Zoom>> {
    let Some(folder_id) = plan.zoom_folder_id else {
        return Ok(None);
    };
    if plan.recordings.is_empty() {
        return Err(io::Error::other(format!(
            "course {} has a zoom folder and no recordings",
            plan.code
        )));
    }
    let zoom = write_zoom(root, manifest, folder_id, plan.recordings, Some(plan.name))?;
    Ok(Some(zoom))
}

fn write_orphan(root: &Path, manifest: &mut BTreeMap<String, String>) -> io::Result<V1Zoom> {
    write_zoom(
        root,
        manifest,
        ORPHAN_ZOOM_ID,
        &["2024_08_08_GRABACION_HUERFANA.mp4", "zoom-424242.mp4"],
        None,
    )
}

fn write_zoom(
    root: &Path,
    manifest: &mut BTreeMap<String, String>,
    folder_id: u64,
    names: &[&str],
    course_name: Option<&str>,
) -> io::Result<V1Zoom> {
    let rel = format!("Zoom/{folder_id}");
    let mut recordings = Vec::with_capacity(names.len());
    for name in names {
        let file_rel = join_rel(&[&rel, name]);
        put(root, manifest, &file_rel, &fake_bytes("mp4", name))?;
        let v2_file_name = match course_name {
            Some(course) => Some(v2_recording_name(name, names, course)?),
            None => None,
        };
        recordings.push(V1RecordingFile {
            rel: file_rel,
            file_name: (*name).to_string(),
            v2_file_name,
        });
    }
    Ok(V1Zoom {
        folder_id,
        rel,
        recordings,
    })
}

fn v2_recording_name(file_name: &str, siblings: &[&str], course_name: &str) -> io::Result<String> {
    let rec = layout::parse_v1_recording(file_name, siblings).ok_or_else(|| {
        io::Error::other(format!(
            "recording {file_name} is not a dated v1 mp4 the layout parser accepts"
        ))
    })?;
    Ok(layout::recording_file(
        rec.date,
        &rec.topic,
        rec.part,
        course_name,
    ))
}

fn write_foreign(root: &Path, manifest: &mut BTreeMap<String, String>) -> io::Result<Vec<String>> {
    let files = [
        (OBSIDIAN_REL, b"{\"synthetic\":true}\n".as_slice()),
        (GIT_HEAD_REL, b"ref: refs/heads/main\n".as_slice()),
        (LOOSE_FILE_REL, b"synthetic-png\n".as_slice()),
    ];
    let mut rels = Vec::with_capacity(files.len());
    for (rel, bytes) in files {
        put(root, manifest, rel, bytes)?;
        rels.push(rel.to_string());
    }
    Ok(rels)
}

fn require_date(date: &str) -> io::Result<()> {
    let bytes = date.as_bytes();
    let ok = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit());
    if ok {
        Ok(())
    } else {
        Err(io::Error::other(format!("bad announcement date {date}")))
    }
}

const INFO_PAGE: &[PagePlan] = &[PagePlan {
    position: 1,
    title: "Informacion general",
    links: false,
    id: 0,
}];

/// Canvas id of `CodeR_S3.zip`, the MINERIA announcement attachment.
const CODER_FILE_ID: u64 = 4242;

const fn plain_module(id: u64, name: &'static str) -> ModulePlan {
    ModulePlan {
        id,
        name,
        pages: INFO_PAGE,
        assignments: &[],
        attachments: &[],
        user_note: false,
    }
}

const MINERIA_MODULES: &[ModulePlan] = &[
    ModulePlan {
        id: 100,
        name: "Informacion General",
        pages: INFO_PAGE,
        assignments: &[PagePlan {
            position: 2,
            title: "Semana 10 - Formativa 6",
            links: false,
            id: 6001,
        }],
        attachments: &[
            (SAME_SIZE_FILES[0], "Guia_1.PDF"),
            (1003, "Diagrama.png"),
            (1004, "Rubrica final.docx"),
        ],
        user_note: true,
    },
    ModulePlan {
        id: 200,
        name: "Semana 1",
        pages: &[
            PagePlan {
                position: 1,
                title: "Contenidos",
                links: false,
                id: 0,
            },
            PagePlan {
                position: 2,
                title: LINKS_PAGE,
                links: true,
                id: 0,
            },
        ],
        assignments: &[
            PagePlan {
                position: 4,
                title: TWIN_ASSIGNMENT,
                links: false,
                id: 6004,
            },
            PagePlan {
                position: 6,
                title: TWIN_ASSIGNMENT,
                links: false,
                id: 6006,
            },
        ],
        attachments: &[(SAME_SIZE_FILES[1], "Guia_2.PDF")],
        user_note: false,
    },
];

const PLANS: &[CoursePlan] = &[
    CoursePlan {
        canvas_id: Some(9001),
        name: "202615_2690_MINERIA DE DATOS",
        code: "ATDF105_202615_2690_EL_ON",
        zoom_folder_id: Some(9001),
        announcements: AnnPlan::Indexed {
            id: 501,
            date: "2026-06-17",
            title: "Bienvenidos al curso",
            media: Some(CAPITAL_MEDIA_FILE),
            twin: None,
        },
        modules: MINERIA_MODULES,
        recordings: &["2026_07_02_Sesion_sincronica_1.mp4"],
    },
    CoursePlan {
        canvas_id: None,
        name: "202615_2520_ELECTIVO",
        code: "ATDF106_202615_2520_EL_ON",
        zoom_folder_id: None,
        announcements: AnnPlan::Body {
            id: 770,
            date: "2026-04-01",
            title: "Aviso de seccion",
        },
        modules: &[plain_module(310, "Avisos")],
        recordings: &[],
    },
    CoursePlan {
        canvas_id: Some(9002),
        name: "202615_2524_ELECTIVO",
        code: "ATDF106_202615_2524_EL_ON",
        zoom_folder_id: Some(9002),
        announcements: AnnPlan::Indexed {
            id: 771,
            date: "2026-04-02",
            title: "Aviso de seccion",
            media: None,
            twin: None,
        },
        modules: &[plain_module(320, "Avisos")],
        recordings: &["2026_05_04_202615_2524_ELECTIVO.mp4"],
    },
    CoursePlan {
        canvas_id: Some(9003),
        name: "202605_2333_DISRUPCION EMPRESARIAL",
        code: "ATDF101_202605_2333_EL_ON",
        zoom_folder_id: Some(9003),
        announcements: AnnPlan::Indexed {
            id: 801,
            date: "2026-03-01",
            title: "Inicio de clases",
            media: None,
            twin: Some(TWIN_ANNOUNCEMENT_ID),
        },
        modules: &[plain_module(400, "Inicio")],
        recordings: &[
            "2026_03_11_202605_2333_DISRUPCION_EMPRESARIAL.mp4",
            "2026_03_11_202605_2333_DISRUPCION_EMPRESARIAL_1.mp4",
        ],
    },
    CoursePlan {
        canvas_id: None,
        name: "202515_1033_ELEMENTOS DE FISICA Y MEDICION",
        code: "FMMA015_202515_1033_EL_ON",
        zoom_folder_id: Some(8101),
        announcements: AnnPlan::Absent,
        modules: &[plain_module(510, "Inicio")],
        recordings: &["2025_06_23_202515_1033_ELEMENTOS_DE_FISICA_Y_MEDICION.mp4"],
    },
    CoursePlan {
        canvas_id: None,
        name: "202515_1226_FISICA GENERAL",
        code: "FMMA015_202515_1226_EL_ON",
        zoom_folder_id: Some(8102),
        announcements: AnnPlan::Absent,
        modules: &[plain_module(520, "Inicio")],
        recordings: &["2025_06_18_Clase_FMMA015_1226_18_06_2025.mp4"],
    },
    CoursePlan {
        canvas_id: None,
        name: "202525_1332_DINAMICA",
        code: "FMMA115_202525_1332_EL_ON",
        zoom_folder_id: Some(8103),
        announcements: AnnPlan::Absent,
        modules: &[plain_module(530, "Inicio")],
        recordings: &["2025_10_06_Sesion_01_FMMA115_NRC_1332_0610.mp4"],
    },
    CoursePlan {
        canvas_id: None,
        name: "202525_1149_MECANICA",
        code: "MECA100_202525_1149_EL_ON",
        zoom_folder_id: Some(8104),
        announcements: AnnPlan::Absent,
        modules: &[plain_module(540, "Inicio")],
        recordings: &["2025_10_01_202525_1149_MECANICA_1.mp4"],
    },
];
