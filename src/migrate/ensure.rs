//! The migration as a whole: what `u_crawler migrate` runs, and what
//! [`ensure_v2`] runs at the start of `sync`, `announcements`, `calendar`
//! and `zoom flow`.
//!
//! [`migrate`] chains the pieces in the one order that is safe to cut at
//! any point and run again:
//!
//! 1. [`apply::pending`] → [`apply::resume`]: a run cut short is finished
//!    from its journal before anything is planned (a plan over a
//!    half-migrated tree is not the same plan).
//! 2. [`super::scan`] → [`plan::plan`] → [`apply::apply`] under a new
//!    journal, when the plan has anything to do.
//! 3. The generated notes each journal renamed get their frontmatter and
//!    local links ([`rewrite::rewrite_file`], backup under
//!    `.u_crawler/backup/<ts>/`, `<ts>` being the journal's).
//! 4. Every index file, from what is now on disk
//!    ([`library::write_indexes_from_disk`]): a `README.md` per course and
//!    per module where there is none, each `curso.json` brought up to date
//!    (readable name, term, section), `AGENTS.md` and `INDEX.md` at the root.
//! 5. `.u_crawler/library.json` last. Until it exists the library is not v2,
//!    so a cut anywhere above is picked up by the next run.
//!
//! Step 3 is driven by the journals, not by the plan, because a plan made
//! after the moves no longer lists them. Without `library.json` the notes of
//! every finished journal after the last undone one are rewritten again:
//! [`rewrite::rewrite_file`] leaves a note that already matches alone, and
//! no `sync` can have written a richer frontmatter in between, since every
//! `sync` runs [`ensure_v2`] first. (Before an undo it may have: the undo
//! set the marker aside, so those journals are not replayed.) With
//! `library.json`, only the journals of this call are.
//!
//! [`undo`] reverses one journal: the rewritten notes first (their current
//! bytes set aside, the backup put back), then [`apply::undo`], then
//! `library.json` set aside, so the next run migrates again.

use super::apply::{self, ApplyError, Entry, JournalState, Roots};
use super::plan::{self, CanvasCourse, Plan, Root, Unresolved};
use super::rewrite::{self, Rewrite, RewriteOutcome};
use super::{scan, Inventory, INDEX_JSON, LIBRARY_JSON};
use crate::canvas::{CanvasClient, CanvasError, Course};
use crate::config::Config;
use crate::frontmatter::{DocKind, Frontmatter};
use crate::fsutil;
use crate::layout::{self, CourseMeta, CourseNaming};
use crate::library;
use crate::status;
use chrono::Utc;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Where a migration reads and writes.
#[derive(Clone, Copy, Debug)]
pub struct Target<'a> {
    /// The library root (`download_root`, or `migrate --root`).
    pub library: &'a Path,
    /// `calendar.caldir_root`, or `None` to leave the calendar tree alone.
    pub caldir: Option<&'a Path>,
    /// The compiled `[layout]`.
    pub naming: &'a CourseNaming,
}

/// Why a migration stopped. Whatever was done is in the journal.
#[derive(Debug, Error)]
pub enum MigrateError {
    /// The library root does not exist.
    #[error("no library at {0}")]
    NoLibrary(PathBuf),
    /// The library and the calendar tree overlap: one is the other, or
    /// inside it. Migrating would write library files into the calendar.
    #[error("the library {library} and calendar.caldir_root {caldir} overlap; give each its own directory")]
    Overlap {
        /// The library root.
        library: PathBuf,
        /// The calendar root.
        caldir: PathBuf,
    },
    /// The executor stopped.
    #[error(transparent)]
    Apply(#[from] ApplyError),
    /// Reading the library, or writing an index file, failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Canvas metadata was asked for and could not be read.
    #[error("reading course metadata from Canvas: {0}")]
    Canvas(#[from] CanvasError),
    /// A blocking task panicked.
    #[error("migration task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}

/// What [`migrate`] or [`ensure_v2`] found or did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// [`ensure_v2`] only: the library root does not exist yet.
    NoLibrary,
    /// [`ensure_v2`] only: `library.json` is there and no run is open.
    AlreadyV2,
    /// `dry_run`: nothing was touched.
    DryRun(DryRun),
    /// The library is v2 now.
    Migrated(Summary),
}

/// What a migration would do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DryRun {
    /// An open journal a real run would resume first. The plan below was
    /// made over the half-migrated tree, so it is not what would run.
    pub pending: Option<String>,
    /// The plan over the library as it is.
    pub plan: Plan,
    /// `library.json` is already there.
    pub marked: bool,
}

/// What a migration did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    /// The open journal that was finished first.
    pub resumed: Option<String>,
    /// The journal of the new run, when the plan had anything to do.
    pub journal: Option<String>,
    /// v1 courses planned.
    pub courses: usize,
    /// Files and directories renamed to their v2 path.
    pub moved: usize,
    /// Sources left in place: their destination holds the same bytes.
    pub duplicates: usize,
    /// Sources renamed to a `conflicto-<hash8>` name.
    pub conflicts: usize,
    /// Sources left in place for another reason (see the log).
    pub skipped: usize,
    /// Empty v1 directories removed.
    pub removed_dirs: usize,
    /// v1 directories kept because something is still in them.
    pub kept_dirs: Vec<String>,
    /// Generated notes given frontmatter and local links.
    pub rewritten: usize,
    /// Generated notes that could not be rewritten (see the log).
    pub rewrite_failed: usize,
    /// What the plan left in place, and why.
    pub unresolved: Vec<Unresolved>,
    /// `README.md` files (course and module) written where there was none.
    pub indexes: usize,
    /// `library.json` was written by this run.
    pub marked: bool,
}

impl Summary {
    /// Anything was moved or rewritten, or a journal was finished.
    pub fn changed_files(&self) -> bool {
        self.resumed.is_some() || self.journal.is_some() || self.rewritten > 0
    }

    fn add(&mut self, report: &apply::Report) {
        self.moved += report.moved.len();
        self.duplicates += report.duplicates.len();
        self.conflicts += report.conflicts.len();
        self.skipped += report.skipped.len();
        self.removed_dirs += report.removed_dirs.len();
        self.kept_dirs.extend(report.kept_dirs.iter().cloned());
    }
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "layout v2 migration: {} course(s), {} moved, {} notes rewritten, \
             {} duplicate(s) left in place, {} conflict(s) renamed, {} skipped, {} unresolved, \
             {} empty v1 dir(s) removed",
            self.courses,
            self.moved,
            self.rewritten,
            self.duplicates,
            self.conflicts,
            self.skipped,
            self.unresolved.len(),
            self.removed_dirs
        )?;
        if !self.kept_dirs.is_empty() {
            write!(
                f,
                ", {} v1 dir(s) kept, not empty: {}",
                self.kept_dirs.len(),
                self.kept_dirs.join(", ")
            )?;
        }
        if self.rewrite_failed > 0 {
            write!(f, ", {} note(s) not rewritten", self.rewrite_failed)?;
        }
        if self.indexes > 0 {
            write!(f, ", {} README(s) written", self.indexes)?;
        }
        if let Some(ts) = &self.resumed {
            write!(f, "; resumed {ts}")?;
        }
        if let Some(ts) = &self.journal {
            write!(f, "; journal {ts} (undo: u_crawler migrate --undo {ts})")?;
        }
        Ok(())
    }
}

/// What [`undo`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UndoSummary {
    /// The journal was already undone; nothing was touched.
    pub already_undone: bool,
    /// Rewritten notes put back from their backup.
    pub restored: usize,
    /// What [`apply::undo`] did.
    pub report: apply::UndoReport,
    /// `README.md` files of the course directories this journal created,
    /// set aside after them.
    pub indexes: usize,
    /// `library.json` was set aside, so the next run migrates again.
    pub unmarked: bool,
}

impl fmt::Display for UndoSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.already_undone {
            return f.write_str("migration was already undone; nothing to do");
        }
        write!(
            f,
            "migration undone: {} rename(s) reverted, {} note(s) restored, \
             {} generated file(s) set aside, {} missing, {} v1 dir(s) recreated, \
             {} new dir(s) removed, {} new dir(s) left (not empty)",
            self.report.reverted,
            self.restored,
            self.report.set_aside.len() + self.indexes + usize::from(self.unmarked),
            self.report.missing.len(),
            self.report.dirs_restored,
            self.report.dirs_removed,
            self.report.dirs_left
        )
    }
}

/// `<library>/.u_crawler/library.json`.
pub fn library_json_path(library: &Path) -> PathBuf {
    library.join(layout::META_DIR).join(LIBRARY_JSON)
}

/// The text of `library.json`.
pub fn library_json() -> String {
    format!("{{\n  \"layout_version\": {}\n}}\n", layout::LAYOUT_VERSION)
}

/// Make sure the library is layout v2 before a writer touches it.
///
/// Cheap when it already is: `library.json` present and no open journal
/// means nothing is scanned and Canvas is not asked; only when a course or
/// module lacks its `README.md` are the index files written from disk
/// ([`library::write_indexes_from_disk`]; not in a dry run). A root that does not
/// exist is left alone (the writer creates it). Otherwise this is
/// [`migrate`], and a migration that moved or rewrote anything is logged
/// with `warn!` and shown to the user.
///
/// # Errors
///
/// As [`migrate`]. The caller should stop: writing v2 paths into a library
/// that is half v1 would download everything a second time.
pub async fn ensure_v2(
    target: &Target<'_>,
    canvas: Option<&CanvasClient>,
    dry_run: bool,
) -> Result<Outcome, MigrateError> {
    if !tokio::fs::try_exists(target.library).await? {
        return Ok(Outcome::NoLibrary);
    }
    if overlaps(target) {
        // A calendar-only deployment may point `download_root` at the
        // caldir tree; that tree is not a library and gets no marker.
        tracing::info!(
            path = %target.library.display(),
            "download_root overlaps calendar.caldir_root; not treated as a library, nothing migrated"
        );
        return Ok(Outcome::NoLibrary);
    }
    let marked = tokio::fs::try_exists(library_json_path(target.library)).await?;
    if marked && apply::pending(target.library)?.is_none() {
        if !dry_run && library::indexes_missing(target.library, &library::read(target.library)?)? {
            let written = library::write_indexes_from_disk(target.library, target.naming).await?;
            tracing::info!(path = %target.library.display(), written = written.len(), "missing index files written");
        }
        return Ok(Outcome::AlreadyV2);
    }
    let outcome = migrate(target, canvas, dry_run).await?;
    match &outcome {
        Outcome::Migrated(summary) if summary.changed_files() => {
            tracing::warn!(
                path = %target.library.display(),
                courses = summary.courses,
                moved = summary.moved,
                rewritten = summary.rewritten,
                duplicates = summary.duplicates,
                conflicts = summary.conflicts,
                removed_dirs = summary.removed_dirs,
                kept_dirs = summary.kept_dirs.len(),
                unresolved = summary.unresolved.len(),
                journal = ?summary.journal,
                resumed = ?summary.resumed,
                "layout v1 library migrated to layout v2"
            );
            status!("{summary}");
        }
        Outcome::Migrated(_) => {
            tracing::info!(path = %target.library.display(), "library marked as layout v2");
        }
        Outcome::DryRun(dry) if !dry.plan.is_empty() || dry.pending.is_some() => {
            status!(
                "dry run: this library is layout v1; a real run migrates it first \
                 ({} course(s), {} move(s), {} note(s) to rewrite). \
                 `u_crawler migrate --dry-run` shows the full plan.",
                dry.plan.courses.len(),
                dry.plan.moves.len(),
                dry.plan.edits.len()
            );
        }
        _ => {}
    }
    Ok(outcome)
}

/// [`ensure_v2`] over the configured `download_root` and
/// `calendar.caldir_root`, with Canvas metadata: what the writer commands
/// call before they read the library.
///
/// # Errors
///
/// As [`ensure_v2`].
pub async fn ensure_configured(
    cfg: &Config,
    naming: &CourseNaming,
    canvas: &CanvasClient,
    dry_run: bool,
) -> Result<Outcome, MigrateError> {
    let library = PathBuf::from(&cfg.download_root);
    let caldir = PathBuf::from(&cfg.calendar.caldir_root);
    let target = Target {
        library: &library,
        caldir: Some(&caldir),
        naming,
    };
    ensure_v2(&target, Some(canvas), dry_run).await
}

/// Migrate the library to layout v2 (see the module docs for the order).
///
/// Unlike [`ensure_v2`] this plans whatever `library.json` says, so a v1
/// course copied into a v2 library is migrated too. `canvas` adds the
/// official names and module positions; without it the plan is offline.
/// With `dry_run` nothing is created, moved or written.
///
/// # Errors
///
/// [`MigrateError::NoLibrary`] when the root does not exist, and the first
/// error of the steps above. The journal records what was done, so running
/// again resumes.
pub async fn migrate(
    target: &Target<'_>,
    canvas: Option<&CanvasClient>,
    dry_run: bool,
) -> Result<Outcome, MigrateError> {
    let library = target.library;
    if !tokio::fs::try_exists(library).await? {
        return Err(MigrateError::NoLibrary(library.to_path_buf()));
    }
    if overlaps(target) {
        return Err(MigrateError::Overlap {
            library: library.to_path_buf(),
            caldir: target.caldir.map(Path::to_path_buf).unwrap_or_default(),
        });
    }
    let was_marked = tokio::fs::try_exists(library_json_path(library)).await?;
    let pending = apply::pending(library)?;

    if dry_run {
        let inv = scan_blocking(target).await?;
        let meta = canvas_metadata(canvas, &inv).await?;
        return Ok(Outcome::DryRun(DryRun {
            pending,
            plan: plan::plan(&inv, target.naming, &meta),
            marked: was_marked,
        }));
    }

    let mut summary = Summary::default();
    let mut ran = Vec::new();
    if let Some(ts) = pending {
        let report = blocking(target, {
            let ts = ts.clone();
            move |roots| apply::resume(roots, &ts)
        })
        .await?;
        summary.add(&report);
        summary.resumed = Some(ts.clone());
        ran.push(ts);
    }

    let inv = scan_blocking(target).await?;
    let meta = canvas_metadata(canvas, &inv).await?;
    let plan = plan::plan(&inv, target.naming, &meta);
    summary.courses = plan.courses.len();
    summary.unresolved = plan.unresolved.clone();
    for u in &plan.unresolved {
        tracing::warn!(root = %u.root, path = %u.path, reason = %u.reason, "left in place by the migration");
    }
    if !plan.is_empty() {
        let ts = fresh_timestamp(library);
        let report = blocking(target, {
            let ts = ts.clone();
            move |roots| apply::apply(&plan, roots, &ts)
        })
        .await?;
        summary.add(&report);
        summary.journal = Some(ts.clone());
        ran.push(ts);
    }

    let to_rewrite = if was_marked {
        ran.clone()
    } else {
        replayable(&apply::journals(library)?)
    };
    for ts in &to_rewrite {
        rewrite_notes(library, ts, &mut summary).await?;
    }

    summary.indexes = library::write_indexes_from_disk(library, target.naming)
        .await?
        .len();
    if !was_marked {
        fsutil::atomic_write(&library_json_path(library), library_json().as_bytes()).await?;
        summary.marked = true;
    }
    Ok(Outcome::Migrated(summary))
}

/// The finished journals whose notes an unmarked library replays: those
/// after the last undone one. An undo sets `library.json` aside, so the
/// marker no longer says "no `sync` ran since"; a journal from before the
/// undo may have notes a `sync` enriched since, and replaying its offline
/// frontmatter would drop their keys.
fn replayable(journals: &[(String, JournalState)]) -> Vec<String> {
    let after = journals
        .iter()
        .rposition(|(_, state)| *state == JournalState::Undone)
        .map_or(0, |i| i + 1);
    journals[after..]
        .iter()
        .filter(|(_, state)| *state == JournalState::Done)
        .map(|(ts, _)| ts.clone())
        .collect()
}

/// Undo the migration journal `ts`.
///
/// # Errors
///
/// [`ApplyError::UnknownJournal`] and [`ApplyError::NewerJournal`] before
/// anything is touched; [`ApplyError::UndoBlocked`] when a rewritten note's
/// set-aside name is taken by something else; and the errors of
/// [`apply::undo`]. Running it again after a failure is safe.
pub async fn undo(target: &Target<'_>, ts: &str) -> Result<UndoSummary, MigrateError> {
    let library = target.library;
    let all = apply::journals(library)?;
    let Some((_, state)) = all.iter().find(|(other, _)| other == ts) else {
        return Err(ApplyError::UnknownJournal(ts.to_string()).into());
    };
    if *state == JournalState::Undone {
        return Ok(UndoSummary {
            already_undone: true,
            ..UndoSummary::default()
        });
    }
    if let Some((newer, _)) = all
        .iter()
        .find(|(other, s)| other.as_str() > ts && *s != JournalState::Undone)
    {
        return Err(ApplyError::NewerJournal {
            ts: ts.to_string(),
            newer: newer.clone(),
        }
        .into());
    }

    let restored = restore_notes(library, ts).await?;
    let mut report = blocking(target, {
        let ts = ts.to_string();
        move |roots| apply::undo(roots, &ts)
    })
    .await?;
    let indexes = set_aside_readmes(library, ts)?;
    // The READMEs kept the directories the run created from being removed.
    let (removed, left) = blocking(target, {
        let ts = ts.to_string();
        move |roots| apply::remove_created_dirs(roots, &ts)
    })
    .await?;
    report.dirs_removed += removed;
    report.dirs_left = left;
    let unmarked = set_aside(
        library,
        &format!("{}/{LIBRARY_JSON}", layout::META_DIR),
        &apply::undo_dir_rel(ts),
    )?;
    Ok(UndoSummary {
        already_undone: false,
        restored,
        report,
        indexes,
        unmarked,
    })
}

/// What Canvas says about the courses of the library's v1 directories:
/// those whose id an `index.json` gave (looked up one by one when they are
/// no longer active) and the active ones whose v1 directory name is there.
///
/// # Errors
///
/// A failed course list, or a failed module list of an active course. An
/// inactive course Canvas will not show is skipped with a warning.
pub async fn canvas_courses(
    client: &CanvasClient,
    inv: &Inventory,
) -> Result<Vec<CanvasCourse>, CanvasError> {
    let v1_dirs = inv.v1_course_dirs();
    if v1_dirs.is_empty() {
        return Ok(Vec::new());
    }
    let ids: BTreeSet<u64> = inv.course_ids.values().copied().collect();
    let mut wanted: BTreeMap<u64, (Course, bool)> = BTreeMap::new();
    for course in client.list_courses().await? {
        let named = v1_dir_name(&course).is_some_and(|d| v1_dirs.contains(d.as_str()));
        if ids.contains(&course.id) || named {
            wanted.insert(course.id, (course, true));
        }
    }
    for id in ids {
        if wanted.contains_key(&id) {
            continue;
        }
        match client.get_course(id).await {
            Ok(course) => {
                wanted.insert(id, (course, false));
            }
            Err(e) => {
                tracing::warn!(course_id = id, error = %e, "course not readable from Canvas; planned offline")
            }
        }
    }
    let mut out = Vec::with_capacity(wanted.len());
    for (id, (course, active)) in wanted {
        let modules = match client.list_modules_with_items(id).await {
            Ok(modules) => modules,
            Err(e) if !active => {
                tracing::warn!(course_id = id, error = %e, "modules not readable; numbered offline");
                Vec::new()
            }
            Err(e) => return Err(e),
        };
        out.push(CanvasCourse {
            meta: CourseMeta::from(&course),
            module_positions: modules
                .iter()
                .filter_map(|m| m.position.map(|p| (m.id, p)))
                .collect(),
        });
    }
    Ok(out)
}

async fn canvas_metadata(
    canvas: Option<&CanvasClient>,
    inv: &Inventory,
) -> Result<Vec<CanvasCourse>, CanvasError> {
    match canvas {
        Some(client) => canvas_courses(client, inv).await,
        None => Ok(Vec::new()),
    }
}

/// The library and the calendar tree are the same directory, or one holds
/// the other. Compared after resolving symlinks when both exist.
fn overlaps(target: &Target<'_>) -> bool {
    target
        .caldir
        .is_some_and(|caldir| roots_overlap(target.library, caldir))
}

/// `a` and `b` are the same directory, or one holds the other. Compared
/// after resolving symlinks where they exist.
pub fn roots_overlap(a: &Path, b: &Path) -> bool {
    let resolve = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let (a, b) = (resolve(a), resolve(b));
    a.starts_with(&b) || b.starts_with(&a)
}

/// The directory name v1 (`fsutil::course_dir`) gave this course.
fn v1_dir_name(course: &Course) -> Option<String> {
    fsutil::course_dir(Path::new(""), course)
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
}

async fn scan_blocking(target: &Target<'_>) -> Result<Inventory, MigrateError> {
    let library = target.library.to_path_buf();
    let caldir = target.caldir.map(Path::to_path_buf);
    Ok(tokio::task::spawn_blocking(move || scan(&library, caldir.as_deref())).await??)
}

/// Run a synchronous executor call off the async runtime.
async fn blocking<T, F>(target: &Target<'_>, f: F) -> Result<T, MigrateError>
where
    T: Send + 'static,
    F: FnOnce(&Roots<'_>) -> Result<T, ApplyError> + Send + 'static,
{
    let library = target.library.to_path_buf();
    let caldir = target.caldir.map(Path::to_path_buf);
    let result = tokio::task::spawn_blocking(move || {
        f(&Roots {
            library: &library,
            caldir: caldir.as_deref(),
        })
    })
    .await?;
    Ok(result?)
}

/// A journal timestamp for now that no journal uses yet.
fn fresh_timestamp(library: &Path) -> String {
    let mut now = Utc::now();
    loop {
        let ts = apply::new_timestamp(now);
        if !apply::journal_path(library, &ts).exists() {
            return ts;
        }
        now += chrono::Duration::milliseconds(1);
    }
}

/// One record of an announcements `index.json`, as far as a rewrite needs it.
#[derive(Debug, Default, Deserialize)]
struct IndexRecord {
    id: u64,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    posted_at: Option<String>,
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

/// What the notes of one course need, read once.
#[derive(Default)]
struct CourseContext {
    /// Announcement records by id, from `<course>/anuncios/index.json`.
    announcements: BTreeMap<u64, IndexRecord>,
}

impl CourseContext {
    async fn read(library: &Path, course: &str) -> CourseContext {
        let path = library
            .join(course)
            .join(layout::ANNOUNCEMENTS_DIR)
            .join(INDEX_JSON);
        let records: Vec<IndexRecord> = match tokio::fs::read_to_string(&path).await {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!(path = %path.display(), error = %e, "unreadable index.json; notes get no announcement metadata");
                Vec::new()
            }),
            Err(_) => Vec::new(),
        };
        CourseContext {
            announcements: records.into_iter().map(|r| (r.id, r)).collect(),
        }
    }

    /// Canvas file id → v2 path, for the media a v1 course directory
    /// recorded (`local_path` is relative to that v1 directory).
    fn files(&self, v1_dir: &str, renamed: &BTreeMap<&str, &str>) -> BTreeMap<u64, PathBuf> {
        self.announcements
            .values()
            .flat_map(|r| &r.media)
            .filter_map(|m| {
                let v1 = format!("{v1_dir}/{}", m.local_path.as_deref()?);
                Some((m.file_id?, PathBuf::from(*renamed.get(v1.as_str())?)))
            })
            .collect()
    }
}

/// Give every generated note journal `ts` renamed its frontmatter and local
/// links. A note that fails is logged and counted, never fatal: `sync`
/// rewrites it again from Canvas.
async fn rewrite_notes(library: &Path, ts: &str, summary: &mut Summary) -> io::Result<()> {
    let entries = apply::read_journal(&apply::journal_path(library, ts))?;
    let renames: Vec<(&str, &str)> = entries
        .iter()
        .filter_map(|e| match e {
            Entry::Rename {
                root: Root::Library,
                from,
                to,
            } => Some((from.as_str(), to.as_str())),
            _ => None,
        })
        .collect();
    let renamed: BTreeMap<&str, &str> = renames.iter().copied().collect();
    let mut contexts: BTreeMap<&str, CourseContext> = BTreeMap::new();
    for (from, to) in &renames {
        if rewrite::generated_kind(Path::new(from)).is_none() {
            continue;
        }
        let Some(kind) = rewrite::generated_kind(Path::new(to)) else {
            continue;
        };
        let (Some((course, _)), Some((v1_dir, _))) = (to.split_once('/'), from.split_once('/'))
        else {
            continue;
        };
        if !contexts.contains_key(course) {
            contexts.insert(course, CourseContext::read(library, course).await);
        }
        let ctx = &contexts[course];
        let mut fm = Frontmatter {
            ramo: course.to_string(),
            tipo: kind,
            canvas_id: None,
            url: None,
            publicado: None,
            vence: None,
            disponible: None,
        };
        if kind == DocKind::Announcement {
            fm.canvas_id = v1_announcement_id(from);
            if let Some(record) = fm.canvas_id.and_then(|id| ctx.announcements.get(&id)) {
                fm.url = record.html_url.clone();
                fm.publicado = record.posted_at.clone();
            }
        }
        let files = ctx.files(v1_dir, &renamed);
        let job = Rewrite {
            root: library,
            path: Path::new(to),
            frontmatter: &fm,
            files: &files,
            backup_ts: ts,
        };
        match rewrite::rewrite_file(&job).await {
            Ok(RewriteOutcome::Rewritten { .. }) => summary.rewritten += 1,
            Ok(RewriteOutcome::Unchanged) => {}
            Err(e) => {
                tracing::warn!(path = %to, error = %e, "generated note not rewritten");
                summary.rewrite_failed += 1;
            }
        }
    }
    Ok(())
}

/// The announcement id at the end of a v1 body name: `…_<id>.md`.
fn v1_announcement_id(path: &str) -> Option<u64> {
    let stem = path.rsplit('/').next()?.strip_suffix(".md")?;
    stem.rsplit_once('_')?.1.parse().ok()
}

/// Put every note journal `ts` rewrote back to its backup, setting the
/// current bytes aside under the journal's undo directory. Idempotent: a
/// note that already holds its backup is skipped, and a cut between the
/// set-aside and the restore is finished.
async fn restore_notes(library: &Path, ts: &str) -> Result<usize, MigrateError> {
    let backup_root = library.join(layout::META_DIR).join("backup").join(ts);
    let mut rels = Vec::new();
    list_files(&backup_root, "", &mut rels)?;
    let aside_root = apply::undo_dir_rel(ts);
    let mut restored = 0;
    for rel in rels {
        let backup = tokio::fs::read(backup_root.join(&rel)).await?;
        let note = library.join(&rel);
        let aside = library.join(&aside_root).join(&rel);
        match tokio::fs::read(&note).await {
            Ok(current) if current == backup => continue,
            Ok(_) => {
                if tokio::fs::try_exists(&aside).await? {
                    return Err(ApplyError::UndoBlocked {
                        from: note,
                        to: aside,
                    }
                    .into());
                }
                if let Some(parent) = aside.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                apply::rename_noreplace(&note, &aside)?;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if !tokio::fs::try_exists(&aside).await? {
                    tracing::warn!(path = %rel, "rewritten note is gone; not restored");
                    continue;
                }
            }
            Err(e) => return Err(e.into()),
        }
        fsutil::atomic_write(&note, &backup).await?;
        restored += 1;
    }
    Ok(restored)
}

/// Set aside the `README.md` files (course and module) of every course
/// directory journal `ts` created, known by the `curso.json` it wrote and
/// the undo set aside. They were written after the journal, so it does not
/// list them; left there, they would make the emptied directory a foreign
/// one the next migration must avoid. Whatever was renamed in has already
/// been renamed back, so what is left is generated. Idempotent: a cut undo
/// run again finds the `curso.json` already set aside.
fn set_aside_readmes(library: &Path, ts: &str) -> Result<usize, MigrateError> {
    let aside_dir = apply::undo_dir_rel(ts);
    let suffix = format!("/{}", super::CURSO_JSON);
    let mut moved = 0;
    for entry in apply::read_journal(&apply::journal_path(library, ts))? {
        let Entry::Write { path } = entry else {
            continue;
        };
        let Some(dir) = path.strip_suffix(&suffix) else {
            continue;
        };
        if !library.join(&aside_dir).join(&path).exists() {
            continue;
        }
        let mut readmes = vec![format!("{dir}/README.md")];
        let modulos = format!("{dir}/{}", layout::MODULES_DIR);
        if let Ok(read) = std::fs::read_dir(library.join(&modulos)) {
            for module in read.flatten() {
                if let Some(name) = module.file_name().to_str() {
                    readmes.push(format!("{modulos}/{name}/README.md"));
                }
            }
        }
        readmes.sort();
        for rel in readmes {
            moved += usize::from(set_aside(library, &rel, &aside_dir)?);
        }
    }
    Ok(moved)
}

/// Rename `<library>/<rel>` to `<library>/<aside_dir>/<rel>` when it exists
/// and the target is free. Returns whether it moved.
fn set_aside(library: &Path, rel: &str, aside_dir: &str) -> Result<bool, MigrateError> {
    let file = library.join(rel);
    let aside = library.join(aside_dir).join(rel);
    if !file.exists() || aside.exists() {
        return Ok(false);
    }
    if let Some(parent) = aside.parent() {
        std::fs::create_dir_all(parent)?;
    }
    apply::rename_noreplace(&file, &aside)?;
    Ok(true)
}

/// Every regular file under `dir`, `/`-separated and relative to it. A
/// missing directory has none.
fn list_files(dir: &Path, prefix: &str, out: &mut Vec<String>) -> io::Result<()> {
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
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let ft = entry.file_type()?;
        if ft.is_dir() {
            list_files(&entry.path(), &rel, out)?;
        } else if ft.is_file() {
            out.push(rel);
        }
    }
    out.sort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_announcement_id_reads_the_trailing_id() {
        assert_eq!(
            v1_announcement_id("X_Y/announcements/2026-06-17_Bienvenidos_al_curso_5001.md"),
            Some(5001)
        );
        assert_eq!(v1_announcement_id("X/announcements/sin_id.md"), None);
        assert_eq!(v1_announcement_id("X/announcements/a_1.txt"), None);
    }

    #[test]
    fn journals_before_an_undo_are_not_replayed() {
        let j = |ts: &str, state| (ts.to_string(), state);
        let all = [
            j("1", JournalState::Done),
            j("2", JournalState::Undone),
            j("3", JournalState::Done),
            j("4", JournalState::Open),
        ];
        assert_eq!(replayable(&all), ["3"]);
        assert_eq!(replayable(&all[..1]), ["1"]);
        assert!(replayable(&all[..2]).is_empty());
    }

    #[test]
    fn library_json_records_the_layout_version() {
        let value: serde_json::Value = serde_json::from_str(&library_json()).unwrap();
        assert_eq!(value["layout_version"], layout::LAYOUT_VERSION);
    }

    #[test]
    fn summary_names_the_undo_command() {
        let summary = Summary {
            journal: Some("20261025T000000000Z".into()),
            moved: 3,
            ..Summary::default()
        };
        let text = summary.to_string();
        assert!(text.contains("3 moved"));
        assert!(text.contains("u_crawler migrate --undo 20261025T000000000Z"));
    }
}
