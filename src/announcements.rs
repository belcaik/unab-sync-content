use crate::canvas::{CanvasClient, FileObj};
use crate::config::Config;
use crate::download::{download_if_needed, Dest};
use crate::fsutil::{atomic_write, ensure_dir, sanitize_component};
use crate::http::{build_http_client, HttpCtx};
use crate::links::{classify_by_filename, extract_links, MediaRef};
use crate::progress::{progress_bar, spinner};
use crate::state::{ItemState, State};
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

pub async fn run_discovery(
    filter_course_id: Option<u64>,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
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
        println!("No matching courses.");
        return Ok(());
    }

    let course_pb = progress_bar(selected.len() as u64, "Scanning announcements");
    let mut totals = (0usize, 0usize, 0usize); // announcements, links, media
    for course in selected {
        course_pb.inc(1);
        course_pb.set_message(format!("Course {}", course.id));
        let course_dir = course_dir_for(&cfg, &course);
        if !dry_run {
            ensure_dir(&course_dir).await?;
        }
        let state_path = course_dir.join("state.json");
        let mut state = State::load(&state_path).await;

        let summary = run_for_course(
            &cfg,
            &canvas,
            &httpctx,
            &course,
            &course_dir,
            &mut state,
            dry_run,
            false,
        )
        .await
        .unwrap_or_else(|e| {
            warn!(course_id = course.id, error = %e, "announcements failed for course");
            CourseSummary::default()
        });
        totals.0 += summary.announcements;
        totals.1 += summary.links;
        totals.2 += summary.media;

        if !dry_run {
            state.save(&state_path).await?;
        }
    }
    course_pb.finish_and_clear();

    println!(
        "{}Announcements: {} | links: {} | media refs: {}",
        if dry_run { "DRY-RUN: " } else { "" },
        totals.0,
        totals.1,
        totals.2,
    );
    Ok(())
}

#[derive(Debug, Default)]
pub struct CourseSummary {
    pub announcements: usize,
    pub links: usize,
    pub media: usize,
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

#[allow(clippy::too_many_arguments)]
pub async fn run_for_course(
    cfg: &Config,
    canvas: &CanvasClient,
    httpctx: &HttpCtx,
    course: &crate::canvas::Course,
    course_dir: &Path,
    state: &mut State,
    dry_run: bool,
    verbose: bool,
) -> Result<CourseSummary, Box<dyn std::error::Error>> {
    let ann_dir = course_dir.join("announcements");
    let media_dir = ann_dir.join("media");
    if !dry_run {
        ensure_dir(&ann_dir).await?;
    }

    let sp = spinner(&format!("Loading announcements for {}", course.name));
    let announcements = match canvas.list_announcements(course.id).await {
        Ok(v) => v,
        Err(e) => {
            sp.finish_and_clear();
            warn!(course_id = course.id, error = %e, "list_announcements failed; skipping");
            return Ok(CourseSummary::default());
        }
    };
    sp.finish_and_clear();

    let mut summary = CourseSummary::default();
    let mut records: Vec<AnnouncementRecord> = Vec::with_capacity(announcements.len());

    let pb = progress_bar(
        announcements.len() as u64,
        &format!("Announcements in {}", course.name),
    );
    for ann in announcements {
        pb.inc(1);
        let title = ann
            .title
            .clone()
            .unwrap_or_else(|| format!("announcement_{}", ann.id));
        pb.set_message(title.clone());
        let html = ann.message.clone().unwrap_or_default();
        let extracted = extract_links(&html);
        summary.announcements += 1;
        summary.links += extracted.all.len();
        summary.media += extracted.media.len();

        let date_prefix = ann
            .posted_at
            .as_deref()
            .and_then(|s| s.get(0..10))
            .map(|d| format!("{}_", d))
            .unwrap_or_default();
        let slug = sanitize_component(&title);
        let slug_short: String = slug.chars().take(60).collect();
        let body_filename = format!("{}{}_{}.md", date_prefix, slug_short, ann.id);
        let body_path = ann_dir.join(&body_filename);

        let key = format!("announcement:{}", ann.id);
        let prev = state.get(&key).cloned();
        let unchanged = match (&prev, ann.posted_at.as_deref()) {
            (Some(p), Some(posted)) => p.updated_at.as_deref() == Some(posted),
            _ => false,
        };

        let md = parse_html(&html);
        let body_md_path_rel = format!("announcements/{}", body_filename);

        if dry_run {
            info!(
                course_id = course.id,
                announcement_id = ann.id,
                links = extracted.all.len(),
                media = extracted.media.len(),
                "dry-run announcement"
            );
        } else if unchanged && body_path.exists() {
            if verbose {
                info!(
                    course_id = course.id,
                    announcement_id = ann.id,
                    "announcement unchanged"
                );
            }
        } else {
            atomic_write(&body_path, md.as_bytes()).await?;
            state.set(
                key.clone(),
                ItemState {
                    etag: None,
                    updated_at: ann.posted_at.clone(),
                    size: Some(md.len() as u64),
                    content_hash: None,
                    last_error: None,
                    error_count: None,
                },
            );
        }

        // Download media (Canvas-hosted files) and inline attachments
        let mut media_with_paths: Vec<MediaRef> = Vec::with_capacity(extracted.media.len());
        if cfg.announcements.download_media && !dry_run {
            ensure_dir(&media_dir).await?;
        }
        for m in extracted.media.iter().cloned() {
            let mut m = m;
            if cfg.announcements.download_media && !dry_run {
                if let Some(fid) = m.file_id {
                    info!(course_id = course.id, announcement_id = ann.id, file_id = fid, url = %m.url, "discovered canvas media in announcement body");
                    match canvas.get_file(fid).await {
                        Ok(f) => match download_if_needed(
                            httpctx,
                            &f,
                            Dest::InDir {
                                dir: &media_dir,
                                name: &media_name(&f),
                            },
                            state,
                        )
                        .await
                        {
                            Ok(path) => {
                                m.local_path = Some(format!(
                                    "announcements/media/{}",
                                    path.file_name().and_then(|s| s.to_str()).unwrap_or("")
                                ));
                            }
                            Err(e) => {
                                warn!(course_id = course.id, file_id = fid, error = %e, "media download failed");
                            }
                        },
                        Err(e) => {
                            warn!(course_id = course.id, file_id = fid, error = %e, "media metadata fetch failed");
                        }
                    }
                }
            }
            media_with_paths.push(m);
        }

        // Inline attachments returned by the API
        for att in &ann.attachments {
            if cfg.announcements.download_media && !dry_run {
                if let Err(e) = ensure_dir(&media_dir).await {
                    warn!(error = %e, "ensure media_dir");
                }
                match download_if_needed(
                    httpctx,
                    att,
                    Dest::InDir {
                        dir: &media_dir,
                        name: &media_name(att),
                    },
                    state,
                )
                .await
                {
                    Ok(path) => {
                        media_with_paths.push(MediaRef {
                            url: att
                                .url
                                .clone()
                                .or(att.download_url.clone())
                                .unwrap_or_default(),
                            kind: classify_by_filename(
                                att.display_name
                                    .as_deref()
                                    .or(att.filename.as_deref())
                                    .unwrap_or(""),
                            ),
                            file_id: Some(att.id),
                            local_path: Some(format!(
                                "announcements/media/{}",
                                path.file_name().and_then(|s| s.to_str()).unwrap_or("")
                            )),
                        });
                        summary.media += 1;
                    }
                    Err(e) => {
                        warn!(course_id = course.id, file_id = att.id, error = %e, "attachment download failed");
                    }
                }
            } else {
                media_with_paths.push(MediaRef {
                    url: att
                        .url
                        .clone()
                        .or(att.download_url.clone())
                        .unwrap_or_default(),
                    kind: classify_by_filename(
                        att.display_name
                            .as_deref()
                            .or(att.filename.as_deref())
                            .unwrap_or(""),
                    ),
                    file_id: Some(att.id),
                    local_path: None,
                });
                summary.media += 1;
            }
        }

        records.push(AnnouncementRecord {
            id: ann.id,
            title,
            posted_at: ann.posted_at.clone(),
            html_url: ann.html_url.clone(),
            author: ann.author.as_ref().and_then(|a| a.display_name.clone()),
            body_md_path: if dry_run {
                None
            } else {
                Some(body_md_path_rel)
            },
            links: extracted.all,
            media: media_with_paths,
            zoom_links: extracted.zoom,
        });
    }
    pb.finish_and_clear();

    if !dry_run {
        let index_path = ann_dir.join("index.json");
        let json = serde_json::to_vec_pretty(&records)?;
        atomic_write(&index_path, &json).await?;
        info!(course_id = course.id, count = records.len(), path = %index_path.display(), "wrote announcements index");
    }

    Ok(summary)
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
