use crate::canvas::{Announcement, CanvasClient, FileObj};
use crate::config::Config;
use crate::download::{download_if_needed, Dest};
use crate::fsutil::{atomic_write, ensure_dir, sanitize_component};
use crate::http::{build_http_client, HttpCtx};
use crate::links::{classify_by_filename, extract_links, MediaRef};
use crate::progress::{progress_bar, spinner};
use crate::state::{ItemState, State};
use crate::status;
use html2md::parse_html;
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

#[derive(Debug, Serialize)]
pub struct AnnouncementRecord {
    pub id: u64,
    pub title: String,
    pub posted_at: Option<String>,
    pub html_url: Option<String>,
    pub author: Option<String>,
    pub body_md_path: Option<String>,
    pub links: Vec<String>,
    pub media: Vec<MediaRef>,
    pub zoom_links: Vec<String>,
}

pub async fn run_discovery(filter_course_id: Option<u64>, dry_run: bool) -> anyhow::Result<()> {
    let cfg = Config::load_or_init()?;
    let http = build_http_client(&cfg);
    let httpctx = HttpCtx::new(&cfg, http);
    let canvas = CanvasClient::from_config().await?;

    let courses = canvas.list_courses().await?;
    let ignored: HashSet<String> = cfg.canvas.ignored_courses.iter().cloned().collect();
    let selected: Vec<crate::canvas::Course> = if let Some(cid) = filter_course_id {
        courses.into_iter().filter(|c| c.id == cid).collect()
    } else {
        courses
            .into_iter()
            .filter(|c| !ignored.contains(&c.id.to_string()))
            .collect()
    };

    if selected.is_empty() {
        status!("No matching courses.");
        return Ok(());
    }

    let course_pb = progress_bar(selected.len() as u64, "Scanning announcements");
    let mut totals = CourseSummary::default();
    for course in selected {
        course_pb.inc(1);
        course_pb.set_message(format!("Course {}", course.id));
        let course_dir = course_dir_for(&cfg, &course);
        if !dry_run {
            ensure_dir(&course_dir).await?;
        }
        let state_path = course_dir.join("state.json");
        let mut state = State::load(&state_path).await;

        let summary = AnnouncementSync::new(
            &cfg,
            &canvas,
            &httpctx,
            course.id,
            &course_dir,
            dry_run,
            false,
        )
        .run(&course.name, &mut state)
        .await
        .unwrap_or_else(|e| {
            warn!(course_id = course.id, error = %e, "announcements failed for course");
            CourseSummary::default()
        });
        totals += summary;

        if !dry_run {
            state.save(&state_path).await?;
        }
    }
    course_pb.finish_and_clear();

    status!(
        "{}Announcements: {} | links: {} | media refs: {}",
        if dry_run { "DRY-RUN: " } else { "" },
        totals.announcements,
        totals.links,
        totals.media,
    );
    Ok(())
}

/// What one course's announcement sync produced.
#[derive(Debug, Default, Clone, Copy)]
pub struct CourseSummary {
    pub announcements: usize,
    pub links: usize,
    pub media: usize,
}

impl std::ops::AddAssign for CourseSummary {
    fn add_assign(&mut self, rhs: Self) {
        self.announcements += rhs.announcements;
        self.links += rhs.links;
        self.media += rhs.media;
    }
}

fn course_dir_for(cfg: &Config, course: &crate::canvas::Course) -> PathBuf {
    let code = course.course_code.clone().unwrap_or_default();
    PathBuf::from(&cfg.download_root).join(if code.is_empty() {
        sanitize_component(&course.name)
    } else {
        format!(
            "{}_{}",
            sanitize_component(&course.name),
            sanitize_component(code)
        )
    })
}

/// One course's announcement sync.
///
/// Holds what every announcement in the course needs, so the per-announcement
/// helpers take two arguments rather than eight.
pub struct AnnouncementSync<'a> {
    cfg: &'a Config,
    canvas: &'a CanvasClient,
    httpctx: &'a HttpCtx,
    course_id: u64,
    ann_dir: PathBuf,
    media_dir: PathBuf,
    dry_run: bool,
    verbose: bool,
}

impl<'a> AnnouncementSync<'a> {
    pub fn new(
        cfg: &'a Config,
        canvas: &'a CanvasClient,
        httpctx: &'a HttpCtx,
        course_id: u64,
        course_dir: &Path,
        dry_run: bool,
        verbose: bool,
    ) -> Self {
        let ann_dir = course_dir.join("announcements");
        let media_dir = ann_dir.join("media");
        Self {
            cfg,
            canvas,
            httpctx,
            course_id,
            ann_dir,
            media_dir,
            dry_run,
            verbose,
        }
    }

    /// Whether media should actually be fetched. A dry run never writes.
    fn downloads_media(&self) -> bool {
        self.cfg.announcements.download_media && !self.dry_run
    }

    /// The path recorded in index.json, relative to the course directory.
    fn relative_media_path(path: &Path) -> Option<String> {
        let name = path.file_name()?.to_str()?;
        Some(format!("announcements/media/{name}"))
    }

    /// Downloads one Canvas-hosted file, returning its course-relative path.
    ///
    /// A failure is logged and yields `None`: one missing attachment must not
    /// abandon the announcement it came from.
    async fn fetch_media(&self, file: &FileObj, state: &mut State) -> Option<String> {
        match download_if_needed(
            self.httpctx,
            file,
            Dest::InDir {
                dir: &self.media_dir,
                name: &media_name(file),
            },
            state,
        )
        .await
        {
            Ok(path) => Self::relative_media_path(&path),
            Err(e) => {
                warn!(course_id = self.course_id, file_id = file.id, error = %e, "media download failed");
                state.record_error(crate::download::state_key(file.id), &e);
                None
            }
        }
    }

    /// Resolves the media referenced in an announcement body, downloading the
    /// Canvas-hosted ones when configured to.
    async fn resolve_body_media(
        &self,
        refs: Vec<MediaRef>,
        state: &mut State,
    ) -> anyhow::Result<Vec<MediaRef>> {
        let mut out = Vec::with_capacity(refs.len());
        for mut m in refs {
            if let (true, Some(fid)) = (self.downloads_media(), m.file_id) {
                match self.canvas.get_file(fid).await {
                    Ok(f) => m.local_path = self.fetch_media(&f, state).await,
                    Err(e) => {
                        warn!(course_id = self.course_id, file_id = fid, error = %e, "media metadata fetch failed");
                        state.record_error(crate::download::state_key(fid), &e);
                    }
                }
            }
            out.push(m);
        }
        Ok(out)
    }

    /// Resolves the attachments the API returned alongside the announcement.
    async fn resolve_attachments(
        &self,
        attachments: &[FileObj],
        state: &mut State,
    ) -> Vec<MediaRef> {
        let mut out = Vec::with_capacity(attachments.len());
        for att in attachments {
            // The reference is recorded either way; only the download is
            // conditional, so both cases share one construction.
            let local_path = if self.downloads_media() {
                self.fetch_media(att, state).await
            } else {
                None
            };
            out.push(MediaRef {
                url: att
                    .url
                    .clone()
                    .or_else(|| att.download_url.clone())
                    .unwrap_or_default(),
                kind: classify_by_filename(
                    att.display_name
                        .as_deref()
                        .or(att.filename.as_deref())
                        .unwrap_or(""),
                ),
                file_id: Some(att.id),
                local_path,
            });
        }
        out
    }

    /// The markdown filename for an announcement: date, slug, id.
    fn body_filename(ann: &Announcement, title: &str) -> String {
        let date_prefix = ann
            .posted_at
            .as_deref()
            .and_then(|s| s.get(0..10))
            .map(|d| format!("{d}_"))
            .unwrap_or_default();
        let slug: String = sanitize_component(title).chars().take(60).collect();
        format!("{date_prefix}{slug}_{}.md", ann.id)
    }

    /// Writes the announcement body as markdown unless it is already current.
    async fn write_body(
        &self,
        ann: &Announcement,
        md: &str,
        path: &Path,
        state: &mut State,
    ) -> anyhow::Result<()> {
        let key = format!("announcement:{}", ann.id);
        let unchanged = match (state.get(&key), ann.posted_at.as_deref()) {
            (Some(prev), Some(posted)) => prev.updated_at.as_deref() == Some(posted),
            _ => false,
        };

        if unchanged && path.exists() {
            if self.verbose {
                info!(
                    course_id = self.course_id,
                    announcement_id = ann.id,
                    "announcement unchanged"
                );
            }
            return Ok(());
        }

        atomic_write(path, md.as_bytes()).await?;
        state.set(
            key,
            ItemState {
                etag: None,
                updated_at: ann.posted_at.clone(),
                size: Some(md.len() as u64),
                content_hash: None,
                last_error: None,
                error_count: None,
            },
        );
        Ok(())
    }

    /// Processes one announcement into the record that goes in index.json.
    async fn process(
        &self,
        ann: Announcement,
        state: &mut State,
    ) -> anyhow::Result<(AnnouncementRecord, CourseSummary)> {
        let title = ann
            .title
            .clone()
            .unwrap_or_else(|| format!("announcement_{}", ann.id));
        let html = ann.message.clone().unwrap_or_default();
        let extracted = extract_links(&html);

        let mut summary = CourseSummary {
            announcements: 1,
            links: extracted.all.len(),
            media: extracted.media.len(),
        };

        let filename = Self::body_filename(&ann, &title);
        let body_path = self.ann_dir.join(&filename);

        if self.dry_run {
            info!(
                course_id = self.course_id,
                announcement_id = ann.id,
                links = extracted.all.len(),
                media = extracted.media.len(),
                "dry-run announcement"
            );
        } else {
            self.write_body(&ann, &parse_html(&html), &body_path, state)
                .await?;
        }

        if self.downloads_media() {
            ensure_dir(&self.media_dir).await?;
        }
        let mut media = self.resolve_body_media(extracted.media, state).await?;
        let attachments = self.resolve_attachments(&ann.attachments, state).await;
        summary.media += attachments.len();
        media.extend(attachments);

        Ok((
            AnnouncementRecord {
                id: ann.id,
                title,
                posted_at: ann.posted_at,
                html_url: ann.html_url,
                author: ann.author.and_then(|a| a.display_name),
                body_md_path: (!self.dry_run).then(|| format!("announcements/{filename}")),
                links: extracted.all,
                media,
                zoom_links: extracted.zoom,
            },
            summary,
        ))
    }

    /// Mirrors every announcement in the course and writes the index.
    pub async fn run(&self, course_name: &str, state: &mut State) -> anyhow::Result<CourseSummary> {
        if !self.dry_run {
            ensure_dir(&self.ann_dir).await?;
        }

        let sp = spinner(&format!("Loading announcements for {course_name}"));
        let announcements = match self.canvas.list_announcements(self.course_id).await {
            Ok(v) => v,
            Err(e) => {
                sp.finish_and_clear();
                warn!(course_id = self.course_id, error = %e, "list_announcements failed; skipping");
                return Ok(CourseSummary::default());
            }
        };
        sp.finish_and_clear();

        let pb = progress_bar(
            announcements.len() as u64,
            &format!("Announcements in {course_name}"),
        );
        let mut totals = CourseSummary::default();
        let mut records = Vec::with_capacity(announcements.len());

        for ann in announcements {
            pb.inc(1);
            pb.set_message(
                ann.title
                    .clone()
                    .unwrap_or_else(|| format!("announcement_{}", ann.id)),
            );
            let (record, summary) = self.process(ann, state).await?;
            totals += summary;
            records.push(record);
        }
        pb.finish_and_clear();

        if !self.dry_run {
            let index_path = self.ann_dir.join("index.json");
            atomic_write(&index_path, &serde_json::to_vec_pretty(&records)?).await?;
            info!(course_id = self.course_id, count = records.len(), path = %index_path.display(), "wrote announcements index");
        }
        Ok(totals)
    }
}

/// The preferred on-disk name for an announcement's media file.
///
/// The extension may be absent here; the downloader derives one from the
/// response `Content-Type` when it is.
fn media_name(f: &FileObj) -> String {
    f.display_name
        .clone()
        .or_else(|| f.filename.clone())
        .unwrap_or_else(|| format!("file_{}", f.id))
}
