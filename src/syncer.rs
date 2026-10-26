//! The main sync: courses, modules, pages, assignments and their files,
//! written in layout v2 (`docs/specs/layout-v2.md`): `modulos/NN-<slug>/`
//! with its `adjuntos/`, one note per assignment in `tareas/`, the course's
//! `README.md` / `curso.json` and the library's `INDEX.md` / `AGENTS.md`.

use crate::canvas::{Assignment, CanvasClient, Module};
use crate::config::Config;
use crate::download::{download_if_needed, Claims, Dest};
use crate::frontmatter::{DocKind, Frontmatter};
use crate::fsutil::{atomic_write, ensure_dir};
use crate::http::{build_http_client, HttpCtx};
use crate::index::{AssignmentIndex, AssignmentRef, CourseIndex, ModuleIndex, PageIndex};
use crate::layout::{self, CourseNaming};
use crate::library;
use crate::links::{canvas_file_ids, FileTarget, LinkMap};
use crate::migrate::{relink, rewrite};
use crate::progress::{progress_bar, spinner};
use crate::state::{ItemState, State};
use crate::status;
use chrono::{DateTime, SecondsFormat, Utc};
use html2md::parse_html;
use regex::Regex;
use sha1::{Digest, Sha1};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use tracing::{info, warn};

pub async fn run_sync(
    filter_course_id: Option<u64>,
    dry_run: bool,
    verbose: bool,
) -> anyhow::Result<()> {
    let cfg = Config::load_or_init()?;
    let naming = CourseNaming::new(&cfg.layout)?;
    let root = PathBuf::from(&cfg.download_root);

    let http = build_http_client(&cfg);
    let httpctx = HttpCtx::new(&cfg, http);
    let canvas = CanvasClient::from_config().await?;
    crate::migrate::ensure::ensure_configured(&cfg, &naming, &canvas, dry_run).await?;

    let courses = canvas.list_courses().await?;
    // Named over the whole active list, before any filtering, so a
    // `--course-id` run picks the same directory a full run would.
    let lib = library::read(&root)?;
    let dirs = library::course_dirs(&naming, &lib, &courses);
    let ignored: std::collections::HashSet<String> =
        cfg.canvas.ignored_courses.iter().cloned().collect();
    // Where every Canvas resource already is, so a note can link to another
    // course's copies too; each course adds what it names and downloads.
    let mut links = relink::disk_map(&root)?;
    links
        .hosts
        .extend(crate::links::url_host(&cfg.canvas.base_url));

    let selected_courses: Vec<crate::canvas::Course> = if let Some(cid) = filter_course_id {
        if ignored.contains(&cid.to_string()) {
            tracing::info!(course_id = cid, "skipping ignored course");
            return Ok(());
        }
        let sel = courses
            .into_iter()
            .filter(move |c| c.id == cid)
            .collect::<Vec<_>>();
        if sel.is_empty() {
            tracing::warn!(
                course_id = cid,
                "course not found in active list; nothing to sync"
            );
            return Ok(());
        }
        sel
    } else {
        courses
            .into_iter()
            .filter(move |c| !ignored.contains(&c.id.to_string()))
            .collect()
    };

    let course_progress = progress_bar(selected_courses.len() as u64, "Syncing courses");

    let mut totals = SyncCounts::default();
    let mut synced: Vec<CourseIndex> = Vec::with_capacity(selected_courses.len());
    for c in selected_courses {
        course_progress.inc(1);
        course_progress.set_message(format!("Syncing course {}", c.id));
        let dir_name = dirs
            .get(&c.id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no directory was named for course {}", c.id))?;
        let course_dir = root.join(&dir_name);
        let mut course_index =
            library::course_index(&naming, &dir_name, &c, lib.records.get(&dir_name));
        if !dry_run {
            ensure_dir(&course_dir).await?;
            library::claim(&course_dir, &course_index).await?;
        }
        info!(course_id = c.id, path = %course_dir.display(), "sync course");

        // Load course state
        let state_path = library::state_path(&course_dir);
        let mut state = State::load(&state_path).await;

        let modules_spinner = spinner(&format!("Loading modules for {}", c.name));
        let modules = canvas.list_modules_with_items(c.id).await?;
        modules_spinner.finish_and_clear();
        // Preload assignments to avoid per-item fetch; map by id
        let assignments_spinner = spinner(&format!("Loading assignments for {}", c.name));
        let assignments_list = canvas.list_assignments(c.id).await.unwrap_or_default();
        assignments_spinner.finish_and_clear();
        let assignment_files = assignment_files(&assignments_list);
        let assignments: std::collections::HashMap<u64, Assignment> =
            assignments_list.into_iter().map(|a| (a.id, a)).collect();
        predict_links(&mut links, &dir_name, c.id, &modules, &assignment_files);
        let course_sync = CourseSync {
            canvas: &canvas,
            httpctx: &httpctx,
            course_dir: &course_dir,
            ramo: &dir_name,
            course_id: c.id,
            assignment_files: &assignment_files,
            dry_run,
            verbose,
        };
        let module_progress = progress_bar(modules.len() as u64, &format!("Modules in {}", c.name));
        for (i, m) in modules.iter().enumerate() {
            module_progress.inc(1);
            module_progress.set_message(format!("Course {} module {}", c.id, m.id));
            let position = m
                .position
                .unwrap_or_else(|| u32::try_from(i + 1).unwrap_or(u32::MAX));
            let synced_module = course_sync
                .sync_module(m, position, &assignments, &mut state, &mut links)
                .await?;
            let counts = synced_module.counts;
            totals += counts;
            course_index.modules.push(synced_module.index);
            course_index.assignments.extend(synced_module.assignments);
            if dry_run && (counts.pages > 0 || counts.files > 0) {
                module_progress.println(format!(
                    "DRY-RUN module {} -> pages: {}, files: {}",
                    m.id, counts.pages, counts.files
                ));
            }
        }
        module_progress.finish_and_clear();
        // An assignment linked from two modules is still one file.
        course_index.assignments.sort_by_key(|a| a.id);
        course_index.assignments.dedup_by_key(|a| a.id);

        // Sync announcements for this course
        if cfg.announcements.enabled {
            match crate::announcements::AnnouncementSync::new(
                &cfg,
                &canvas,
                &httpctx,
                c.id,
                &course_dir,
                dry_run,
                verbose,
            )
            .run(&c.name, &mut state, &mut links)
            .await
            {
                Ok(run) => {
                    let s = run.summary;
                    if dry_run && s.announcements > 0 {
                        status!(
                            "DRY-RUN announcements for course {}: {} (links: {}, media: {})",
                            c.id,
                            s.announcements,
                            s.links,
                            s.media
                        );
                    }
                    course_index.announcements = run.index;
                }
                Err(e) => {
                    warn!(course_id = c.id, error = %e, "announcements failed for course");
                }
            }
        }

        // Sync Zoom recordings for this course.
        //
        // The Zoom flow launches a browser, performs an interactive SSO and downloads
        // video, so it must not run under --dry-run, and it must honour zoom.enabled.
        // It is only compiled in when the `zoom` feature is enabled (see AGENTS.md).
        #[cfg(feature = "zoom")]
        {
            if !cfg.zoom.enabled {
                info!(course_id = c.id, "zoom disabled in config; skipping");
            } else if dry_run {
                status!("DRY-RUN: would sync Zoom recordings for course {}", c.id);
            } else {
                status!("Starting Zoom sync for course {}...", c.id);
                let target = crate::zoom::ZoomTarget {
                    course_id: c.id,
                    course_dir: &course_dir,
                    course_name: &c.name,
                };
                match crate::zoom::zoom_flow(&target, None, &mut state).await {
                    Ok(()) => {
                        status!("✓ Zoom sync completed for course {}", c.id);
                    }
                    Err(e) => {
                        warn!(course_id = c.id, error = %e, "zoom flow failed for course");
                        // Continue with other courses even if Zoom fails
                    }
                }
            }
        }
        #[cfg(not(feature = "zoom"))]
        {
            info!(
                course_id = c.id,
                "zoom feature not compiled into this build; skipping"
            );
        }

        if !dry_run {
            state.save(&state_path).await?;
            course_index.recordings = library::list_recordings(&course_dir).await?;
            library::write_course_files(&course_dir, &course_index).await?;
        }
        synced.push(course_index);
    }
    course_progress.finish_and_clear();
    if !dry_run {
        // A filtered run has not seen the other courses' deadlines, so it
        // leaves INDEX.md as the last full run wrote it.
        library::write_library_files(&root, &lib, &naming, synced, filter_course_id.is_none())
            .await?;
    }
    if dry_run {
        status!(
            "DRY-RUN summary: pages to write: {}, files to download: {}",
            totals.pages,
            totals.files
        );
    }
    Ok(())
}

/// The file name under `tareas/` of every assignment in a course, by id.
///
/// Assignments are named in id order through [`layout::unique_names`], so
/// the oldest of two with the same name keeps it, the newer one gets `-2`
/// (then `-3`…), and no two ever share a file — the same names the
/// migrator gives two v1 assignments with one title. A name depends only on
/// the lower ids, so a new assignment never renames an existing one.
pub fn assignment_files(assignments: &[Assignment]) -> HashMap<u64, String> {
    let mut sorted: Vec<&Assignment> = assignments.iter().collect();
    sorted.sort_by_key(|a| a.id);
    let wanted: Vec<String> = sorted
        .iter()
        .map(|a| layout::assignment_file(a.name.as_deref().unwrap_or("")))
        .collect();
    let names = layout::unique_names(&wanted, &|_| false);
    sorted.into_iter().map(|a| a.id).zip(names).collect()
}

/// Where this sync will write each page and assignment the modules list,
/// put in `links` before any note is written, so a page can link to one
/// written after it. A page is named from its module item's title here and
/// from the page's own title when written (which replaces the entry); the
/// two agree unless the page was renamed after it was added to the module.
fn predict_links(
    links: &mut LinkMap,
    ramo: &str,
    course_id: u64,
    modules: &[Module],
    assignment_files: &HashMap<u64, String>,
) {
    links.courses.insert(course_id, PathBuf::from(ramo));
    for (i, m) in modules.iter().enumerate() {
        let position = m
            .position
            .unwrap_or_else(|| u32::try_from(i + 1).unwrap_or(u32::MAX));
        let module_rel = Path::new(ramo)
            .join(layout::MODULES_DIR)
            .join(layout::module_dir(position, &m.name));
        for (idx, item) in m.items.iter().enumerate() {
            let item_position = u32::try_from(idx + 1).unwrap_or(u32::MAX);
            if let Some(slug) = item_page_slug(item, course_id) {
                let title = item.title.clone().unwrap_or_else(|| slug.clone());
                let path = module_rel.join(layout::page_file(item_position, &title));
                links
                    .pages
                    .insert((course_id, slug.to_ascii_lowercase()), path.clone());
                links.module_items.insert(item.id, path);
                continue;
            }
            if item.kind.as_deref() != Some("Assignment") {
                continue;
            }
            let Some((aid, file)) = item
                .content_id
                .and_then(|aid| Some((aid, assignment_files.get(&aid)?)))
            else {
                continue;
            };
            let path = Path::new(ramo).join(layout::ASSIGNMENTS_DIR).join(file);
            links.assignments.insert(aid, path.clone());
            links.module_items.insert(item.id, path);
        }
    }
}

/// The page a module item shows: its `page_url`, or the slug of an
/// `html_url` that is a page of this course.
fn item_page_slug(item: &crate::canvas::ModuleItem, course_id: u64) -> Option<String> {
    item.page_url.clone().or_else(|| {
        item.html_url
            .as_deref()
            .filter(|u| is_course_page_url(u, course_id))
            .and_then(extract_page_slug)
    })
}

/// `rel` with `/` separators, as `state.json` records paths.
fn slash(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Record in `state` that `key` is at `rel` (relative to the course
/// directory), creating the entry when there is none.
fn record_path(state: &mut State, key: String, rel: &Path) {
    let path = Some(slash(rel));
    match state.items.get_mut(&key) {
        Some(item) => item.path = path,
        None => state.set(
            key,
            ItemState {
                path,
                ..ItemState::default()
            },
        ),
    }
}

/// A Canvas timestamp as the frontmatter writes it: RFC 3339, UTC, seconds.
fn rfc3339(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Number of items a module sync wrote, or would write under `--dry-run`.
#[derive(Debug, Default, Clone, Copy)]
struct SyncCounts {
    pages: usize,
    files: usize,
}

impl std::ops::AddAssign for SyncCounts {
    fn add_assign(&mut self, rhs: Self) {
        self.pages += rhs.pages;
        self.files += rhs.files;
    }
}

/// The ambient state of one module sync.
///
/// Exists so the per-item helpers below take two arguments instead of ten, and
/// so `processed` is owned in one place rather than threaded through each arm.
struct ModuleCtx<'a> {
    canvas: &'a CanvasClient,
    httpctx: &'a HttpCtx,
    course_id: u64,
    module_id: u64,
    course_dir: &'a Path,
    /// The course directory name: where `links` paths start.
    ramo: &'a str,
    /// `modulos/NN-<slug>`, relative to the course directory.
    module_rel: PathBuf,
    dry_run: bool,
    verbose: bool,
    /// Canvas file ids already handled in this module. A file may be linked from
    /// a page, an assignment body and a module item all at once.
    processed: HashSet<u64>,
    /// Where every Canvas resource is, relative to the library root: the
    /// map the markdown link rewrite points at. Each file downloaded here
    /// and each note written here is added.
    links: &'a mut LinkMap,
    /// Paths handed out under this module's `adjuntos/` ([`Claims`]).
    claims: Claims,
}

impl ModuleCtx<'_> {
    /// Fetches and downloads one Canvas file, returning how many downloads this
    /// planned (0 or 1).
    ///
    /// Failures are recorded against the item's state rather than propagated: one
    /// unavailable file must not abandon the rest of the module.
    async fn sync_file(&mut self, file_id: u64, state: &mut State) -> anyhow::Result<usize> {
        if !self.processed.insert(file_id) {
            return Ok(0);
        }
        let (course_id, module_id) = (self.course_id, self.module_id);
        let key = crate::download::state_key(file_id);

        let f = match self.canvas.get_file(file_id).await {
            Ok(f) => f,
            Err(e) => {
                warn!(course_id, module_id, file_id, error = %e, "unable to fetch file metadata");
                state.record_error(key, &e);
                return Ok(0);
            }
        };

        let name = f
            .display_name
            .clone()
            .or_else(|| f.filename.clone())
            .unwrap_or_else(|| format!("file_{file_id}"));
        let rel = self
            .module_rel
            .join(layout::ATTACHMENTS_DIR)
            .join(layout::attachment_file(&name));
        let dest = self.course_dir.join(&rel);
        let ext = dest
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or_default();

        if self.dry_run {
            if state.get(&key).is_some() {
                info!(course_id, module_id, file_id, path = %dest.display(), "dry-run skip file; already synced");
                return Ok(0);
            }
            info!(course_id, module_id, file_id, path = %dest.display(), file_ext = ext, "dry-run file planned");
            return Ok(1);
        }

        if let Some(parent) = dest.parent() {
            ensure_dir(parent).await?;
        }
        match download_if_needed(
            self.httpctx,
            &f,
            Dest::Exact(&dest),
            state,
            &mut self.claims,
        )
        .await
        {
            Ok(path) => {
                info!(course_id, module_id, file_id, path = %path.display(), file_ext = ext, "downloaded file");
                let rel = path
                    .strip_prefix(self.course_dir)
                    .map(Path::to_path_buf)
                    .unwrap_or(rel);
                record_path(state, key, &rel);
                self.links
                    .files
                    .insert(file_id, FileTarget::Path(Path::new(self.ramo).join(&rel)));
                Ok(1)
            }
            Err(e) => {
                warn!(course_id, module_id, file_id, error = %e, "download failed");
                state.record_error(key, &e);
                Ok(0)
            }
        }
    }

    /// Downloads every Canvas file `html` links to, so the note written from
    /// it can point at the local copies.
    async fn sync_linked_files(&mut self, html: &str, state: &mut State) -> anyhow::Result<usize> {
        let mut planned = 0;
        for fid in discover_file_ids(html) {
            planned += self.sync_file(fid, state).await?;
        }
        Ok(planned)
    }

    /// Writes a markdown note at `rel` (relative to the course directory),
    /// skipping when its content hash is unchanged. Returns how many writes
    /// this planned (0 or 1).
    async fn write_markdown(
        &self,
        key: String,
        rel: &Path,
        md: &str,
        updated_at: Option<String>,
        state: &mut State,
        what: &str,
    ) -> anyhow::Result<usize> {
        let (course_id, module_id) = (self.course_id, self.module_id);
        let dest = self.course_dir.join(rel);
        let hash = sha1_hex(md.as_bytes());

        if state.get(&key).and_then(|s| s.content_hash.as_deref()) == Some(hash.as_str()) {
            record_path(state, key, rel);
            if !self.dry_run && self.verbose {
                info!(course_id, module_id, path = %dest.display(), what, "unchanged; skipping");
            }
            return Ok(0);
        }
        if self.dry_run {
            info!(course_id, module_id, path = %dest.display(), bytes = md.len(), what, "dry-run write planned");
            return Ok(1);
        }

        atomic_write(&dest, md.as_bytes()).await?;
        state.set(
            key,
            ItemState {
                etag: None,
                updated_at,
                size: Some(md.len() as u64),
                content_hash: Some(hash),
                last_error: None,
                error_count: None,
                file_name: None,
                path: Some(slash(rel)),
            },
        );
        info!(course_id, module_id, path = %dest.display(), what, "wrote markdown");
        Ok(1)
    }
}

/// One course's sync settings, shared by every module in it.
struct CourseSync<'a> {
    canvas: &'a CanvasClient,
    httpctx: &'a HttpCtx,
    course_dir: &'a Path,
    /// The course directory name, written as each note's `ramo`.
    ramo: &'a str,
    course_id: u64,
    /// Every assignment's file under `tareas/` ([`assignment_files`]).
    assignment_files: &'a HashMap<u64, String>,
    dry_run: bool,
    verbose: bool,
}

/// What one module sync produced: its counts and what the indexes list.
struct SyncedModule {
    counts: SyncCounts,
    index: ModuleIndex,
    assignments: Vec<AssignmentIndex>,
}

impl CourseSync<'_> {
    fn frontmatter(&self, tipo: DocKind) -> Frontmatter {
        Frontmatter {
            ramo: self.ramo.to_string(),
            tipo,
            canvas_id: None,
            url: None,
            publicado: None,
            vence: None,
            disponible: None,
        }
    }

    /// Mirrors one module: its pages, its assignments, and every file they link to.
    async fn sync_module(
        &self,
        m: &Module,
        position: u32,
        assignments: &HashMap<u64, Assignment>,
        state: &mut State,
        links: &mut LinkMap,
    ) -> anyhow::Result<SyncedModule> {
        let course_id = self.course_id;
        let canvas = self.canvas;
        let dry_run = self.dry_run;

        let mut index = ModuleIndex::new(position, m.name.clone());
        let module_rel = Path::new(layout::MODULES_DIR).join(&index.dir);
        if !dry_run {
            ensure_dir(&self.course_dir.join(&module_rel)).await?;
        }
        info!(course_id, module_id = m.id, "sync module");

        let mut ctx = ModuleCtx {
            canvas,
            httpctx: self.httpctx,
            course_id,
            module_id: m.id,
            course_dir: self.course_dir,
            ramo: self.ramo,
            module_rel,
            dry_run,
            verbose: self.verbose,
            processed: HashSet::new(),
            links,
            claims: Claims::default(),
        };
        let mut counts = SyncCounts::default();
        let mut module_assignments = Vec::new();

        for (idx, item) in m.items.iter().enumerate() {
            let item_position = u32::try_from(idx + 1).unwrap_or(u32::MAX);
            // A page is reachable either by its own page_url or via an html_url that
            // points at one. Both used to be separate match arms rendering identical
            // markdown; resolving the slug up front collapses them.
            let page_slug = item_page_slug(item, course_id);

            if let Some(slug) = page_slug {
                let page = canvas.get_page(course_id, &slug).await?;
                let title = page
                    .title
                    .clone()
                    .unwrap_or_else(|| item.title.clone().unwrap_or_else(|| slug.clone()));
                let html = page.body.clone().unwrap_or_default();
                counts.files += ctx.sync_linked_files(&html, state).await?;

                let page_index = PageIndex::new(item_position, title);
                let rel = ctx.module_rel.join(&page_index.file);
                let fm = Frontmatter {
                    canvas_id: page.page_id,
                    url: page.html_url.clone(),
                    ..self.frontmatter(DocKind::Page)
                };
                let note = Path::new(self.ramo).join(&rel);
                ctx.links
                    .pages
                    .insert((course_id, slug.to_ascii_lowercase()), note.clone());
                ctx.links.module_items.insert(item.id, note);
                record_path(state, relink::module_item_key(item.id), &rel);
                let md = render_note(&html, &fm, &Path::new(self.ramo).join(&rel), ctx.links);
                counts.pages += ctx
                    .write_markdown(
                        format!("page:{slug}"),
                        &rel,
                        &md,
                        page.updated_at,
                        state,
                        "page",
                    )
                    .await?;
                index.pages.push(page_index);
                continue;
            }

            match item.kind.as_deref() {
                Some("File") => {
                    if let Some(fid) = item.content_id {
                        counts.files += ctx.sync_file(fid, state).await?;
                        if let Some(FileTarget::Path(path)) = ctx.links.files.get(&fid).cloned() {
                            if let Ok(rel) = path.strip_prefix(self.ramo) {
                                record_path(state, relink::module_item_key(item.id), rel);
                            }
                            ctx.links.module_items.insert(item.id, path);
                        }
                    }
                }
                Some("Assignment") => {
                    let Some(assign) = item.content_id.and_then(|aid| assignments.get(&aid)) else {
                        continue;
                    };
                    let aid = assign.id;
                    let atitle = assign.name.clone().unwrap_or_else(|| {
                        item.title
                            .clone()
                            .unwrap_or_else(|| format!("assignment_{aid}"))
                    });
                    let html = assign.description.clone().unwrap_or_default();
                    counts.files += ctx.sync_linked_files(&html, state).await?;

                    let file = self
                        .assignment_files
                        .get(&aid)
                        .cloned()
                        .unwrap_or_else(|| layout::assignment_file(&atitle));
                    let rel = Path::new(layout::ASSIGNMENTS_DIR).join(&file);
                    let fm = Frontmatter {
                        canvas_id: Some(aid),
                        url: assign.html_url.clone(),
                        vence: assign.due_at.map(rfc3339),
                        disponible: assign.unlock_at.map(rfc3339),
                        ..self.frontmatter(DocKind::Assignment)
                    };
                    let note = Path::new(self.ramo).join(&rel);
                    ctx.links.assignments.insert(aid, note.clone());
                    ctx.links.module_items.insert(item.id, note);
                    record_path(state, relink::module_item_key(item.id), &rel);
                    let md = render_note(&html, &fm, &Path::new(self.ramo).join(&rel), ctx.links);
                    counts.pages += ctx
                        .write_markdown(
                            format!("assignment:{aid}"),
                            &rel,
                            &md,
                            assign.updated_at.clone(),
                            state,
                            "assignment",
                        )
                        .await?;

                    index.assignments.push(AssignmentRef {
                        title: atitle.clone(),
                        file: file.clone(),
                    });
                    let mut entry = AssignmentIndex::from(assign);
                    entry.title = atitle;
                    entry.file = file;
                    module_assignments.push(entry);
                }
                _ => {}
            }
        }
        Ok(SyncedModule {
            counts,
            index,
            assignments: module_assignments,
        })
    }
}

/// The note for `html` at `rel` (relative to the library root): the
/// markdown body with its Canvas links pointing at the local copies in
/// `links`, under the frontmatter `fm` — exactly what `migrate::rewrite`
/// produces, so a sync never undoes what a migration wrote.
fn render_note(html: &str, fm: &Frontmatter, rel: &Path, links: &LinkMap) -> String {
    rewrite::apply(&parse_html(html), fm, rel, links)
}

fn sha1_hex(data: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// Matches `/courses/12345/pages/some-slug`, capturing course id and slug.
static COURSE_PAGE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/courses/(\d+)/pages/([A-Za-z0-9_\-]+)").unwrap());

/// Canvas file ids referenced anywhere in a body of HTML, in id order.
fn discover_file_ids(html: &str) -> BTreeSet<u64> {
    canvas_file_ids(html).collect()
}

/// Whether `url` points at a wiki page belonging to `course_id`.
fn is_course_page_url(url: &str, course_id: u64) -> bool {
    COURSE_PAGE_RE
        .captures(url)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse::<u64>().ok())
        .is_some_and(|id| id == course_id)
}

/// The page slug in a Canvas wiki-page URL.
fn extract_page_slug(url: &str) -> Option<String> {
    COURSE_PAGE_RE
        .captures(url)
        .and_then(|c| c.get(2))
        .map(|m| m.as_str().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assignment(id: u64, name: &str) -> Assignment {
        serde_json::from_value(serde_json::json!({ "id": id, "name": name })).unwrap()
    }

    #[test]
    fn assignments_are_named_by_title_and_the_newer_twin_is_numbered() {
        let got = assignment_files(&[
            assignment(30, "Semana 10: Formativa 6"),
            assignment(12, "Tarea"),
            assignment(7, "Tarea"),
        ]);
        assert_eq!(got[&30], "semana-10-formativa-6.md");
        assert_eq!(got[&7], "tarea.md");
        assert_eq!(got[&12], "tarea-2.md");
    }

    #[test]
    fn a_new_assignment_never_renames_an_existing_twin() {
        let twins = [assignment(7, "Tarea"), assignment(10, "Tarea")];
        let before = assignment_files(&twins);
        assert_eq!(before[&10], "tarea-2.md");
        // Canvas adds "Tarea 2" later: the twin keeps its file.
        let mut more = twins.to_vec();
        more.push(assignment(20, "Tarea 2"));
        let after = assignment_files(&more);
        assert_eq!(after[&7], before[&7]);
        assert_eq!(after[&10], before[&10]);
        assert_eq!(after[&20], "tarea-2-2.md");
        let names: HashSet<&String> = after.values().collect();
        assert_eq!(names.len(), 3, "{after:?}");
    }

    #[test]
    fn two_long_identical_titles_still_get_two_files() {
        let title = "Informe ".repeat(20);
        let got = assignment_files(&[assignment(100, &title), assignment(200, &title)]);
        assert_ne!(got[&100], got[&200]);
        assert!(got[&200].ends_with("-2.md"), "{got:?}");
    }

    #[test]
    fn a_note_gets_frontmatter_and_local_file_links() {
        let fm = Frontmatter {
            ramo: "atdf105-mineria-de-datos".into(),
            tipo: DocKind::Assignment,
            canvas_id: Some(30),
            url: None,
            publicado: None,
            vence: Some(rfc3339(
                DateTime::parse_from_rfc3339("2026-06-20T03:59:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            )),
            disponible: None,
        };
        let mut links = LinkMap::default();
        links.files.insert(
            22,
            FileTarget::Path(PathBuf::from(
                "ramo/modulos/01-informacion-general/adjuntos/guia-1.pdf",
            )),
        );
        let got = render_note(
            r#"<p><a href="https://x.instructure.com/courses/1/files/22?verifier=a">Guía</a></p>"#,
            &fm,
            Path::new("ramo/tareas/semana-10.md"),
            &links,
        );
        assert!(
            got.starts_with("---\nramo: \"atdf105-mineria-de-datos\"\ntipo: tarea\ncanvas_id: 30\nvence: \"2026-06-20T03:59:00Z\"\n---\n"),
            "{got}"
        );
        assert!(
            got.contains("](../modulos/01-informacion-general/adjuntos/guia-1.pdf)"),
            "{got}"
        );
    }

    #[test]
    fn predicted_links_reach_pages_and_assignments_written_later() {
        let modules: Vec<Module> = serde_json::from_value(serde_json::json!([
            { "id": 1, "name": "Semana 1", "position": 2, "items": [
                { "id": 70, "title": "Fase 1", "type": "Page", "page_url": "fase-1" },
                { "id": 71, "title": "Entrega", "type": "Assignment", "content_id": 30 },
                { "id": 72, "title": "Guía", "type": "File", "content_id": 5 }
            ]}
        ]))
        .unwrap();
        let files = HashMap::from([(30, "entrega.md".to_string())]);
        let mut links = LinkMap::default();
        predict_links(&mut links, "ramo", 9, &modules, &files);
        let page = PathBuf::from("ramo/modulos/02-semana-1/01-fase-1.md");
        assert_eq!(links.pages[&(9, "fase-1".to_string())], page);
        assert_eq!(links.module_items[&70], page);
        assert_eq!(
            links.assignments[&30],
            PathBuf::from("ramo/tareas/entrega.md")
        );
        assert_eq!(
            links.module_items[&71],
            PathBuf::from("ramo/tareas/entrega.md")
        );
        assert!(!links.module_items.contains_key(&72));
        assert_eq!(links.courses[&9], PathBuf::from("ramo"));
    }

    #[test]
    fn discover_file_ids_matches_both_api_and_plain_paths() {
        let html = r#"<a href="/courses/1/files/22">a</a><img src="/api/v1/files/33"/>"#;
        let ids = discover_file_ids(html);
        assert!(ids.contains(&22));
        assert!(ids.contains(&33));
    }

    #[test]
    fn is_course_page_url_rejects_another_course() {
        let url = "https://canvas.example.com/courses/999/pages/intro";
        assert!(is_course_page_url(url, 999));
        assert!(!is_course_page_url(url, 1000));
    }

    #[test]
    fn extract_page_slug_returns_the_slug_not_the_course_id() {
        assert_eq!(
            extract_page_slug("https://canvas.example.com/courses/999/pages/week-01"),
            Some("week-01".to_string())
        );
    }

    #[test]
    fn extract_page_slug_is_none_for_a_non_page_url() {
        assert_eq!(
            extract_page_slug("https://canvas.example.com/courses/999/files/3"),
            None
        );
    }
}
