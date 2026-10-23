use crate::core::config::Config;
use crate::core::fsutil::sanitize_filename_preserve_ext;
use crate::core::http::HttpCtx;
use crate::zoom::app_conf;
use crate::zoom::db::ZoomDb;
use crate::zoom::models::{ReplayHeader, ZoomCookie, ZoomRecordingFile};
use crate::zoom::sso::{self, SsoCreds};
use base64::prelude::*;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, EnableParams, EventRequestPaused, GetResponseBodyParams, RequestPattern,
    RequestStage,
};
use chromiumoxide::cdp::browser_protocol::network::{EventRequestWillBeSent, Headers};
use chromiumoxide::Page;
use futures::StreamExt;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tracing::{info, warn};
use url::Url;

pub struct ZoomHeadless<'a> {
    config: &'a Config,
    db: &'a ZoomDb,
    course_id: u64,
    /// The one media-download context for the flow: paced and retried like
    /// every request in the crate, but built from
    /// [`crate::zoom::download::build_media_client`] so a large recording is
    /// not killed by a total request timeout.
    media_http: HttpCtx,
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

/// Flattens CDP request headers into a plain map of header name to value.
///
/// CDP carries headers as a JSON blob; only string values are kept.
fn cdp_headers_to_map(headers: &Headers) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Ok(serde_json::Value::Object(obj)) = serde_json::to_value(headers) {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                map.insert(k, s.to_string());
            }
        }
    }
    map
}

/// Launches the headless browser and spawns the task that drives its CDP
/// handler; that task must keep running for the browser to answer commands.
async fn launch_browser() -> anyhow::Result<(Browser, JoinHandle<()>)> {
    let (browser, mut handler) = Browser::launch(
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

    Ok((browser, handle))
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

/// Enables Fetch-domain interception of the Zoom LTI responses and spawns the
/// two capture tasks: one scrapes `window.appConf` out of paused responses,
/// the other watches outgoing requests for the scid and the API headers.
async fn install_interception(page: &Page, captured: Arc<Mutex<Captured>>) -> anyhow::Result<()> {
    let patterns = vec![RequestPattern::builder()
        .url_pattern("*applications.zoom.us/lti/advantage*")
        .request_stage(RequestStage::Response)
        .build()];
    page.execute(EnableParams::builder().patterns(patterns).build())
        .await?;

    let mut paused_events = page.event_listener::<EventRequestPaused>().await?;
    let page_clone = page.clone();
    let captured_fetch = captured.clone();

    let mut request_events = page.event_listener::<EventRequestWillBeSent>().await?;

    // Spawn Fetch interception task
    tokio::spawn(async move {
        while let Some(event) = paused_events.next().await {
            handle_paused_response(&page_clone, &captured_fetch, &event).await;
        }
    });

    let _capture_task = tokio::spawn(async move {
        while let Some(event) = request_events.next().await {
            record_outgoing_request(&captured, &event);
        }
    });

    Ok(())
}

/// Handles one paused Fetch response: captures its request headers, scrapes
/// `window.appConf` from the body, then always lets the request continue.
async fn handle_paused_response(
    page: &Page,
    captured: &Mutex<Captured>,
    event: &EventRequestPaused,
) {
    // Always continue the request eventually
    let req_id = event.request_id.clone();

    // We only care if we have a response status code (response stage)
    if event.response_status_code.is_some() {
        match page
            .execute(GetResponseBodyParams::new(req_id.clone()))
            .await
        {
            Ok(body) => {
                // Capture headers
                let mut headers = cdp_headers_to_map(&event.request.headers);
                headers.retain(|k, _| {
                    let key_lower = k.to_ascii_lowercase();
                    key_lower != "cookie" && key_lower != "host" && key_lower != "content-length"
                });

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
                    let mut data = lock_captured(captured);
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
    let _ = page.execute(ContinueRequestParams::new(req_id)).await;
}

/// Records capture data from one outgoing request: the scid when it appears
/// in the URL, or the Zoom API request headers.
fn record_outgoing_request(captured: &Mutex<Captured>, event: &EventRequestWillBeSent) {
    let url = event.request.url.clone();
    let mut data = lock_captured(captured);

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
        let headers = cdp_headers_to_map(&event.request.headers);
        info!("Captured Zoom API headers");
        data.headers = Some(headers);
    }
}

/// Whether `url` is the recording asset itself, served from a Zoom media
/// host. Only `.mp4` counts: an `.m3u8` is a playlist, not a recording.
fn is_replay_asset(url: &str) -> bool {
    if let Ok(parsed) = Url::parse(url) {
        let host_ok = parsed
            .host_str()
            .map(|host| host.ends_with("zoom.us") || host.contains("cloudfront.net"))
            .unwrap_or(false);
        host_ok && parsed.path().to_ascii_lowercase().ends_with(".mp4")
    } else {
        false
    }
}

/// Lists the `.mp4` files already in the recordings directory.
async fn scan_existing_recordings(dir: &Path) -> anyhow::Result<HashSet<String>> {
    let mut existing = HashSet::new();
    if tokio::fs::try_exists(dir).await? {
        let mut entries = tokio::fs::read_dir(dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            if let Some(name) = entry.file_name().to_str() {
                if name.ends_with(".mp4") {
                    existing.insert(name.to_string());
                }
            }
        }
    }
    Ok(existing)
}

/// Filters `files` down to the recordings not already on disk under `base`,
/// creating `base` if needed.
async fn pending_recordings(
    base: &Path,
    files: Vec<ZoomRecordingFile>,
) -> anyhow::Result<Vec<ZoomRecordingFile>> {
    tokio::fs::create_dir_all(base).await?;

    // Scan for existing recordings to avoid redownloading
    let existing_files = scan_existing_recordings(base).await?;
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
    } else {
        info!(
            "Found {} recordings, {} new to download",
            files_to_download.len() + existing_files.len(),
            files_to_download.len()
        );
    }

    Ok(files_to_download)
}

impl<'a> ZoomHeadless<'a> {
    pub fn new(config: &'a Config, db: &'a ZoomDb, course_id: u64, media_http: HttpCtx) -> Self {
        Self {
            config,
            db,
            course_id,
            media_http,
        }
    }

    /// Opens the Canvas external-tool page for this course and drives whatever
    /// SSO the navigation lands on.
    async fn navigate_to_tool(&self, page: &Page) -> anyhow::Result<()> {
        let target_url = format!(
            "{}/courses/{}/external_tools/{}",
            self.config.canvas.base_url, self.course_id, self.config.zoom.external_tool_id
        );

        info!("Navigating to: {}", target_url);
        page.goto(&target_url).await?;

        // Handle SSO
        sso::handle_sso(page, &SsoCreds::from_config(self.config)).await?;

        // Wait for Zoom LTI to load and capture data
        info!("Waiting for Zoom LTI to load...");
        Ok(())
    }

    /// Writes the captured scid, request headers and cookies to the session
    /// database, then logs what is stored. A missing scid or cookie jar is an
    /// error; missing headers only warn.
    fn persist_session(
        &self,
        scid: Option<String>,
        captured_headers: HashMap<String, String>,
        cookies: Vec<ZoomCookie>,
    ) -> anyhow::Result<()> {
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
        let has_scid = self.db.get_scid(self.course_id)?.is_some();
        let cookies_after = self.db.load_cookies()?;
        let headers_after = self.db.get_all_request_headers(self.course_id)?;

        info!(
            has_scid,
            cookies = cookies_after.len(),
            headers = headers_after.len(),
            "after headless save"
        );

        Ok(())
    }

    pub async fn authenticate_and_capture(&self) -> anyhow::Result<()> {
        let (mut browser, handle) = launch_browser().await?;

        let page = browser.new_page("about:blank").await?;
        page.set_user_agent(&self.config.zoom.user_agent).await?;

        if self.db.get_scid(self.course_id)?.is_some() {
            info!("found existing lti_scid in DB");
            // We still proceed to refresh cookies and verify scid
        }

        // Filled in by the interception tasks, read by the polling loop.
        let captured = Arc::new(Mutex::new(Captured::default()));
        install_interception(&page, captured.clone()).await?;
        self.navigate_to_tool(&page).await?;

        let (scid, captured_headers) = wait_for_capture(&captured).await;

        // Get cookies
        let cookies = harvest_cookies(&page, &["zoom.us"]).await?;

        self.persist_session(scid, captured_headers, cookies)?;

        browser.close().await?;
        handle.await?;

        Ok(())
    }

    /// Visits one recording's play URL and waits up to 30 s for the `.mp4`
    /// asset request, capturing the exact headers it was sent with. Refreshes
    /// the stored cookies on the first visit. Returns `None` when SSO fails or
    /// no asset request shows up in time.
    async fn capture_replay_asset(
        &self,
        page: &Page,
        file: &ZoomRecordingFile,
        cookies_captured: &mut bool,
    ) -> anyhow::Result<Option<(ReplayHeader, Vec<ZoomCookie>)>> {
        let mut events = page.event_listener::<EventRequestWillBeSent>().await?;
        page.goto(&file.play_url).await?;

        // Authenticate if needed
        if let Err(e) = sso::handle_zoom_play_sso(page, &SsoCreds::from_config(self.config)).await {
            warn!("Warning: SSO failed for {}: {:?}", file.play_url, e);
            info!("Skipping this file...");
            return Ok(None);
        }

        // Capture fresh cookies (first file only) and load for downloads
        let zoom_cookies = if !*cookies_captured {
            info!("Capturing fresh cookies after SSO...");
            let fresh_cookies = harvest_cookies(page, &["zoom.us", "cloudfront.net"]).await?;
            if !fresh_cookies.is_empty() {
                self.db.replace_cookies(&fresh_cookies)?;
                info!("Saved {} fresh cookies for downloads", fresh_cookies.len());
            }
            *cookies_captured = true;
            fresh_cookies
        } else {
            // Load cookies from DB for subsequent files
            self.db.load_cookies()?
        };

        // Wait for the media request (capture EXACT headers from the .mp4 request)
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(30) {
            tokio::select! {
                event = events.next() => {
                    if let Some(event) = event {
                        let url = event.request.url.clone();
                        if is_replay_asset(&url) {
                            let headers = cdp_headers_to_map(&event.request.headers);

                            // Header names only: the values carry the session cookie.
                            let names: Vec<&str> = headers.keys().map(String::as_str).collect();
                            info!(url = super::redact_url(&url), ?names, "captured download URL");

                            let asset = ReplayHeader {
                                download_url: url,
                                headers,
                            };
                            return Ok(Some((asset, zoom_cookies)));
                        }
                    }
                }
                _ = sleep(Duration::from_millis(100)) => {}
            }
        }

        warn!("✗ Could not capture download URL, skipping...");
        Ok(None)
    }

    /// Downloads one captured asset to `base`, de-duplicating the filename
    /// against the ones already used this run. A failed download warns and
    /// does not abort the run.
    async fn download_recording(
        &self,
        base: &Path,
        file: &ZoomRecordingFile,
        asset: &ReplayHeader,
        zoom_cookies: &[ZoomCookie],
        name_counts: &mut HashMap<String, usize>,
    ) -> anyhow::Result<()> {
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

        let headers =
            crate::zoom::download::replay_request_headers(asset, zoom_cookies, &asset.download_url);

        info!("⬇ Downloading to: {}", dest.display());
        let part = dest.with_extension("mp4.part");
        match crate::core::transfer::fetch_resumable(
            &self.media_http,
            &asset.download_url,
            headers,
            &part,
            &dest,
        )
        .await
        {
            Ok(()) => info!(
                url = super::redact_url(&asset.download_url),
                "✓ Downloaded successfully"
            ),
            Err(e) => warn!(
                url = super::redact_url(&asset.download_url),
                error = %e,
                "✗ Download failed"
            ),
        }

        Ok(())
    }

    pub async fn capture_and_download_immediately(
        &self,
        files: Vec<ZoomRecordingFile>,
    ) -> anyhow::Result<()> {
        let base = PathBuf::from(&self.config.download_root)
            .join("Zoom")
            .join(self.course_id.to_string());

        let files_to_download = pending_recordings(&base, files).await?;
        if files_to_download.is_empty() {
            return Ok(());
        }

        let (mut browser, handle) = launch_browser().await?;
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

            let Some((asset, zoom_cookies)) = self
                .capture_replay_asset(&page, file, &mut cookies_captured)
                .await?
            else {
                continue;
            };

            self.download_recording(&base, file, &asset, &zoom_cookies, &mut name_counts)
                .await?;
        }

        browser.close().await?;
        handle.await?;

        info!(
            "\nAll files processed! Downloads saved to: {}",
            base.display()
        );
        Ok(())
    }
}

/// Polls the shared capture state until the scid arrives or 60 s elapse,
/// returning whatever was captured.
async fn wait_for_capture(captured: &Mutex<Captured>) -> (Option<String>, HashMap<String, String>) {
    let mut scid = None;
    let mut captured_headers: HashMap<String, String> = HashMap::new();

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(60) {
        // Check shared state
        {
            let data = lock_captured(captured);
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

    (scid, captured_headers)
}

#[cfg(test)]
mod tests {
    use super::{cdp_headers_to_map, is_replay_asset};
    use chromiumoxide::cdp::browser_protocol::network::Headers;

    #[test]
    fn a_zoom_hosted_mp4_is_a_replay_asset() {
        assert!(is_replay_asset(
            "https://ssrweb.zoom.us/rec/download/abc.mp4?access_token=t"
        ));
    }

    #[test]
    fn a_cloudfront_hosted_mp4_is_a_replay_asset() {
        assert!(is_replay_asset("https://d123.cloudfront.net/abc.mp4"));
    }

    #[test]
    fn a_playlist_is_not_a_replay_asset() {
        assert!(!is_replay_asset(
            "https://ssrweb.zoom.us/rec/download/playlist.m3u8"
        ));
    }

    #[test]
    fn an_mp4_on_an_unrelated_host_is_not_a_replay_asset() {
        assert!(!is_replay_asset("https://example.com/abc.mp4"));
    }

    #[test]
    fn cdp_headers_to_map_keeps_only_string_values() {
        let headers = Headers::new(serde_json::json!({
            "x-xsrf-token": "token-value",
            "accept": "application/json",
            "x-mixed": 3
        }));
        let map = cdp_headers_to_map(&headers);
        assert_eq!(
            map.get("x-xsrf-token").map(String::as_str),
            Some("token-value")
        );
        assert_eq!(
            map.get("accept").map(String::as_str),
            Some("application/json")
        );
        assert!(!map.contains_key("x-mixed"));
    }
}
