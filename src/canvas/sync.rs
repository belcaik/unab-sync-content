use crate::canvas::download::{download_if_needed, Dest};
use crate::canvas::{Assignment, CanvasClient, Course, Module};
use crate::core::fsutil::{
    atomic_write, ensure_dir, sanitize_component, sanitize_filename_preserve_ext,
};
use crate::core::http::HttpCtx;
use crate::core::state::{content_hash, ItemState, State};
use crate::ui::progress::{progress_bar, spinner};
use html2md::parse_html;
use regex::Regex;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use tracing::{info, warn};

/// Mirrors one course's modules: pages, assignments, and every file they link
/// to. The per-course orchestration around it (course directory, `state.json`,
/// and the other flows) lives in [`crate::app::run_sync`].
pub async fn sync_course(
    canvas: &CanvasClient,
    course_dir: &Path,
    course: &Course,
    state: &mut State,
    dry_run: bool,
    verbose: bool,
) -> anyhow::Result<SyncCounts> {
    let modules_spinner = spinner(&format!("Loading modules for {}", course.name));
    let modules = canvas.list_modules_with_items(course.id).await?;
    modules_spinner.finish_and_clear();
    // Preload assignments to avoid per-item fetch; map by id
    let assignments_spinner = spinner(&format!("Loading assignments for {}", course.name));
    let assignments_list = canvas.list_assignments(course.id).await.unwrap_or_default();
    assignments_spinner.finish_and_clear();
    let assignments: std::collections::HashMap<u64, Assignment> =
        assignments_list.into_iter().map(|a| (a.id, a)).collect();
    let course_sync = CourseSync {
        canvas,
        httpctx: &canvas.http,
        course_dir,
        course_id: course.id,
        dry_run,
        verbose,
    };
    let module_progress =
        progress_bar(modules.len() as u64, &format!("Modules in {}", course.name));
    let mut totals = SyncCounts::default();
    for m in modules {
        module_progress.inc(1);
        module_progress.set_message(format!("Course {} module {}", course.id, m.id));
        let counts = course_sync.sync_module(&m, &assignments, state).await?;
        totals += counts;
        if dry_run && (counts.pages > 0 || counts.files > 0) {
            module_progress.println(format!(
                "DRY-RUN module {} -> pages: {}, files: {}",
                m.id, counts.pages, counts.files
            ));
        }
    }
    module_progress.finish_and_clear();
    Ok(totals)
}

/// Number of items a module sync wrote, or would write under `--dry-run`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SyncCounts {
    pub pages: usize,
    pub files: usize,
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
    async fn sync_file(&mut self, file_id: u64, state: &mut State) -> anyhow::Result<usize> {
        if !self.processed.insert(file_id) {
            return Ok(0);
        }
        let (course_id, module_id) = (self.course_id, self.module_id);
        let key = crate::canvas::download::state_key(file_id);

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
    ) -> anyhow::Result<usize> {
        let (course_id, module_id) = (self.course_id, self.module_id);
        let hash = content_hash(md.as_bytes());

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
    ) -> anyhow::Result<SyncCounts> {
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
