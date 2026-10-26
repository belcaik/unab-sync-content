//! The whole migration (`migrate::ensure`) over the synthetic v1 library
//! (T1), offline, the way `sync` runs it: it migrates, a second pass does
//! nothing, and no byte is lost (every hash of the v1 manifest is still in
//! the tree, a rewritten note's original under `.u_crawler/backup/`).
#[path = "fixtures/v1/mod.rs"]
mod v1;

use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tempfile::{tempdir, TempDir};
use u_crawler::frontmatter::{self, DocKind};
use u_crawler::layout::CourseNaming;
use u_crawler::migrate::apply::{self, apply_with_hook, Roots};
use u_crawler::migrate::ensure::{library_json_path, Summary};
use u_crawler::migrate::plan::{plan, Plan};
use u_crawler::migrate::{ensure_v2, migrate, scan, undo, Outcome, Target};

type TestResult = Result<(), Box<dyn Error>>;

const MINERIA: &str = "ATDF105_202615_2690_EL_ON";
/// The Canvas file id the fixture's `CodeR_S3.zip` has in `index.json`.
const CODER_FILE_ID: u64 = 4242;

/// The fixture at `<tmp>/lib`, a calendar tree at `<tmp>/cal` holding the
/// first course's v1 directory, and the manifest of both before migrating.
struct Lib {
    _dir: TempDir,
    root: PathBuf,
    cal: PathBuf,
    naming: CourseNaming,
    lib: v1::V1Library,
    /// Every file before the migration, after the test's own edits.
    before: BTreeMap<String, String>,
}

impl Lib {
    fn new() -> Result<Lib, Box<dyn Error>> {
        let dir = tempdir()?;
        let root = dir.path().join("lib");
        let cal = dir.path().join("cal");
        let lib = v1::write_v1_library(&root)?;
        let deadlines = cal.join(&lib.courses[0].v1_dir).join("deadlines");
        fs::create_dir_all(&deadlines)?;
        fs::write(deadlines.join("assignment-1.ics"), "BEGIN:VCALENDAR\n")?;

        // A Canvas link to the announcement attachment, as v1 wrote it.
        let mineria = lib.course_by_code(MINERIA).ok_or("no mineria course")?;
        let note = only_file(&root.join(&mineria.v1_dir).join("announcements"), ".md")?;
        let mut body = fs::read_to_string(&note)?;
        body.push_str(&format!(
            "\n[CodeR](https://canvas.example.edu/courses/9001/files/{CODER_FILE_ID}/download)\n"
        ));
        fs::write(&note, body)?;

        let before = v1::hash_tree(&root)?;
        Ok(Lib {
            _dir: dir,
            root,
            cal,
            naming: v1::unab_naming()?,
            lib,
            before,
        })
    }

    fn target(&self) -> Target<'_> {
        Target {
            library: &self.root,
            caldir: Some(&self.cal),
            naming: &self.naming,
        }
    }

    fn plan(&self) -> Result<Plan, Box<dyn Error>> {
        let inv = scan(&self.root, Some(&self.cal))?;
        Ok(plan(&inv, &self.naming, &[]))
    }

    /// Every library file outside `.u_crawler/`.
    fn user_files(&self) -> io::Result<BTreeMap<String, String>> {
        let mut files = v1::hash_tree(&self.root)?;
        files.retain(|path, _| !path.starts_with(".u_crawler/"));
        Ok(files)
    }
}

fn only_file(dir: &Path, ext: &str) -> io::Result<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_file() && path.to_string_lossy().ends_with(ext) {
            found.push(path);
        }
    }
    match found.as_slice() {
        [one] => Ok(one.clone()),
        _ => Err(io::Error::other(format!(
            "{} {ext} files in {}",
            found.len(),
            dir.display()
        ))),
    }
}

fn migrated(outcome: Outcome) -> Result<Summary, Box<dyn Error>> {
    match outcome {
        Outcome::Migrated(summary) => Ok(summary),
        other => Err(format!("expected a migration, got {other:?}").into()),
    }
}

/// Each hash of `before`, as many times as it appears, is somewhere in `after`.
fn no_loss(
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) -> Result<(), String> {
    let mut have: BTreeMap<&str, usize> = BTreeMap::new();
    for hash in after.values() {
        *have.entry(hash).or_default() += 1;
    }
    for (path, hash) in before {
        match have.get_mut(hash.as_str()) {
            Some(n) if *n > 0 => *n -= 1,
            _ => return Err(format!("the bytes of {path} are gone")),
        }
    }
    Ok(())
}

#[tokio::test]
async fn a_sync_migrates_the_v1_library_once_and_loses_nothing() -> TestResult {
    let lib = Lib::new()?;
    let planned = lib.plan()?;
    assert!(!planned.edits.is_empty());

    let summary = migrated(ensure_v2(&lib.target(), None, false).await?)?;
    assert!(summary.marked);
    assert!(summary.changed_files());
    assert_eq!(summary.courses, lib.lib.courses.len());
    assert_eq!(summary.moved, planned.moves.len());
    assert_eq!(summary.rewritten, planned.edits.len());
    assert_eq!(summary.rewrite_failed, 0);
    assert!(summary.journal.is_some());

    // Marked v2, with the root index files.
    let marker: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(library_json_path(&lib.root))?)?;
    assert_eq!(marker["layout_version"], 2);
    let index = fs::read_to_string(lib.root.join("INDEX.md"))?;
    assert!(
        index.contains("atdf105-mineria-de-datos/README.md"),
        "{index}"
    );
    assert!(lib.root.join("AGENTS.md").is_file());

    // No loss: every v1 byte is in the tree; the v1 course directories and
    // mapped `Zoom/<id>` folders are gone, emptied and removed.
    let after = v1::hash_tree(&lib.root)?;
    no_loss(&lib.before, &after)?;
    for course in &lib.lib.courses {
        assert!(
            !lib.root.join(&course.v1_dir).exists(),
            "{} is still there",
            course.v1_dir
        );
        if let Some(zoom) = &course.zoom {
            assert!(!lib.root.join(&zoom.rel).exists(), "{}", zoom.rel);
        }
    }
    assert!(summary.removed_dirs > 0);
    // Only `Zoom/` is kept, for its orphan folder.
    assert_eq!(summary.kept_dirs, ["Zoom"]);
    for path in &lib.lib.foreign {
        assert_eq!(after.get(path), lib.before.get(path), "{path} changed");
    }
    // The calendar directory followed its course.
    let course = &lib.lib.courses[0];
    assert!(lib
        .cal
        .join(&course.v2_dir)
        .join("deadlines/assignment-1.ics")
        .is_file());
    assert!(!lib.cal.join(&course.v1_dir).exists());

    // Every generated note has its frontmatter, and the original is backed up.
    for edit in &planned.edits {
        let text = fs::read_to_string(lib.root.join(&edit.path))?;
        let fm = frontmatter::parse(&text)?.ok_or_else(|| format!("{} has none", edit.path))?;
        assert_eq!(fm.ramo, edit.course, "{}", edit.path);
    }
    let mineria = lib.course_by_code_v2(MINERIA)?;
    let note = only_file(&lib.root.join(&mineria).join("anuncios"), ".md")?;
    let text = fs::read_to_string(&note)?;
    let fm = frontmatter::parse(&text)?.ok_or("announcement has no frontmatter")?;
    assert_eq!(fm.tipo, DocKind::Announcement);
    assert_eq!(fm.canvas_id, Some(501));
    assert!(fm.url.is_some_and(|u| u.contains("/courses/9001/")));
    assert!(text.contains("[CodeR](adjuntos/coder-s3.zip)"), "{text}");
    // The twin announcement got its own note, with its own id and url.
    let disrupcion = lib.course_by_code_v2("ATDF101_202605_2333_EL_ON")?;
    let twin = lib
        .root
        .join(&disrupcion)
        .join("anuncios/2026-03-01_inicio-de-clases-2.md");
    let fm = frontmatter::parse(&fs::read_to_string(&twin)?)?.ok_or("twin has no frontmatter")?;
    assert_eq!(fm.canvas_id, Some(v1::TWIN_ANNOUNCEMENT_ID));
    assert!(fm
        .url
        .is_some_and(|u| u.ends_with(&format!("/{}", v1::TWIN_ANNOUNCEMENT_ID))));

    // Second pass: nothing to do, nothing touched.
    assert_eq!(
        ensure_v2(&lib.target(), None, false).await?,
        Outcome::AlreadyV2
    );
    let again = migrated(migrate(&lib.target(), None, false).await?)?;
    assert!(!again.changed_files(), "{again}");
    assert!(lib.plan()?.is_empty());
    assert_eq!(v1::hash_tree(&lib.root)?, after);
    Ok(())
}

impl Lib {
    fn course_by_code_v2(&self, code: &str) -> Result<String, Box<dyn Error>> {
        Ok(self
            .lib
            .course_by_code(code)
            .ok_or("no such course")?
            .v2_dir
            .clone())
    }
}

#[tokio::test]
async fn a_dry_run_moves_and_writes_nothing() -> TestResult {
    let lib = Lib::new()?;
    let cal_before = v1::hash_tree(&lib.cal)?;
    match ensure_v2(&lib.target(), None, true).await? {
        Outcome::DryRun(dry) => {
            assert!(!dry.plan.is_empty());
            assert!(!dry.marked);
            assert_eq!(dry.pending, None);
        }
        other => return Err(format!("expected a dry run, got {other:?}").into()),
    }
    assert_eq!(v1::hash_tree(&lib.root)?, lib.before);
    assert_eq!(v1::hash_tree(&lib.cal)?, cal_before);
    assert!(!lib.root.join(".u_crawler").exists());
    Ok(())
}

#[tokio::test]
async fn undo_puts_the_v1_tree_back_and_the_next_run_migrates_again() -> TestResult {
    let lib = Lib::new()?;
    let summary = migrated(ensure_v2(&lib.target(), None, false).await?)?;
    let ts = summary.journal.ok_or("no journal")?;

    let undone = undo(&lib.target(), &ts).await?;
    assert_eq!(undone.restored, summary.rewritten);
    assert!(undone.unmarked);
    assert!(!library_json_path(&lib.root).exists());

    // Byte for byte, except the two regenerated root index files, and every
    // v1 directory is back.
    let mut files = lib.user_files()?;
    files.remove("AGENTS.md");
    files.remove("INDEX.md");
    assert_eq!(files, lib.before);
    for course in &lib.lib.courses {
        assert!(lib.root.join(&course.v1_dir).is_dir(), "{}", course.v1_dir);
        assert!(!lib.root.join(&course.v2_dir).exists(), "{}", course.v2_dir);
    }
    assert!(lib.cal.join(&lib.lib.courses[0].v1_dir).is_dir());

    assert!(undo(&lib.target(), &ts).await?.already_undone);
    let again = migrated(ensure_v2(&lib.target(), None, false).await?)?;
    assert!(again.changed_files());
    no_loss(&lib.before, &v1::hash_tree(&lib.root)?)?;
    Ok(())
}

#[tokio::test]
async fn a_run_cut_mid_apply_is_resumed_from_its_journal() -> TestResult {
    let lib = Lib::new()?;
    let planned = lib.plan()?;
    let roots = Roots {
        library: &lib.root,
        caldir: Some(&lib.cal),
    };
    let mut ops = 0;
    let cut = apply_with_hook(&planned, &roots, "20261025T120000000Z", &mut |_| {
        ops += 1;
        if ops > 10 {
            Err(io::Error::other("power cut"))
        } else {
            Ok(())
        }
    });
    assert!(cut.is_err());
    assert!(apply::pending(&lib.root)?.is_some());

    let summary = migrated(ensure_v2(&lib.target(), None, false).await?)?;
    assert_eq!(summary.resumed.as_deref(), Some("20261025T120000000Z"));
    assert_eq!(summary.rewritten, planned.edits.len());
    assert!(lib.plan()?.is_empty());
    no_loss(&lib.before, &v1::hash_tree(&lib.root)?)?;
    Ok(())
}

#[tokio::test]
async fn notes_are_still_rewritten_when_the_cut_came_after_the_moves() -> TestResult {
    let lib = Lib::new()?;
    let planned = lib.plan()?;
    let roots = Roots {
        library: &lib.root,
        caldir: Some(&lib.cal),
    };
    // Every rename done and journaled, then the process died.
    apply::apply(&planned, &roots, "20261025T120000000Z")?;
    assert!(lib.plan()?.is_empty());

    let summary = migrated(ensure_v2(&lib.target(), None, false).await?)?;
    assert_eq!(summary.journal, None);
    assert_eq!(summary.rewritten, planned.edits.len());
    assert!(summary.marked);
    Ok(())
}

#[tokio::test]
async fn a_missing_root_is_left_alone_and_an_empty_one_is_marked() -> TestResult {
    let dir = tempdir()?;
    let naming = v1::unab_naming()?;
    let missing = dir.path().join("nada");
    let target = Target {
        library: &missing,
        caldir: None,
        naming: &naming,
    };
    assert_eq!(ensure_v2(&target, None, false).await?, Outcome::NoLibrary);
    assert!(!missing.exists());

    let empty = dir.path().join("vacia");
    fs::create_dir_all(&empty)?;
    fs::write(empty.join("notas.md"), "mias\n")?;
    let target = Target {
        library: &empty,
        caldir: None,
        naming: &naming,
    };
    let summary = migrated(ensure_v2(&target, None, false).await?)?;
    assert!(summary.marked);
    assert!(!summary.changed_files());
    assert_eq!(fs::read_to_string(empty.join("notas.md"))?, "mias\n");
    assert_eq!(ensure_v2(&target, None, false).await?, Outcome::AlreadyV2);
    Ok(())
}

#[tokio::test]
async fn a_caldir_used_as_download_root_is_never_marked() -> TestResult {
    let dir = tempdir()?;
    let naming = v1::unab_naming()?;
    let cal = dir.path().join("cal");
    fs::create_dir_all(cal.join("atdf105-mineria-de-datos/deadlines"))?;
    let target = Target {
        library: &cal,
        caldir: Some(&cal),
        naming: &naming,
    };
    assert_eq!(ensure_v2(&target, None, false).await?, Outcome::NoLibrary);
    assert!(migrate(&target, None, false).await.is_err());
    assert!(!cal.join(".u_crawler").exists());
    assert!(!cal.join("AGENTS.md").exists());
    Ok(())
}
