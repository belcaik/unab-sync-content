//! The pure migration planner: an [`Inventory`] (plus optional Canvas
//! metadata) in, a [`Plan`] out. No I/O, no clock, no randomness: the same
//! input always gives the same plan, with every list sorted.
//!
//! What goes where (v1 → v2, inside the course directory unless noted):
//!
//! | v1 | v2 |
//! |---|---|
//! | `state.json` | `.u_crawler/state.json` |
//! | `Modules/<id>_<Name>/NN-<Title>.md` | `modulos/<module_dir>/<page_file>` |
//! | `Modules/<id>_<Name>/NN-ASSIGN-<Title>.md` | `tareas/<assignment_file>` |
//! | `Modules/<id>_<Name>/Attachments/<file>` | `modulos/<module_dir>/adjuntos/<attachment_file>` |
//! | `announcements/<date>_<slug>_<id>.md` | `anuncios/<announcement_file>` |
//! | `announcements/index.json` | `anuncios/index.json` |
//! | `announcements/media/<file>` | `anuncios/adjuntos/<attachment_file>` |
//! | `<root>/Zoom/<id>/<YYYY_MM_DD_topic[_N]>.mp4` | `grabaciones/<recording_file>` |
//! | `<caldir_root>/<v1 dir>/` | `<caldir_root>/<v2 dir>/` (one directory rename) |
//!
//! Anything else is foreign and travels with the folder it sits in, under its
//! own name: a note in a module directory lands in the module's v2 directory,
//! a file at the top of a course directory at the top of the v2 one, an
//! unknown folder under `announcements/` under `anuncios/`. Files outside v1
//! course directories and `Zoom/` (the library root, `.obsidian/`, ...) never
//! move; `.git` is not even in the inventory.
//!
//! A course's Canvas id comes from its `announcements/index.json`. With
//! Canvas metadata ([`CanvasCourse`]), a course without one is matched by
//! name — `fsutil::course_dir` of the Canvas course equals the v1 directory —
//! and the official name and code, and the module positions, replace what
//! the directory name gives. Offline, modules are numbered in `module_id`
//! order and name and code come from [`CourseNaming::parse_v1_course_dir`].
//!
//! A `Zoom/<id>/` folder belongs to the course whose Canvas id is `<id>`.
//! Failing that, to the one course its recording names point at — by
//! section (NRC), then by short code, then by every word of the short name.
//! A folder matching no course, or more than one at the first tier that
//! matches anything, is [`Reason::OrphanZoom`] / [`Reason::AmbiguousZoom`]
//! and stays where it is.
//!
//! Two v1 notes that are different Canvas items and want the same name —
//! two announcements with one date and title (their v1 names carry
//! different ids), two assignments with one title and different `NN`
//! prefixes — are not a collision: [`layout::unique_names`] numbers them in
//! id / `NN` order (`tarea.md`, `tarea-2.md`), as `sync` would.
//!
//! Two sources of the same item with the same destination, or a destination
//! that already exists, are a [`Collision`]: same sizes go to
//! [`Plan::duplicates`] (identical unless BLAKE3 says otherwise), different
//! sizes to [`Plan::conflicts`]. The executor hashes and decides; the
//! planner never reads file contents.
//!
//! [`Plan::prune`] lists the v1 directories the migration empties; the
//! executor removes what is left of them only where it is empty.

use super::detect::{is_assignment_file, module_id, numeric};
use super::{
    Inventory, INDEX_JSON, V1_ANNOUNCEMENTS_DIR, V1_ATTACHMENTS_DIR, V1_MEDIA_DIR, V1_MODULES_DIR,
    V1_STATE_FILE, V1_ZOOM_DIR,
};
use crate::canvas::Course;
use crate::fsutil;
use crate::layout::{self, CourseMeta, CourseNaming, ExistingCourseDir};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

/// What Canvas says about one course, when the migrator may ask it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CanvasCourse {
    /// The official Canvas id, name and code (`id` should be `Some`).
    pub meta: CourseMeta,
    /// Canvas `position` of each module, by module id.
    pub module_positions: BTreeMap<u64, u32>,
}

/// The two trees a plan touches. Journaled by [`super::apply`] as
/// `"library"` / `"caldir"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Root {
    /// `download_root`.
    Library,
    /// `calendar.caldir_root`.
    Caldir,
}

/// Whether a [`Move`] renames one file or a whole directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    /// A regular file.
    File,
    /// A directory, renamed with everything in it.
    Dir,
}

/// One `rename(2)`. `to` does not exist and nothing else in the plan targets it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Move {
    /// The tree both paths are relative to.
    pub root: Root,
    /// File or directory.
    pub kind: EntryKind,
    /// Current path.
    pub from: String,
    /// Path after the move. Its parent directories may not exist yet.
    pub to: String,
}

/// Which kind of u_crawler-generated markdown an [`Edit`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EditKind {
    /// A module page (`NN-<Title>.md`).
    Page,
    /// An assignment description (`NN-ASSIGN-<Title>.md`).
    Assignment,
    /// An announcement body.
    Announcement,
}

/// A generated markdown file to rewrite (frontmatter, local links) after its
/// [`Move`]. Only cleanly moved files get one; the path is the v2 path.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Edit {
    /// v2 path, relative to the library root.
    pub path: String,
    /// What the file is.
    pub kind: EditKind,
    /// The v2 course directory it belongs to.
    pub course: String,
}

/// Several sources for one destination, or a destination that already exists.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Collision {
    /// The tree the paths are relative to (always [`Root::Library`] today).
    pub root: Root,
    /// The contested destination.
    pub to: String,
    /// Every source that wants `to`, sorted. When `to` is free the executor
    /// may move the first one there; the rest are compared against it.
    pub from: Vec<String>,
    /// `to` is already a file in the library.
    pub occupied: bool,
}

/// Something the planner will not move, and why. It stays where it is.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Unresolved {
    /// The tree `path` is relative to.
    pub root: Root,
    /// The file or directory left in place.
    pub path: String,
    /// Why.
    pub reason: Reason,
}

/// Why an [`Unresolved`] entry was left alone.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reason {
    /// A `Zoom/<id>/` folder that matches no course.
    OrphanZoom,
    /// A `Zoom/<id>/` folder whose recordings point at more than one course
    /// (the v2 directories, sorted).
    AmbiguousZoom(Vec<String>),
    /// Something under `Zoom/` that is not inside a numeric folder.
    UnrecognizedZoomEntry,
    /// The v2 calendar directory already exists.
    CaldirTargetExists(String),
    /// The destination is an existing directory, or sits under an existing
    /// file, so no rename can put a file there.
    DestinationBlocked(String),
}

/// One v1 course and where it goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedCourse {
    /// The v1 directory name.
    pub v1_dir: String,
    /// The v2 directory name ([`CourseNaming::course_dirs`]).
    pub dir: String,
    /// What is known about the course: `id` is `None` without an
    /// `index.json` or a Canvas match.
    pub course: CourseMeta,
    /// The `Zoom/<id>` folders migrated into it, ascending.
    pub zoom_ids: Vec<u64>,
}

/// Everything a migration would do, and everything it would not.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// v1 courses, by v1 directory.
    pub courses: Vec<PlannedCourse>,
    /// Clean renames.
    pub moves: Vec<Move>,
    /// Generated markdown to rewrite once moved.
    pub edits: Vec<Edit>,
    /// Collisions where every file has the same size: likely identical.
    pub duplicates: Vec<Collision>,
    /// Collisions with different sizes: certainly different content.
    pub conflicts: Vec<Collision>,
    /// Left in place.
    pub unresolved: Vec<Unresolved>,
    /// v1 directories (library root) the moves empty, sorted: every migrated
    /// v1 course directory and every mapped `Zoom/<id>`. The executor
    /// removes each empty directory under them, bottom-up, then their empty
    /// parents (`Zoom/`); anything with a file left in it stays.
    pub prune: Vec<String>,
}

impl Plan {
    /// Nothing to move, rewrite or compare. [`Plan::unresolved`] may still
    /// list things left in place: a migrated library keeps its orphan
    /// `Zoom/<id>/`, and re-planning it must still count as having nothing to do.
    pub fn is_empty(&self) -> bool {
        self.moves.is_empty()
            && self.edits.is_empty()
            && self.duplicates.is_empty()
            && self.conflicts.is_empty()
    }
}

/// Plan the migration of the library `inv` was scanned from.
///
/// `canvas` is optional metadata (`[]` offline): one entry per Canvas course
/// the caller could look up.
pub fn plan(inv: &Inventory, naming: &CourseNaming, canvas: &[CanvasCourse]) -> Plan {
    let mut planner = Planner {
        inv,
        candidates: BTreeMap::new(),
        caldir_moves: Vec::new(),
        unresolved: Vec::new(),
        prune: Vec::new(),
    };
    let mut courses = resolve_courses(inv, naming, canvas);
    let zoom = planner.map_zoom(naming, &mut courses);
    for course in &courses {
        planner.course_files(course);
        if course.planned.v1_dir != course.planned.dir {
            planner.prune.push(course.planned.v1_dir.clone());
        }
    }
    for (folder, course) in &zoom {
        planner.zoom_files(*folder, &courses[*course]);
        planner.prune.push(format!("{V1_ZOOM_DIR}/{folder}"));
    }
    planner.caldir(&courses);
    planner.finish(courses)
}

/// A v1 course while planning.
struct Resolved {
    planned: PlannedCourse,
    module_positions: BTreeMap<u64, u32>,
}

fn resolve_courses(
    inv: &Inventory,
    naming: &CourseNaming,
    canvas: &[CanvasCourse],
) -> Vec<Resolved> {
    let v1_dirs = inv.v1_course_dirs();
    let mut metas = Vec::with_capacity(v1_dirs.len());
    let mut positions = Vec::with_capacity(v1_dirs.len());
    for dir in &v1_dirs {
        let id = inv.course_ids.get(*dir).copied();
        let by_id = id.and_then(|id| canvas.iter().find(|c| c.meta.id == Some(id)));
        let by_name = || {
            let mut named = canvas
                .iter()
                .filter(|c| c.meta.id.is_some() && v1_dir_of(&c.meta).as_deref() == Some(*dir));
            match (named.next(), named.next()) {
                (Some(only), None) => Some(only),
                _ => None,
            }
        };
        let matched = if id.is_some() { by_id } else { by_name() };
        let meta = match matched {
            Some(c) => c.meta.clone(),
            None => CourseMeta {
                id,
                ..naming.parse_v1_course_dir(dir)
            },
        };
        positions.push(
            matched
                .map(|c| c.module_positions.clone())
                .unwrap_or_default(),
        );
        metas.push(meta);
    }

    // Every name already at the root is taken, so a course never lands in a
    // folder it does not own. v1 course directories are not taken: each is
    // emptied by its own migration, and a course may keep its name. Neither
    // is a v2 course directory without its `curso.json` (it has
    // `.u_crawler/state.json`): that is a migration cut short, and re-planning
    // must send the course back into it rather than split it in two.
    let mut existing = inv.existing.clone();
    let owned: BTreeSet<&str> = existing.iter().map(|e| e.dir.as_str()).collect();
    let reserved: Vec<ExistingCourseDir> = inv
        .root_names()
        .into_iter()
        .filter(|n| !v1_dirs.contains(n) && !owned.contains(n))
        .filter(|n| {
            !inv.files
                .contains_key(&format!("{n}/{}/{V1_STATE_FILE}", layout::META_DIR))
        })
        .map(|n| ExistingCourseDir {
            dir: n.to_string(),
            course: CourseMeta::default(),
        })
        .collect();
    existing.extend(reserved);
    let dirs = naming.course_dirs(&existing, &metas);

    v1_dirs
        .iter()
        .zip(metas)
        .zip(dirs)
        .zip(positions)
        .map(|(((v1_dir, course), dir), module_positions)| Resolved {
            planned: PlannedCourse {
                v1_dir: (*v1_dir).to_string(),
                dir,
                course,
                zoom_ids: Vec::new(),
            },
            module_positions,
        })
        .collect()
}

/// The v1 directory name `fsutil::course_dir` gave this course.
fn v1_dir_of(meta: &CourseMeta) -> Option<String> {
    let course = Course {
        id: meta.id.unwrap_or_default(),
        name: meta.name.clone(),
        course_code: meta.code.clone(),
    };
    fsutil::course_dir(Path::new(""), &course)
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
}

/// A file proposed for a destination, with the edit it carries once moved.
struct Source {
    from: String,
    /// Edit kind and v2 course directory.
    edit: Option<(EditKind, String)>,
    /// [`Dest::item`].
    item: Option<u64>,
}

/// Where a v1 file goes.
struct Dest {
    to: String,
    edit: Option<EditKind>,
    /// Which Canvas item a generated note is, when its v1 name says: an
    /// announcement's id, an assignment's `NN`. Sources of one destination
    /// with different items are different items, numbered apart.
    item: Option<u64>,
}

impl Dest {
    fn plain(to: String) -> Dest {
        Dest {
            to,
            edit: None,
            item: None,
        }
    }
}

/// One course-matching rule of [`match_by_tokens`].
/// Its second argument is a topic's words, then those words minus the date's.
type Tier<'a> = &'a dyn Fn(&CourseMeta, &(BTreeSet<String>, BTreeSet<String>)) -> bool;

struct Planner<'a> {
    inv: &'a Inventory,
    /// Library destination → the files that want it.
    candidates: BTreeMap<String, Vec<Source>>,
    caldir_moves: Vec<Move>,
    unresolved: Vec<Unresolved>,
    prune: Vec<String>,
}

impl Planner<'_> {
    /// `dest` for the file `from` of the v2 course `course`.
    fn propose(&mut self, from: &str, dest: Dest, course: &str) {
        if from == dest.to {
            return;
        }
        self.candidates.entry(dest.to).or_default().push(Source {
            from: from.to_string(),
            edit: dest.edit.map(|k| (k, course.to_string())),
            item: dest.item,
        });
    }

    fn files_under<'b>(inv: &'b Inventory, dir: &str) -> impl Iterator<Item = (&'b str, &'b str)> {
        let prefix = format!("{dir}/");
        inv.files
            .keys()
            .filter_map(move |p| p.strip_prefix(&prefix).map(|rest| (p.as_str(), rest)))
    }

    /// `Zoom/<id>` folder → index into `courses`, for the folders that map.
    fn map_zoom(
        &mut self,
        naming: &CourseNaming,
        courses: &mut [Resolved],
    ) -> BTreeMap<u64, usize> {
        let mut folders: BTreeMap<u64, Vec<&str>> = BTreeMap::new();
        let mut stray = BTreeSet::new();
        for (path, rest) in Self::files_under(self.inv, V1_ZOOM_DIR) {
            match rest.split_once('/') {
                Some((id, file)) => match numeric(id) {
                    Some(id) => {
                        let names = folders.entry(id).or_default();
                        if !file.contains('/') {
                            names.push(file);
                        }
                    }
                    None => {
                        stray.insert(format!("{V1_ZOOM_DIR}/{id}"));
                    }
                },
                None => {
                    stray.insert(path.to_string());
                }
            }
        }
        for path in stray {
            self.unresolved.push(Unresolved {
                root: Root::Library,
                path,
                reason: Reason::UnrecognizedZoomEntry,
            });
        }

        let mut mapped = BTreeMap::new();
        for (folder, names) in folders {
            let by_id = courses
                .iter()
                .position(|c| c.planned.course.id == Some(folder));
            let found = match by_id {
                Some(i) => Ok(i),
                None => match_by_tokens(naming, courses, &names),
            };
            let path = format!("{V1_ZOOM_DIR}/{folder}");
            match found {
                Ok(i) => {
                    courses[i].planned.zoom_ids.push(folder);
                    mapped.insert(folder, i);
                }
                Err(candidates) if candidates.is_empty() => self.unresolved.push(Unresolved {
                    root: Root::Library,
                    path,
                    reason: Reason::OrphanZoom,
                }),
                Err(candidates) => self.unresolved.push(Unresolved {
                    root: Root::Library,
                    path,
                    reason: Reason::AmbiguousZoom(
                        candidates
                            .iter()
                            .map(|&i| courses[i].planned.dir.clone())
                            .collect::<BTreeSet<_>>()
                            .into_iter()
                            .collect(),
                    ),
                }),
            }
        }
        mapped
    }

    fn course_files(&mut self, resolved: &Resolved) {
        let course = &resolved.planned;
        let v2 = course.dir.as_str();
        let module_dirs = module_dirs(self.inv, &course.v1_dir, &resolved.module_positions);
        let files: Vec<(&str, &str)> = Self::files_under(self.inv, &course.v1_dir).collect();
        for (path, rel) in files {
            let parts: Vec<&str> = rel.split('/').collect();
            let dest = match parts.as_slice() {
                [V1_STATE_FILE] => Dest::plain(join(&[v2, layout::META_DIR, V1_STATE_FILE])),
                [V1_MODULES_DIR, module, rest @ ..] if module_dirs.contains_key(*module) => {
                    let mdir = join(&[v2, layout::MODULES_DIR, &module_dirs[*module]]);
                    module_file(v2, &mdir, rest)
                }
                [V1_MODULES_DIR, rest @ ..] => {
                    Dest::plain(join(&[v2, layout::MODULES_DIR, &rest.join("/")]))
                }
                [V1_ANNOUNCEMENTS_DIR, rest @ ..] => announcement_file(v2, rest),
                _ => Dest::plain(join(&[v2, rel])),
            };
            self.propose(path, dest, v2);
        }
    }

    fn zoom_files(&mut self, folder: u64, resolved: &Resolved) {
        let course = &resolved.planned;
        let dir = format!("{V1_ZOOM_DIR}/{folder}");
        let files: Vec<(&str, &str)> = Self::files_under(self.inv, &dir).collect();
        let siblings: Vec<&str> = files
            .iter()
            .map(|(_, rel)| *rel)
            .filter(|rel| !rel.contains('/'))
            .collect();
        for (path, rel) in &files {
            let name = if rel.contains('/') {
                (*rel).to_string()
            } else if let Some(rec) = layout::parse_v1_recording(rel, &siblings) {
                layout::recording_file(rec.date, &rec.topic, rec.part, &course.course.name)
            } else if rel.ends_with(".mp4") {
                layout::attachment_file(rel)
            } else {
                (*rel).to_string()
            };
            self.propose(
                path,
                Dest::plain(join(&[&course.dir, layout::RECORDINGS_DIR, &name])),
                &course.dir,
            );
        }
    }

    fn caldir(&mut self, courses: &[Resolved]) {
        let taken = &self.inv.caldir_dirs;
        for c in courses.iter().map(|c| &c.planned) {
            if !taken.contains(&c.v1_dir) || c.v1_dir == c.dir {
                continue;
            }
            if taken.contains(&c.dir) {
                self.unresolved.push(Unresolved {
                    root: Root::Caldir,
                    path: c.v1_dir.clone(),
                    reason: Reason::CaldirTargetExists(c.dir.clone()),
                });
            } else {
                self.caldir_moves.push(Move {
                    root: Root::Caldir,
                    kind: EntryKind::Dir,
                    from: c.v1_dir.clone(),
                    to: c.dir.clone(),
                });
            }
        }
    }

    /// Give each Canvas item of a contested destination its own name: the
    /// sources of one destination are grouped by [`Source::item`], the
    /// lowest item keeps the destination, and the others get the names
    /// [`layout::unique_names`] gives them, free in the plan and on disk.
    /// Sources of one item stay together, a collision as before.
    fn number_distinct_items(&mut self) {
        let contested: Vec<String> = self
            .candidates
            .iter()
            .filter(|(_, sources)| {
                let items: BTreeSet<Option<u64>> = sources.iter().map(|s| s.item).collect();
                items.len() > 1
            })
            .map(|(to, _)| to.clone())
            .collect();
        for to in contested {
            let Some(sources) = self.candidates.remove(&to) else {
                continue;
            };
            let mut items: BTreeMap<Option<u64>, Vec<Source>> = BTreeMap::new();
            for source in sources {
                items.entry(source.item).or_default().push(source);
            }
            let (dir, name) = match to.rsplit_once('/') {
                Some((dir, name)) => (format!("{dir}/"), name),
                None => (String::new(), to.as_str()),
            };
            let wanted = vec![name.to_string(); items.len()];
            let taken = |n: &str| {
                let path = format!("{dir}{n}");
                self.candidates.contains_key(&path) || self.inv.files.contains_key(&path)
            };
            let names = layout::unique_names(&wanted, &taken);
            for (name, sources) in names.into_iter().zip(items.into_values()) {
                self.candidates
                    .entry(format!("{dir}{name}"))
                    .or_default()
                    .extend(sources);
            }
        }
    }

    fn finish(mut self, courses: Vec<Resolved>) -> Plan {
        self.number_distinct_items();
        let inv = self.inv;
        // Every directory a file sits in, now or after the plan: a file can
        // never be renamed onto one.
        let dirs: BTreeSet<&str> = inv
            .files
            .keys()
            .chain(self.candidates.keys())
            .flat_map(|p| ancestors(p))
            .collect();
        let mut plan = Plan {
            courses: courses.into_iter().map(|c| c.planned).collect(),
            moves: self.caldir_moves,
            unresolved: self.unresolved,
            prune: self.prune,
            ..Plan::default()
        };
        for (to, sources) in &self.candidates {
            let blocked =
                dirs.contains(to.as_str()) || ancestors(to).any(|a| inv.files.contains_key(a));
            if blocked {
                for source in sources {
                    plan.unresolved.push(Unresolved {
                        root: Root::Library,
                        path: source.from.clone(),
                        reason: Reason::DestinationBlocked(to.clone()),
                    });
                }
                continue;
            }
            let occupied = inv.files.get(to);
            if let ([source], None) = (sources.as_slice(), occupied) {
                plan.moves.push(Move {
                    root: Root::Library,
                    kind: EntryKind::File,
                    from: source.from.clone(),
                    to: to.clone(),
                });
                if let Some((kind, course)) = &source.edit {
                    plan.edits.push(Edit {
                        path: to.clone(),
                        kind: *kind,
                        course: course.clone(),
                    });
                }
                continue;
            }
            let mut from: Vec<String> = sources.iter().map(|s| s.from.clone()).collect();
            from.sort();
            let sizes: BTreeSet<Option<u64>> = from
                .iter()
                .map(|f| inv.files.get(f).copied())
                .chain(occupied.map(|s| Some(*s)))
                .collect();
            let collision = Collision {
                root: Root::Library,
                to: to.clone(),
                from,
                occupied: occupied.is_some(),
            };
            if sizes.len() == 1 {
                plan.duplicates.push(collision);
            } else {
                plan.conflicts.push(collision);
            }
        }
        plan.moves.sort();
        plan.edits.sort();
        plan.duplicates.sort();
        plan.conflicts.sort();
        plan.unresolved.sort();
        plan.prune.sort();
        plan
    }
}

/// The proper ancestors of a `/`-separated relative path, innermost last.
fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/').map(move |(i, _)| &path[..i])
}

/// The one course a `Zoom/<id>/` folder's recording names point at.
///
/// Only courses with no known Canvas id take part: v1 named the folder after
/// the course id, so a course whose id is known and is not `<id>` cannot own it.
///
/// Tiers, strongest first: the course section (NRC) as a word of a topic,
/// the short code as a word, every word of the short name. Each tier that
/// matches anything narrows the candidates; a tier that matches only courses
/// already ruled out means the names disagree, and the folder is ambiguous.
/// One course left is `Ok`; several are `Err` with their indices; no tier
/// matching is `Err` of nothing.
///
/// Numbers taken from the recording date (`2025`, `0610` for 2025-10-06)
/// never count as a section: dated topics repeat them (`…_18_06_2025`).
fn match_by_tokens(
    naming: &CourseNaming,
    courses: &[Resolved],
    names: &[&str],
) -> Result<usize, Vec<usize>> {
    let topics: Vec<(BTreeSet<String>, BTreeSet<String>)> = names
        .iter()
        .map(|name| {
            let (topic, date) = match layout::parse_v1_recording(name, names) {
                Some(r) => (r.topic, Some(r.date)),
                None => (
                    name.rsplit_once('.').map_or(*name, |(s, _)| s).to_string(),
                    None,
                ),
            };
            let all: BTreeSet<String> = words(&topic).into_iter().collect();
            let dated: BTreeSet<String> = date
                .map(|d| {
                    [d.format("%Y"), d.format("%d%m"), d.format("%m%d")]
                        .iter()
                        .map(ToString::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let numbers = all.difference(&dated).cloned().collect();
            (all, numbers)
        })
        .collect();
    let by_section = |c: &CourseMeta, t: &(BTreeSet<String>, BTreeSet<String>)| {
        naming.section(c).is_some_and(|s| t.1.contains(&s))
    };
    let by_code = |c: &CourseMeta, t: &(BTreeSet<String>, BTreeSet<String>)| {
        let code = layout::slugify(&naming.short_code(c));
        !code.is_empty() && t.0.contains(&code)
    };
    let by_name = |c: &CourseMeta, t: &(BTreeSet<String>, BTreeSet<String>)| {
        let name = words(&naming.short_name(c));
        !name.is_empty() && name.iter().all(|w| t.0.contains(w))
    };
    let tiers: [Tier; 3] = [&by_section, &by_code, &by_name];
    let mut candidates: Option<BTreeSet<usize>> = None;
    for tier in tiers {
        let hits: BTreeSet<usize> = courses
            .iter()
            .enumerate()
            .filter(|(_, c)| c.planned.course.id.is_none())
            .filter(|(_, c)| topics.iter().any(|t| tier(&c.planned.course, t)))
            .map(|(i, _)| i)
            .collect();
        if hits.is_empty() {
            continue;
        }
        candidates = Some(match candidates {
            None => hits,
            Some(prev) => {
                let both: BTreeSet<usize> = prev.intersection(&hits).copied().collect();
                if both.is_empty() {
                    return Err(prev.union(&hits).copied().collect());
                }
                both
            }
        });
    }
    let candidates: Vec<usize> = candidates.unwrap_or_default().into_iter().collect();
    match candidates.as_slice() {
        [only] => Ok(*only),
        _ => Err(candidates),
    }
}

/// The words of a name, as slugs.
fn words(text: &str) -> Vec<String> {
    layout::slugify(text)
        .split('-')
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// v1 module directory name → v2 module directory name, for one course.
///
/// Canvas positions win; modules Canvas does not list (or every module,
/// offline) follow in `module_id` order, numbered after the highest Canvas
/// position.
fn module_dirs(
    inv: &Inventory,
    v1_course: &str,
    positions: &BTreeMap<u64, u32>,
) -> BTreeMap<String, String> {
    let prefix = format!("{v1_course}/{V1_MODULES_DIR}/");
    let dirs: BTreeSet<(u64, &str)> = inv
        .files
        .keys()
        .filter_map(|p| {
            let rest = p.strip_prefix(&prefix)?;
            let (dir, _) = rest.split_once('/')?;
            module_id(dir).map(|id| (id, dir))
        })
        .collect();
    let mut next = positions.values().copied().max().unwrap_or(0);
    dirs.into_iter()
        .map(|(id, dir)| {
            let position = positions.get(&id).copied().unwrap_or_else(|| {
                next += 1;
                next
            });
            let name = dir.split_once('_').map_or("", |(_, name)| name);
            (dir.to_string(), layout::module_dir(position, name))
        })
        .collect()
}

/// Where a file inside a v1 module directory goes. `rest` is its path below
/// the module directory; `mdir` the v2 module directory (from the library root).
fn module_file(v2: &str, mdir: &str, rest: &[&str]) -> Dest {
    match rest {
        [file] if is_assignment_file(file) => {
            let title = v1_md_title(file).unwrap_or_default();
            let title = title.strip_prefix("ASSIGN-").unwrap_or(title);
            Dest {
                to: join(&[v2, layout::ASSIGNMENTS_DIR, &layout::assignment_file(title)]),
                edit: Some(EditKind::Assignment),
                item: v1_md_position(file).map(u64::from),
            }
        }
        [file] => match (v1_md_position(file), v1_md_title(file)) {
            (Some(position), Some(title)) => Dest {
                to: join(&[mdir, &layout::page_file(position, title)]),
                edit: Some(EditKind::Page),
                item: None,
            },
            _ => Dest::plain(join(&[mdir, file])),
        },
        [V1_ATTACHMENTS_DIR, file] => Dest::plain(join(&[
            mdir,
            layout::ATTACHMENTS_DIR,
            &layout::attachment_file(file),
        ])),
        [V1_ATTACHMENTS_DIR, nested @ ..] => {
            Dest::plain(join(&[mdir, layout::ATTACHMENTS_DIR, &nested.join("/")]))
        }
        _ => Dest::plain(join(&[mdir, &rest.join("/")])),
    }
}

/// Where a file inside a v1 `announcements/` directory goes.
fn announcement_file(v2: &str, rest: &[&str]) -> Dest {
    let anuncios = join(&[v2, layout::ANNOUNCEMENTS_DIR]);
    match rest {
        [INDEX_JSON] => Dest::plain(join(&[&anuncios, INDEX_JSON])),
        [V1_MEDIA_DIR, file] => Dest::plain(join(&[
            &anuncios,
            layout::ATTACHMENTS_DIR,
            &layout::attachment_file(file),
        ])),
        [V1_MEDIA_DIR, nested @ ..] => Dest::plain(join(&[
            &anuncios,
            layout::ATTACHMENTS_DIR,
            &nested.join("/"),
        ])),
        [file] => match parse_v1_announcement(file) {
            Some((date, title, id)) => Dest {
                to: join(&[&anuncios, &layout::announcement_file(date, title)]),
                edit: Some(EditKind::Announcement),
                item: Some(id),
            },
            None => Dest::plain(join(&[&anuncios, file])),
        },
        _ => Dest::plain(join(&[&anuncios, &rest.join("/")])),
    }
}

/// `NN` of a v1 `NN-<Title>.md`.
fn v1_md_position(file: &str) -> Option<u32> {
    let (nn, _) = file.split_once('-')?;
    numeric(nn).and_then(|n| u32::try_from(n).ok())
}

/// `<Title>` of a v1 `NN-<Title>.md`.
fn v1_md_title(file: &str) -> Option<&str> {
    let stem = file.strip_suffix(".md")?;
    let (nn, title) = stem.split_once('-')?;
    numeric(nn)?;
    Some(title)
}

/// A v1 announcement body, `YYYY-MM-DD_<slug>_<id>.md`, as date, title and id.
fn parse_v1_announcement(file: &str) -> Option<(NaiveDate, &str, u64)> {
    let stem = file.strip_suffix(".md")?;
    let (date, rest) = stem.split_once('_')?;
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let (title, id) = rest.rsplit_once('_')?;
    Some((date, title, numeric(id)?))
}

fn join(parts: &[&str]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("/")
}

impl fmt::Display for Root {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Root::Library => "library",
            Root::Caldir => "caldir",
        })
    }
}

impl fmt::Display for EditKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            EditKind::Page => "page",
            EditKind::Assignment => "assignment",
            EditKind::Announcement => "announcement",
        })
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Reason::OrphanZoom => f.write_str("zoom folder matches no course"),
            Reason::AmbiguousZoom(dirs) => {
                write!(
                    f,
                    "zoom folder matches several courses: {}",
                    dirs.join(", ")
                )
            }
            Reason::UnrecognizedZoomEntry => f.write_str("not inside a numeric zoom folder"),
            Reason::CaldirTargetExists(to) => write!(f, "calendar directory {to} already exists"),
            Reason::DestinationBlocked(to) => {
                write!(f, "{to} is a directory or under a file")
            }
        }
    }
}

/// A stable, line-oriented report: the golden snapshot format, and what a
/// `--dry-run` shows.
impl fmt::Display for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "courses ({}):", self.courses.len())?;
        for c in &self.courses {
            write!(f, "  {} -> {}", c.v1_dir, c.dir)?;
            match c.course.id {
                Some(id) => write!(f, " canvas_id={id}")?,
                None => write!(f, " canvas_id=null")?,
            }
            if !c.zoom_ids.is_empty() {
                let ids: Vec<String> = c.zoom_ids.iter().map(u64::to_string).collect();
                write!(f, " zoom={}", ids.join(","))?;
            }
            writeln!(f)?;
        }
        writeln!(f, "moves ({}):", self.moves.len())?;
        for m in &self.moves {
            let slash = if m.kind == EntryKind::Dir { "/" } else { "" };
            writeln!(f, "  {}: {}{slash} -> {}{slash}", m.root, m.from, m.to)?;
        }
        writeln!(f, "edits ({}):", self.edits.len())?;
        for e in &self.edits {
            writeln!(f, "  {}: {}", e.kind, e.path)?;
        }
        for (title, list) in [
            ("duplicates", &self.duplicates),
            ("conflicts", &self.conflicts),
        ] {
            writeln!(f, "{title} ({}):", list.len())?;
            for c in list {
                let occupied = if c.occupied { " (exists)" } else { "" };
                writeln!(
                    f,
                    "  {}: {}{occupied} <- {}",
                    c.root,
                    c.to,
                    c.from.join(", ")
                )?;
            }
        }
        writeln!(f, "unresolved ({}):", self.unresolved.len())?;
        for u in &self.unresolved {
            writeln!(f, "  {}: {}: {}", u.root, u.path, u.reason)?;
        }
        writeln!(f, "prune ({}):", self.prune.len())?;
        for dir in &self.prune {
            writeln!(f, "  {}: {dir}/ (if empty)", Root::Library)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;

    /// The commented UNAB example from `assets/config.toml`.
    fn unab() -> CourseNaming {
        let commented: String = include_str!("../../assets/config.toml")
            .lines()
            .filter_map(|l| l.strip_prefix("# "))
            .filter(|l| l.contains("_pattern = '"))
            .collect::<Vec<_>>()
            .join("\n");
        let layout: config::Layout = toml::from_str(&commented).unwrap();
        CourseNaming::new(&layout).unwrap()
    }

    const MIN: &str = "202615_2690_MINERIA_DE_DATOS_ATDF105_202615_2690_EL_ON";

    fn inv(files: &[(&str, u64)]) -> Inventory {
        Inventory {
            files: files.iter().map(|(p, s)| ((*p).to_string(), *s)).collect(),
            ..Inventory::default()
        }
    }

    fn moved<'a>(plan: &'a Plan, from: &str) -> Option<&'a str> {
        plan.moves
            .iter()
            .find(|m| m.from == from)
            .map(|m| m.to.as_str())
    }

    #[test]
    fn offline_modules_are_numbered_by_id() {
        let inv = inv(&[
            (&format!("{MIN}/Modules/900_Ultimo/01-A.md"), 1),
            (&format!("{MIN}/Modules/30_Primero/01-A.md"), 1),
        ]);
        let plan = plan(&inv, &unab(), &[]);
        assert_eq!(
            moved(&plan, &format!("{MIN}/Modules/30_Primero/01-A.md")),
            Some("atdf105-mineria-de-datos/modulos/01-primero/01-a.md")
        );
        assert_eq!(
            moved(&plan, &format!("{MIN}/Modules/900_Ultimo/01-A.md")),
            Some("atdf105-mineria-de-datos/modulos/02-ultimo/01-a.md")
        );
    }

    #[test]
    fn canvas_metadata_names_the_course_and_orders_its_modules() {
        let inv = inv(&[
            (&format!("{MIN}/state.json"), 1),
            (&format!("{MIN}/Modules/100_Uno/01-A.md"), 1),
            (&format!("{MIN}/Modules/200_Dos/01-A.md"), 1),
            (&format!("{MIN}/Modules/300_Borrado/01-A.md"), 1),
        ]);
        let canvas = [CanvasCourse {
            meta: CourseMeta {
                id: Some(77),
                name: "202615_2690_MINERIA DE DATOS".into(),
                code: Some("ATDF105_202615_2690_EL_ON".into()),
            },
            module_positions: BTreeMap::from([(100, 2), (200, 1)]),
        }];
        let plan = plan(&inv, &unab(), &canvas);
        // No index.json: matched by the v1 directory Canvas's name gives.
        assert_eq!(plan.courses[0].course, canvas[0].meta);
        let to = |m: &str| moved(&plan, &format!("{MIN}/Modules/{m}/01-A.md"));
        assert_eq!(
            to("200_Dos"),
            Some("atdf105-mineria-de-datos/modulos/01-dos/01-a.md")
        );
        assert_eq!(
            to("100_Uno"),
            Some("atdf105-mineria-de-datos/modulos/02-uno/01-a.md")
        );
        // Not in Canvas any more: after the last Canvas position.
        assert_eq!(
            to("300_Borrado"),
            Some("atdf105-mineria-de-datos/modulos/03-borrado/01-a.md")
        );
    }

    #[test]
    fn an_index_id_is_not_overridden_by_a_name_match() {
        let mut inv = inv(&[(&format!("{MIN}/state.json"), 1)]);
        inv.course_ids.insert(MIN.into(), 5);
        let other = CanvasCourse {
            meta: CourseMeta {
                id: Some(6),
                name: "202615_2690_MINERIA DE DATOS".into(),
                code: Some("ATDF105_202615_2690_EL_ON".into()),
            },
            module_positions: BTreeMap::new(),
        };
        let plan = plan(&inv, &unab(), &[other]);
        assert_eq!(plan.courses[0].course.id, Some(5));
        assert_eq!(plan.courses[0].course.name, "202615_2690_MINERIA_DE_DATOS");
    }

    #[test]
    fn caldir_course_directories_are_renamed() {
        let other = "202615_2520_ELECTIVO_ATDF106_202615_2520_EL_ON";
        let mut inv = inv(&[
            (&format!("{MIN}/state.json"), 1),
            (&format!("{other}/state.json"), 1),
        ]);
        inv.caldir_dirs = [MIN, other, "atdf106-electivo", "ajeno"]
            .into_iter()
            .map(String::from)
            .collect();
        let plan = plan(&inv, &unab(), &[]);
        let caldir: Vec<&Move> = plan
            .moves
            .iter()
            .filter(|m| m.root == Root::Caldir)
            .collect();
        assert_eq!(
            caldir,
            [&Move {
                root: Root::Caldir,
                kind: EntryKind::Dir,
                from: MIN.into(),
                to: "atdf105-mineria-de-datos".into(),
            }]
        );
        assert_eq!(
            plan.unresolved,
            [Unresolved {
                root: Root::Caldir,
                path: other.into(),
                reason: Reason::CaldirTargetExists("atdf106-electivo".into()),
            }]
        );
    }

    #[test]
    fn distinct_items_with_one_name_are_numbered_not_collided() {
        // Announcements 1 and 2 share date and title; 9 is titled "Aviso 2".
        let a1 = format!("{MIN}/announcements/2026-01-01_Aviso_1.md");
        let a2 = format!("{MIN}/announcements/2026-01-01_Aviso_2.md");
        let a9 = format!("{MIN}/announcements/2026-01-01_Aviso_2_9.md");
        // Two assignments with one title, items 04 and 06 of one module.
        let t4 = format!("{MIN}/Modules/1_Uno/04-ASSIGN-Informe_de_caso_3.md");
        let t6 = format!("{MIN}/Modules/1_Uno/06-ASSIGN-Informe_de_caso_3.md");
        let inv = inv(&[(&a1, 10), (&a2, 10), (&a9, 12), (&t4, 5), (&t6, 7)]);
        let plan = plan(&inv, &unab(), &[]);
        assert!(plan.duplicates.is_empty() && plan.conflicts.is_empty());
        let anuncios = "atdf105-mineria-de-datos/anuncios";
        let tareas = "atdf105-mineria-de-datos/tareas";
        let want = [
            (&a1, format!("{anuncios}/2026-01-01_aviso.md")),
            (&a2, format!("{anuncios}/2026-01-01_aviso-3.md")),
            (&a9, format!("{anuncios}/2026-01-01_aviso-2.md")),
            (&t4, format!("{tareas}/informe-de-caso-3.md")),
            (&t6, format!("{tareas}/informe-de-caso-3-2.md")),
        ];
        for (from, to) in &want {
            assert_eq!(moved(&plan, from), Some(to.as_str()), "{from}");
            assert!(plan.edits.iter().any(|e| &e.path == to), "no edit for {to}");
        }
    }

    #[test]
    fn colliding_sources_of_one_item_are_split_by_size() {
        // Same `NN` and title once slugged: the same page, twice.
        let a = format!("{MIN}/Modules/1_Uno/01-Pagina.md");
        let b = format!("{MIN}/Modules/1_Uno/01-PAGINA.md");
        let c = format!("{MIN}/Modules/1_Uno/02-Otra.md");
        let d = format!("{MIN}/Modules/1_Uno/02-OTRA.md");
        // One assignment linked from two modules: same `NN`, same item.
        let e = format!("{MIN}/Modules/1_Uno/04-ASSIGN-Tarea.md");
        let f = format!("{MIN}/Modules/2_Dos/04-ASSIGN-Tarea.md");
        let inv = inv(&[(&a, 10), (&b, 10), (&c, 10), (&d, 11), (&e, 3), (&f, 3)]);
        let plan = plan(&inv, &unab(), &[]);
        assert!(plan.moves.is_empty() && plan.edits.is_empty());
        let collision = |to: &str, from: Vec<String>| Collision {
            root: Root::Library,
            to: format!("atdf105-mineria-de-datos/{to}"),
            from,
            occupied: false,
        };
        assert_eq!(
            plan.duplicates,
            [
                collision("modulos/01-uno/01-pagina.md", vec![b, a]),
                collision("tareas/tarea.md", vec![e, f]),
            ]
        );
        assert_eq!(
            plan.conflicts,
            [collision("modulos/01-uno/02-otra.md", vec![d, c])]
        );
    }

    #[test]
    fn an_existing_destination_is_a_collision_and_a_directory_blocks() {
        let state = format!("{MIN}/state.json");
        let page = format!("{MIN}/Modules/1_Uno/01-Pagina.md");
        let mut inv = inv(&[
            (&state, 3),
            ("atdf105-mineria-de-datos/.u_crawler/state.json", 3),
            (&page, 1),
            ("atdf105-mineria-de-datos/modulos/01-uno/01-pagina.md/x", 1),
        ]);
        // The v2 directory is this course's, by its `curso.json`.
        inv.existing.push(ExistingCourseDir {
            dir: "atdf105-mineria-de-datos".into(),
            course: unab().parse_v1_course_dir(MIN),
        });
        let plan = plan(&inv, &unab(), &[]);
        assert_eq!(plan.courses[0].dir, "atdf105-mineria-de-datos");
        assert!(plan.moves.is_empty());
        assert_eq!(
            plan.duplicates,
            [Collision {
                root: Root::Library,
                to: "atdf105-mineria-de-datos/.u_crawler/state.json".into(),
                from: vec![state],
                occupied: true,
            }]
        );
        assert_eq!(
            plan.unresolved,
            [Unresolved {
                root: Root::Library,
                path: page,
                reason: Reason::DestinationBlocked(
                    "atdf105-mineria-de-datos/modulos/01-uno/01-pagina.md".into()
                ),
            }]
        );
    }

    #[test]
    fn a_root_name_is_never_taken_over() {
        // A foreign folder already holds the slug: the course goes elsewhere.
        let inv = inv(&[
            (&format!("{MIN}/state.json"), 1),
            ("atdf105-mineria-de-datos/mis-notas.md", 1),
        ]);
        let plan = plan(&inv, &unab(), &[]);
        assert_eq!(plan.courses[0].dir, "atdf105-mineria-de-datos-s2690");
    }

    #[test]
    fn a_zoom_folder_matching_two_courses_stays() {
        let a = "202515_1033_FISICA_FMMA015_202515_1033_EL_ON";
        let b = "202515_1226_FISICA_FMMA015_202515_1226_EL_ON";
        let inv = inv(&[
            (&format!("{a}/state.json"), 1),
            (&format!("{b}/state.json"), 1),
            ("Zoom/5/2025_06_18_Clase_FMMA015.mp4", 1),
            ("Zoom/6/2025_06_18_Clase_FMMA015_1226.mp4", 1),
            ("Zoom/notas/x.mp4", 1),
            ("Zoom/suelto.txt", 1),
        ]);
        let plan = plan(&inv, &unab(), &[]);
        assert_eq!(
            plan.unresolved,
            [
                Unresolved {
                    root: Root::Library,
                    path: "Zoom/5".into(),
                    reason: Reason::AmbiguousZoom(vec![
                        "fmma015-fisica-s1033".into(),
                        "fmma015-fisica-s1226".into()
                    ]),
                },
                Unresolved {
                    root: Root::Library,
                    path: "Zoom/notas".into(),
                    reason: Reason::UnrecognizedZoomEntry,
                },
                Unresolved {
                    root: Root::Library,
                    path: "Zoom/suelto.txt".into(),
                    reason: Reason::UnrecognizedZoomEntry,
                },
            ]
        );
        // The section beats the shared code.
        assert_eq!(
            moved(&plan, "Zoom/6/2025_06_18_Clase_FMMA015_1226.mp4"),
            Some("fmma015-fisica-s1226/grabaciones/2025-06-18_clase-fmma015-1226.mp4")
        );
    }

    #[test]
    fn foreign_entries_keep_their_names() {
        let inv = inv(&[
            (&format!("{MIN}/Notas Propias.md"), 1),
            (&format!("{MIN}/extra/Mi Archivo.txt"), 1),
            (&format!("{MIN}/Modules/1_Uno/Sub Carpeta/x.md"), 1),
            (&format!("{MIN}/Modules/suelto.md"), 1),
            (&format!("{MIN}/announcements/Leeme.txt"), 1),
            (&format!("{MIN}/announcements/media/Sub/Raro.ZIP"), 1),
            ("Zoom/9/notas.txt", 1),
            ("Zoom/9/zoom-123.mp4", 1),
        ]);
        let mut inv = inv;
        inv.course_ids.insert(MIN.into(), 9);
        let plan = plan(&inv, &unab(), &[]);
        let to = |p: &str| moved(&plan, p);
        let v2 = "atdf105-mineria-de-datos";
        assert_eq!(
            to(&format!("{MIN}/Notas Propias.md")),
            Some(format!("{v2}/Notas Propias.md").as_str())
        );
        assert_eq!(
            to(&format!("{MIN}/extra/Mi Archivo.txt")),
            Some(format!("{v2}/extra/Mi Archivo.txt").as_str())
        );
        assert_eq!(
            to(&format!("{MIN}/Modules/1_Uno/Sub Carpeta/x.md")),
            Some(format!("{v2}/modulos/01-uno/Sub Carpeta/x.md").as_str())
        );
        assert_eq!(
            to(&format!("{MIN}/Modules/suelto.md")),
            Some(format!("{v2}/modulos/suelto.md").as_str())
        );
        assert_eq!(
            to(&format!("{MIN}/announcements/Leeme.txt")),
            Some(format!("{v2}/anuncios/Leeme.txt").as_str())
        );
        assert_eq!(
            to(&format!("{MIN}/announcements/media/Sub/Raro.ZIP")),
            Some(format!("{v2}/anuncios/adjuntos/Sub/Raro.ZIP").as_str())
        );
        assert_eq!(
            to("Zoom/9/notas.txt"),
            Some(format!("{v2}/grabaciones/notas.txt").as_str())
        );
        assert_eq!(
            to("Zoom/9/zoom-123.mp4"),
            Some(format!("{v2}/grabaciones/zoom-123.mp4").as_str())
        );
        assert!(plan.edits.is_empty());
        assert_eq!(plan.courses[0].zoom_ids, [9]);
    }

    #[test]
    fn a_course_with_another_canvas_id_never_takes_a_zoom_folder() {
        // A retaken course: its old offering's folder names the same course.
        let mut inv = inv(&[
            (&format!("{MIN}/state.json"), 1),
            ("Zoom/157107/2025_06_23_202515_1033_MINERIA_DE_DATOS.mp4", 1),
        ]);
        inv.course_ids.insert(MIN.into(), 9001);
        let plan = plan(&inv, &unab(), &[]);
        assert!(plan.courses[0].zoom_ids.is_empty());
        assert!(plan.moves.iter().all(|m| !m.from.starts_with("Zoom/")));
        assert_eq!(
            plan.unresolved,
            [Unresolved {
                root: Root::Library,
                path: "Zoom/157107".into(),
                reason: Reason::OrphanZoom,
            }]
        );
    }

    #[test]
    fn date_numbers_are_not_a_section_and_tiers_must_agree() {
        let fis = "202515_1226_FISICA_GENERAL_FMMA015_202515_1226_EL_ON";
        let qui = "202515_2025_QUIMICA_QUIM100_202515_2025_EL_ON";
        let inv = inv(&[
            (&format!("{fis}/state.json"), 1),
            (&format!("{qui}/state.json"), 1),
            ("Zoom/8102/2025_06_18_Clase_FMMA015_18_06_2025.mp4", 1),
            // The section says quimica, the code says fisica: ambiguous.
            ("Zoom/8103/2026_06_19_Clase_FMMA015_NRC_2025.mp4", 1),
        ]);
        let plan = plan(&inv, &unab(), &[]);
        assert_eq!(
            moved(&plan, "Zoom/8102/2025_06_18_Clase_FMMA015_18_06_2025.mp4"),
            Some("fmma015-fisica-general/grabaciones/2025-06-18_clase-fmma015-18-06-2025.mp4")
        );
        assert_eq!(
            plan.unresolved,
            [Unresolved {
                root: Root::Library,
                path: "Zoom/8103".into(),
                reason: Reason::AmbiguousZoom(vec![
                    "fmma015-fisica-general".into(),
                    "quim100-quimica".into()
                ]),
            }]
        );
    }

    #[test]
    fn replanning_a_cut_migration_resumes_into_the_same_directory() {
        let inv = inv(&[
            ("atdf105-mineria-de-datos/.u_crawler/state.json", 1),
            (&format!("{MIN}/Modules/1_Uno/01-A.md"), 1),
        ]);
        let plan = plan(&inv, &unab(), &[]);
        assert_eq!(plan.courses[0].dir, "atdf105-mineria-de-datos");
        assert_eq!(
            moved(&plan, &format!("{MIN}/Modules/1_Uno/01-A.md")),
            Some("atdf105-mineria-de-datos/modulos/01-uno/01-a.md")
        );
    }

    #[test]
    fn v1_markdown_names() {
        assert_eq!(v1_md_position("12-Titulo.md"), Some(12));
        assert_eq!(
            v1_md_title("12-Titulo-con-guion.md"),
            Some("Titulo-con-guion")
        );
        assert_eq!(v1_md_title("nota.md"), None);
        assert_eq!(
            parse_v1_announcement("2026-06-17_Hola_mundo_501.md"),
            NaiveDate::from_ymd_opt(2026, 6, 17).map(|d| (d, "Hola_mundo", 501))
        );
        assert_eq!(parse_v1_announcement("2026-06-17_Hola.md"), None);
        assert_eq!(parse_v1_announcement("17-06-2026_Hola_1.md"), None);
        assert_eq!(ancestors("a/b/c").collect::<Vec<_>>(), ["a", "a/b"]);
    }
}
