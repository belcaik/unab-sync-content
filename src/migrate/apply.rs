//! The migration executor: carries a [`Plan`] out with `rename(2)` and
//! nothing else, behind a write-ahead journal.
//!
//! Every operation is first written to `<root>/.u_crawler/migrations/<ts>.jsonl`
//! as one JSON line and `fsync`ed, and only then performed. The first line,
//! [`Entry::Begin`], holds the whole [`Intent`]; after it come the operations,
//! [`Entry::Mkdir`], [`Entry::Rename`], [`Entry::Rmdir`] (an empty v1
//! directory) and [`Entry::Write`] (a `curso.json` that did not exist yet),
//! and [`Entry::Done`] closes the run. There is no file delete, no copy and
//! no overwrite anywhere in this module:
//!
//! - A rename never replaces its destination. On Linux it is
//!   `renameat2(RENAME_NOREPLACE)`; elsewhere, and on a filesystem without
//!   that flag, the destination is checked first.
//! - A rename that would cross filesystems (`EXDEV`) stops the run with
//!   [`ApplyError::CrossDevice`]. Nothing is copied.
//! - A destination that already exists is compared by size and BLAKE3. Same
//!   content: the source stays where it is and is reported as a duplicate.
//!   Different content: the source goes to `name.conflicto-<hash8>.ext`
//!   ([`conflict_name`]) next to the destination.
//! - A path that goes through a symlink is refused, so no move leaves its root.
//! - Once everything is moved, the v1 directories of [`Intent::prune`] are
//!   removed with `rmdir(2)` where they are empty, bottom-up, and so are
//!   their empty parents (`Zoom/`). `rmdir` cannot remove a directory with
//!   anything in it, and nothing is ever removed recursively: a directory
//!   still holding a file (a duplicate left in place, a foreign file, a
//!   symlink) stays and is reported in [`Report::kept_dirs`].
//!
//! **Resuming.** A run cut short leaves an open journal ([`pending`]).
//! [`resume`] carries out the intent recorded in it, not a new plan: a plan
//! made from a half-migrated tree would not be the same (a recording's
//! `_1` part is only a part while its sibling is next to it). Rename is
//! atomic, so each logged rename either happened or did not, and the
//! filesystem says which: a source that is gone and that the journal renamed
//! is skipped.
//!
//! **Undo.** [`undo`] walks a journal backwards. Each rename is reversed when
//! its destination is there and its source is not; anything else is skipped,
//! so an undo cut short can simply be run again. A `curso.json` the run wrote
//! is set aside under `.u_crawler/migrations/<ts>.undo/`, never deleted.
//! Each directory the run removed is created again, and each one it created
//! is removed once it is empty again (one holding anything stays), so the
//! tree comes back to its directories as well as its files.
//!
//! The executor does not rewrite markdown (`Plan::edits`, `migrate::rewrite`)
//! and does not write `.u_crawler/library.json`: whoever orchestrates the
//! migration does both once this returns.

use super::plan::{Collision, EntryKind, Move, Plan, Root};
use super::CURSO_JSON;
use crate::index::{self, CourseIndex};
use crate::layout::{self, CourseMeta};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Directory under [`layout::META_DIR`] holding one journal per run.
pub const MIGRATIONS_DIR: &str = "migrations";
/// Extension of a journal file.
pub const JOURNAL_EXT: &str = "jsonl";
/// Infix of a conflict copy: `guia.conflicto-<hash8>.pdf`.
pub const CONFLICT_INFIX: &str = "conflicto";
/// Suffix of the directory an undo sets generated files aside in.
const UNDO_SUFFIX: &str = "undo";
/// Suffix of the temporary file a generated file is written to before its rename.
const TMP_SUFFIX: &str = "u_crawler-tmp";

/// The two trees a plan touches.
#[derive(Clone, Copy, Debug)]
pub struct Roots<'a> {
    /// `download_root`. The journal lives under it.
    pub library: &'a Path,
    /// `calendar.caldir_root`, needed only when the plan or journal has
    /// [`Root::Caldir`] entries.
    pub caldir: Option<&'a Path>,
}

/// What a run sets out to do: the parts of a [`Plan`] the executor acts on.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intent {
    /// [`Plan::moves`].
    pub moves: Vec<Move>,
    /// [`Plan::duplicates`] then [`Plan::conflicts`].
    pub collisions: Vec<Collision>,
    /// One `curso.json` per planned course.
    pub courses: Vec<IntentCourse>,
    /// [`Plan::prune`]. Absent from journals written before it existed.
    #[serde(default)]
    pub prune: Vec<String>,
}

/// The `curso.json` a planned course gets.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentCourse {
    /// v2 course directory.
    pub dir: String,
    /// Canvas id, when known.
    pub canvas_id: Option<u64>,
    /// Course name.
    pub name: String,
    /// Course code.
    pub code: Option<String>,
    /// `Zoom/<id>` folders migrated into it.
    pub zoom_ids: Vec<u64>,
}

impl From<&Plan> for Intent {
    fn from(plan: &Plan) -> Intent {
        Intent {
            moves: plan.moves.clone(),
            collisions: plan
                .duplicates
                .iter()
                .chain(&plan.conflicts)
                .cloned()
                .collect(),
            courses: plan
                .courses
                .iter()
                .map(|c| IntentCourse {
                    dir: c.dir.clone(),
                    canvas_id: c.course.id,
                    name: c.course.name.clone(),
                    code: c.course.code.clone(),
                    zoom_ids: c.zoom_ids.clone(),
                })
                .collect(),
            prune: plan.prune.clone(),
        }
    }
}

/// One line of a journal. Paths are relative to their root, `/`-separated.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Entry {
    /// The run's intent, written before its first operation.
    Begin {
        /// What the run will do.
        intent: Intent,
    },
    /// A directory that did not exist was created.
    Mkdir {
        /// Tree.
        root: Root,
        /// The directory.
        path: String,
    },
    /// `from` was renamed to `to`, which did not exist.
    Rename {
        /// Tree.
        root: Root,
        /// Source.
        from: String,
        /// Destination.
        to: String,
    },
    /// An empty directory was removed.
    Rmdir {
        /// Tree.
        root: Root,
        /// The directory.
        path: String,
    },
    /// A generated file that did not exist was written (library root).
    Write {
        /// The file.
        path: String,
    },
    /// Undo renamed `from` back to `to`.
    Revert {
        /// Tree.
        root: Root,
        /// The migrated path.
        from: String,
        /// The original path.
        to: String,
    },
    /// Undo moved a generated file to `to`, under the journal's undo directory.
    SetAside {
        /// The generated file.
        from: String,
        /// Where it went (library root).
        to: String,
    },
    /// The run finished.
    Done,
    /// The run was undone.
    Undone,
}

/// Where a journal stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JournalState {
    /// A run started and did not finish: [`resume`] it, or [`undo`] it.
    Open,
    /// The run finished.
    Done,
    /// An undo started and did not finish: run it again.
    Undoing,
    /// The run was undone.
    Undone,
}

/// A source and its destination in one of the two trees.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Placed {
    /// Tree.
    pub root: Root,
    /// Source.
    pub from: String,
    /// Destination.
    pub to: String,
}

/// What [`apply`] or [`resume`] did in this call.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// The journal, or `None` when there was nothing to do and none was created.
    pub journal: Option<PathBuf>,
    /// An open journal was continued.
    pub resumed: bool,
    /// Renames to the planned destination.
    pub moved: Vec<Placed>,
    /// Sources renamed to a `conflicto-<hash8>` name; `to` is that name.
    pub conflicts: Vec<Placed>,
    /// Sources left in place because the destination holds the same bytes.
    pub duplicates: Vec<Placed>,
    /// Sources left in place because the destination became a directory, or
    /// (a calendar directory) already exists.
    pub skipped: Vec<Placed>,
    /// Generated files written (`curso.json`), relative to the library root.
    pub written: Vec<String>,
    /// Empty v1 directories removed, library root.
    pub removed_dirs: Vec<String>,
    /// [`Intent::prune`] directories left in place because something is
    /// still in them.
    pub kept_dirs: Vec<String>,
}

/// What [`undo`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UndoReport {
    /// The journal was already undone; nothing was touched.
    pub already_undone: bool,
    /// Renames reversed.
    pub reverted: usize,
    /// Generated files set aside, with where they went.
    pub set_aside: Vec<Placed>,
    /// Logged renames whose destination is gone: skipped. `from` is the
    /// migrated path, `to` the original.
    pub missing: Vec<Placed>,
    /// Directories the run removed, created again.
    pub dirs_restored: usize,
    /// Directories the run created, removed again.
    pub dirs_removed: usize,
    /// Directories the run created, left in place because something is in them.
    pub dirs_left: usize,
}

/// Why the executor stopped. Whatever was done up to then is in the journal.
#[derive(Debug, Error)]
pub enum ApplyError {
    /// The timestamp is not one path segment of `[A-Za-z0-9._-]`.
    #[error("migration timestamp must be one path segment of [A-Za-z0-9._-]: {0:?}")]
    BadTimestamp(String),
    /// A plan or journal path is empty, absolute, or has `.`/`..` components.
    #[error("not a plain relative path: {0:?}")]
    BadPath(String),
    /// A calendar entry, and no `caldir_root`.
    #[error("the plan touches the calendar tree and no caldir_root was given")]
    NoCaldirRoot,
    /// [`apply`] was given the timestamp of an existing journal.
    #[error("migration journal {0} already exists; resume or undo it")]
    JournalExists(String),
    /// The journal is not open, so there is nothing to resume.
    #[error("migration {0} is not open")]
    NotOpen(String),
    /// The journal has operations but no readable [`Entry::Begin`].
    #[error("migration journal {0} has no intent to resume from")]
    NoIntent(String),
    /// No journal with this timestamp.
    #[error("no migration journal {0}")]
    UnknownJournal(String),
    /// A later migration is not undone; undo it first.
    #[error("migration {newer} came after {ts}; undo it first")]
    NewerJournal {
        /// The journal asked for.
        ts: String,
        /// The later one still in effect.
        newer: String,
    },
    /// A path goes through a symlink.
    #[error("refusing to go through a symlink: {0}")]
    Symlink(PathBuf),
    /// A planned source is gone, and this run did not move it.
    #[error("source no longer exists: {0}")]
    SourceMissing(PathBuf),
    /// A planned source is not a regular file (or not a directory, for a
    /// directory move).
    #[error("source is not of the planned kind: {0}")]
    WrongKind(PathBuf),
    /// The rename would cross filesystems. Nothing was copied.
    #[error("{from} and {to} are on different filesystems; refusing to copy")]
    CrossDevice {
        /// Source.
        from: PathBuf,
        /// Destination.
        to: PathBuf,
    },
    /// The conflict name is taken by a file with other content.
    #[error("conflict name already taken by different content: {0}")]
    ConflictNameTaken(PathBuf),
    /// Undo found both the migrated and the original path.
    #[error("cannot undo, both {from} and {to} exist")]
    UndoBlocked {
        /// The migrated path.
        from: PathBuf,
        /// The original path.
        to: PathBuf,
    },
    /// The fault-injection hook stopped the run.
    #[error("interrupted: {0}")]
    Interrupted(io::Error),
    /// A filesystem call failed.
    #[error("{path}: {source}")]
    Io {
        /// The path involved.
        path: PathBuf,
        /// The error.
        source: io::Error,
    },
}

type Result<T> = std::result::Result<T, ApplyError>;

trait IoAt<T> {
    fn at(self, path: &Path) -> Result<T>;
}

impl<T> IoAt<T> for io::Result<T> {
    fn at(self, path: &Path) -> Result<T> {
        self.map_err(|source| ApplyError::Io {
            path: path.to_path_buf(),
            source,
        })
    }
}

/// A fresh journal timestamp, `YYYYMMDDTHHMMSSmmmZ` in UTC.
pub fn new_timestamp(now: DateTime<Utc>) -> String {
    now.format("%Y%m%dT%H%M%S%3fZ").to_string()
}

/// `<library>/.u_crawler/migrations/<ts>.jsonl`.
pub fn journal_path(library: &Path, ts: &str) -> PathBuf {
    migrations_dir(library).join(format!("{ts}.{JOURNAL_EXT}"))
}

/// Where `rel` goes when it conflicts with a different file: `hash8` is put
/// before the last extension. `a/guia.pdf` → `a/guia.conflicto-<hash8>.pdf`,
/// `a/LEEME` → `a/LEEME.conflicto-<hash8>`, `.nota` → `.nota.conflicto-<hash8>`.
pub fn conflict_name(rel: &str, hash8: &str) -> String {
    let (dir, name) = rel.rsplit_once('/').unwrap_or(("", rel));
    let file = match name.rfind('.') {
        Some(dot) if dot > 0 => {
            format!("{}.{CONFLICT_INFIX}-{hash8}{}", &name[..dot], &name[dot..])
        }
        _ => format!("{name}.{CONFLICT_INFIX}-{hash8}"),
    };
    if dir.is_empty() {
        file
    } else {
        format!("{dir}/{file}")
    }
}

/// Read a journal. A line that does not parse is skipped with a warning: the
/// only way to produce one is a crash while appending it, and an entry is
/// performed only after its line was written in full.
///
/// # Errors
///
/// Fails when the file cannot be read.
pub fn read_journal(path: &Path) -> io::Result<Vec<Entry>> {
    let text = fs::read_to_string(path)?;
    let mut entries = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str(line) {
            Ok(entry) => entries.push(entry),
            Err(e) => tracing::warn!(
                path = %path.display(),
                line = n + 1,
                error = %e,
                "unreadable journal line skipped"
            ),
        }
    }
    Ok(entries)
}

/// Where a journal's entries leave it.
pub fn journal_state(entries: &[Entry]) -> JournalState {
    if entries.iter().any(|e| matches!(e, Entry::Undone)) {
        JournalState::Undone
    } else if entries
        .iter()
        .any(|e| matches!(e, Entry::Revert { .. } | Entry::SetAside { .. }))
    {
        JournalState::Undoing
    } else if entries.iter().any(|e| matches!(e, Entry::Done)) {
        JournalState::Done
    } else {
        JournalState::Open
    }
}

/// Every journal under `library`, oldest first, with its state.
///
/// # Errors
///
/// Fails when the migrations directory or a journal cannot be read.
pub fn journals(library: &Path) -> io::Result<Vec<(String, JournalState)>> {
    let dir = migrations_dir(library);
    let read = match fs::read_dir(&dir) {
        Ok(read) => read,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for entry in read {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some(ts) = name.strip_suffix(&format!(".{JOURNAL_EXT}")) else {
            continue;
        };
        if entry.file_type()?.is_file() && valid_timestamp(ts) {
            out.push((ts.to_string(), journal_state(&read_journal(&entry.path())?)));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// The latest journal a run left open, if any: pass its timestamp to
/// [`resume`], or to [`undo`].
///
/// # Errors
///
/// As [`journals`].
pub fn pending(library: &Path) -> io::Result<Option<String>> {
    Ok(journals(library)?
        .into_iter()
        .rev()
        .find(|(_, state)| *state == JournalState::Open)
        .map(|(ts, _)| ts))
}

/// Carry `plan` out under a new journal `ts`.
///
/// Moves first (library, then calendar), then collisions, then the empty
/// directories of [`Plan::prune`], then a `curso.json` for each planned
/// course whose directory exists and has none. A plan with nothing to do
/// creates no journal.
///
/// # Errors
///
/// [`ApplyError::JournalExists`] when `ts` is taken, and any other
/// [`ApplyError`]: the run stops at the first one, and the journal stays open
/// and records everything done so far, for [`resume`] or [`undo`].
pub fn apply(plan: &Plan, roots: &Roots<'_>, ts: &str) -> Result<Report> {
    apply_with_hook(plan, roots, ts, &mut |_| Ok(()))
}

/// [`apply`], calling `before_op` after each operation is journaled and
/// synced and before it is performed. An error from the hook stops the run
/// right there with [`ApplyError::Interrupted`], which is what a power cut at
/// the worst moment leaves behind. For fault-injection tests.
#[doc(hidden)]
pub fn apply_with_hook(
    plan: &Plan,
    roots: &Roots<'_>,
    ts: &str,
    before_op: &mut dyn FnMut(&Entry) -> io::Result<()>,
) -> Result<Report> {
    let mut journal = Journal::open(roots.library, ts)?;
    if journal.state.is_some() {
        return Err(ApplyError::JournalExists(ts.to_string()));
    }
    let intent = Intent::from(plan);
    journal.begin = Some(Entry::Begin {
        intent: intent.clone(),
    });
    run(&intent, roots, journal, before_op, BTreeSet::new())
}

/// Finish the run an open journal `ts` recorded, from its [`Intent`].
///
/// # Errors
///
/// [`ApplyError::UnknownJournal`], [`ApplyError::NotOpen`],
/// [`ApplyError::NoIntent`], and the errors of [`apply`].
pub fn resume(roots: &Roots<'_>, ts: &str) -> Result<Report> {
    let journal = Journal::open(roots.library, ts)?;
    match journal.state {
        None => return Err(ApplyError::UnknownJournal(ts.to_string())),
        Some(JournalState::Open) => {}
        Some(_) => return Err(ApplyError::NotOpen(ts.to_string())),
    }
    let intent = journal.entries.iter().find_map(|e| match e {
        Entry::Begin { intent } => Some(intent.clone()),
        _ => None,
    });
    let intent = match intent {
        Some(intent) => intent,
        // Torn before its first operation: nothing was done.
        None if journal.entries.is_empty() => Intent::default(),
        None => return Err(ApplyError::NoIntent(ts.to_string())),
    };
    let renamed = journal
        .entries
        .iter()
        .filter_map(|e| match e {
            Entry::Rename { root, from, .. } => Some((*root, from.clone())),
            _ => None,
        })
        .collect();
    tracing::info!(ts, "resuming migration from its journal");
    run(&intent, roots, journal, &mut |_| Ok(()), renamed)
}

fn run(
    intent: &Intent,
    roots: &Roots<'_>,
    journal: Journal,
    hook: &mut dyn FnMut(&Entry) -> io::Result<()>,
    renamed: BTreeSet<(Root, String)>,
) -> Result<Report> {
    let mut exec = Exec {
        roots: *roots,
        report: Report {
            resumed: journal.state.is_some(),
            ..Report::default()
        },
        journal,
        hook,
        renamed,
    };
    for m in &intent.moves {
        match m.kind {
            EntryKind::File => exec.file_to(m.root, &m.from, &m.to)?,
            EntryKind::Dir => exec.dir_to(m.root, &m.from, &m.to)?,
        }
    }
    for c in &intent.collisions {
        // The first source takes a free destination; the rest compare with it.
        for from in &c.from {
            exec.file_to(c.root, from, &c.to)?;
        }
    }
    for dir in &intent.prune {
        exec.prune(dir)?;
    }
    for course in &intent.courses {
        exec.curso_json(course)?;
    }
    if exec.journal.started() {
        exec.journal.record(&Entry::Done)?;
    }
    let mut report = exec.report;
    report.journal = exec.journal.started().then_some(exec.journal.path);
    tracing::info!(
        moved = report.moved.len(),
        conflicts = report.conflicts.len(),
        duplicates = report.duplicates.len(),
        skipped = report.skipped.len(),
        written = report.written.len(),
        removed_dirs = report.removed_dirs.len(),
        kept_dirs = report.kept_dirs.len(),
        resumed = report.resumed,
        "migration applied"
    );
    Ok(report)
}

/// Undo the migration `ts`, newest operation first. Running it again after a
/// cut, or on a journal already undone, is safe.
///
/// # Errors
///
/// [`ApplyError::UnknownJournal`], [`ApplyError::NewerJournal`] when a later
/// migration is still in effect, [`ApplyError::UndoBlocked`] when both ends
/// of a rename exist, and the path and I/O errors of [`apply`].
pub fn undo(roots: &Roots<'_>, ts: &str) -> Result<UndoReport> {
    let journal = Journal::open(roots.library, ts)?;
    let Some(state) = journal.state else {
        return Err(ApplyError::UnknownJournal(ts.to_string()));
    };
    if state == JournalState::Undone {
        return Ok(UndoReport {
            already_undone: true,
            ..UndoReport::default()
        });
    }
    let all = journals(roots.library).at(&migrations_dir(roots.library))?;
    if let Some((newer, _)) = all
        .iter()
        .find(|(other, s)| other.as_str() > ts && *s != JournalState::Undone)
    {
        return Err(ApplyError::NewerJournal {
            ts: ts.to_string(),
            newer: newer.clone(),
        });
    }
    let entries = journal.entries.clone();
    let mut exec = Exec {
        roots: *roots,
        journal,
        hook: &mut |_| Ok(()),
        report: Report::default(),
        renamed: BTreeSet::new(),
    };
    let mut report = UndoReport::default();
    for entry in entries.iter().rev() {
        match entry {
            Entry::Rename { root, from, to } => {
                let migrated = exec.resolve(*root, to)?;
                let original = exec.resolve(*root, from)?;
                match (exists(&migrated)?, exists(&original)?) {
                    (true, false) => {
                        exec.mkdirs(*root, from)?;
                        exec.journal.record(&Entry::Revert {
                            root: *root,
                            from: to.clone(),
                            to: from.clone(),
                        })?;
                        rename_noreplace(&migrated, &original)?;
                        report.reverted += 1;
                    }
                    // Never done (cut before it), or already reverted.
                    (false, true) => {}
                    (true, true) => {
                        return Err(ApplyError::UndoBlocked {
                            from: migrated,
                            to: original,
                        })
                    }
                    (false, false) => {
                        tracing::warn!(path = %to, "migrated path is gone; nothing to revert");
                        report.missing.push(Placed {
                            root: *root,
                            from: to.clone(),
                            to: from.clone(),
                        });
                    }
                }
            }
            Entry::Write { path } => {
                let aside_rel = format!("{}/{path}", undo_dir_rel(ts));
                let file = exec.resolve(Root::Library, path)?;
                let aside = exec.resolve(Root::Library, &aside_rel)?;
                if exists(&file)? && !exists(&aside)? {
                    exec.mkdirs(Root::Library, &aside_rel)?;
                    exec.journal.record(&Entry::SetAside {
                        from: path.clone(),
                        to: aside_rel.clone(),
                    })?;
                    rename_noreplace(&file, &aside)?;
                    report.set_aside.push(Placed {
                        root: Root::Library,
                        from: path.clone(),
                        to: aside_rel,
                    });
                }
            }
            Entry::Rmdir { root, path } => {
                let dir = exec.resolve(*root, path)?;
                if !exists(&dir)? {
                    // Its parent is back already unless someone removed it
                    // since; then it comes back too.
                    fs::create_dir_all(&dir).at(&dir)?;
                    report.dirs_restored += 1;
                }
            }
            Entry::Mkdir { root, path } => {
                let dir = exec.resolve(*root, path)?;
                match fs::symlink_metadata(&dir) {
                    Ok(m) if m.is_dir() && is_empty_dir(&dir)? => {
                        fs::remove_dir(&dir).at(&dir)?;
                        report.dirs_removed += 1;
                    }
                    Ok(_) => report.dirs_left += 1,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => {
                        return Err(ApplyError::Io {
                            path: dir,
                            source: e,
                        })
                    }
                }
            }
            Entry::Begin { .. }
            | Entry::Revert { .. }
            | Entry::SetAside { .. }
            | Entry::Done
            | Entry::Undone => {}
        }
    }
    exec.journal.record(&Entry::Undone)?;
    tracing::info!(
        reverted = report.reverted,
        set_aside = report.set_aside.len(),
        missing = report.missing.len(),
        dirs_restored = report.dirs_restored,
        dirs_removed = report.dirs_removed,
        dirs_left = report.dirs_left,
        "migration undone"
    );
    Ok(report)
}

/// `.u_crawler/migrations/<ts>.undo`, relative to the library root: where an
/// undo sets generated files aside instead of deleting them.
pub fn undo_dir_rel(ts: &str) -> String {
    format!("{}/{MIGRATIONS_DIR}/{ts}.{UNDO_SUFFIX}", layout::META_DIR)
}

fn migrations_dir(library: &Path) -> PathBuf {
    library.join(layout::META_DIR).join(MIGRATIONS_DIR)
}

fn valid_timestamp(ts: &str) -> bool {
    !ts.is_empty()
        && ts != "."
        && ts != ".."
        && ts
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The append-only journal of one run. A new file is created on the first
/// entry, with [`Journal::begin`] written ahead of it.
struct Journal {
    path: PathBuf,
    /// `None` when the file does not exist yet.
    state: Option<JournalState>,
    entries: Vec<Entry>,
    /// The [`Entry::Begin`] a new journal starts with.
    begin: Option<Entry>,
    file: Option<File>,
}

impl Journal {
    fn open(library: &Path, ts: &str) -> Result<Journal> {
        if !valid_timestamp(ts) {
            return Err(ApplyError::BadTimestamp(ts.to_string()));
        }
        let path = journal_path(library, ts);
        let (state, entries) = match read_journal(&path) {
            Ok(entries) => (Some(journal_state(&entries)), entries),
            Err(e) if e.kind() == io::ErrorKind::NotFound => (None, Vec::new()),
            Err(e) => return Err(ApplyError::Io { path, source: e }),
        };
        Ok(Journal {
            path,
            state,
            entries,
            begin: None,
            file: None,
        })
    }

    fn started(&self) -> bool {
        self.state.is_some() || self.file.is_some()
    }

    /// Append `entry` and sync it to disk.
    fn record(&mut self, entry: &Entry) -> Result<()> {
        if self.file.is_none() {
            self.file = Some(self.create_or_append()?);
            if let Some(begin) = self.begin.take() {
                self.write_line(&begin)?;
            }
        }
        self.write_line(entry)
    }

    fn write_line(&mut self, entry: &Entry) -> Result<()> {
        let path = &self.path;
        let mut line = serde_json::to_string(entry)
            .map_err(io::Error::other)
            .at(path)?;
        line.push('\n');
        if let Some(file) = &mut self.file {
            file.write_all(line.as_bytes()).at(path)?;
            file.sync_data().at(path)?;
        }
        self.entries.push(entry.clone());
        Ok(())
    }

    fn create_or_append(&self) -> Result<File> {
        let path = &self.path;
        let dir = path.parent().unwrap_or(path);
        if self.state.is_none() {
            fs::create_dir_all(dir).at(dir)?;
            let file = OpenOptions::new()
                .append(true)
                .create_new(true)
                .open(path)
                .at(path)?;
            sync_dir(dir)?;
            return Ok(file);
        }
        // A crash while appending can leave a torn last line: start a new one.
        let mut text = Vec::new();
        File::open(path)
            .and_then(|mut f| f.read_to_end(&mut text))
            .at(path)?;
        let mut file = OpenOptions::new().append(true).open(path).at(path)?;
        if text.last().is_some_and(|b| *b != b'\n') {
            file.write_all(b"\n").at(path)?;
        }
        Ok(file)
    }
}

/// Make a new directory entry durable.
fn sync_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(dir).and_then(|d| d.sync_all()).at(dir)?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

struct Exec<'a, 'h> {
    roots: Roots<'a>,
    journal: Journal,
    hook: &'h mut dyn FnMut(&Entry) -> io::Result<()>,
    report: Report,
    /// Sources an earlier part of this run already renamed (when resuming).
    renamed: BTreeSet<(Root, String)>,
}

impl Exec<'_, '_> {
    /// Journal `entry`, then give the hook its chance to stop the run.
    fn log(&mut self, entry: Entry) -> Result<()> {
        self.journal.record(&entry)?;
        (self.hook)(&entry).map_err(ApplyError::Interrupted)
    }

    fn root(&self, root: Root) -> Result<&Path> {
        match root {
            Root::Library => Ok(self.roots.library),
            Root::Caldir => self.roots.caldir.ok_or(ApplyError::NoCaldirRoot),
        }
    }

    /// `root/rel`, refusing anything but plain components and any existing
    /// ancestor that is a symlink.
    fn resolve(&self, root: Root, rel: &str) -> Result<PathBuf> {
        let parts: Vec<&str> = rel.split('/').collect();
        if Path::new(rel).is_absolute()
            || parts
                .iter()
                .any(|p| p.is_empty() || *p == "." || *p == ".." || p.contains('\\'))
        {
            return Err(ApplyError::BadPath(rel.to_string()));
        }
        let mut path = self.root(root)?.to_path_buf();
        let mut checking = true;
        for (i, part) in parts.iter().enumerate() {
            path.push(part);
            if checking && i + 1 < parts.len() {
                match fs::symlink_metadata(&path) {
                    Ok(m) if m.file_type().is_symlink() => return Err(ApplyError::Symlink(path)),
                    Ok(_) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => checking = false,
                    Err(e) => return Err(ApplyError::Io { path, source: e }),
                }
            }
        }
        Ok(path)
    }

    /// Create, one journaled level at a time, the missing parents of `rel`.
    fn mkdirs(&mut self, root: Root, rel: &str) -> Result<()> {
        let Some((parent, _)) = rel.rsplit_once('/') else {
            return Ok(());
        };
        let mut prefix = String::new();
        for part in parent.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            let dir = self.resolve(root, &prefix)?;
            match fs::symlink_metadata(&dir) {
                Ok(m) if m.is_dir() => continue,
                Ok(_) => {
                    return Err(ApplyError::Io {
                        path: dir,
                        source: io::Error::from(io::ErrorKind::NotADirectory),
                    })
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(ApplyError::Io {
                        path: dir,
                        source: e,
                    })
                }
            }
            self.log(Entry::Mkdir {
                root,
                path: prefix.clone(),
            })?;
            fs::create_dir(&dir).at(&dir)?;
        }
        Ok(())
    }

    /// Journal and perform one rename.
    fn rename(&mut self, root: Root, from: &str, to: &str) -> Result<()> {
        self.mkdirs(root, to)?;
        let src = self.resolve(root, from)?;
        let dst = self.resolve(root, to)?;
        self.log(Entry::Rename {
            root,
            from: from.to_string(),
            to: to.to_string(),
        })?;
        tracing::debug!(%root, from, to, "rename");
        rename_noreplace(&src, &dst)
    }

    /// Check that `from` is still there and of the planned kind. `false` when
    /// it is gone because this run already renamed it.
    fn source(&self, root: Root, from: &str, kind: EntryKind) -> Result<bool> {
        let src = self.resolve(root, from)?;
        match fs::symlink_metadata(&src) {
            Ok(m)
                if (kind == EntryKind::File && m.is_file())
                    || (kind == EntryKind::Dir && m.is_dir()) =>
            {
                Ok(true)
            }
            Ok(_) => Err(ApplyError::WrongKind(src)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if self.renamed.contains(&(root, from.to_string())) {
                    Ok(false)
                } else {
                    Err(ApplyError::SourceMissing(src))
                }
            }
            Err(e) => Err(ApplyError::Io {
                path: src,
                source: e,
            }),
        }
    }

    /// Move the file `from` to `to`, or compare it with what is already there.
    fn file_to(&mut self, root: Root, from: &str, to: &str) -> Result<()> {
        if !self.source(root, from, EntryKind::File)? {
            return Ok(());
        }
        let src = self.resolve(root, from)?;
        let dst = self.resolve(root, to)?;
        let placed = Placed {
            root,
            from: from.to_string(),
            to: to.to_string(),
        };
        match fs::symlink_metadata(&dst) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.rename(root, from, to)?;
                self.report.moved.push(placed);
            }
            Ok(m) if m.is_file() => self.collide(placed, &src, &dst)?,
            Ok(_) => {
                tracing::warn!(%root, from, to, "destination is not a file; source left in place");
                self.report.skipped.push(placed);
            }
            Err(e) => {
                return Err(ApplyError::Io {
                    path: dst,
                    source: e,
                })
            }
        }
        Ok(())
    }

    /// The destination exists: the same bytes leave the source in place,
    /// other bytes send it to its conflict name.
    fn collide(&mut self, placed: Placed, src: &Path, dst: &Path) -> Result<()> {
        let Placed { root, from, to } = &placed;
        let src_hash = blake3_file(src)?;
        if same_content(src, &src_hash, dst)? {
            tracing::warn!(%root, from, to, "identical file already at destination; source left in place");
            self.report.duplicates.push(placed);
            return Ok(());
        }
        let hex = src_hash.to_hex();
        let name = conflict_name(to, &hex.as_str()[..8]);
        let target = self.resolve(*root, &name)?;
        let renamed = Placed {
            root: *root,
            from: from.clone(),
            to: name.clone(),
        };
        match fs::symlink_metadata(&target) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                tracing::warn!(%root, from, to = %name, "different file at destination; kept under a conflict name");
                self.rename(*root, from, &name)?;
                self.report.conflicts.push(renamed);
            }
            Ok(m) if m.is_file() && same_content(src, &src_hash, &target)? => {
                self.report.duplicates.push(renamed);
            }
            Ok(_) => return Err(ApplyError::ConflictNameTaken(target)),
            Err(e) => {
                return Err(ApplyError::Io {
                    path: target,
                    source: e,
                })
            }
        }
        Ok(())
    }

    fn dir_to(&mut self, root: Root, from: &str, to: &str) -> Result<()> {
        if !self.source(root, from, EntryKind::Dir)? {
            return Ok(());
        }
        let placed = Placed {
            root,
            from: from.to_string(),
            to: to.to_string(),
        };
        if exists(&self.resolve(root, to)?)? {
            tracing::warn!(%root, from, to, "destination directory exists; source left in place");
            self.report.skipped.push(placed);
            return Ok(());
        }
        self.rename(root, from, to)?;
        self.report.moved.push(placed);
        Ok(())
    }

    /// Remove every empty directory under the v1 directory `rel`, bottom-up,
    /// `rel` itself included, then each empty parent up to the library root.
    /// A directory with anything left in it is kept and reported.
    fn prune(&mut self, rel: &str) -> Result<()> {
        let dir = self.resolve(Root::Library, rel)?;
        match fs::symlink_metadata(&dir) {
            Ok(m) if m.is_dir() => {}
            // Gone already (a resumed run), or not ours to walk.
            _ => return Ok(()),
        }
        if !self.prune_tree(rel)? {
            tracing::warn!(path = rel, "v1 directory not empty; left in place");
            self.report.kept_dirs.push(rel.to_string());
            return Ok(());
        }
        let mut parent = rel;
        while let Some((up, _)) = parent.rsplit_once('/') {
            let dir = self.resolve(Root::Library, up)?;
            if !exists(&dir)? || !is_empty_dir(&dir)? {
                break;
            }
            self.rmdir(up)?;
            parent = up;
        }
        Ok(())
    }

    /// Prune below `rel` and then `rel`. `true` when `rel` was removed.
    fn prune_tree(&mut self, rel: &str) -> Result<bool> {
        let dir = self.resolve(Root::Library, rel)?;
        let mut children = Vec::new();
        for entry in fs::read_dir(&dir).at(&dir)? {
            let entry = entry.at(&dir)?;
            // `file_type` does not follow symlinks: a symlink is content.
            if entry.file_type().at(&entry.path())?.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    children.push(format!("{rel}/{name}"));
                }
            }
        }
        children.sort();
        for child in children {
            self.prune_tree(&child)?;
        }
        if !is_empty_dir(&dir)? {
            return Ok(false);
        }
        self.rmdir(rel)?;
        Ok(true)
    }

    /// Journal and remove one empty directory. `rmdir(2)` refuses one that
    /// is not empty, so nothing can be lost here.
    fn rmdir(&mut self, rel: &str) -> Result<()> {
        let dir = self.resolve(Root::Library, rel)?;
        self.log(Entry::Rmdir {
            root: Root::Library,
            path: rel.to_string(),
        })?;
        tracing::debug!(path = rel, "rmdir");
        fs::remove_dir(&dir).at(&dir)?;
        self.report.removed_dirs.push(rel.to_string());
        Ok(())
    }

    /// Write `<dir>/curso.json` when the course directory exists and has none.
    fn curso_json(&mut self, course: &IntentCourse) -> Result<()> {
        let rel = format!("{}/{CURSO_JSON}", course.dir);
        let dir = self.resolve(Root::Library, &course.dir)?;
        let path = self.resolve(Root::Library, &rel)?;
        let is_dir = fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir());
        if !is_dir || exists(&path)? {
            return Ok(());
        }
        let text = index::render_curso_json(&CourseIndex {
            dir: course.dir.clone(),
            course: CourseMeta {
                id: course.canvas_id,
                name: course.name.clone(),
                code: course.code.clone(),
            },
            zoom_ids: course.zoom_ids.clone(),
            ..CourseIndex::default()
        });
        self.log(Entry::Write { path: rel.clone() })?;
        // The temporary name is ours; a cut run may have left it behind.
        let tmp = path.with_file_name(format!("{CURSO_JSON}.{TMP_SUFFIX}"));
        let mut file = File::create(&tmp).at(&tmp)?;
        file.write_all(text.as_bytes()).at(&tmp)?;
        file.sync_all().at(&tmp)?;
        rename_noreplace(&tmp, &path)?;
        self.report.written.push(rel);
        Ok(())
    }
}

fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(ApplyError::Io {
            path: path.to_path_buf(),
            source: e,
        }),
    }
}

fn is_empty_dir(dir: &Path) -> Result<bool> {
    Ok(fs::read_dir(dir).at(dir)?.next().is_none())
}

fn blake3_file(path: &Path) -> Result<blake3::Hash> {
    let mut hasher = blake3::Hasher::new();
    File::open(path)
        .and_then(|f| hasher.update_reader(f).map(|_| ()))
        .at(path)?;
    Ok(hasher.finalize())
}

/// Same size and same BLAKE3. `other` is only hashed when the sizes match.
fn same_content(src: &Path, src_hash: &blake3::Hash, other: &Path) -> Result<bool> {
    let a = fs::metadata(src).at(src)?.len();
    let b = fs::metadata(other).at(other)?.len();
    Ok(a == b && blake3_file(other)? == *src_hash)
}

/// `rename(2)` that never replaces `to` and never copies.
pub(crate) fn rename_noreplace(from: &Path, to: &Path) -> Result<()> {
    match rename_os(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::CrossesDevices => Err(ApplyError::CrossDevice {
            from: from.to_path_buf(),
            to: to.to_path_buf(),
        }),
        Err(e) => Err(ApplyError::Io {
            path: to.to_path_buf(),
            source: e,
        }),
    }
}

#[cfg(target_os = "linux")]
fn rename_os(from: &Path, to: &Path) -> io::Result<()> {
    use rustix::fs::{renameat_with, RenameFlags, CWD};
    use rustix::io::Errno;
    match renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE) {
        // The filesystem (or kernel) does not support the flag.
        Err(Errno::INVAL | Errno::NOSYS | Errno::NOTSUP) => rename_checked(from, to),
        other => other.map_err(io::Error::from),
    }
}

#[cfg(not(target_os = "linux"))]
fn rename_os(from: &Path, to: &Path) -> io::Result<()> {
    rename_checked(from, to)
}

/// Check, then rename. Racy against a concurrent writer, which the library
/// does not have; used only where `RENAME_NOREPLACE` is unavailable.
fn rename_checked(from: &Path, to: &Path) -> io::Result<()> {
    match fs::symlink_metadata(to) {
        Ok(_) => Err(io::Error::from(io::ErrorKind::AlreadyExists)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => fs::rename(from, to),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn conflict_name_goes_before_the_last_extension() {
        assert_eq!(
            conflict_name("a/b/guia-1.pdf", "0123abcd"),
            "a/b/guia-1.conflicto-0123abcd.pdf"
        );
        assert_eq!(conflict_name("a/x.tar.gz", "h"), "a/x.tar.conflicto-h.gz");
        assert_eq!(conflict_name("a/LEEME", "h"), "a/LEEME.conflicto-h");
        assert_eq!(conflict_name(".nota", "h"), ".nota.conflicto-h");
        assert_eq!(conflict_name("x.md", "h"), "x.conflicto-h.md");
    }

    #[test]
    fn timestamps_are_one_safe_segment_and_sort_by_time() {
        let at = |ms| new_timestamp(DateTime::from_timestamp_millis(ms).unwrap_or_default());
        let (a, b) = (at(1_790_000_000_123), at(1_790_000_000_124));
        assert_eq!(a, "20260921T141320123Z");
        assert!(valid_timestamp(&a));
        assert!(a < b);
        for bad in ["", ".", "..", "a/b", "a b"] {
            assert!(!valid_timestamp(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_rename_never_replaces_its_destination() -> TestResult {
        let dir = tempfile::tempdir()?;
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, b"new")?;
        fs::write(&b, b"old")?;
        assert!(rename_noreplace(&a, &b).is_err());
        assert!(rename_checked(&a, &b).is_err());
        assert_eq!(fs::read(&b)?, b"old");
        assert_eq!(fs::read(&a)?, b"new");
        Ok(())
    }

    /// `/dev/shm` is a tmpfs on most Linux systems. When it is a different
    /// filesystem from the temp dir, a rename between them must stop with
    /// `CrossDevice` and leave the source alone; otherwise there is nothing
    /// to test here.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_cross_device_rename_stops_without_copying() -> TestResult {
        use std::os::unix::fs::MetadataExt;
        let here = tempfile::tempdir()?;
        let Ok(there) = tempfile::tempdir_in("/dev/shm") else {
            return Ok(());
        };
        if fs::metadata(here.path())?.dev() == fs::metadata(there.path())?.dev() {
            return Ok(());
        }
        let from = here.path().join("a.mp4");
        fs::write(&from, b"irreplaceable")?;
        let to = there.path().join("a.mp4");
        assert!(matches!(
            rename_noreplace(&from, &to),
            Err(ApplyError::CrossDevice { .. })
        ));
        assert_eq!(fs::read(&from)?, b"irreplaceable");
        assert!(!to.exists());
        Ok(())
    }

    #[test]
    fn a_torn_last_line_is_skipped() -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("j.jsonl");
        fs::write(
            &path,
            "{\"op\":\"mkdir\",\"root\":\"library\",\"path\":\"a\"}\n{\"op\":\"ren",
        )?;
        let entries = read_journal(&path)?;
        assert_eq!(
            entries,
            [Entry::Mkdir {
                root: Root::Library,
                path: "a".into()
            }]
        );
        assert_eq!(journal_state(&entries), JournalState::Open);
        Ok(())
    }

    #[test]
    fn journal_states_follow_the_last_phase() {
        let rename = Entry::Rename {
            root: Root::Library,
            from: "a".into(),
            to: "b".into(),
        };
        let revert = Entry::Revert {
            root: Root::Library,
            from: "b".into(),
            to: "a".into(),
        };
        assert_eq!(
            journal_state(std::slice::from_ref(&rename)),
            JournalState::Open
        );
        assert_eq!(
            journal_state(&[rename.clone(), Entry::Done]),
            JournalState::Done
        );
        assert_eq!(
            journal_state(&[rename.clone(), Entry::Done, revert.clone()]),
            JournalState::Undoing
        );
        assert_eq!(
            journal_state(&[rename, revert, Entry::Undone]),
            JournalState::Undone
        );
    }

    #[test]
    fn paths_that_leave_the_root_or_cross_a_symlink_are_refused() -> TestResult {
        let dir = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        let journal = Journal::open(dir.path(), "t")?;
        let exec = Exec {
            roots: Roots {
                library: dir.path(),
                caldir: None,
            },
            journal,
            hook: &mut |_| Ok(()),
            report: Report::default(),
            renamed: BTreeSet::new(),
        };
        for bad in ["", "/etc/passwd", "a/../b", "./a", "a//b", "a\\b"] {
            assert!(
                matches!(
                    exec.resolve(Root::Library, bad),
                    Err(ApplyError::BadPath(_))
                ),
                "{bad:?}"
            );
        }
        assert!(matches!(
            exec.resolve(Root::Caldir, "a"),
            Err(ApplyError::NoCaldirRoot)
        ));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), dir.path().join("link"))?;
            assert!(matches!(
                exec.resolve(Root::Library, "link/x.mp4"),
                Err(ApplyError::Symlink(_))
            ));
        }
        assert!(exec.resolve(Root::Library, "missing/deeper/x.mp4").is_ok());
        Ok(())
    }
}
