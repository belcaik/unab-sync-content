//! `migrate::apply` over the synthetic v1 library (T1) and small hand-made
//! trees: journal, resume after a cut, undo, collisions, `cp -al` clones.
#[path = "fixtures/v1/mod.rs"]
mod v1;

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tempfile::{tempdir, TempDir};
use u_crawler::migrate::apply::{
    self, apply, apply_with_hook, journal_path, pending, read_journal, resume, undo, ApplyError,
    Entry, JournalState, Roots,
};
use u_crawler::migrate::plan::{plan, Collision, EntryKind, Move, Plan, Root};
use u_crawler::migrate::scan;

type TestResult = Result<(), Box<dyn Error>>;

const TS: &str = "20261025T120000000Z";
const TS2: &str = "20261025T130000000Z";
/// An empty directory nested in a v1 course, relative to it.
const EMPTY_V1_DIR: &str = "Modules/999_Vacio/Attachments";
/// A `Zoom/<id>` with no recordings: no file names it, so no course owns it.
const EMPTY_ZOOM_DIR: &str = "Zoom/555";

/// The fixture library at `<tmp>/lib`, plus a calendar tree at `<tmp>/cal`
/// with a `deadlines/` directory for two of its courses, and an empty
/// [`EMPTY_V1_DIR`] in the first course and an empty [`EMPTY_ZOOM_DIR`], as
/// v1 could leave behind.
struct Lib {
    _dir: TempDir,
    root: PathBuf,
    cal: PathBuf,
    lib: v1::V1Library,
}

impl Lib {
    fn new() -> Result<Lib, Box<dyn Error>> {
        let dir = tempdir()?;
        let root = dir.path().join("lib");
        let cal = dir.path().join("cal");
        let lib = v1::write_v1_library(&root)?;
        fs::create_dir_all(root.join(&lib.courses[0].v1_dir).join(EMPTY_V1_DIR))?;
        fs::create_dir_all(root.join(EMPTY_ZOOM_DIR))?;
        for course in lib.courses.iter().take(2) {
            let deadlines = cal.join(&course.v1_dir).join("deadlines");
            fs::create_dir_all(&deadlines)?;
            fs::write(deadlines.join("assignment-1.ics"), &course.v1_dir)?;
        }
        Ok(Lib {
            _dir: dir,
            root,
            cal,
            lib,
        })
    }

    fn roots(&self) -> Roots<'_> {
        Roots {
            library: &self.root,
            caldir: Some(&self.cal),
        }
    }

    fn plan(&self) -> Result<Plan, Box<dyn Error>> {
        let inv = scan(&self.root, Some(&self.cal))?;
        Ok(plan(&inv, &v1::unab_naming()?, &[]))
    }

    /// Every file of both trees, by path, minus the library's `.u_crawler/`
    /// (journals and what an undo set aside).
    fn tree(&self) -> io::Result<BTreeMap<String, String>> {
        let mut out = user_files(&self.root)?;
        for (path, hash) in v1::hash_tree(&self.cal)? {
            out.insert(format!("cal:{path}"), hash);
        }
        Ok(out)
    }

    /// Every directory of both trees, minus the library's `.u_crawler/`.
    fn dirs(&self) -> io::Result<BTreeSet<String>> {
        let mut out = BTreeSet::new();
        dirs_under(&self.root, "", &mut out)?;
        out.retain(|d| d != ".u_crawler" && !d.starts_with(".u_crawler/"));
        let mut cal = BTreeSet::new();
        dirs_under(&self.cal, "", &mut cal)?;
        out.extend(cal.into_iter().map(|d| format!("cal:{d}")));
        Ok(out)
    }
}

fn dirs_under(dir: &Path, prefix: &str, out: &mut BTreeSet<String>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            dirs_under(&entry.path(), &rel, out)?;
            out.insert(rel);
        }
    }
    Ok(())
}

/// The directories under `root` with nothing in them.
fn empty_dirs(root: &Path) -> io::Result<Vec<String>> {
    let mut all = BTreeSet::new();
    dirs_under(root, "", &mut all)?;
    let mut out = Vec::new();
    for d in all {
        if fs::read_dir(root.join(&d))?.next().is_none() {
            out.push(d);
        }
    }
    Ok(out)
}

fn user_files(root: &Path) -> io::Result<BTreeMap<String, String>> {
    let mut files = v1::hash_tree(root)?;
    files.retain(|path, _| !path.starts_with(".u_crawler/"));
    Ok(files)
}

fn is_curso_json(path: &str) -> bool {
    path.split('/').count() == 2 && path.ends_with("/curso.json")
}

fn multiset<'a>(files: impl Iterator<Item = &'a String>) -> BTreeMap<&'a str, usize> {
    let mut out = BTreeMap::new();
    for hash in files {
        *out.entry(hash.as_str()).or_insert(0) += 1;
    }
    out
}

fn operations(entries: &[Entry]) -> usize {
    entries
        .iter()
        .filter(|e| {
            matches!(
                e,
                Entry::Mkdir { .. }
                    | Entry::Rename { .. }
                    | Entry::Rmdir { .. }
                    | Entry::Write { .. }
            )
        })
        .count()
}

#[test]
fn apply_migrates_the_fixture_and_loses_nothing() -> TestResult {
    let l = Lib::new()?;
    let before = l.tree()?;
    let plan = l.plan()?;
    let report = apply(&plan, &l.roots(), TS)?;

    let after = l.tree()?;
    let content = after
        .iter()
        .filter(|(p, _)| !is_curso_json(p))
        .map(|(_, h)| h);
    assert_eq!(
        multiset(content),
        multiset(before.values()),
        "content was lost"
    );
    for foreign in &l.lib.foreign {
        assert_eq!(after.get(foreign), l.lib.manifest.get(foreign), "{foreign}");
    }
    let mineria = l
        .lib
        .course_by_code("ATDF105_202615_2690_EL_ON")
        .ok_or("missing mineria")?;
    assert!(l.root.join(&mineria.v2_dir).join("curso.json").is_file());
    let mut moved: Vec<(&str, &str)> = report
        .moved
        .iter()
        .map(|p| (p.from.as_str(), p.to.as_str()))
        .collect();
    moved.sort_unstable();
    let mut planned: Vec<(&str, &str)> = plan
        .moves
        .iter()
        .map(|m| (m.from.as_str(), m.to.as_str()))
        .collect();
    planned.sort_unstable();
    assert_eq!(moved, planned);
    assert!(report.conflicts.is_empty() && report.duplicates.is_empty());
    assert!(report.skipped.is_empty() && !report.resumed);

    // Calendar directories moved under their v2 name, with their content.
    for course in l.lib.courses.iter().take(2) {
        let ics = l
            .cal
            .join(&course.v2_dir)
            .join("deadlines/assignment-1.ics");
        assert_eq!(fs::read_to_string(ics)?, course.v1_dir);
        assert!(!l.cal.join(&course.v1_dir).exists());
    }
    // Every course got its curso.json, readable back as the planned course.
    assert_eq!(report.written.len(), plan.courses.len());
    for course in &plan.courses {
        let text = fs::read_to_string(l.root.join(&course.dir).join("curso.json"))?;
        assert_eq!(
            u_crawler::migrate::course_from_curso_json(&text).as_ref(),
            Some(&course.course)
        );
    }

    let entries = read_journal(&journal_path(&l.root, TS))?;
    assert!(matches!(entries.first(), Some(Entry::Begin { .. })));
    assert_eq!(entries.last(), Some(&Entry::Done));
    assert_eq!(apply::journal_state(&entries), JournalState::Done);
    assert_eq!(pending(&l.root)?, None);

    let again = l.plan()?;
    assert!(again.is_empty(), "second plan is not empty:\n{again}");
    Ok(())
}

#[test]
fn a_migration_leaves_no_empty_v1_directory() -> TestResult {
    let l = Lib::new()?;
    let plan = l.plan()?;
    let report = apply(&plan, &l.roots(), TS)?;
    for course in &l.lib.courses {
        assert!(!l.root.join(&course.v1_dir).exists(), "{}", course.v1_dir);
        if let Some(zoom) = &course.zoom {
            assert!(!l.root.join(&zoom.rel).exists(), "{}", zoom.rel);
        }
    }
    // `Zoom/` stays: the orphan folder is still in it.
    assert!(l.root.join(&l.lib.orphan_zoom.rel).is_dir());
    assert_eq!(empty_dirs(&l.root)?, Vec::<String>::new());
    assert!(!l.root.join(EMPTY_ZOOM_DIR).exists());
    // `Zoom/` is kept, and reported: the orphan folder is in it.
    assert_eq!(report.kept_dirs, ["Zoom"]);
    let empty = format!("{}/{EMPTY_V1_DIR}", l.lib.courses[0].v1_dir);
    assert!(report.removed_dirs.contains(&empty), "{empty} not removed");
    // Each removal is journaled, children before their parent.
    let entries = read_journal(&journal_path(&l.root, TS))?;
    let removed: Vec<&str> = entries
        .iter()
        .filter_map(|e| match e {
            Entry::Rmdir {
                root: Root::Library,
                path,
            } => Some(path.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(removed, report.removed_dirs);
    for (i, dir) in removed.iter().enumerate() {
        assert!(
            !removed[..i]
                .iter()
                .any(|d| dir.starts_with(&format!("{d}/"))),
            "{dir} removed after its parent"
        );
    }
    Ok(())
}

#[test]
fn a_v1_directory_with_a_file_left_is_kept_and_reported() -> TestResult {
    // `a/x.pdf` is a duplicate: it stays, so `a/` stays; `a/vacia/` goes.
    let dir = tree_of(&[("a/x.pdf", "same"), ("b/x.pdf", "same")])?;
    fs::create_dir_all(dir.path().join("a/vacia"))?;
    let plan = Plan {
        duplicates: vec![collision("b/x.pdf", &["a/x.pdf"], true)],
        prune: vec!["a".into()],
        ..Plan::default()
    };
    let report = apply(&plan, &library(&dir), TS)?;
    assert_eq!(report.removed_dirs, ["a/vacia"]);
    assert_eq!(report.kept_dirs, ["a"]);
    assert_eq!(fs::read_to_string(dir.path().join("a/x.pdf"))?, "same");
    assert!(!dir.path().join("a/vacia").exists());

    undo(&library(&dir), TS)?;
    assert!(dir.path().join("a/vacia").is_dir());
    assert_eq!(fs::read_to_string(dir.path().join("a/x.pdf"))?, "same");
    Ok(())
}

#[test]
fn an_emptied_zoom_folder_and_its_parent_go_and_undo_brings_them_back() -> TestResult {
    let dir = tree_of(&[("Zoom/1/r.mp4", "video")])?;
    let plan = Plan {
        moves: vec![Move {
            root: Root::Library,
            kind: EntryKind::File,
            from: "Zoom/1/r.mp4".into(),
            to: "ramo/grabaciones/r.mp4".into(),
        }],
        prune: vec!["Zoom/1".into()],
        ..Plan::default()
    };
    let report = apply(&plan, &library(&dir), TS)?;
    assert_eq!(report.removed_dirs, ["Zoom/1", "Zoom"]);
    assert!(!dir.path().join("Zoom").exists());

    let undone = undo(&library(&dir), TS)?;
    assert_eq!(
        fs::read_to_string(dir.path().join("Zoom/1/r.mp4"))?,
        "video"
    );
    assert_eq!(undone.dirs_restored, 2);
    // The directories the run created are gone again; only `.u_crawler/` is new.
    let mut left: Vec<String> = fs::read_dir(dir.path())?
        .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect::<io::Result<_>>()?;
    left.sort();
    assert_eq!(left, [".u_crawler", "Zoom"]);
    Ok(())
}

#[test]
fn a_second_run_does_nothing() -> TestResult {
    let l = Lib::new()?;
    apply(&l.plan()?, &l.roots(), TS)?;
    let before = l.tree()?;
    let report = apply(&l.plan()?, &l.roots(), TS2)?;
    assert_eq!(report, apply::Report::default());
    assert!(
        !journal_path(&l.root, TS2).exists(),
        "a no-op run left a journal"
    );
    assert_eq!(l.tree()?, before);
    // A journal is never reused for a new run.
    assert!(matches!(
        apply(&l.plan()?, &l.roots(), TS),
        Err(ApplyError::JournalExists(_))
    ));
    Ok(())
}

#[test]
fn every_operation_is_journaled_before_it_happens() -> TestResult {
    let l = Lib::new()?;
    let journal = journal_path(&l.root, TS);
    let root = l.root.clone();
    let cal = l.cal.clone();
    let mut checked = 0;
    apply_with_hook(&l.plan()?, &l.roots(), TS, &mut |entry| {
        let on_disk = read_journal(&journal)?;
        assert_eq!(on_disk.last(), Some(entry), "line not on disk yet");
        let base = |r: &Root| if *r == Root::Library { &root } else { &cal };
        match entry {
            Entry::Rename { root: r, from, to } => {
                assert!(base(r).join(from).exists() && !base(r).join(to).exists());
            }
            Entry::Mkdir { root: r, path } => assert!(!base(r).join(path).exists()),
            Entry::Rmdir { root: r, path } => {
                assert!(fs::read_dir(base(r).join(path))?.next().is_none(), "{path}");
            }
            Entry::Write { path } => assert!(!root.join(path).exists()),
            _ => {}
        }
        checked += 1;
        Ok(())
    })?;
    assert!(checked > 0);
    Ok(())
}

/// The reference: the fixture migrated in one go, files and directories.
type Reference = (BTreeMap<String, String>, BTreeSet<String>, usize);

fn reference() -> Result<Reference, Box<dyn Error>> {
    let l = Lib::new()?;
    apply(&l.plan()?, &l.roots(), TS)?;
    let ops = operations(&read_journal(&journal_path(&l.root, TS))?);
    Ok((l.tree()?, l.dirs()?, ops))
}

#[test]
fn a_cut_anywhere_resumes_from_the_journal_to_the_same_tree() -> TestResult {
    let (want, want_dirs, ops) = reference()?;
    assert!(ops > 10, "fixture plan too small to cut: {ops}");
    let cuts: BTreeSet<usize> = [0, 1, 2, ops / 3, ops / 2, ops - 2, ops - 1].into();
    for cut in cuts {
        let l = Lib::new()?;
        let original = l.tree()?;
        let original_dirs = l.dirs()?;
        let mut n = 0;
        let result = apply_with_hook(&l.plan()?, &l.roots(), TS, &mut |_| {
            n += 1;
            if n > cut {
                Err(io::Error::other("power cut"))
            } else {
                Ok(())
            }
        });
        assert!(
            matches!(result, Err(ApplyError::Interrupted(_))),
            "cut {cut}: {result:?}"
        );
        assert_eq!(pending(&l.root)?.as_deref(), Some(TS), "cut {cut}");
        let report = resume(&l.roots(), TS)?;
        assert!(report.resumed, "cut {cut}");
        assert_eq!(l.tree()?, want, "cut {cut}: resumed tree differs");
        assert_eq!(
            l.dirs()?,
            want_dirs,
            "cut {cut}: resumed directories differ"
        );
        assert_eq!(pending(&l.root)?, None, "cut {cut}");
        assert!(matches!(
            resume(&l.roots(), TS),
            Err(ApplyError::NotOpen(_))
        ));

        undo(&l.roots(), TS)?;
        assert_eq!(l.tree()?, original, "cut {cut}: undo after resume differs");
        assert_eq!(
            l.dirs()?,
            original_dirs,
            "cut {cut}: undo left other directories"
        );
    }
    Ok(())
}

#[test]
fn undo_returns_the_identical_manifest() -> TestResult {
    let l = Lib::new()?;
    let before = l.tree()?;
    let before_dirs = l.dirs()?;
    let plan_before = l.plan()?;
    assert_eq!(user_files(&l.root)?, l.lib.manifest);

    apply(&plan_before, &l.roots(), TS)?;
    assert_ne!(l.tree()?, before);
    let report = undo(&l.roots(), TS)?;
    assert!(report.reverted > 0 && report.missing.is_empty());
    assert_eq!(report.set_aside.len(), plan_before.courses.len());
    assert_eq!(user_files(&l.root)?, l.lib.manifest, "library differs");
    assert_eq!(l.tree()?, before, "calendar differs");
    assert_eq!(l.dirs()?, before_dirs, "directories differ");
    assert!(report.dirs_restored > 0 && report.dirs_removed > 0);
    assert_eq!(report.dirs_left, 0);
    // The tree plans exactly as it did before the migration.
    assert_eq!(l.plan()?, plan_before);

    let again = undo(&l.roots(), TS)?;
    assert!(again.already_undone);
    assert_eq!(l.tree()?, before);
    Ok(())
}

#[test]
fn undo_of_a_cut_run_restores_the_manifest_and_can_be_repeated() -> TestResult {
    let l = Lib::new()?;
    let before = l.tree()?;
    let before_dirs = l.dirs()?;
    let mut n = 0;
    let cut = apply_with_hook(&l.plan()?, &l.roots(), TS, &mut |_| {
        n += 1;
        if n > 20 {
            Err(io::Error::other("cut"))
        } else {
            Ok(())
        }
    });
    assert!(cut.is_err());
    undo(&l.roots(), TS)?;
    assert_eq!(l.tree()?, before);
    assert_eq!(l.dirs()?, before_dirs);
    assert!(undo(&l.roots(), TS)?.already_undone);
    // An undone journal cannot be resumed.
    assert!(matches!(
        resume(&l.roots(), TS),
        Err(ApplyError::NotOpen(_))
    ));
    Ok(())
}

#[test]
fn undo_refuses_while_a_newer_migration_is_in_effect() -> TestResult {
    let l = Lib::new()?;
    let before = l.tree()?;
    apply(&l.plan()?, &l.roots(), TS)?;
    let second = Plan {
        moves: vec![Move {
            root: Root::Library,
            kind: EntryKind::File,
            from: v1::LOOSE_FILE_REL.to_string(),
            to: format!("otra/{}", v1::LOOSE_FILE_REL),
        }],
        ..Plan::default()
    };
    apply(&second, &l.roots(), TS2)?;
    assert!(matches!(
        undo(&l.roots(), TS),
        Err(ApplyError::NewerJournal { .. })
    ));
    undo(&l.roots(), TS2)?;
    undo(&l.roots(), TS)?;
    assert_eq!(l.tree()?, before);
    Ok(())
}

/// A bare tree for collision tests: `files` written under a temp root.
fn tree_of(files: &[(&str, &str)]) -> Result<TempDir, Box<dyn Error>> {
    let dir = tempdir()?;
    for (path, text) in files {
        let path = dir.path().join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, text)?;
    }
    Ok(dir)
}

fn collision(to: &str, from: &[&str], occupied: bool) -> Collision {
    Collision {
        root: Root::Library,
        to: to.to_string(),
        from: from.iter().map(|s| s.to_string()).collect(),
        occupied,
    }
}

fn hash8(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().as_str()[..8].to_string()
}

fn library(dir: &TempDir) -> Roots<'_> {
    Roots {
        library: dir.path(),
        caldir: None,
    }
}

#[test]
fn an_identical_destination_leaves_the_source_in_place() -> TestResult {
    let dir = tree_of(&[("a/x.pdf", "same"), ("b/x.pdf", "same")])?;
    let plan = Plan {
        duplicates: vec![collision("b/x.pdf", &["a/x.pdf"], true)],
        ..Plan::default()
    };
    let report = apply(&plan, &library(&dir), TS)?;
    assert_eq!(report.duplicates.len(), 1);
    assert!(report.moved.is_empty() && report.conflicts.is_empty());
    assert_eq!(fs::read_to_string(dir.path().join("a/x.pdf"))?, "same");
    assert_eq!(fs::read_to_string(dir.path().join("b/x.pdf"))?, "same");
    assert_eq!(report.journal, None, "nothing was done, nothing journaled");
    Ok(())
}

#[test]
fn a_different_destination_gets_a_conflict_name_and_is_not_touched() -> TestResult {
    // Same size, different bytes: listed as a likely duplicate, BLAKE3 decides.
    let dir = tree_of(&[("a/x.pdf", "new!"), ("b/x.pdf", "old!")])?;
    let plan = Plan {
        duplicates: vec![collision("b/x.pdf", &["a/x.pdf"], true)],
        ..Plan::default()
    };
    let report = apply(&plan, &library(&dir), TS)?;
    let name = format!("b/x.conflicto-{}.pdf", hash8("new!"));
    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(report.conflicts[0].to, name);
    assert_eq!(fs::read_to_string(dir.path().join(&name))?, "new!");
    assert_eq!(fs::read_to_string(dir.path().join("b/x.pdf"))?, "old!");
    assert!(!dir.path().join("a/x.pdf").exists());

    undo(&library(&dir), TS)?;
    assert_eq!(fs::read_to_string(dir.path().join("a/x.pdf"))?, "new!");
    assert!(!dir.path().join(&name).exists());
    Ok(())
}

#[test]
fn several_sources_for_a_free_destination() -> TestResult {
    let dir = tree_of(&[("a/x.md", "uno"), ("c/x.md", "uno"), ("d/x.md", "dos!")])?;
    let plan = Plan {
        conflicts: vec![collision("v2/x.md", &["a/x.md", "c/x.md", "d/x.md"], false)],
        ..Plan::default()
    };
    let report = apply(&plan, &library(&dir), TS)?;
    assert_eq!(fs::read_to_string(dir.path().join("v2/x.md"))?, "uno");
    assert_eq!(report.moved.len(), 1);
    assert_eq!(report.duplicates.len(), 1);
    assert!(dir.path().join("c/x.md").exists(), "duplicate source moved");
    let name = format!("v2/x.conflicto-{}.md", hash8("dos!"));
    assert_eq!(fs::read_to_string(dir.path().join(name))?, "dos!");
    Ok(())
}

#[test]
fn a_destination_that_appeared_after_planning_is_never_overwritten() -> TestResult {
    let dir = tree_of(&[("a/x.mp4", "grabacion"), ("v2/x.mp4", "otra")])?;
    let plan = Plan {
        moves: vec![Move {
            root: Root::Library,
            kind: EntryKind::File,
            from: "a/x.mp4".into(),
            to: "v2/x.mp4".into(),
        }],
        ..Plan::default()
    };
    let report = apply(&plan, &library(&dir), TS)?;
    assert_eq!(fs::read_to_string(dir.path().join("v2/x.mp4"))?, "otra");
    assert_eq!(report.conflicts.len(), 1);
    Ok(())
}

#[test]
fn a_missing_source_stops_the_run() -> TestResult {
    let dir = tree_of(&[("a/y", "y")])?;
    let plan = Plan {
        moves: vec![Move {
            root: Root::Library,
            kind: EntryKind::File,
            from: "a/x".into(),
            to: "b/x".into(),
        }],
        ..Plan::default()
    };
    assert!(matches!(
        apply(&plan, &library(&dir), TS),
        Err(ApplyError::SourceMissing(_))
    ));
    Ok(())
}

/// The spec's dry run on the real library: migrate a `cp -al` clone, rewrite
/// its markdown the way `migrate::rewrite` does (a new file renamed over the
/// old name), and the original is untouched.
#[cfg(unix)]
#[test]
fn a_hardlink_clone_migrates_without_touching_the_original() -> TestResult {
    use std::os::unix::fs::MetadataExt;

    let dir = tempdir()?;
    let orig = dir.path().join("orig");
    let clone = dir.path().join("clone");
    v1::write_v1_library(&orig)?;
    let status = std::process::Command::new("cp")
        .arg("-al")
        .arg(&orig)
        .arg(&clone)
        .status()?;
    assert!(status.success(), "cp -al failed");
    let probe = v1::LOOSE_FILE_REL;
    assert_eq!(
        fs::metadata(orig.join(probe))?.ino(),
        fs::metadata(clone.join(probe))?.ino(),
        "cp -al did not hardlink"
    );
    let before = v1::hash_tree(&orig)?;

    let inv = scan(&clone, None)?;
    let plan = plan(&inv, &v1::unab_naming()?, &[]);
    assert!(!plan.edits.is_empty());
    apply(
        &plan,
        &Roots {
            library: &clone,
            caldir: None,
        },
        TS,
    )?;
    let runtime = tokio::runtime::Runtime::new()?;
    for edit in &plan.edits {
        let path = clone.join(&edit.path);
        let mut text = b"---\nramo: x\n---\n".to_vec();
        text.extend(fs::read(&path)?);
        runtime.block_on(u_crawler::fsutil::atomic_write(&path, &text))?;
    }

    assert_eq!(v1::hash_tree(&orig)?, before, "the original changed");
    let migrated = user_files(&clone)?;
    let count = migrated.keys().filter(|p| !is_curso_json(p)).count();
    assert_eq!(count, before.len(), "file count differs");
    Ok(())
}

/// Why [`resume`] follows the journal's intent instead of a new plan: cut
/// right after a recording's base file moved, its `_1` sibling is no longer
/// recognizably a second part, and a fresh plan names it differently.
#[test]
fn a_plan_made_after_a_cut_is_not_the_original_plan() -> TestResult {
    let l = Lib::new()?;
    let original = l.plan()?;
    let (base, part) = l
        .lib
        .courses
        .iter()
        .flat_map(|c| c.zoom.iter().flat_map(|z| &z.recordings))
        .find_map(|r| {
            let base = format!("{}.mp4", r.rel.strip_suffix("_1.mp4")?);
            l.lib
                .manifest
                .contains_key(&base)
                .then(|| (base, r.rel.clone()))
        })
        .ok_or("fixture has no recording pair")?;
    let target = |p: &Plan, from: &str| {
        p.moves
            .iter()
            .find(|m| m.from == from)
            .map(|m| m.to.clone())
    };
    let part_target = target(&original, &part).ok_or("part not planned")?;

    let mut base_moved = false;
    let cut = apply_with_hook(&original, &l.roots(), TS, &mut |entry| {
        if base_moved {
            return Err(io::Error::other("cut"));
        }
        base_moved = matches!(entry, Entry::Rename { from, .. } if *from == base);
        Ok(())
    });
    assert!(cut.is_err());
    let replanned = l.plan()?;
    assert_ne!(target(&replanned, &part), Some(part_target.clone()));

    resume(&l.roots(), TS)?;
    assert!(l.root.join(&part_target).exists(), "{part_target}");
    Ok(())
}
