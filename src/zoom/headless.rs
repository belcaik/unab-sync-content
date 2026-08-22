use crate::config::Config;
use crate::zoom::app_conf;
use crate::zoom::db::ZoomDb;
use crate::zoom::models::{ZoomCookie, ZoomRecordingFile};
use crate::zoom::sso::{self, SsoCreds};
use base64::prelude::*;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::network::EventRequestWillBeSent;
use chromiumoxide::Page;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tracing::{info, warn};
use url::Url;

pub struct ZoomHeadless<'a> {
    config: &'a Config,
    db: &'a ZoomDb,
    course_id: u64,
}

/// What the CDP interception tasks have captured so far.
///
/// The identifiers arrive across several intercepted responses, so this
/// accumulates rather than being written once.
#[derive(Debug, Default)]
struct Captured {
    scid: Option<String>,
    headers: Option<HashMap<String, String>>,
}

/// Locks shared capture state, recovering from a poisoned mutex.
///
/// The guarded value is plain accumulated data with no invariant a panicking
/// task could leave broken, so a poisoned lock is worth recovering from rather
/// than propagating as a second panic.
fn lock_captured(m: &Mutex<Captured>) -> std::sync::MutexGuard<'_, Captured> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Collects the page's cookies for the given domains, in the shape the
/// recordings database and the downloader expect.
///
/// The domain set differs by caller: capturing a session needs `zoom.us`, while
/// downloading also needs the CDN the media is served from.
async fn harvest_cookies(page: &Page, domains: &[&str]) -> anyhow::Result<Vec<ZoomCookie>> {
    Ok(page
        .get_cookies()
        .await?
        .into_iter()
        .filter(|c| domains.iter().any(|d| c.domain.contains(d)))
        .map(|c| ZoomCookie {
            domain: c.domain,
            name: c.name,
            value: c.value,
            path: c.path,
            expires: Some(c.expires as i64),
            secure: c.secure,
            http_only: c.http_only,
        })
        .collect())
}

impl<'a> ZoomHeadless<'a> {
    pub fn new(config: &'a Config, db: &'a ZoomDb, course_id: u64) -> Self {
        Self {
            config,
            db,
            course_id,
        }
    }

    pub async fn authenticate_and_capture(&self) -> anyhow::Result<()> {
        let (mut browser, mut handler) = Browser::launch(
            BrowserConfig::builder()
                .arg("--no-sandbox")
                .arg("--disable-gpu")
                .arg("--disable-dev-shm-usage")
                .build()
                .map_err(|e| anyhow::anyhow!("could not build browser config: {e}"))?,
        )
        .await?;

        let handle = tokio::spawn(async move {
            while let Some(h) = handler.next().await {
                if let Err(e) = h {
                    warn!("Browser handler error: {:?}", e);
                    break;
                }
            }
            info!("Browser handler loop exited.");
        });

        let page = browser.new_page("about:blank").await?;
        page.set_user_agent(&self.config.zoom.user_agent).await?;

        // Enable network events
        // Check if we already have scid in DB
        if let Ok(Some(stored_scid)) = self.db.get_scid(self.course_id) {
            // The scid is a live session credential; log its presence, not its value.
            let _ = stored_scid;
            info!("found existing lti_scid in DB");
            // We still proceed to refresh cookies and verify scid
        }

        // Filled in by the interception tasks below, read by the polling loop.
        let captured_data = Arc::new(Mutex::new(Captured::default()));
        let captured_data_clone_for_fetch = captured_data.clone(); // Renamed to avoid conflict with new `captured_data_clone`

        // Enable Fetch domain for interception
        let patterns = vec![
            chromiumoxide::cdp::browser_protocol::fetch::RequestPattern::builder()
                .url_pattern("*applications.zoom.us/lti/advantage*")
                .request_stage(chromiumoxide::cdp::browser_protocol::fetch::RequestStage::Response)
                .build(),
        ];
        page.execute(
            chromiumoxide::cdp::browser_protocol::fetch::EnableParams::builder()
                .patterns(patterns)
                .build(),
        )
        .await?;

        let mut request_paused_events = page
            .event_listener::<chromiumoxide::cdp::browser_protocol::fetch::EventRequestPaused>()
            .await?;

        let page_clone = page.clone();
        let captured_data_clone = captured_data.clone(); // This is the one used by the new task

        let mut request_events = page.event_listener::<EventRequestWillBeSent>().await?;

        // Spawn Fetch interception task
        tokio::spawn(async move {
            while let Some(event) = request_paused_events.next().await {
                let req_id = event.request_id.clone();
                // Always continue the request eventually
                let page_inner = page_clone.clone();
                let req_id_inner = req_id.clone();

                // We only care if we have a response status code (response stage)
                if event.response_status_code.is_some() {
                    match page_inner
                        .execute(
                            chromiumoxide::cdp::browser_protocol::fetch::GetResponseBodyParams::new(
                                req_id.clone(),
                            ),
                        )
                        .await
                    {
                        Ok(body) => {
                            // Capture headers
                            let mut headers = HashMap::new();
                            let headers_val = serde_json::to_value(event.request.headers.clone())
                                .unwrap_or(serde_json::Value::Null);
                            if let Some(obj) = headers_val.as_object() {
                                for (k, v) in obj {
                                    let key_lower = k.to_ascii_lowercase();
                                    if key_lower != "cookie"
                                        && key_lower != "host"
                                        && key_lower != "content-length"
                                    {
                                        if let Some(s) = v.as_str() {
                                            headers.insert(k.clone(), s.to_string());
                                        }
                                    }
                                }
                            }

                            let content = if body.base64_encoded {
                                match BASE64_STANDARD.decode(&body.body) {
                                    Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
                                    Err(_) => body.body.clone(),
                                }
                            } else {
                                body.body.clone()
                            };

                            let found = app_conf::parse(&content);
                            if !found.is_empty() {
                                let mut data = lock_captured(&captured_data_clone);
                                if let Some(scid) = found.scid {
                                    info!("Captured lti_scid from Fetch");
                                    data.scid = Some(scid);
                                }
                                if !found.headers.is_empty() {
                                    info!(
                                        "Captured {} session headers from Fetch",
                                        found.headers.len()
                                    );
                                    data.headers
                                        .get_or_insert_with(HashMap::new)
                                        .extend(found.headers);
                                }
                            }
                        }
                        Err(e) => {
                            warn!("Failed to get body in Fetch interception: {:?}", e);
                        }
                    }
                }

                // Continue request
                let _ = page_inner
                    .execute(
                        chromiumoxide::cdp::browser_protocol::fetch::ContinueRequestParams::new(
                            req_id_inner,
                        ),
                    )
                    .await;
            }
        });

        let _capture_task = tokio::spawn(async move {
            while let Some(event) = request_events.next().await {
                let url = event.request.url.clone();
                let mut data = lock_captured(&captured_data_clone_for_fetch);

                if data.scid.is_none() && url.contains("lti_scid=") {
                    if let Ok(parsed) = Url::parse(&url) {
                        for (k, v) in parsed.query_pairs() {
                            if k == "lti_scid" {
                                info!("captured lti_scid from URL");
                                data.scid = Some(v.to_string());
                            }
                        }
                    }
                }

                // Capture headers for Zoom API calls
                if data.headers.is_none() && url.contains("/api/v1/lti/rich/recording") {
                    let headers_val = serde_json::to_value(event.request.headers.clone())
                        .unwrap_or(serde_json::Value::Null);
                    let mut headers = HashMap::new();
                    if let Some(obj) = headers_val.as_object() {
                        for (k, v) in obj {
                            if let Some(s) = v.as_str() {
                                headers.insert(k.clone(), s.to_string());
                            }
                        }
                    }
                    info!("Captured Zoom API headers");
                    data.headers = Some(headers);
                }
            }
        });

        let target_url = format!(
            "{}/courses/{}/external_tools/{}",
            self.config.canvas.base_url, self.course_id, self.config.zoom.external_tool_id
        );

        info!("Navigating to: {}", target_url);
        page.goto(&target_url).await?;

        // Handle SSO
        sso::handle_sso(&page, &SsoCreds::from_config(self.config)).await?;

        // Wait for Zoom LTI to load and capture data
        info!("Waiting for Zoom LTI to load...");

        let mut scid = None;
        let mut captured_headers: HashMap<String, String> = HashMap::new();

        // Wait up to 60 seconds for the LTI load
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(60) {
            // Check shared state
            {
                let data = lock_captured(&captured_data);
                if let Some(s) = &data.scid {
                    scid = Some(s.clone());
                }
                if let Some(h) = &data.headers {
                    captured_headers = h.clone();
                }
            }

            if scid.is_some() {
                break;
            }

            sleep(Duration::from_millis(500)).await;
        }

        // Get cookies
        let cookies = harvest_cookies(&page, &["zoom.us"]).await?;

        if let Some(s) = scid {
            self.db.save_scid(self.course_id, &s)?;
            info!("saved lti_scid to DB");
        } else {
            return Err(anyhow::anyhow!("Failed to capture lti_scid"));
        }

        if !captured_headers.is_empty() {
            // Clear old headers first to avoid mixing with stale data
            self.db.delete_all_request_headers(self.course_id)?;

            let header_list: Vec<(String, String)> = captured_headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();

            // Log keys to verify we have x-xsrf-token
            let keys: Vec<String> = header_list.iter().map(|(k, _)| k.clone()).collect();
            info!("Saving headers: {:?}", keys);

            self.db.save_request_headers(
                self.course_id,
                "/api/v1/lti/rich/recording",
                &header_list,
            )?;
            info!("Saved {} request headers to DB", captured_headers.len());
        } else {
            warn!("Warning: No request headers captured");
        }

        if !cookies.is_empty() {
            self.db.replace_cookies(&cookies)?;
        } else {
            return Err(anyhow::anyhow!("Failed to capture Zoom cookies"));
        }

        // Verification log
        let scid_after = self.db.get_scid(self.course_id)?;
        let cookies_after = self.db.load_cookies()?;
        let headers_after = self.db.get_all_request_headers(self.course_id)?;

        info!(
            "AFTER HEADLESS SAVE -> scid={:?}, cookies={}, headers={}",
            scid_after,
            cookies_after.len(),
            headers_after.len()
        );

        browser.close().await?;
        handle.await?;

        Ok(())
    }

    pub async fn capture_and_download_immediately(
        &self,
        files: Vec<ZoomRecordingFile>,
    ) -> anyhow::Result<()> {
        use crate::ffmpeg::{download_via_ffmpeg, ensure_ffmpeg_available, FfmpegError};
        use crate::fsutil::sanitize_filename_preserve_ext;
        use crate::zoom::models::ReplayHeader;
        use std::collections::HashMap;
        use std::path::PathBuf;

        let cfg = self.config;
        let course_id = self.course_id;

        ensure_ffmpeg_available(&cfg.zoom.ffmpeg_path).await?;

        let base = PathBuf::from(&cfg.download_root)
            .join("Zoom")
            .join(course_id.to_string());

        tokio::fs::create_dir_all(&base).await?;

        // Scan for existing recordings to avoid redownloading
        let existing_files = scan_existing_recordings(&base)?;
        let files_to_download: Vec<_> = files
            .into_iter()
            .filter(|file| {
                let filename = sanitize_filename_preserve_ext(&(file.filename_hint() + ".mp4"));
                if existing_files.contains(&filename) {
                    info!("⏩ Skipping (already exists): {}", filename);
                    false
                } else {
                    true
                }
            })
            .collect();

        if files_to_download.is_empty() {
            info!("All recordings already downloaded!");
            return Ok(());
        }

        info!(
            "Found {} recordings, {} new to download",
            files_to_download.len() + existing_files.len(),
            files_to_download.len()
        );

        let (mut browser, mut handler) = Browser::launch(
            BrowserConfig::builder()
                // Running in full headless mode (no GUI)
                .arg("--no-sandbox")
                .arg("--disable-gpu")
                .build()
                .map_err(|e| anyhow::anyhow!("could not build browser config: {e}"))?,
        )
        .await?;

        let handle = tokio::spawn(async move {
            while let Some(h) = handler.next().await {
                if h.is_err() {
                    break;
                }
            }
        });

        let page = browser.new_page("about:blank").await?;
        page.set_user_agent(&self.config.zoom.user_agent).await?;

        let mut name_counts: HashMap<String, usize> = HashMap::new();
        info!("Starting capture and download (tokens expire quickly, processing one by one)...");
        info!(
            "Processing {} recordings (capture → download → next)...\n",
            files_to_download.len()
        );

        let mut cookies_captured = false;

        for (idx, file) in files_to_download.iter().enumerate() {
            info!(
                "\n[{}/{}] Processing: {}",
                idx + 1,
                files_to_download.len(),
                file.play_url
            );

            // STEP 1: Navigate to play URL
            let mut events = page.event_listener::<EventRequestWillBeSent>().await?;
            page.goto(&file.play_url).await?;

            // STEP 2: Authenticate if needed
            if let Err(e) =
                sso::handle_zoom_play_sso(&page, &SsoCreds::from_config(self.config)).await
            {
                warn!("Warning: SSO failed for {}: {:?}", file.play_url, e);
                info!("Skipping this file...");
                continue;
            }

            // STEP 3: Capture fresh cookies (first file only) and load for downloads
            let zoom_cookies = if !cookies_captured {
                info!("Capturing fresh cookies after SSO...");
                let fresh_cookies = harvest_cookies(&page, &["zoom.us", "cloudfront.net"]).await?;
                if !fresh_cookies.is_empty() {
                    self.db.replace_cookies(&fresh_cookies)?;
                    info!("Saved {} fresh cookies for downloads", fresh_cookies.len());
                }
                cookies_captured = true;
                fresh_cookies
            } else {
                // Load cookies from DB for subsequent files
                self.db.load_cookies()?
            };

            // STEP 4: Wait for media request (capture EXACT headers from .mp4 request)
            let start = Instant::now();
            let mut asset: Option<ReplayHeader> = None;

            while start.elapsed() < Duration::from_secs(30) {
                tokio::select! {
                    event = events.next() => {
                        if let Some(event) = event {
                            let url = event.request.url.clone();
                            if self.is_replay_asset(&url) {
                                // Capture ALL headers without filtering (including cookie, host, etc.)
                                let headers_val = serde_json::to_value(event.request.headers.clone())
                                    .unwrap_or(serde_json::Value::Null);
                                let mut headers = HashMap::new();
                                if let Some(obj) = headers_val.as_object() {
                                    for (k, v) in obj {
                                        if let Some(s) = v.as_str() {
                                            headers.insert(k.clone(), s.to_string());
                                        }
                                    }
                                }

                                info!("✓ Captured download URL: {}", url);
                                info!("  Captured {} headers from MP4 request:", headers.len());
                                for (k, v) in &headers {
                                    // Log all headers (truncate long values like cookies)
                                    let display_val = if v.len() > 100 {
                                        format!("{}...", &v[..100])
                                    } else {
                                        v.clone()
                                    };
                                    info!("    {}: {}", k, display_val);
                                }

                                asset = Some(ReplayHeader {
                                    download_url: url.clone(),
                                    headers,
                                });
                                break;
                            }
                        }
                    }
                    _ = sleep(Duration::from_millis(100)) => {}
                }
            }

            let asset = match asset {
                Some(a) => a,
                None => {
                    warn!("✗ Could not capture download URL, skipping...");
                    continue;
                }
            };

            // STEP 5: Download immediately (while token is fresh!)
            let mut filename = sanitize_filename_preserve_ext(file.filename_hint() + ".mp4");
            let count = name_counts.entry(filename.clone()).or_insert(0);
            if *count > 0 {
                let stem = filename.trim_end_matches(".mp4");
                filename = format!("{}_{}.mp4", stem, count);
            }
            *count += 1;

            let dest = base.join(&filename);
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }

            let headers = crate::zoom::download::build_ffmpeg_headers(
                cfg,
                &asset,
                &file.play_url,
                &zoom_cookies,
                &asset.download_url,
            );

            info!("⬇ Downloading to: {}", dest.display());
            match download_via_ffmpeg(&cfg.zoom.ffmpeg_path, &headers, &asset.download_url, &dest)
                .await
            {
                Ok(()) => warn!("✓ Downloaded successfully!"),
                Err(FfmpegError::Process { .. }) => {
                    warn!("ffmpeg failed, trying HTTP fallback");
                    if let Err(e) = crate::zoom::download::http_download(
                        cfg,
                        &headers,
                        &asset.download_url,
                        &dest,
                    )
                    .await
                    {
                        warn!("✗ HTTP download also failed: {:?}", e);
                    } else {
                        info!("✓ Downloaded via HTTP!");
                    }
                }
                Err(e) => {
                    warn!("✗ Download error: {:?}", e);
                }
            }
        }

        browser.close().await?;
        handle.await?;

        info!(
            "\nAll files processed! Downloads saved to: {}",
            base.display()
        );
        Ok(())
    }

    fn is_replay_asset(&self, url: &str) -> bool {
        if let Ok(parsed) = Url::parse(url) {
            let host_ok = parsed
                .host_str()
                .map(|host| host.ends_with("zoom.us") || host.contains("cloudfront.net"))
                .unwrap_or(false);
            let path = parsed.path().to_ascii_lowercase();
            host_ok
                && (path.ends_with(".mp4")
                    || path.contains(".mp4?")
                    || path.ends_with(".m3u8")
                    || path.contains("playlist.m3u8"))
        } else {
            false
        }
    }
}

/// Helper function to scan existing .mp4 files in the recordings directory
fn scan_existing_recordings(
    dir: &std::path::Path,
) -> anyhow::Result<std::collections::HashSet<String>> {
    let mut existing = std::collections::HashSet::new();
    if dir.exists() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if let Some(name) = entry.file_name().to_str() {
                if name.ends_with(".mp4") {
                    existing.insert(name.to_string());
                }
            }
        }
    }
    Ok(existing)
}
