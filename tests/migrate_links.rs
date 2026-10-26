//! Canvas links → local copies (T10), over the synthetic v1 library, offline:
//! migrating it leaves local links and no broken one; `migrate` over a v2
//! library whose notes still link to Canvas (what an earlier version left)
//! repairs them, once, under a journal `--undo` reverses byte for byte.
#[path = "fixtures/v1/mod.rs"]
mod v1;

use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
use tempfile::{tempdir, TempDir};
use u_crawler::frontmatter;
use u_crawler::layout::CourseNaming;
use u_crawler::migrate::ensure::{library_json_path, Summary};
use u_crawler::migrate::rewrite::generated_kind;
use u_crawler::migrate::{ensure_v2, migrate, undo, Outcome, Target};

type TestResult = Result<(), Box<dyn Error>>;

const LINKS_NOTE: &str = "atdf105-mineria-de-datos/modulos/02-semana-1/02-fase-4.md";
/// `library.json` as layout v2 wrote it before `links_version`.
const OLD_MARKER: &str = "{\n  \"layout_version\": 2\n}\n";

struct Lib {
    _dir: TempDir,
    root: PathBuf,
    naming: CourseNaming,
    lib: v1::V1Library,
}

impl Lib {
    fn new() -> Result<Lib, Box<dyn Error>> {
        let dir = tempdir()?;
        let root = dir.path().join("lib");
        let lib = v1::write_v1_library(&root)?;
        Ok(Lib {
            _dir: dir,
            root,
            naming: v1::unab_naming()?,
            lib,
        })
    }

    fn target(&self) -> Target<'_> {
        Target {
            library: &self.root,
            caldir: None,
            naming: &self.naming,
        }
    }

    fn read(&self, rel: &str) -> std::io::Result<String> {
        fs::read_to_string(self.root.join(rel))
    }

    /// Every library file outside the root's `.u_crawler/`.
    fn user_files(&self) -> std::io::Result<BTreeMap<String, String>> {
        let mut files = v1::hash_tree(&self.root)?;
        files.retain(|path, _| !path.starts_with(".u_crawler/"));
        Ok(files)
    }
}

fn migrated(outcome: Outcome) -> Result<Summary, Box<dyn Error>> {
    match outcome {
        Outcome::Migrated(summary) => Ok(summary),
        other => Err(format!("expected a migration, got {other:?}").into()),
    }
}

/// The body of [`LINKS_NOTE`] once migrated offline: what has a single
/// local copy points at it, the rest stays as Canvas wrote it.
const LOCAL_BODY: &str = "\
- [Guia\\_1.PDF](../01-informacion-general/adjuntos/guia-1.pdf)
- [descarga](/courses/9001/files/1002/download?download_frd=1)
- ![Diagrama](../01-informacion-general/adjuntos/diagrama.png)
- [Rubrica](../01-informacion-general/adjuntos/rubrica-final.docx)
- [rubrica](../01-informacion-general/adjuntos/rubrica-final.docx)
- [CodeR](../../anuncios/adjuntos/coder-s3.zip)
- [Fase 1](../01-informacion-general/01-informacion-general.md)
- [Contenidos](01-contenidos.md)
- [Item](https://canvas.example.edu/courses/9001/modules/items/7001)
- [Tarea](../../tareas/semana-10-formativa-6.md)
- [Aviso](../../anuncios/2026-06-17_bienvenidos-al-curso.md)
- [Otro ramo](../../../atdf101-disrupcion-empresarial/modulos/01-inicio/01-informacion-general.md)
- [Ramo](../../../atdf106-electivo-s2524/README.md)
- [Quiz](https://canvas.example.edu/courses/9001/quizzes/12)
- [Video](https://canvas.example.edu/media_attachments_iframe/333)
- [Zoom](https://canvas.example.edu/courses/9001/external_tools/187)
- ![ecuacion](https://canvas.example.edu/equation_images/x%2By)
- [correo](mailto:docente@example.edu)
- [correo bueno](mailto:docente@example.edu)

```
[en codigo](https://canvas.example.edu/courses/9001/pages/contenidos)
```
";

/// Local links resolved and not found: every relative markdown destination
/// of every generated note, percent-decoded, must be a file.
fn broken_links(root: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    let link = Regex::new(r"\]\(([^)\s]+)")?;
    let mut broken = Vec::new();
    for rel in v1::hash_tree(root)?.keys() {
        if generated_kind(Path::new(rel)).is_none() {
            continue;
        }
        let text = fs::read_to_string(root.join(rel))?;
        let dir = Path::new(rel).parent().unwrap_or(Path::new(""));
        for cap in link.captures_iter(&text) {
            let dest = &cap[1];
            if dest.contains(':') || dest.starts_with('/') || dest.starts_with('#') {
                continue;
            }
            let target = root.join(dir).join(percent_decode(dest));
            if !target.is_file() {
                broken.push(format!("{rel} -> {dest}"));
            }
        }
    }
    Ok(broken)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[tokio::test]
async fn migrating_the_v1_library_leaves_local_links_and_none_broken() -> TestResult {
    let lib = Lib::new()?;
    let summary = migrated(ensure_v2(&lib.target(), None, false).await?)?;
    assert_eq!(summary.rewrite_failed, 0, "{summary}");
    // Every link of LOCAL_BODY now pointing at a file (the `mailto:` fix is
    // not a local link).
    assert_eq!(summary.links, 11, "{summary}");
    assert!(
        summary.to_string().contains("11 local link(s)"),
        "{summary}"
    );

    let mineria = lib.lib.course_by_code("ATDF105_202615_2690_EL_ON");
    assert!(LINKS_NOTE.starts_with(&format!("{}/", mineria.ok_or("no mineria")?.v2_dir)));
    let note = lib.read(LINKS_NOTE)?;
    assert!(note.ends_with(LOCAL_BODY), "{note}");
    assert!(frontmatter::parse(&note)?.is_some());
    assert_eq!(broken_links(&lib.root)?, Vec::<String>::new());

    let marker: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(library_json_path(&lib.root))?)?;
    assert_eq!(marker["links_version"], 1);

    // Nothing left to do: the writers' gate is cheap, and `migrate` agrees.
    let after = v1::hash_tree(&lib.root)?;
    assert_eq!(
        ensure_v2(&lib.target(), None, false).await?,
        Outcome::AlreadyV2
    );
    let again = migrated(migrate(&lib.target(), None, false).await?)?;
    assert!(!again.changed_files(), "{again}");
    assert_eq!(v1::hash_tree(&lib.root)?, after);
    Ok(())
}

/// A v2 library as the previous version left it: frontmatter on every note
/// but the Canvas links of the v1 body, and `library.json` without
/// `links_version`. Built from a migration's own backups.
async fn remote_v2_library() -> Result<(Lib, String), Box<dyn Error>> {
    let lib = Lib::new()?;
    let summary = migrated(ensure_v2(&lib.target(), None, false).await?)?;
    let ts = summary.journal.ok_or("no journal")?;
    let backup = lib.root.join(".u_crawler/backup").join(&ts);
    for (rel, _) in v1::hash_tree(&backup)? {
        // The backup holds the v1 bytes under the v2 path.
        let original = fs::read_to_string(backup.join(&rel))?;
        let current = lib.read(&rel)?;
        let fm = frontmatter::parse(&current)?.ok_or("migrated note without frontmatter")?;
        fs::write(lib.root.join(&rel), frontmatter::upsert(&original, &fm))?;
    }
    fs::write(library_json_path(&lib.root), OLD_MARKER)?;
    Ok((lib, ts))
}

#[tokio::test]
async fn migrate_repairs_a_v2_library_with_remote_links_and_undo_reverts_it() -> TestResult {
    let (lib, migration) = remote_v2_library().await?;
    assert!(lib
        .read(LINKS_NOTE)?
        .contains("courses/9001/pages/contenidos?"));
    let remote = lib.user_files()?;

    // A dry run counts and writes nothing.
    match migrate(&lib.target(), None, true).await? {
        Outcome::DryRun(dry) => assert_eq!(dry.links, Some(11)),
        other => return Err(format!("expected a dry run, got {other:?}").into()),
    }
    assert_eq!(lib.user_files()?, remote);

    // The writers' gate relinks a library from before `links_version`.
    let summary = migrated(ensure_v2(&lib.target(), None, false).await?)?;
    assert_eq!(summary.links, 11, "{summary}");
    assert_eq!(summary.moved, 0);
    assert!(!summary.marked, "it was v2 already");
    let relink = summary.journal.clone().ok_or("no relink journal")?;
    assert_ne!(relink, migration);
    assert!(lib.read(LINKS_NOTE)?.ends_with(LOCAL_BODY));
    assert_eq!(broken_links(&lib.root)?, Vec::<String>::new());

    // Second pass: nothing.
    let repaired = v1::hash_tree(&lib.root)?;
    assert_eq!(
        ensure_v2(&lib.target(), None, false).await?,
        Outcome::AlreadyV2
    );
    let again = migrated(migrate(&lib.target(), None, false).await?)?;
    assert!(!again.changed_files(), "{again}");
    assert_eq!(v1::hash_tree(&lib.root)?, repaired);

    // The migration under it cannot be undone first.
    assert!(undo(&lib.target(), &migration).await.is_err());
    // Undo puts the notes back byte for byte and keeps the library v2.
    let undone = undo(&lib.target(), &relink).await?;
    assert_eq!(undone.restored, summary.rewritten);
    assert!(!undone.unmarked);
    assert_eq!(lib.user_files()?, remote);
    assert!(library_json_path(&lib.root).is_file());
    // `links_version` stays, so the writers do not relink it again.
    assert_eq!(
        ensure_v2(&lib.target(), None, false).await?,
        Outcome::AlreadyV2
    );
    assert_eq!(lib.user_files()?, remote);
    Ok(())
}

#[tokio::test]
async fn a_note_of_another_course_without_canvas_id_keeps_its_page_links() -> TestResult {
    // ATDF106-2520 has no index.json, so no Canvas id: a page link names a
    // course, and nothing says it is this one.
    let lib = Lib::new()?;
    let rel = "202615_2520_ELECTIVO_ATDF106_202615_2520_EL_ON/Modules/310_Avisos/01-Informacion_general.md";
    let mut body = fs::read_to_string(lib.root.join(rel))?;
    body.push_str("[Inicio](https://canvas.example.edu/courses/7777/pages/informacion-general)\n");
    fs::write(lib.root.join(rel), &body)?;
    migrated(ensure_v2(&lib.target(), None, false).await?)?;
    let note = lib.read("atdf106-electivo-s2520/modulos/01-avisos/01-informacion-general.md")?;
    assert!(
        note.contains("(https://canvas.example.edu/courses/7777/pages/informacion-general)"),
        "{note}"
    );
    Ok(())
}
