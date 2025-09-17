use crate::canvas::{Assignment, CanvasClient, FileObj, Module};
use crate::config::{Config, ConfigPaths};
use crate::fsutil::{
    atomic_rename, atomic_write, ensure_dir, sanitize_component, sanitize_filename_preserve_ext,
};
use crate::http::{build_http_client, HttpCtx};
use crate::state::{ItemState, State};
use html2md::parse_html;
use regex::Regex;
use reqwest::header;
use sha1::{Digest, Sha1};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;
use tracing::{debug, info, warn};

pub async fn run_sync(
    filter_course_id: Option<u64>,
    dry_run: bool,
    verbose: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let paths = ConfigPaths::default()?;
    let mut cfg = crate::config::load_config_from_path(&paths.config_file).await?;
    cfg.expand_paths();

    let http = build_http_client(&cfg);
    let httpctx = HttpCtx::new(&cfg, http);
    let canvas = CanvasClient::from_config().await?;

    let courses = canvas.list_courses().await?;
    let ignored: std::collections::HashSet<String> =
        cfg.canvas.ignored_courses.iter().cloned().collect();

    let iter: Box<dyn Iterator<Item = crate::canvas::Course>> = if let Some(cid) = filter_course_id
    {
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
        Box::new(sel.into_iter())
    } else {
        Box::new(
            courses
                .into_iter()
                .filter(move |c| !ignored.contains(&c.id.to_string())),
        )
    };

    let mut total_pages = 0usize;
    let mut total_files = 0usize;
    for c in iter {
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

        let modules = canvas.list_modules_with_items(c.id).await?;
        // Preload assignments to avoid per-item fetch; map by id
        let assignments_list = canvas.list_assignments(c.id).await.unwrap_or_default();
        let assignments: std::collections::HashMap<u64, Assignment> =
            assignments_list.into_iter().map(|a| (a.id, a)).collect();
        for m in modules {
            let (p, f) = sync_module(
                &cfg,
                &canvas,
                &httpctx,
                &course_dir,
                c.id,
                &assignments,
                &mut state,
                &m,
                dry_run,
                verbose,
            )
            .await?;
            total_pages += p;
            total_files += f;
        }

        if !dry_run {
            state.save(&state_path).await?;
        }
    }
    if dry_run {
        println!(
            "DRY-RUN summary: pages to write: {}, files to download: {}",
            total_pages, total_files
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn sync_module(
    _cfg: &Config,
    canvas: &CanvasClient,
    httpctx: &HttpCtx,
    course_dir: &Path,
    course_id: u64,
    assignments: &std::collections::HashMap<u64, Assignment>,
    state: &mut State,
    m: &Module,
    dry_run: bool,
    verbose: bool,
) -> Result<(usize, usize), Box<dyn std::error::Error>> {
    let module_dir =
        course_dir
            .join("Modules")
            .join(format!("{}_{}", m.id, sanitize_component(&m.name)));
    if !dry_run {
        ensure_dir(&module_dir).await?;
    }
    info!(course_id, module_id = m.id, "sync module");

    let mut pages_planned = 0usize;
    let mut files_planned = 0usize;
    let mut processed_ids: HashSet<u64> = HashSet::new();
    for (idx, item) in m.items.iter().enumerate() {
        match item.kind.as_deref() {
            Some("Page") => {
                if let Some(page_url) = &item.page_url {
                    let key = format!("page:{}", page_url);
                    let page = canvas.get_page(course_id, page_url).await?;
                    let title = page.title.clone().unwrap_or_else(|| {
                        item.title
                            .clone()
                            .unwrap_or_else(|| format!("item_{}", idx))
                    });
                    let html = page.body.unwrap_or_default();
                    let md = parse_html(&html);
                    let hash = sha1_hex(md.as_bytes());
                    if state.get(&key).and_then(|s| s.content_hash.as_deref())
                        == Some(hash.as_str())
                    {
                        debug!(course_id, module_id = m.id, page_url, "page unchanged");
                        if !dry_run && verbose {
                            let fname = format!("{:02}-{}.md", idx + 1, sanitize_component(&title));
                            let dest = module_dir.join(fname);
                            println!("SKIP page -> {}", dest.display());
                        }
                    } else {
                        let fname = format!("{:02}-{}.md", idx + 1, sanitize_component(&title));
                        let dest = module_dir.join(fname);
                        if dry_run {
                            pages_planned += 1;
                            println!("DRY-RUN page -> {} ({} bytes)", dest.display(), md.len());
                        } else {
                            atomic_write(&dest, md.as_bytes()).await?;
                            state.set(
                                key,
                                ItemState {
                                    etag: None,
                                    updated_at: page.updated_at,
                                    size: Some(md.len() as u64),
                                    content_hash: Some(hash),
                                },
                            );
                            info!(course_id, module_id = m.id, path = %dest.display(), "wrote page markdown");
                        }
                    }

                    // Discover file links inside the page HTML and download
                    let file_ids = discover_file_ids(&html);
                    for fid in file_ids {
                        if !processed_ids.insert(fid) {
                            continue;
                        }
                        match canvas.get_file(fid).await {
                            Ok(f) => {
                                let fname = f
                                    .display_name
                                    .clone()
                                    .or(f.filename.clone())
                                    .unwrap_or_else(|| format!("file_{}", fid));
                                let dest = module_dir
                                    .join("Attachments")
                                    .join(sanitize_filename_preserve_ext(&fname));
                                let f_ext = dest
                                    .extension()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or_default();
                                let keyf = format!("file:{}", f.id);
                                if dry_run {
                                    if state.get(&keyf).is_some() {
                                        println!(
                                            "DRY-RUN skip file -> [{}] {}",
                                            f_ext,
                                            dest.display()
                                        );
                                    } else {
                                        files_planned += 1;
                                        println!("DRY-RUN file -> [{}] {}", f_ext, dest.display());
                                    }
                                } else {
                                    ensure_dir(dest.parent().unwrap()).await?;
                                    download_if_needed(httpctx, &f, &dest, state, verbose).await?;
                                    info!(course_id, module_id = m.id, file_id = fid, path = %dest.display(), "downloaded file [{}]", f_ext);
                                }
                            }
                            Err(e) => {
                                warn!(course_id, module_id = m.id, file_id = fid, error = %e, "unable to fetch file metadata (discovered)");
                            }
                        }
                    }
                }
            }
            // Some modules link to pages via html_url even if kind isn't Page (e.g., ExternalUrl)
            _ if item
                .html_url
                .as_deref()
                .is_some_and(|u| is_course_page_url(u, course_id)) =>
            {
                // Extract slug from html_url
                if let Some(slug) = extract_page_slug(item.html_url.as_ref().unwrap()) {
                    let key = format!("page:{}", slug);
                    let page = canvas.get_page(course_id, &slug).await?;
                    let title = page
                        .title
                        .clone()
                        .unwrap_or_else(|| item.title.clone().unwrap_or_else(|| slug.clone()));
                    let html = page.body.unwrap_or_default();
                    let md = parse_html(&html);
                    let hash = sha1_hex(md.as_bytes());
                    if state.get(&key).and_then(|s| s.content_hash.as_deref())
                        == Some(hash.as_str())
                    {
                        if !dry_run && verbose {
                            let fname = format!("{:02}-{}.md", idx + 1, sanitize_component(&title));
                            let dest = module_dir.join(fname);
                            println!("SKIP page -> {}", dest.display());
                        }
                    } else if dry_run {
                        let fname = format!("{:02}-{}.md", idx + 1, sanitize_component(&title));
                        let dest = module_dir.join(fname);
                        pages_planned += 1;
                        println!("DRY-RUN page -> {} ({} bytes)", dest.display(), md.len());
                    } else {
                        let fname = format!("{:02}-{}.md", idx + 1, sanitize_component(&title));
                        let dest = module_dir.join(fname);
                        atomic_write(&dest, md.as_bytes()).await?;
                        state.set(
                            key,
                            ItemState {
                                etag: None,
                                updated_at: page.updated_at,
                                size: Some(md.len() as u64),
                                content_hash: Some(hash),
                            },
                        );
                        info!(course_id, module_id = m.id, path = %dest.display(), "wrote page markdown");
                    }
                    let file_ids = discover_file_ids(&html);
                    for fid in file_ids {
                        if !processed_ids.insert(fid) {
                            continue;
                        }
                        match canvas.get_file(fid).await {
                            Ok(f) => {
                                let fname = f
                                    .display_name
                                    .clone()
                                    .or(f.filename.clone())
                                    .unwrap_or_else(|| format!("file_{}", fid));
                                let dest = module_dir
                                    .join("Attachments")
                                    .join(sanitize_filename_preserve_ext(&fname));
                                let f_ext = dest
                                    .extension()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or_default();
                                let keyf = format!("file:{}", f.id);
                                if dry_run {
                                    if state.get(&keyf).is_some() {
                                        println!(
                                            "DRY-RUN skip file -> [{}] {}",
                                            f_ext,
                                            dest.display()
                                        );
                                    } else {
                                        files_planned += 1;
                                        println!("DRY-RUN file -> [{}] {}", f_ext, dest.display());
                                    }
                                } else {
                                    ensure_dir(dest.parent().unwrap()).await?;
                                    download_if_needed(httpctx, &f, &dest, state, verbose).await?;
                                }
                            }
                            Err(e) => {
                                warn!(course_id, module_id = m.id, file_id = fid, error = %e, "unable to fetch file (page link)")
                            }
                        }
                    }
                }
            }
            Some("File") => {
                if let Some(fid) = item.content_id {
                    if !processed_ids.insert(fid) {
                        continue;
                    }
                    match canvas.get_file(fid).await {
                        Ok(f) => {
                            let fname = f
                                .display_name
                                .clone()
                                .or(f.filename.clone())
                                .unwrap_or_else(|| format!("file_{}", fid));
                            let dest = module_dir
                                .join("Attachments")
                                .join(sanitize_filename_preserve_ext(&fname));
                            let f_ext = dest
                                .extension()
                                .and_then(|s| s.to_str())
                                .unwrap_or_default();
                            let keyf = format!("file:{}", f.id);
                            if dry_run {
                                if state.get(&keyf).is_some() {
                                    println!("DRY-RUN skip file -> [{}] {}", f_ext, dest.display());
                                } else {
                                    files_planned += 1;
                                    println!("DRY-RUN file -> [{}] {}", f_ext, dest.display());
                                }
                            } else {
                                ensure_dir(dest.parent().unwrap()).await?;
                                download_if_needed(httpctx, &f, &dest, state, verbose).await?;
                            }
                        }
                        Err(e) => {
                            warn!(course_id, module_id = m.id, file_id = fid, error = %e, "unable to fetch file metadata");
                        }
                    }
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
                        let key = format!("assignment:{}", aid);
                        let hash = sha1_hex(md.as_bytes());
                        let fname =
                            format!("{:02}-ASSIGN-{}.md", idx + 1, sanitize_component(&atitle));
                        let dest = module_dir.join(fname);
                        if state.get(&key).and_then(|s| s.content_hash.as_deref())
                            == Some(hash.as_str())
                        {
                            if !dry_run && verbose {
                                println!("SKIP assignment -> {}", dest.display());
                            }
                        } else if dry_run {
                            pages_planned += 1;
                            println!(
                                "DRY-RUN assignment -> {} ({} bytes)",
                                dest.display(),
                                md.len()
                            );
                        } else {
                            atomic_write(&dest, md.as_bytes()).await?;
                            state.set(
                                key,
                                ItemState {
                                    etag: None,
                                    updated_at: assign.updated_at.clone(),
                                    size: Some(md.len() as u64),
                                    content_hash: Some(hash),
                                },
                            );
                            info!(course_id, module_id = m.id, path = %dest.display(), "wrote assignment markdown");
                        }

                        let file_ids = discover_file_ids(&html);
                        for fid in file_ids {
                            if !processed_ids.insert(fid) {
                                continue;
                            }
                            match canvas.get_file(fid).await {
                                Ok(f) => {
                                    let fname = f
                                        .display_name
                                        .clone()
                                        .or(f.filename.clone())
                                        .unwrap_or_else(|| format!("file_{}", fid));
                                    let dest = module_dir
                                        .join("Attachments")
                                        .join(sanitize_filename_preserve_ext(&fname));
                                    let f_ext = dest
                                        .extension()
                                        .and_then(|s| s.to_str())
                                        .unwrap_or_default();
                                    let keyf = format!("file:{}", f.id);
                                    if dry_run {
                                        if state.get(&keyf).is_some() {
                                            println!(
                                                "DRY-RUN skip file -> [{}] {}",
                                                f_ext,
                                                dest.display()
                                            );
                                        } else {
                                            files_planned += 1;
                                            println!(
                                                "DRY-RUN file -> [{}] {}",
                                                f_ext,
                                                dest.display()
                                            );
                                        }
                                    } else {
                                        ensure_dir(dest.parent().unwrap()).await?;
                                        download_if_needed(httpctx, &f, &dest, state, verbose)
                                            .await?;
                                    }
                                }
                                Err(e) => {
                                    warn!(course_id, module_id = m.id, file_id = fid, error = %e, "unable to fetch file (assignment)")
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok((pages_planned, files_planned))
}

async fn download_if_needed(
    httpctx: &HttpCtx,
    f: &FileObj,
    dest: &Path,
    state: &mut State,
    verbose: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let key = format!("file:{}", f.id);
    let url = f
        .download_url
        .as_ref()
        .or(f.url.as_ref())
        .ok_or("missing file url")?;

    // Probe HEAD for ETag/size
    let head = httpctx.send(httpctx.client.head(url)).await?;
    let status = head.status();
    if !status.is_success() {
        warn!(file_id = f.id, status = %status.as_u16(), "head non-success, will GET");
    }
    let etag = head
        .headers()
        .get(header::ETAG)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.trim_matches('"').to_string());
    let size = head
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());

    let prev = state.get(&key);
    if let (Some(prev), Some(et)) = (prev, etag.as_ref()) {
        if prev.etag.as_deref() == Some(et) {
            info!(file_id = f.id, path = %dest.display(), "unchanged (etag)");
            if verbose {
                println!("SKIP file -> {}", dest.display());
            }
            return Ok(());
        }
    }

    // Prepare dest and part
    let part = dest.with_extension("part");
    let mut start = 0u64;
    if let Ok(meta) = tokio::fs::metadata(&part).await {
        start = meta.len();
    }

    // GET with Range if resuming
    let mut req = httpctx.client.get(url);
    if start > 0 {
        req = req.header(header::RANGE, format!("bytes={}-", start));
    }
    let resp = httpctx.send(req).await?;
    if !(resp.status().is_success() || resp.status().as_u16() == 206) {
        return Err(format!("GET failed: {}", resp.status()).into());
    }

    // Stream to part
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&part)
        .await?;
    let mut stream = resp.bytes_stream();
    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let bytes = chunk?;
        file.write_all(&bytes).await?;
    }
    file.flush().await?;
    atomic_rename(&part, dest).await?;
    info!(file_id = f.id, path = %dest.display(), "downloaded");

    // Update state
    let final_size = match tokio::fs::metadata(dest).await {
        Ok(m) => Some(m.len()),
        Err(_) => size,
    };
    state.set(
        key,
        ItemState {
            etag,
            updated_at: f.updated_at.clone(),
            size: final_size,
            content_hash: None,
        },
    );
    Ok(())
}

fn sha1_hex(data: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

fn discover_file_ids(html: &str) -> HashSet<u64> {
    let mut out = HashSet::new();
    // Matches /files/12345 or /api/v1/files/12345 in any absolute or relative URL
    let re = Regex::new(r"(?i)(?:/api/v1)?/files/(\d+)").unwrap();
    for cap in re.captures_iter(html) {
        if let Some(m) = cap.get(1) {
            if let Ok(id) = m.as_str().parse::<u64>() {
                out.insert(id);
            }
        }
    }
    out
}

fn is_course_page_url(url: &str, course_id: u64) -> bool {
    // e.g., https://.../courses/12345/pages/some-slug
    let re = Regex::new(r"/courses/(\d+)/pages/([A-Za-z0-9_\-]+)").unwrap();
    re.captures(url)
        .and_then(|c| c.get(1).zip(c.get(2)))
        .and_then(|(id, _)| id.as_str().parse::<u64>().ok())
        .map(|id| id == course_id)
        .unwrap_or(false)
}

fn extract_page_slug(url: &str) -> Option<String> {
    let re = Regex::new(r"/courses/(\d+)/pages/([A-Za-z0-9_\-]+)").unwrap();
    re.captures(url)
        .and_then(|c| c.get(2))
        .map(|m| m.as_str().to_string())
}
