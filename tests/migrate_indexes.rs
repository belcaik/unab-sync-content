//! The index files an offline migration leaves in the synthetic v1 library
//! (T1): a `README.md` per course and per module, `curso.json` with the
//! readable name, term and section, and a root `INDEX.md` whose links all
//! resolve. Also what `ensure_v2` does for a v2 library that lacks them.
#[path = "fixtures/v1/mod.rs"]
mod v1;

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::{tempdir, TempDir};
use u_crawler::layout::CourseNaming;
use u_crawler::migrate::{ensure_v2, Outcome, Target};

type TestResult = Result<(), Box<dyn Error>>;

const GOLDEN: &str = "tests/golden/migrate_indexes_v1.txt";
const MINERIA: &str = "ATDF105_202615_2690_EL_ON";

/// The fixture at `<tmp>/lib`, migrated offline the way a `sync` does it.
struct Migrated {
    _dir: TempDir,
    root: PathBuf,
    naming: CourseNaming,
    lib: v1::V1Library,
}

impl Migrated {
    async fn new() -> Result<Migrated, Box<dyn Error>> {
        let dir = tempdir()?;
        let root = dir.path().join("lib");
        let lib = v1::write_v1_library(&root)?;
        let naming = v1::unab_naming()?;
        let m = Migrated {
            _dir: dir,
            root,
            naming,
            lib,
        };
        match ensure_v2(&m.target(), None, false).await? {
            Outcome::Migrated(summary) => assert!(summary.indexes > 0, "{summary}"),
            other => return Err(format!("expected a migration, got {other:?}").into()),
        }
        Ok(m)
    }

    fn target(&self) -> Target<'_> {
        Target {
            library: &self.root,
            caldir: None,
            naming: &self.naming,
        }
    }

    /// Every `README.md`, `curso.json` and `INDEX.md` in the library,
    /// relative paths, sorted (`.u_crawler/` excluded).
    fn index_files(&self) -> Result<Vec<String>, Box<dyn Error>> {
        let mut files: Vec<String> = v1::hash_tree(&self.root)?
            .into_keys()
            .filter(|p| !p.starts_with(".u_crawler/"))
            .filter(|p| {
                let name = p.rsplit('/').next().unwrap_or(p);
                matches!(name, "README.md" | "curso.json" | "INDEX.md")
            })
            .collect();
        files.sort();
        Ok(files)
    }
}

/// The relative targets of the Markdown links in `text`.
fn relative_links(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("](") {
        rest = &rest[i + 2..];
        let Some(end) = rest.find(')') else { break };
        let target = &rest[..end];
        if !target.contains("://") && !target.starts_with('#') {
            out.push(target.to_string());
        }
        rest = &rest[end..];
    }
    out
}

#[tokio::test]
async fn the_indexes_match_the_golden_snapshot() -> TestResult {
    let m = Migrated::new().await?;
    let mut got = String::new();
    for rel in m.index_files()? {
        got.push_str(&format!("=== {rel}\n"));
        got.push_str(&fs::read_to_string(m.root.join(&rel))?);
    }
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN);
    if std::env::var_os("U_CRAWLER_BLESS").is_some() {
        fs::write(&golden, &got)?;
    }
    let want = fs::read_to_string(&golden)?;
    assert_eq!(
        got, want,
        "indexes differ from {GOLDEN}; rerun with U_CRAWLER_BLESS=1 to accept"
    );
    Ok(())
}

#[tokio::test]
async fn every_course_and_module_has_a_readme_and_no_link_is_broken() -> TestResult {
    let m = Migrated::new().await?;
    for course in &m.lib.courses {
        let dir = m.root.join(&course.v2_dir);
        assert!(dir.join("README.md").is_file(), "{}", course.v2_dir);
        for module in fs::read_dir(dir.join("modulos"))? {
            let module = module?.path();
            assert!(module.join("README.md").is_file(), "{}", module.display());
        }
    }

    let files = m.index_files()?;
    let markdown: Vec<&String> = files.iter().filter(|p| p.ends_with(".md")).collect();
    assert!(markdown.iter().any(|p| *p == "INDEX.md"));
    let mut checked = 0;
    for rel in markdown {
        let file = m.root.join(rel);
        let base = file.parent().ok_or("no parent")?;
        for link in relative_links(&fs::read_to_string(&file)?) {
            assert!(
                base.join(&link).is_file(),
                "{rel} links to {link}, which does not exist"
            );
            checked += 1;
        }
    }
    assert!(
        checked > m.lib.courses.len(),
        "only {checked} links checked"
    );
    Ok(())
}

#[tokio::test]
async fn curso_json_has_the_readable_name_term_and_section() -> TestResult {
    let m = Migrated::new().await?;
    let course = m.lib.course_by_code(MINERIA).ok_or("no mineria course")?;
    let curso: serde_json::Value = serde_json::from_str(&fs::read_to_string(
        m.root.join(&course.v2_dir).join("curso.json"),
    )?)?;
    assert_eq!(curso["name"], "Mineria de Datos");
    assert_eq!(curso["canvas_name"], "202615_2690_MINERIA_DE_DATOS");
    assert_eq!(curso["term"], "202615");
    assert_eq!(curso["section"], "2690");

    let index = fs::read_to_string(m.root.join("INDEX.md"))?;
    assert!(
        index.contains(&format!(
            "- [Mineria de Datos]({}/README.md)",
            course.v2_dir
        )),
        "{index}"
    );
    assert!(!index.contains("202615_2690_MINERIA"), "{index}");
    Ok(())
}

#[tokio::test]
async fn a_v2_library_without_readmes_gets_them_and_keeps_its_own() -> TestResult {
    let m = Migrated::new().await?;
    let course = &m.lib.courses[0];
    let dir = m.root.join(&course.v2_dir);
    let mut removed = Vec::new();
    for rel in m.index_files()? {
        if rel.ends_with("README.md") && !rel.starts_with(&format!("{}/", course.v2_dir)) {
            fs::remove_file(m.root.join(&rel))?;
            removed.push(rel);
        }
    }
    assert!(!removed.is_empty());
    // A README already there, whoever wrote it, is left alone.
    fs::write(dir.join("README.md"), "# Mis apuntes\n")?;

    assert_eq!(
        ensure_v2(&m.target(), None, false).await?,
        Outcome::AlreadyV2
    );
    for rel in &removed {
        assert!(m.root.join(rel).is_file(), "{rel} not written back");
    }
    assert_eq!(
        fs::read_to_string(dir.join("README.md"))?,
        "# Mis apuntes\n"
    );

    // A dry run writes nothing.
    fs::remove_file(m.root.join(&removed[0]))?;
    assert_eq!(
        ensure_v2(&m.target(), None, true).await?,
        Outcome::AlreadyV2
    );
    assert!(!m.root.join(&removed[0]).exists());
    Ok(())
}
