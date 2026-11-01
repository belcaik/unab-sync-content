//! Pure rendering of the layout v2 index files.
//!
//! Both `sync` (online, from Canvas data) and `migrate` (offline, from a disk
//! scan) build the same in-memory model — [`LibraryIndex`] — and these
//! functions turn it into text: a `README.md` per course and per module, a
//! `curso.json` per course, and the library-level `INDEX.md` and `AGENTS.md`.
//! Nothing here touches the filesystem or the network; every name comes from
//! [`crate::layout`], never from ad-hoc formatting.

use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;

use crate::layout::{self, CourseMeta};

/// Announcements listed in a course README before folding the rest away.
pub const RECENT_ANNOUNCEMENTS: usize = 10;

/// The whole library: every course, in any order. Rendering sorts by
/// directory name, so the same model always produces the same text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LibraryIndex {
    /// One entry per course directory.
    pub courses: Vec<CourseIndex>,
}

/// One course directory of the library.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CourseIndex {
    /// The directory name under the library root (from
    /// [`layout::CourseNaming::course_dirs`]).
    pub dir: String,
    /// Canvas identity of the course; `id` is `None` after an offline
    /// migration that never saw a `courses/<id>`.
    pub course: CourseMeta,
    /// The name a person reads ([`layout::CourseNaming::display_name`]);
    /// empty falls back to `course.name`, then to `dir`.
    pub title: String,
    /// Academic term (e.g. `202615`), when a builder recovered one.
    pub term: Option<String>,
    /// Section / NRC (e.g. `2690`), when a builder recovered one.
    pub section: Option<String>,
    /// Ids of the v1 `Zoom/<id>` folders whose recordings migrated here.
    pub zoom_ids: Vec<u64>,
    /// Modules in any order; rendering sorts by position.
    pub modules: Vec<ModuleIndex>,
    /// Assignments with a file under `tareas/`, in any order.
    pub assignments: Vec<AssignmentIndex>,
    /// Announcements with a file under `anuncios/`, in any order.
    pub announcements: Vec<AnnouncementIndex>,
    /// Recordings with a file under `grabaciones/`, in any order.
    pub recordings: Vec<RecordingIndex>,
}

impl CourseIndex {
    /// What the indexes call the course: [`Self::title`], else the Canvas
    /// name, else the directory.
    pub fn display(&self) -> &str {
        [&self.title, &self.course.name, &self.dir]
            .into_iter()
            .find(|s| !s.is_empty())
            .map_or("", String::as_str)
    }
}

/// One module of a course, with the pages and assignments it links.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModuleIndex {
    /// Canvas `position`, or the module-id order when offline.
    pub position: u32,
    /// Display name.
    pub name: String,
    /// Directory name under `modulos/` (from [`layout::module_dir`]).
    pub dir: String,
    /// Pages rendered inside the module directory.
    pub pages: Vec<PageIndex>,
    /// Assignments of the module; their files live under `tareas/`.
    pub assignments: Vec<AssignmentRef>,
}

impl ModuleIndex {
    /// A module whose directory name is derived from its position and name.
    pub fn new(position: u32, name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            position,
            dir: layout::module_dir(position, &name),
            name,
            ..Default::default()
        }
    }
}

/// A page inside a module directory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageIndex {
    /// Position of the item inside its module.
    pub position: u32,
    /// Display title.
    pub title: String,
    /// File name inside the module directory (from [`layout::page_file`]).
    pub file: String,
}

impl PageIndex {
    /// A page whose file name is derived from its position and title.
    pub fn new(position: u32, title: impl Into<String>) -> Self {
        let title = title.into();
        Self {
            position,
            file: layout::page_file(position, &title),
            title,
        }
    }
}

/// A link to an assignment, as listed by a module README.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AssignmentRef {
    /// Display title.
    pub title: String,
    /// File name under `tareas/` (from [`layout::assignment_file`]).
    pub file: String,
}

impl AssignmentRef {
    /// A reference whose file name is derived from the assignment title.
    pub fn new(title: impl Into<String>) -> Self {
        let title = title.into();
        let file = layout::assignment_file(&title);
        Self { title, file }
    }
}

/// An assignment with a file under `tareas/`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AssignmentIndex {
    /// Canvas assignment id, when known.
    pub id: Option<u64>,
    /// Display title.
    pub title: String,
    /// File name under `tareas/` (from [`layout::assignment_file`]).
    pub file: String,
    /// Due date, when Canvas set one.
    pub due_at: Option<DateTime<Utc>>,
    /// Unlock date, when Canvas set one.
    pub unlock_at: Option<DateTime<Utc>>,
    /// Canvas URL of the assignment, when known.
    pub html_url: Option<String>,
}

impl AssignmentIndex {
    /// An assignment whose file name is derived from its title.
    pub fn new(id: Option<u64>, title: impl Into<String>) -> Self {
        let title = title.into();
        let file = layout::assignment_file(&title);
        Self {
            id,
            title,
            file,
            ..Default::default()
        }
    }
}

impl From<&crate::canvas::Assignment> for AssignmentIndex {
    fn from(a: &crate::canvas::Assignment) -> Self {
        let mut idx = Self::new(Some(a.id), a.name.clone().unwrap_or_default());
        idx.due_at = a.due_at;
        idx.unlock_at = a.unlock_at;
        idx.html_url = a.html_url.clone();
        idx
    }
}

/// An announcement with a file under `anuncios/`.
///
/// There is deliberately no constructor that invents a file name without a
/// date: the v2 name is `YYYY-MM-DD_<slug>.md`, so a builder that has no
/// `posted` date (a disk scan of an already-migrated tree) records the file
/// name it found instead of making one up.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AnnouncementIndex {
    /// Canvas discussion topic id, when known.
    pub id: Option<u64>,
    /// Display title.
    pub title: String,
    /// Posting date, when known.
    pub posted: Option<NaiveDate>,
    /// File name under `anuncios/`.
    pub file: String,
}

impl AnnouncementIndex {
    /// An announcement whose file name is derived from its date and title.
    pub fn new(id: Option<u64>, title: impl Into<String>, posted: NaiveDate) -> Self {
        let title = title.into();
        Self {
            id,
            file: layout::announcement_file(posted, &title),
            title,
            posted: Some(posted),
        }
    }
}

/// A recording with a file under `grabaciones/`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecordingIndex {
    /// File name under `grabaciones/` (from [`layout::recording_file`], or the
    /// name found on disk).
    pub file: String,
    /// Recording date, when known.
    pub date: Option<NaiveDate>,
    /// Display topic.
    pub topic: String,
}

/// The `README.md` of a course directory: metadata, then the ordered module
/// list, assignments with their due dates, recent announcements and the
/// recordings. Empty sections are omitted.
pub fn render_course_readme(course: &CourseIndex) -> String {
    let mut out = format!("# {}\n", course.display());

    let mut meta: Vec<String> = Vec::new();
    if let Some(code) = &course.course.code {
        meta.push(format!("**Código:** {code}"));
    }
    if let Some(term) = &course.term {
        meta.push(format!("**Término:** {term}"));
    }
    if let Some(section) = &course.section {
        meta.push(format!("**Sección:** {section}"));
    }
    if let Some(id) = course.course.id {
        meta.push(format!("**ID Canvas:** {id}"));
    }
    if !meta.is_empty() {
        out.push('\n');
        out.push_str(&meta.join(" · "));
        out.push('\n');
    }

    if !course.modules.is_empty() {
        out.push_str("\n## Módulos\n\n");
        let mut modules: Vec<&ModuleIndex> = course.modules.iter().collect();
        modules.sort_by_key(|m| m.position);
        for m in modules {
            out.push_str(&format!(
                "{}. [{}]({}/{}/README.md)\n",
                m.position,
                md_text(&m.name),
                layout::MODULES_DIR,
                m.dir
            ));
        }
    }

    if !course.assignments.is_empty() {
        out.push_str("\n## Tareas\n\n");
        let mut assignments: Vec<&AssignmentIndex> = course.assignments.iter().collect();
        // No due date sorts last.
        assignments.sort_by(|a, b| {
            (a.due_at.is_none(), &a.due_at, &a.title).cmp(&(
                b.due_at.is_none(),
                &b.due_at,
                &b.title,
            ))
        });
        for a in assignments {
            out.push_str(&format!(
                "- [{}]({}/{})",
                md_text(&a.title),
                layout::ASSIGNMENTS_DIR,
                a.file
            ));
            if let Some(due) = a.due_at {
                out.push_str(&format!(" — vence {}", fmt_dt(&due)));
            }
            out.push('\n');
        }
    }

    if !course.announcements.is_empty() {
        out.push_str("\n## Anuncios recientes\n\n");
        let mut announcements: Vec<&AnnouncementIndex> = course.announcements.iter().collect();
        announcements.sort_by(|a, b| b.posted.cmp(&a.posted).then_with(|| a.title.cmp(&b.title)));
        for a in announcements.iter().take(RECENT_ANNOUNCEMENTS) {
            let date = a
                .posted
                .map(|d| d.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "sin fecha".to_string());
            out.push_str(&format!(
                "- {date} — [{}]({}/{})\n",
                md_text(&a.title),
                layout::ANNOUNCEMENTS_DIR,
                a.file
            ));
        }
        let rest = announcements.len().saturating_sub(RECENT_ANNOUNCEMENTS);
        if rest > 0 {
            out.push_str(&format!(
                "\n…y {rest} más en `{}/`.\n",
                layout::ANNOUNCEMENTS_DIR
            ));
        }
    }

    if !course.recordings.is_empty() {
        out.push_str("\n## Grabaciones\n\n");
        let mut recordings: Vec<&RecordingIndex> = course.recordings.iter().collect();
        recordings.sort_by(|a, b| a.date.cmp(&b.date).then_with(|| a.file.cmp(&b.file)));
        for r in recordings {
            let label = if r.topic.is_empty() {
                &r.file
            } else {
                &r.topic
            };
            match r.date {
                Some(date) => out.push_str(&format!(
                    "- {} — [{}]({}/{})\n",
                    date.format("%Y-%m-%d"),
                    md_text(label),
                    layout::RECORDINGS_DIR,
                    r.file
                )),
                None => out.push_str(&format!(
                    "- [{}]({}/{})\n",
                    md_text(label),
                    layout::RECORDINGS_DIR,
                    r.file
                )),
            }
        }
    }

    out
}

/// The `README.md` inside a module directory: links to its pages (relative,
/// same directory) and to its assignments (`../../tareas/`).
pub fn render_module_readme(module: &ModuleIndex) -> String {
    let mut out = format!("# {}\n", module.name);

    if !module.pages.is_empty() {
        out.push_str("\n## Páginas\n\n");
        let mut pages: Vec<&PageIndex> = module.pages.iter().collect();
        pages.sort_by_key(|p| p.position);
        for p in pages {
            out.push_str(&format!("- [{}]({})\n", md_text(&p.title), p.file));
        }
    }

    if !module.assignments.is_empty() {
        out.push_str("\n## Tareas\n\n");
        for a in &module.assignments {
            out.push_str(&format!(
                "- [{}](../../{}/{})\n",
                md_text(&a.title),
                layout::ASSIGNMENTS_DIR,
                a.file
            ));
        }
    }

    out
}

/// The `curso.json` of a course directory: the metadata an offline reader
/// (the migrator, [`layout::CourseNaming::course_dirs`]) needs to recognize
/// the course without a network. `name` is the readable name
/// ([`CourseIndex::display`]); `canvas_name` is the name as Canvas gives it
/// (or as a v1 directory spelled it), which is what identifies the course.
pub fn render_curso_json(course: &CourseIndex) -> String {
    #[derive(Serialize)]
    struct CursoJson<'a> {
        canvas_id: Option<u64>,
        course_code: Option<&'a str>,
        name: &'a str,
        canvas_name: &'a str,
        term: Option<&'a str>,
        section: Option<&'a str>,
        zoom_ids: &'a [u64],
    }
    let json = CursoJson {
        canvas_id: course.course.id,
        course_code: course.course.code.as_deref(),
        name: course.display(),
        canvas_name: &course.course.name,
        term: course.term.as_deref(),
        section: course.section.as_deref(),
        zoom_ids: &course.zoom_ids,
    };
    // Serializing a plain struct of strings and numbers cannot fail.
    let mut out = serde_json::to_string_pretty(&json).unwrap_or_default();
    out.push('\n');
    out
}

/// The library-root `INDEX.md`: the course list and the upcoming deadlines
/// (due today or later) across every course, soonest first.
pub fn render_index_md(library: &LibraryIndex, today: NaiveDate) -> String {
    let mut out = String::from("# Índice de la biblioteca\n");

    let mut courses: Vec<&CourseIndex> = library.courses.iter().collect();
    courses.sort_by(|a, b| a.dir.cmp(&b.dir));

    if !courses.is_empty() {
        out.push_str("\n## Ramos\n\n");
        for c in &courses {
            let title = c.display();
            let mut extra: Vec<&str> = Vec::new();
            if let Some(code) = &c.course.code {
                extra.push(code);
            }
            if let Some(term) = &c.term {
                extra.push(term);
            }
            out.push_str(&format!("- [{}]({}/README.md)", md_text(title), c.dir));
            if !extra.is_empty() {
                out.push_str(&format!(" — {}", extra.join(" · ")));
            }
            out.push('\n');
        }
    }

    let mut upcoming: Vec<(&CourseIndex, &AssignmentIndex)> = Vec::new();
    for c in &courses {
        for a in &c.assignments {
            if a.due_at.is_some_and(|due| due.date_naive() >= today) {
                upcoming.push((c, a));
            }
        }
    }
    if !upcoming.is_empty() {
        upcoming.sort_by(|(ca, aa), (cb, ab)| {
            aa.due_at
                .cmp(&ab.due_at)
                .then_with(|| ca.dir.cmp(&cb.dir))
                .then_with(|| aa.title.cmp(&ab.title))
        });
        out.push_str("\n## Próximas entregas\n\n");
        for (c, a) in upcoming {
            let due = a.due_at.map(|d| fmt_dt(&d)).unwrap_or_default();
            let title = c.display();
            out.push_str(&format!(
                "- {due} — {title}: [{}]({}/{}/{})\n",
                md_text(&a.title),
                c.dir,
                layout::ASSIGNMENTS_DIR,
                a.file
            ));
        }
    }

    out
}

/// The library-root `AGENTS.md`: static guidance for an AI agent on how to
/// navigate the layout v2 tree. Regenerated verbatim on every run.
pub fn render_agents_md() -> String {
    let v = layout::LAYOUT_VERSION;
    format!(
        r#"# AGENTS.md — biblioteca u_crawler

Esta biblioteca la genera `u_crawler` con el layout v{v}. Es de solo lectura
para agentes: no crees, edites ni borres archivos aquí.

## Cómo navegar

1. Empieza en `INDEX.md` (esta misma carpeta): lista los ramos y las próximas
   entregas.
2. Cada ramo es una carpeta kebab-case (`atdf105-mineria-de-datos/`). Dentro:
   - `README.md`: índice del ramo — módulos ordenados, tareas con su fecha de
     vencimiento, anuncios recientes y grabaciones.
   - `curso.json`: metadatos del ramo (`canvas_id`, `course_code`, `name`,
     `term`, `section`, `zoom_ids`). `canvas_id` puede ser `null` si el ramo
   - se migró sin conexión.
3. `{modulos}/NN-<nombre>/` contiene las páginas del módulo (`NN-<titulo>.md`)
   y un `README.md` que enlaza sus páginas y tareas. Los adjuntos del módulo
   están en `{modulos}/NN-<nombre>/{adjuntos}/`.
4. `{tareas}/` tiene un `.md` por tarea, con frontmatter (`vence`,
   `disponible`, `url`).
5. `{anuncios}/` tiene un `.md` por anuncio (`YYYY-MM-DD_<slug>.md`), sus
   adjuntos en `{anuncios}/{adjuntos}/` y el índice completo en
   `{anuncios}/index.json`.
6. `{grabaciones}/` tiene los mp4 de Zoom (`YYYY-MM-DD_<tema>[-parte-N].mp4`).
7. `.u_crawler/` es metadata interna de u_crawler (estado de sync, diario de
   migraciones, respaldos). No la leas como contenido del curso.

## Convenciones

- Todos los nombres son kebab-case ASCII en minúsculas; las rutas son
  estables entre corridas, así que un enlace local no se rompe al re-sincronizar.
- Las fechas en los textos están en UTC salvo que digan otra cosa.
- Los enlaces entre archivos son relativos: puedes seguirlos con un lector de
  Markdown común.
"#,
        v = v,
        modulos = layout::MODULES_DIR,
        adjuntos = layout::ATTACHMENTS_DIR,
        tareas = layout::ASSIGNMENTS_DIR,
        anuncios = layout::ANNOUNCEMENTS_DIR,
        grabaciones = layout::RECORDINGS_DIR,
    )
}

/// `2026-06-20 23:59 UTC`.
fn fmt_dt(dt: &DateTime<Utc>) -> String {
    dt.format("%Y-%m-%d %H:%M UTC").to_string()
}

/// Escape the two characters that would break a Markdown link text.
fn md_text(s: &str) -> String {
    s.replace('[', "\\[").replace(']', "\\]")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().to_utc()
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn sample_course() -> CourseIndex {
        let mut general = ModuleIndex::new(1, "Información General");
        general.pages = vec![
            PageIndex::new(2, "Presentación"),
            PageIndex::new(1, "Programa del curso"),
        ];
        let mut weeks = ModuleIndex::new(2, "Semanas");
        weeks.assignments = vec![AssignmentRef::new("Semana 10: Formativa 6")];

        let mut formative = AssignmentIndex::new(Some(9001), "Semana 10: Formativa 6");
        formative.due_at = Some(dt("2026-06-20T23:59:00Z"));
        let undated = AssignmentIndex::new(Some(9002), "Tarea sin fecha");

        CourseIndex {
            dir: "atdf105-mineria-de-datos".into(),
            course: CourseMeta {
                id: Some(210432),
                name: "202615_2690_MINERIA DE DATOS".into(),
                code: Some("ATDF105_202615_2690_EL_ON".into()),
            },
            title: "Minería de Datos".into(),
            term: Some("202615".into()),
            section: Some("2690".into()),
            zoom_ids: vec![210432],
            // Scrambled on purpose: rendering must sort.
            modules: vec![weeks, general],
            assignments: vec![undated, formative],
            announcements: vec![
                AnnouncementIndex::new(Some(501), "¡Bienvenidos al curso!", date(2026, 6, 17)),
                AnnouncementIndex::new(Some(502), "Cambio de sala", date(2026, 6, 18)),
            ],
            recordings: vec![RecordingIndex {
                file: "2026-06-18_sesion-sincronica-1.mp4".into(),
                date: Some(date(2026, 6, 18)),
                topic: "Sesión sincrónica 1".into(),
            }],
        }
    }

    #[test]
    fn course_readme_snapshot() {
        let got = render_course_readme(&sample_course());
        let want = "\
# Minería de Datos

**Código:** ATDF105_202615_2690_EL_ON · **Término:** 202615 · **Sección:** 2690 · **ID Canvas:** 210432

## Módulos

1. [Información General](modulos/01-informacion-general/README.md)
2. [Semanas](modulos/02-semanas/README.md)

## Tareas

- [Semana 10: Formativa 6](tareas/semana-10-formativa-6.md) — vence 2026-06-20 23:59 UTC
- [Tarea sin fecha](tareas/tarea-sin-fecha.md)

## Anuncios recientes

- 2026-06-18 — [Cambio de sala](anuncios/2026-06-18_cambio-de-sala.md)
- 2026-06-17 — [¡Bienvenidos al curso!](anuncios/2026-06-17_bienvenidos-al-curso.md)

## Grabaciones

- 2026-06-18 — [Sesión sincrónica 1](grabaciones/2026-06-18_sesion-sincronica-1.mp4)
";
        assert_eq!(got, want);
    }

    #[test]
    fn course_readme_omits_empty_sections_and_falls_back_to_dir() {
        let course = CourseIndex {
            dir: "curso-7".into(),
            ..Default::default()
        };
        assert_eq!(render_course_readme(&course), "# curso-7\n");
    }

    #[test]
    fn course_readme_escapes_brackets_in_link_text() {
        let mut course = CourseIndex {
            dir: "x".into(),
            ..Default::default()
        };
        course
            .assignments
            .push(AssignmentIndex::new(None, "Guía [borrador]"));
        let got = render_course_readme(&course);
        assert!(
            got.contains("[Guía \\[borrador\\]](tareas/guia-borrador.md)"),
            "{got}"
        );
    }

    #[test]
    fn course_readme_caps_recent_announcements() {
        let mut course = sample_course();
        course.announcements = (1..=12)
            .map(|i| AnnouncementIndex::new(Some(i), format!("Aviso {i}"), date(2026, 6, i as u32)))
            .collect();
        let got = render_course_readme(&course);
        assert_eq!(got.matches("](anuncios/").count(), 10, "{got}");
        assert!(got.contains("- 2026-06-12 — [Aviso 12]"), "{got}");
        assert!(got.contains("…y 2 más en `anuncios/`."), "{got}");
    }

    #[test]
    fn module_readme_snapshot() {
        let course = sample_course();
        let general = &course.modules[1];
        let got = render_module_readme(general);
        let want = "\
# Información General

## Páginas

- [Programa del curso](01-programa-del-curso.md)
- [Presentación](02-presentacion.md)
";
        assert_eq!(got, want);

        let weeks = &course.modules[0];
        let want = "\
# Semanas

## Tareas

- [Semana 10: Formativa 6](../../tareas/semana-10-formativa-6.md)
";
        assert_eq!(render_module_readme(weeks), want);
    }

    #[test]
    fn curso_json_snapshot() {
        let got = render_curso_json(&sample_course());
        let want = r#"{
  "canvas_id": 210432,
  "course_code": "ATDF105_202615_2690_EL_ON",
  "name": "Minería de Datos",
  "canvas_name": "202615_2690_MINERIA DE DATOS",
  "term": "202615",
  "section": "2690",
  "zoom_ids": [
    210432
  ]
}
"#;
        assert_eq!(got, want);
    }

    #[test]
    fn curso_json_after_offline_migration_has_null_canvas_id() {
        let course = CourseIndex {
            dir: "atdf106-electricidad".into(),
            course: CourseMeta {
                id: None,
                name: "ATDF106 ELECTRICIDAD".into(),
                code: None,
            },
            ..Default::default()
        };
        let got = render_curso_json(&course);
        let want = r#"{
  "canvas_id": null,
  "course_code": null,
  "name": "ATDF106 ELECTRICIDAD",
  "canvas_name": "ATDF106 ELECTRICIDAD",
  "term": null,
  "section": null,
  "zoom_ids": []
}
"#;
        assert_eq!(got, want);
    }

    #[test]
    fn index_md_snapshot() {
        let mut other = CourseIndex {
            dir: "cs101-intro".into(),
            course: CourseMeta {
                id: Some(9),
                name: "Intro to CS".into(),
                code: Some("CS101".into()),
            },
            ..Default::default()
        };
        let mut past = AssignmentIndex::new(Some(1), "Entrega pasada");
        past.due_at = Some(dt("2026-06-18T23:59:00Z"));
        let mut today = AssignmentIndex::new(Some(2), "Entrega de hoy");
        today.due_at = Some(dt("2026-06-19T00:00:00Z"));
        other.assignments = vec![past, AssignmentIndex::new(Some(3), "Sin fecha"), today];

        // Scrambled course order: rendering sorts by directory name.
        let library = LibraryIndex {
            courses: vec![other, sample_course()],
        };
        let got = render_index_md(&library, date(2026, 6, 19));
        let want = "\
# Índice de la biblioteca

## Ramos

- [Minería de Datos](atdf105-mineria-de-datos/README.md) — ATDF105_202615_2690_EL_ON · 202615
- [Intro to CS](cs101-intro/README.md) — CS101

## Próximas entregas

- 2026-06-19 00:00 UTC — Intro to CS: [Entrega de hoy](cs101-intro/tareas/entrega-de-hoy.md)
- 2026-06-20 23:59 UTC — Minería de Datos: [Semana 10: Formativa 6](atdf105-mineria-de-datos/tareas/semana-10-formativa-6.md)
";
        assert_eq!(got, want);
    }

    #[test]
    fn index_md_with_no_courses_and_no_upcoming() {
        let got = render_index_md(&LibraryIndex::default(), date(2026, 6, 19));
        assert_eq!(got, "# Índice de la biblioteca\n");

        let library = LibraryIndex {
            courses: vec![sample_course()],
        };
        let got = render_index_md(&library, date(2027, 1, 1));
        assert!(!got.contains("## Próximas entregas"), "{got}");
    }

    #[test]
    fn agents_md_is_static_and_names_the_layout() {
        let got = render_agents_md();
        assert_eq!(got, render_agents_md());
        assert!(
            got.contains(&format!("layout v{}", layout::LAYOUT_VERSION)),
            "{got}"
        );
        for needle in [
            "INDEX.md",
            "curso.json",
            "modulos/NN-<nombre>/",
            "tareas/",
            "anuncios/",
            "grabaciones/",
            ".u_crawler/",
        ] {
            assert!(got.contains(needle), "missing {needle}");
        }
        assert!(got.ends_with('\n'));
    }

    #[test]
    fn assignment_index_from_canvas_assignment() {
        let a = crate::canvas::Assignment {
            id: 42,
            name: Some("Control 2".into()),
            description: None,
            updated_at: None,
            due_at: Some(dt("2026-07-01T20:00:00Z")),
            unlock_at: Some(dt("2026-06-25T00:00:00Z")),
            lock_at: None,
            points_possible: Some(10.0),
            omit_from_final_grade: None,
            html_url: Some("https://canvas.example/courses/1/assignments/42".into()),
            assignment_group_id: None,
            submission_types: None,
            published: Some(true),
        };
        let idx = AssignmentIndex::from(&a);
        assert_eq!(idx.id, Some(42));
        assert_eq!(idx.title, "Control 2");
        assert_eq!(idx.file, "control-2.md");
        assert_eq!(idx.due_at, Some(dt("2026-07-01T20:00:00Z")));
        assert_eq!(
            idx.html_url.as_deref(),
            Some("https://canvas.example/courses/1/assignments/42")
        );
    }
}
