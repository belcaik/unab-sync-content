use crate::canvas::{Assignment, CanvasClient, Module};
use crate::config::Config;
use crate::download::{download_if_needed, Dest};
use crate::fsutil::{atomic_write, ensure_dir, sanitize_component, sanitize_filename_preserve_ext};
use crate::http::{build_http_client, HttpCtx};
use crate::progress::{progress_bar, spinner};
use crate::state::{ItemState, State};
use html2md::parse_html;
use regex::Regex;
use sha1::{Digest, Sha1};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use tracing::{info, warn};

pub async fn run_sync(
    filter_course_id: Option<u64>,
    dry_run: bool,
    verbose: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::load_or_init()?;

    let http = build_http_client(&cfg);
    let httpctx = HttpCtx::new(&cfg, http);
    let canvas = CanvasClient::from_config().await?;

    let courses = canvas.list_courses().await?;
    let ignored: std::collections::HashSet<String> =
        cfg.canvas.ignored_courses.iter().cloned().collect();

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
    for c in selected_courses {
        course_progress.inc(1);
        course_progress.set_message(format!("Syncing course {}", c.id));
        let code = c.course_code.clone().unwrap_or_default();
        let course_dir = PathBuf::from(&cfg.download_root).join(if code.is_empty() {
            sanitize_component(&c.name)
        } else {
            format!(
                "{}_{}",
                sanitize_component(&c.name),
                sanitize_component(code)
            )
        });
        if !dry_run {
            ensure_dir(&course_dir).await?;
        }
        info!(course_id = c.id, path = %course_dir.display(), "sync course");

        // Load course state
        let state_path = course_dir.join("state.json");
        let mut state = State::load(&state_path).await;

        let modules_spinner = spinner(&format!("Loading modules for {}", c.name));
        let modules = canvas.list_modules_with_items(c.id).await?;
        modules_spinner.finish_and_clear();
        // Preload assignments to avoid per-item fetch; map by id
        let assignments_spinner = spinner(&format!("Loading assignments for {}", c.name));
        let assignments_list = canvas.list_assignments(c.id).await.unwrap_or_default();
        assignments_spinner.finish_and_clear();
        let assignments: std::collections::HashMap<u64, Assignment> =
            assignments_list.into_iter().map(|a| (a.id, a)).collect();
        let course_sync = CourseSync {
            canvas: &canvas,
            httpctx: &httpctx,
            course_dir: &course_dir,
            course_id: c.id,
            dry_run,
            verbose,
        };
        let module_progress = progress_bar(modules.len() as u64, &format!("Modules in {}", c.name));
        for m in modules {
            module_progress.inc(1);
            module_progress.set_message(format!("Course {} module {}", c.id, m.id));
            let counts = course_sync
                .sync_module(&m, &assignments, &mut state)
                .await?;
            totals += counts;
            if dry_run && (counts.pages > 0 || counts.files > 0) {
                module_progress.println(format!(
                    "DRY-RUN module {} -> pages: {}, files: {}",
                    m.id, counts.pages, counts.files
                ));
            }
        }
        module_progress.finish_and_clear();

        // Sync announcements for this course
        if cfg.announcements.enabled {
            match crate::announcements::run_for_course(
                &cfg,
                &canvas,
                &httpctx,
                &c,
                &course_dir,
                &mut state,
                dry_run,
                verbose,
            )
            .await
            {
                Ok(s) => {
                    if dry_run && s.announcements > 0 {
                        println!(
                            "DRY-RUN announcements for course {}: {} (links: {}, media: {})",
                            c.id, s.announcements, s.links, s.media
                        );
                    }
                }
                Err(e) => {
                    warn!(course_id = c.id, error = %e, "announcements failed for course");
                    eprintln!("Warning: announcements failed for course {}: {}", c.id, e);
                }
            }
        }

        // Sync Zoom recordings for this course.
        //
        // The Zoom flow launches a browser, performs an interactive SSO and downloads
        // video, so it must not run under --dry-run, and it must honour zoom.enabled.
        if !cfg.zoom.enabled {
            info!(course_id = c.id, "zoom disabled in config; skipping");
        } else if dry_run {
            println!("DRY-RUN: would sync Zoom recordings for course {}", c.id);
        } else {
            println!("Starting Zoom sync for course {}...", c.id);
            match crate::zoom::zoom_flow(c.id, None).await {
                Ok(()) => {
                    println!("✓ Zoom sync completed for course {}", c.id);
                }
                Err(e) => {
                    warn!(course_id = c.id, error = %e, "zoom flow failed for course");
                    // Continue with other courses even if Zoom fails
                }
            }
        }

        if !dry_run {
            state.save(&state_path).await?;
        }
    }
    course_progress.finish_and_clear();
    if dry_run {
        println!(
            "DRY-RUN summary: pages to write: {}, files to download: {}",
            totals.pages, totals.files
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
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
    module_dir: PathBuf,
    dry_run: bool,
    verbose: bool,
    /// Canvas file ids already handled in this module. A file may be linked from
    /// a page, an assignment body and a module item all at once.
    processed: HashSet<u64>,
}

impl ModuleCtx<'_> {
    /// Fetches and downloads one Canvas file, returning how many downloads this
    /// planned (0 or 1).
    ///
    /// Failures are recorded against the item's state rather than propagated: one
    /// unavailable file must not abandon the rest of the module.
    async fn sync_file(
        &mut self,
        file_id: u64,
        state: &mut State,
    ) -> Result<usize, Box<dyn std::error::Error>> {
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
        let dest = self
            .module_dir
            .join("Attachments")
            .join(sanitize_filename_preserve_ext(&name));
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
        match download_if_needed(self.httpctx, &f, Dest::Exact(&dest), state).await {
            Ok(_) => {
                info!(course_id, module_id, file_id, path = %dest.display(), file_ext = ext, "downloaded file");
                Ok(1)
            }
            Err(e) => {
                warn!(course_id, module_id, file_id, error = %e, "download failed");
                state.record_error(key, &e);
                Ok(0)
            }
        }
    }

    /// Writes a markdown rendering of a Canvas item, skipping when its content
    /// hash is unchanged. Returns how many writes this planned (0 or 1).
    async fn write_markdown(
        &self,
        key: String,
        dest: &Path,
        md: &str,
        updated_at: Option<String>,
        state: &mut State,
        what: &str,
    ) -> Result<usize, Box<dyn std::error::Error>> {
        let (course_id, module_id) = (self.course_id, self.module_id);
        let hash = sha1_hex(md.as_bytes());

        if state.get(&key).and_then(|s| s.content_hash.as_deref()) == Some(hash.as_str()) {
            if !self.dry_run && self.verbose {
                info!(course_id, module_id, path = %dest.display(), what, "unchanged; skipping");
            }
            return Ok(0);
        }
        if self.dry_run {
            info!(course_id, module_id, path = %dest.display(), bytes = md.len(), what, "dry-run write planned");
            return Ok(1);
        }

        atomic_write(dest, md.as_bytes()).await?;
        state.set(
            key,
            ItemState {
                etag: None,
                updated_at,
                size: Some(md.len() as u64),
                content_hash: Some(hash),
                last_error: None,
                error_count: None,
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
    course_id: u64,
    dry_run: bool,
    verbose: bool,
}

impl CourseSync<'_> {
    /// Mirrors one module: its pages, its assignments, and every file they link to.
    async fn sync_module(
        &self,
        m: &Module,
        assignments: &std::collections::HashMap<u64, Assignment>,
        state: &mut State,
    ) -> Result<SyncCounts, Box<dyn std::error::Error>> {
        let course_id = self.course_id;
        let canvas = self.canvas;
        let dry_run = self.dry_run;

        let module_dir = self.course_dir.join("Modules").join(format!(
            "{}_{}",
            m.id,
            sanitize_component(&m.name)
        ));
        if !dry_run {
            ensure_dir(&module_dir).await?;
        }
        info!(course_id, module_id = m.id, "sync module");

        let mut ctx = ModuleCtx {
            canvas,
            httpctx: self.httpctx,
            course_id,
            module_id: m.id,
            module_dir,
            dry_run,
            verbose: self.verbose,
            processed: HashSet::new(),
        };
        let mut counts = SyncCounts::default();

        for (idx, item) in m.items.iter().enumerate() {
            // A page is reachable either by its own page_url or via an html_url that
            // points at one. Both used to be separate match arms rendering identical
            // markdown; resolving the slug up front collapses them.
            let page_slug = item.page_url.clone().or_else(|| {
                item.html_url
                    .as_deref()
                    .filter(|u| is_course_page_url(u, course_id))
                    .and_then(extract_page_slug)
            });

            if let Some(slug) = page_slug {
                let page = canvas.get_page(course_id, &slug).await?;
                let title = page
                    .title
                    .clone()
                    .unwrap_or_else(|| item.title.clone().unwrap_or_else(|| slug.clone()));
                let html = page.body.unwrap_or_default();
                let md = parse_html(&html);
                let dest = ctx.module_dir.join(format!(
                    "{:02}-{}.md",
                    idx + 1,
                    sanitize_component(&title)
                ));

                counts.pages += ctx
                    .write_markdown(
                        format!("page:{slug}"),
                        &dest,
                        &md,
                        page.updated_at,
                        state,
                        "page",
                    )
                    .await?;

                for fid in discover_file_ids(&html) {
                    counts.files += ctx.sync_file(fid, state).await?;
                }
                continue;
            }

            match item.kind.as_deref() {
                Some("File") => {
                    if let Some(fid) = item.content_id {
                        counts.files += ctx.sync_file(fid, state).await?;
                    }
                }
                Some("Assignment") => {
                    if let Some(aid) = item.content_id {
                        if let Some(assign) = assignments.get(&aid) {
                            let atitle = assign.name.clone().unwrap_or_else(|| {
                                item.title
                                    .clone()
                                    .unwrap_or_else(|| format!("assignment_{}", aid))
                            });
                            let html = assign.description.clone().unwrap_or_default();
                            let md = parse_html(&html);
                            let dest = ctx.module_dir.join(format!(
                                "{:02}-ASSIGN-{}.md",
                                idx + 1,
                                sanitize_component(&atitle)
                            ));
                            counts.pages += ctx
                                .write_markdown(
                                    format!("assignment:{aid}"),
                                    &dest,
                                    &md,
                                    assign.updated_at.clone(),
                                    state,
                                    "assignment",
                                )
                                .await?;

                            let file_ids = discover_file_ids(&html);
                            for fid in file_ids {
                                counts.files += ctx.sync_file(fid, state).await?;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(counts)
    }
}

fn sha1_hex(data: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// Matches `/files/12345` or `/api/v1/files/12345` in an absolute or relative URL.
static FILE_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:/api/v1)?/files/(\d+)").unwrap());

/// Matches `/courses/12345/pages/some-slug`, capturing course id and slug.
static COURSE_PAGE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/courses/(\d+)/pages/([A-Za-z0-9_\-]+)").unwrap());

/// Canvas file ids referenced anywhere in a body of HTML.
fn discover_file_ids(html: &str) -> HashSet<u64> {
    FILE_ID_RE
        .captures_iter(html)
        .filter_map(|c| c.get(1)?.as_str().parse::<u64>().ok())
        .collect()
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
