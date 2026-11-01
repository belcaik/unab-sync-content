//! The repair of a migrated library (layout v2) whose course has no Canvas
//! id in its `curso.json`: the 2025 courses of the real library, migrated
//! before `scan` read an id from a course's own links. Offline, here: the
//! course is renamed whole, gets its id and its orphaned `Zoom/<id>`, its
//! notes and README follow, a second pass does nothing, and undo puts the
//! tree back byte for byte.

use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tempfile::{tempdir, TempDir};
use u_crawler::config::Layout;
use u_crawler::layout::CourseNaming;
use u_crawler::migrate::plan::plan;
use u_crawler::migrate::{migrate, scan, undo, Outcome, Target};

type TestResult = Result<(), Box<dyn Error>>;

const OLD: &str = "x-mecanica-vieja";
/// What `course_dirs` names the course from its `canvas_name`, offline.
const NEW: &str = "202525-1149-mecanica-cfsa3220-202525-1149-el-on";
const NOTE: &str = "modulos/01-unidad/01-sesion.md";

struct Lib {
    _dir: TempDir,
    root: PathBuf,
    cal: PathBuf,
    naming: CourseNaming,
}

impl Lib {
    fn new() -> Result<Lib, Box<dyn Error>> {
        let dir = tempdir()?;
        let root = dir.path().join("lib");
        let cal = dir.path().join("cal");
        let put = |rel: &str, text: &str| -> io::Result<()> {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().ok_or_else(|| io::Error::other("no parent"))?)?;
            fs::write(path, text)
        };
        put(
            ".u_crawler/library.json",
            "{\n  \"layout_version\": 2,\n  \"links_version\": 2\n}\n",
        )?;
        put(
            &format!("{OLD}/curso.json"),
            r#"{"canvas_id":null,"course_code":null,"name":"Vieja Mecanica","canvas_name":"202525_1149_MECANICA_CFSA3220_202525_1149_EL_ON","term":null,"section":null,"zoom_ids":[]}"#,
        )?;
        put(
            &format!("{OLD}/.u_crawler/state.json"),
            r#"{"items":{"file:17317335":{"size":5}}}"#,
        )?;
        put(
            &format!("{OLD}/{NOTE}"),
            &format!(
                "---\nramo: \"{OLD}\"\ntipo: page\n---\nTexto.\n\n\
                 ![g](https://canvas.example.edu/courses/188678/files/17317335/preview)\n"
            ),
        )?;
        put(&format!("{OLD}/README.md"), "# Vieja Mecanica\n\nNada.\n")?;
        put(&format!("{OLD}/modulos/01-unidad/README.md"), "# Unidad\n")?;
        put("Zoom/188678/2025_10_01_202525_1149_MECANICA.mp4", "video")?;
        fs::create_dir_all(cal.join(OLD).join("deadlines"))?;
        fs::write(cal.join(OLD).join("deadlines/a.ics"), "BEGIN:VCALENDAR\n")?;
        Ok(Lib {
            _dir: dir,
            root,
            cal,
            naming: CourseNaming::new(&Layout::default())?,
        })
    }

    fn target(&self) -> Target<'_> {
        Target {
            library: &self.root,
            caldir: Some(&self.cal),
            naming: &self.naming,
        }
    }

    fn read(&self, rel: &str) -> io::Result<String> {
        fs::read_to_string(self.root.join(rel))
    }
}

/// Every file and directory under `root`, with the contents of files,
/// leaving out the migrator's own records (journals, backups, set-asides).
fn tree(root: &Path) -> io::Result<BTreeMap<String, Option<String>>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Option<String>>) -> io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            let rel = path
                .strip_prefix(root)
                .map_err(io::Error::other)?
                .to_string_lossy()
                .into_owned();
            if rel.starts_with(".u_crawler/migrations") || rel.starts_with(".u_crawler/backup") {
                continue;
            }
            if path.is_dir() {
                out.insert(rel, None);
                walk(root, &path, out)?;
            } else {
                out.insert(rel, Some(fs::read_to_string(&path)?));
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out)?;
    Ok(out)
}

#[tokio::test]
async fn a_course_without_canvas_id_is_repaired_in_place() -> TestResult {
    let lib = Lib::new()?;
    let Outcome::Migrated(summary) = migrate(&lib.target(), None, false).await? else {
        return Err("not migrated".into());
    };
    assert_eq!(summary.courses, 1);
    assert!(!lib.root.join(OLD).exists());

    let curso: serde_json::Value = serde_json::from_str(&lib.read(&format!("{NEW}/curso.json"))?)?;
    assert_eq!(curso["canvas_id"], 188678);
    assert_eq!(curso["zoom_ids"], serde_json::json!([188678]));
    assert_eq!(
        curso["canvas_name"],
        "202525_1149_MECANICA_CFSA3220_202525_1149_EL_ON"
    );

    let note = lib.read(&format!("{NEW}/{NOTE}"))?;
    assert!(
        note.starts_with(&format!("---\nramo: \"{NEW}\"\ntipo: page\n---\n")),
        "{note}"
    );
    let readme = lib.read(&format!("{NEW}/README.md"))?;
    assert!(!readme.starts_with("# Vieja Mecanica"), "{readme}");
    assert!(
        readme.contains("grabaciones/2025-10-01_clase.mp4"),
        "{readme}"
    );
    assert_eq!(
        lib.read(&format!("{NEW}/modulos/01-unidad/README.md"))?,
        "# Unidad\n"
    );
    assert_eq!(
        lib.read(&format!("{NEW}/grabaciones/2025-10-01_clase.mp4"))?,
        "video"
    );
    assert!(!lib.root.join("Zoom").exists());
    assert!(lib.cal.join(NEW).join("deadlines/a.ics").exists());
    assert!(!lib.cal.join(OLD).exists());
    assert!(lib.read("INDEX.md")?.contains(NEW));

    // Nothing left to repair.
    let inv = scan(&lib.root, Some(&lib.cal))?;
    let again = plan(&inv, &lib.naming, &[]);
    assert!(again.courses.is_empty() && again.is_empty(), "{again}");
    Ok(())
}

#[tokio::test]
async fn undo_puts_a_repaired_course_back_byte_for_byte() -> TestResult {
    let lib = Lib::new()?;
    let before = tree(&lib.root)?;
    let cal_before = tree(&lib.cal)?;
    let Outcome::Migrated(summary) = migrate(&lib.target(), None, false).await? else {
        return Err("not migrated".into());
    };
    let ts = summary.journal.ok_or("no journal")?;
    undo(&lib.target(), &ts).await?;
    let mut after = tree(&lib.root)?;
    // Undo sets library.json aside, so the next writer migrates again, and
    // leaves the root AGENTS.md / INDEX.md the run wrote (as any undo does).
    assert!(after.remove(".u_crawler/library.json").is_none());
    assert!(after.remove("AGENTS.md").is_some());
    assert!(after.remove("INDEX.md").is_some());
    let mut before = before;
    before.remove(".u_crawler/library.json");
    assert_eq!(after, before);
    assert_eq!(tree(&lib.cal)?, cal_before);
    Ok(())
}

#[tokio::test]
async fn a_dry_run_reports_the_repair_and_touches_nothing() -> TestResult {
    let lib = Lib::new()?;
    let before = tree(&lib.root)?;
    let Outcome::DryRun(dry) = migrate(&lib.target(), None, true).await? else {
        return Err("not a dry run".into());
    };
    assert_eq!(dry.plan.courses.len(), 1);
    assert!(dry.plan.courses[0].repair);
    assert_eq!(dry.plan.courses[0].dir, NEW);
    assert!(dry.plan.to_string().contains(" (repair)"));
    assert_eq!(tree(&lib.root)?, before);
    Ok(())
}
