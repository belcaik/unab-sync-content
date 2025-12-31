use crate::config::Config;
use crate::zoom::db::ZoomDb;
use crate::zoom::models::{ZoomCookie, ZoomRecordingFile};
use base64::prelude::*;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::network::EventRequestWillBeSent;
use chromiumoxide::Page;
use futures::StreamExt;
use regex::Regex;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::time::sleep;
use url::Url;

pub struct ZoomHeadless<'a> {
    config: &'a Config,
    db: &'a ZoomDb,
    course_id: u64,
}

impl<'a> ZoomHeadless<'a> {
    pub fn new(config: &'a Config, db: &'a ZoomDb, course_id: u64) -> Self {
        Self {
            config,
            db,
            course_id,
        }
    }

    pub async fn authenticate_and_capture(&self) -> Result<(), Box<dyn std::error::Error>> {
        let (mut browser, mut handler) = Browser::launch(
            BrowserConfig::builder()
            // .with_head()
                // Running in full headless mode (no GUI)
                // Let's try headless first, but maybe provide an option?
                // The user said "headless browser", so let's stick to headless unless debugging.
                // Actually, for SSO, sometimes headful is required if there are captchas or complex interactions,
                // but standard Azure AD usually works in headless if user agent is set correctly.
                // Let's use the config user agent.
                .arg("--no-sandbox")
                .arg("--disable-gpu")
                .arg("--disable-dev-shm-usage")
                .build()?,
        )
        .await?;

        let handle = tokio::spawn(async move {
            while let Some(h) = handler.next().await {
                if let Err(e) = h {
                    eprintln!("Browser handler error: {:?}", e);
                    break;
                }
            }
            println!("Browser handler loop exited.");
        });

        let page = browser.new_page("about:blank").await?;
        page.set_user_agent(&self.config.zoom.user_agent).await?;

        // Enable network events
        // Check if we already have scid in DB
        if let Ok(Some(stored_scid)) = self.db.get_scid(self.course_id) {
            println!("Found existing lti_scid in DB: {}", stored_scid);
            // We still proceed to refresh cookies and verify scid
        }

        // Shared state for captured data: (scid, api_headers)
        let captured_data = Arc::new(Mutex::new((
            None::<String>,
            None::<HashMap<String, String>>,
        )));
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
            .await
            .unwrap();

        let page_clone = page.clone();
        let captured_data_clone = captured_data.clone(); // This is the one used by the new task

        let mut request_events = page
            .event_listener::<EventRequestWillBeSent>()
            .await
            .unwrap();

        // Pre-compile regexes
        let re_scid = Regex::new(r#"scid\s*:\s*['"]([^'"]+)['"]"#).unwrap();
        let re_ajax = Regex::new(r#"(?s)ajaxHeaders\s*:\s*\[(.*?)\]"#).unwrap();
        let re_kv = Regex::new(r#"\{\s*key\s*:\s*['"]([^'"]+)['"]\s*,\s*value\s*:\s*['"]([^'"]+)['"]\s*\}"#).unwrap();
        let re_xsrf = Regex::new(r#"(?i)['"]?x-xsrf-token['"]?\s*:\s*['"]([^'"]+)['"]"#).unwrap();

        // Spawn Fetch interception task
        tokio::spawn(async move {
            while let Some(event) = request_paused_events.next().await {
                let req_id = event.request_id.clone();
                // Always continue the request eventually
                let page_inner = page_clone.clone();
                let req_id_inner = req_id.clone();

                // We only care if we have a response status code (response stage)
                if event.response_status_code.is_some() {
                    // let url = event.request.url.clone();
                    // println!("Fetch Interception: Response Paused for {}", url);

                    // Get body
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

                            // Extract window.appConf
                            if let Some(idx) = content.find("window.appConf") {
                                // println!("Found window.appConf in intercepted body!");
                                // Extract a chunk to parse
                                // Extract a chunk to parse
                                let start = idx;
                                // Capture a larger chunk to ensure we get ajaxHeaders even if they are far down
                                let end = (idx + 20000).min(content.len());
                                let chunk = &content[start..end];

                                // Debug log to verify we are seeing the right content
                                println!(
                                    "DEBUG appConf chunk (first 500 chars):\n{}",
                                    &chunk[..chunk.len().min(500)]
                                );

                                // Regex for scid
                                // scid: "..."
                                // scid: "..."
                                if let Some(caps) = re_scid.captures(chunk) {
                                    if let Some(val) = caps.get(1) {
                                        let s = val.as_str().to_string();
                                        println!("Captured lti_scid from Fetch: {}", s);
                                        let mut data = captured_data_clone.lock().unwrap();
                                        data.0 = Some(s);
                                    }
                                }

                                // Regex for ajaxHeaders
                                // User says it looks like: ajaxHeaders: [{key: "...", value: "..."}, ...]
                                // We will capture the content inside ajaxHeaders: [...]
                                // Regex for ajaxHeaders
                                // User says it looks like: ajaxHeaders: [{key: "...", value: "..."}, ...]
                                // We will capture the content inside ajaxHeaders: [...]
                                // Use (?s) to allow . to match newlines
                                if let Some(caps) = re_ajax.captures(chunk) {
                                    if let Some(ajax_body) = caps.get(1) {
                                        let ajax_body_str = ajax_body.as_str();
                                        // Now extract key-value pairs
                                        // Regex for {key: "...", value: "..."}
                                        // We need to be careful about quotes and spacing
                                        // We need to be careful about quotes and spacing

                                        let mut headers = HashMap::new();
                                        for cap in re_kv.captures_iter(ajax_body_str) {
                                            if let (Some(k), Some(v)) = (cap.get(1), cap.get(2)) {
                                                let key = k.as_str().to_string();
                                                let val = v.as_str().to_string();
                                                // Filter for interesting headers
                                                let key_lower = key.to_lowercase();
                                                if key_lower.starts_with("x-zm-")
                                                    || key_lower == "x-xsrf-token"
                                                {
                                                    headers.insert(key, val);
                                                }
                                            }
                                        }

                                        if !headers.is_empty() {
                                            println!(
                                                "Captured {} ajaxHeaders from Fetch (array format)",
                                                headers.len()
                                            );
                                            let mut data = captured_data_clone.lock().unwrap();
                                            if data.1.is_none() {
                                                data.1 = Some(headers);
                                            } else {
                                                data.1.as_mut().unwrap().extend(headers);
                                            }
                                        }
                                    }
                                } else {
                                    // Fallback to previous object format logic if array format fails
                                    // ... (previous logic or just log warning)
                                    // Actually, let's just try the previous logic as a backup or assume array format is correct.
                                    // The user was quite specific.
                                    // But I'll keep the explicit XSRF capture just in case.
                                }

                                // Explicitly capture x-xsrf-token if missed (sometimes it's in a different format or location)
                                if let Some(caps) = re_xsrf.captures(chunk) {
                                    if let Some(val) = caps.get(1) {
                                        let mut data = captured_data_clone.lock().unwrap();
                                        if data.1.is_none() {
                                            data.1 = Some(HashMap::new());
                                        }
                                        let headers = data.1.as_mut().unwrap();
                                        if !headers.contains_key("x-xsrf-token")
                                            && !headers.contains_key("X-XSRF-TOKEN")
                                        {
                                            headers.insert(
                                                "x-xsrf-token".to_string(),
                                                val.as_str().to_string(),
                                            );
                                            println!(
                                                "Explicitly captured x-xsrf-token: {}",
                                                val.as_str()
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            println!("Failed to get body in Fetch interception: {:?}", e);
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
                let mut data = captured_data_clone_for_fetch.lock().unwrap();

                if data.0.is_none() && url.contains("lti_scid=") {
                    if let Ok(parsed) = Url::parse(&url) {
                        for (k, v) in parsed.query_pairs() {
                            if k == "lti_scid" {
                                println!("Captured lti_scid from URL: {}", v);
                                data.0 = Some(v.to_string());
                            }
                        }
                    }
                }

                // Capture headers for Zoom API calls
                if data.1.is_none() && url.contains("/api/v1/lti/rich/recording") {
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
                    println!("Captured Zoom API headers");
                    data.1 = Some(headers);
                }
            }
        });

        let target_url = format!(
            "{}/courses/{}/external_tools/{}",
            self.config.canvas.base_url, self.course_id, self.config.zoom.external_tool_id
        );

        println!("Navigating to: {}", target_url);
        page.goto(&target_url).await?;

        // Handle SSO
        self.handle_sso(&page).await?;

        // Wait for Zoom LTI to load and capture data
        println!("Waiting for Zoom LTI to load...");

        let mut scid = None;
        let mut captured_headers: HashMap<String, String> = HashMap::new();

        // Wait up to 60 seconds for the LTI load
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(60) {
            // Check shared state
            {
                let data = captured_data.lock().unwrap();
                if let Some(s) = &data.0 {
                    scid = Some(s.clone());
                }
                if let Some(h) = &data.1 {
                    captured_headers = h.clone();
                }
                // The API headers capture logic was removed from the old network listener.
                // If API headers are still needed, a separate Fetch interception or network listener
                // for specific API calls would be required. For now, it will remain empty.
                // The user's instruction only provided scid capture via Fetch.
                // For now, we'll keep the `captured_headers` variable but it won't be populated by this new logic.
                // If the user wants to capture API headers via Fetch, they need to provide that pattern.
                // For now, we'll assume the `captured_headers` map will remain empty unless explicitly added.
                // The original code had:
                // if data.1.is_none() && url.contains("/api/v1/lti/rich/recording") { ... data.1 = Some(headers); }
                // This part is not covered by the new Fetch interception.
                // To keep the functionality, we would need to add another Fetch pattern for API calls.
                // For now, following the instruction, this part of `captured_headers` will not be filled.
            }

            if scid.is_some() {
                break;
            }

            sleep(Duration::from_millis(500)).await;
        }

        // Get cookies
        let current_cookies = page.get_cookies().await?;
        let mut cookies = Vec::new();
        for c in current_cookies {
            if c.domain.contains("zoom.us") {
                cookies.push(ZoomCookie {
                    domain: c.domain,
                    name: c.name,
                    value: c.value,
                    path: c.path,
                    expires: Some(c.expires as i64),
                    secure: c.secure,
                    http_only: c.http_only,
                });
            }
        }

        if let Some(s) = scid {
            self.db.save_scid(self.course_id, &s)?;
            println!("Saved lti_scid to DB: {}", s);
        } else {
            return Err("Failed to capture lti_scid".into());
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
            println!("Saving headers: {:?}", keys);

            self.db.save_request_headers(
                self.course_id,
                "/api/v1/lti/rich/recording",
                &header_list,
            )?;
            println!("Saved {} request headers to DB", captured_headers.len());
        } else {
            println!("Warning: No request headers captured");
        }

        if !cookies.is_empty() {
            self.db.replace_cookies(&cookies)?;
        } else {
            return Err("Failed to capture Zoom cookies".into());
        }

        // Verification log
        let scid_after = self.db.get_scid(self.course_id)?;
        let cookies_after = self.db.load_cookies()?;
        let headers_after = self.db.get_all_request_headers(self.course_id)?;

        println!(
            "AFTER HEADLESS SAVE -> scid={:?}, cookies={}, headers={}",
            scid_after,
            cookies_after.len(),
            headers_after.len()
        );

        browser.close().await?;
        handle.await?;

        Ok(())
    }

    async fn handle_sso(&self, page: &Page) -> Result<(), Box<dyn std::error::Error>> {
        // Simple heuristic for Microsoft SSO
        // 1. Check for email input
        // 2. Check for password input
        // 3. Check for "Stay signed in"

        println!("Checking for SSO login...");

        // Wait a bit for redirects
        sleep(Duration::from_secs(5)).await;

        let mut url = page.url().await?.unwrap_or_default();

        // Handle Canvas Login Page (Pre-SSO)
        if url.contains("/login/canvas") {
            println!("Detected Canvas login page. Attempting to initiate SSO...");
            // Find the "ESTUDIANTES Y DOCENTES" button
            let buttons = page.find_elements(".ic-Login__body button").await?;
            let mut clicked = false;
            for button in buttons {
                if let Ok(Some(text)) = button.inner_text().await {
                    if text.to_uppercase().contains("ESTUDIANTES Y DOCENTES") {
                        println!("Found SSO initiation button. Clicking...");
                        button.click().await?;
                        clicked = true;
                        sleep(Duration::from_secs(5)).await; // Wait for redirect
                        url = page.url().await?.unwrap_or_default(); // Update URL
                        break;
                    }
                }
            }
            if !clicked {
                println!(
                    "Warning: Could not find 'ESTUDIANTES Y DOCENTES' button on Canvas login page."
                );
            }
        }

        if !url.contains("login.microsoftonline.com") {
            println!(
                "Not on Microsoft SSO page (URL: {}), assuming already logged in or not required.",
                url
            );
            return Ok(());
        }

        self.handle_microsoft_sso(page).await?;
        Ok(())
    }

    async fn handle_microsoft_sso(&self, page: &Page) -> Result<(), Box<dyn std::error::Error>> {
        println!("Handling Microsoft SSO...");
        self.handle_ms_account(page).await
    }

    async fn handle_ms_account(&self, page: &Page) -> Result<(), Box<dyn std::error::Error>> {
        // First, check for remembered account tiles (account picker)
        sleep(Duration::from_secs(2)).await;

        let email_input_present = page.find_element("input[type='email']").await.is_ok()
            || page.find_element("input[name='loginfmt']").await.is_ok();

        // Look for account tiles - the clickable element is .table[role="button"] inside .tile-container.
        // Only attempt this flow if we do not already see the email input.
        if !email_input_present {
            if let Ok(tiles) = page.find_elements(".table[role='button']").await {
                if !tiles.is_empty() {
                    let mut matching_tile_idx = None;
                    let mut first_email_tile_idx = None;
                    let mut use_other_tile_idx = None;

                    let normalized_email = self
                        .config
                        .canvas
                        .sso_email
                        .as_ref()
                        .map(|email| email.to_lowercase());

                    for (idx, tile) in tiles.iter().enumerate() {
                        let text = tile.inner_text().await?.unwrap_or_default();
                        let lowered = text.to_lowercase();

                        if lowered.contains("sign-in options")
                            || lowered.contains("other ways to sign in")
                            || lowered.contains("otros metodos")
                            || lowered.contains("otras formas")
                        {
                            continue;
                        }

                        if lowered.contains("use another account")
                            || lowered.contains("usar otra cuenta")
                            || lowered.contains("otra cuenta")
                        {
                            if use_other_tile_idx.is_none() {
                                use_other_tile_idx = Some(idx);
                            }
                            continue;
                        }

                        if lowered.contains('@') {
                            if first_email_tile_idx.is_none() {
                                first_email_tile_idx = Some(idx);
                            }
                            if let Some(email) = &normalized_email {
                                if lowered.contains(email) {
                                    matching_tile_idx = Some(idx);
                                    break;
                                }
                            }
                        }
                    }

                    let selected_idx = match (matching_tile_idx, normalized_email.as_ref()) {
                        (Some(idx), _) => Some(idx),
                        (None, Some(_)) => use_other_tile_idx.or(first_email_tile_idx),
                        (None, None) => first_email_tile_idx,
                    };

                    if let Some(idx) = selected_idx {
                        println!("Found remembered account tile, clicking...");
                        if let Err(e) = tiles[idx].click().await {
                            println!("Warning: Failed to click account tile: {:?}", e);
                        } else {
                            sleep(Duration::from_secs(3)).await;
                        }
                    }
                }
            }
        }

        // Fallback: manual credential entry
        if let Some(email) = &self.config.canvas.sso_email {
            println!("Attempting to enter email...");
            // Selector for email input. Usually 'input[type="email"]' or 'input[name="loginfmt"]'
            if let Ok(input) = page.find_element("input[type='email']").await {
                input.click().await?.type_str(email).await?;
                if let Ok(button) = page.find_element("input[type='submit']").await {
                    button.click().await?;
                } else if let Ok(button) = page.find_element("button[type='submit']").await {
                    button.click().await?;
                } else if let Ok(button) = page.find_element("#idSIButton9").await {
                    button.click().await?;
                }
                sleep(Duration::from_secs(2)).await;
            } else if let Ok(input) = page.find_element("input[name='loginfmt']").await {
                input.click().await?.type_str(email).await?;
                if let Ok(button) = page.find_element("input[type='submit']").await {
                    button.click().await?;
                } else if let Ok(button) = page.find_element("button[type='submit']").await {
                    button.click().await?;
                } else if let Ok(button) = page.find_element("#idSIButton9").await {
                    button.click().await?;
                }
                sleep(Duration::from_secs(2)).await;
            }
        } else {
            println!("Warning: sso_email not set; skipping email entry.");
        }

        if let Some(password) = &self.config.canvas.sso_password {
            println!("Attempting to enter password...");
            // Selector for password input. 'input[type="password"]' or 'input[name="passwd"]'
            if let Ok(input) = page.find_element("input[type='password']").await {
                input.click().await?.type_str(password).await?;
                if let Ok(button) = page.find_element("input[type='submit']").await {
                    button.click().await?;
                } else if let Ok(button) = page.find_element("button[type='submit']").await {
                    button.click().await?;
                } else if let Ok(button) = page.find_element("#idSIButton9").await {
                    button.click().await?;
                }
                sleep(Duration::from_secs(2)).await;
            } else if let Ok(input) = page.find_element("input[name='passwd']").await {
                input.click().await?.type_str(password).await?;
                if let Ok(button) = page.find_element("input[type='submit']").await {
                    button.click().await?;
                } else if let Ok(button) = page.find_element("button[type='submit']").await {
                    button.click().await?;
                } else if let Ok(button) = page.find_element("#idSIButton9").await {
                    button.click().await?;
                }
                sleep(Duration::from_secs(2)).await;
            }
        } else {
            println!("Warning: sso_password not set; skipping password entry.");
        }

        // "Stay signed in?" - usually has a "Yes" button (input[type="submit"] or button)
        if page.content().await?.contains("Stay signed in?") {
            println!("Handling 'Stay signed in' prompt...");
            // The "Yes" button often has id "idSIButton9"
            if page.find_element("#idSIButton9").await.is_ok() {
                page.find_element("#idSIButton9").await?.click().await?;
            }
        }

        sleep(Duration::from_secs(5)).await;
        Ok(())
    }

    async fn is_zoom_login_page(&self, page: &Page) -> Result<bool, Box<dyn std::error::Error>> {
        let url = page.url().await?.unwrap_or_default();
        let html = page.content().await?;

        Ok(url.contains("zoom.us/signin")
            || html.contains("zm-login-methods__item")
            || html.contains("Sign in with Microsoft"))
    }

    async fn handle_zoom_play_sso(&self, page: &Page) -> Result<(), Box<dyn std::error::Error>> {
        // Step 1: Wait for page to settle after navigation
        sleep(Duration::from_secs(3)).await;

        let url = page.url().await?.unwrap_or_default();

        // Step 2: Check if already authenticated (player loaded)
        if url.contains("zoom.us/rec/play") {
            // Additional check: look for player elements, not login elements
            if let Ok(html) = page.content().await {
                if !html.contains("zm-login-methods__item")
                    && !html.contains("Sign in with Microsoft")
                {
                    println!("Zoom player already loaded, no authentication needed");
                    return Ok(());
                }
            }
        }

        // Step 3: Detect Zoom login screen
        if !self.is_zoom_login_page(page).await.unwrap_or(false) {
            println!("No Zoom login detected, assuming already authenticated");
            return Ok(());
        }

        println!("Zoom play_url: detected login screen, initiating Microsoft SSO...");

        // Step 4: Click "Sign in with Microsoft" on Zoom
        let start = Instant::now();
        let mut clicked = false;

        while start.elapsed() < Duration::from_secs(10) {
            // Try multiple selectors
            if let Ok(el) = page
                .find_element("a[aria-label='Sign in with Microsoft']")
                .await
            {
                println!("Clicked 'Sign in with Microsoft' button (aria-label match)");
                el.click().await?;
                clicked = true;
                break;
            }

            if let Ok(el) = page.find_element("a[aria-label*='Microsoft']").await {
                println!("Clicked 'Sign in with Microsoft' button (aria-label partial match)");
                el.click().await?;
                clicked = true;
                break;
            }

            // Fallback: search by text in login methods
            if let Ok(methods) = page.find_elements(".zm-login-methods__item").await {
                for method in methods {
                    if let Ok(Some(text)) = method.inner_text().await {
                        if text.to_lowercase().contains("microsoft") {
                            println!("Clicked 'Microsoft' login method (text match)");
                            method.click().await?;
                            clicked = true;
                            break;
                        }
                    }
                }
            }

            if clicked {
                break;
            }

            sleep(Duration::from_millis(500)).await;
        }

        if !clicked {
            return Err("Could not find 'Sign in with Microsoft' button on Zoom login page".into());
        }

        // Step 5: Wait for redirect to Microsoft
        println!("Clicked Microsoft sign-in button, waiting for redirect...");
        sleep(Duration::from_secs(3)).await;

        let start = Instant::now();
        let mut on_microsoft = false;
        while start.elapsed() < Duration::from_secs(30) {
            let current_url = page.url().await?.unwrap_or_default();
            if current_url.contains("login.microsoftonline.com") {
                println!("Redirected to Microsoft login: {}", current_url);
                on_microsoft = true;
                break;
            }
            sleep(Duration::from_millis(500)).await;
        }

        if !on_microsoft {
            return Err("Timeout waiting for redirect to Microsoft login".into());
        }

        // Step 6: Handle Microsoft authentication (account picker or credentials)
        self.handle_ms_account(page).await?;
        println!("Microsoft authentication complete, waiting for Zoom player...");

        // Step 7: Wait for return to Zoom
        let start = Instant::now();
        let mut back_on_zoom = false;
        while start.elapsed() < Duration::from_secs(30) {
            let current_url = page.url().await?.unwrap_or_default();
            if current_url.contains("zoom.us") && !current_url.contains("signin") {
                println!("Back on Zoom page: {}", current_url);
                back_on_zoom = true;
                break;
            }
            sleep(Duration::from_millis(1000)).await;
        }

        if !back_on_zoom {
            return Err("Timeout waiting to return to Zoom after Microsoft authentication".into());
        }

        // Give the player time to initialize
        sleep(Duration::from_secs(2)).await;
        println!("Zoom player should now be loaded");

        Ok(())
    }

    pub async fn capture_and_download_immediately(
        &self,
        cfg: &crate::config::Config,
        _db: &ZoomDb,
        course_id: u64,
        files: Vec<ZoomRecordingFile>,
        _concurrency: usize, // Not used since we process one-by-one
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::ffmpeg::{download_via_ffmpeg, ensure_ffmpeg_available, FfmpegError};
        use crate::fsutil::sanitize_filename_preserve_ext;
        use crate::zoom::models::ReplayHeader;
        use std::collections::HashMap;
        use std::path::PathBuf;

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
                    println!("⏩ Skipping (already exists): {}", filename);
                    false
                } else {
                    true
                }
            })
            .collect();

        if files_to_download.is_empty() {
            println!("All recordings already downloaded!");
            return Ok(());
        }

        println!(
            "Found {} recordings, {} new to download",
            files_to_download.len() + existing_files.len(),
            files_to_download.len()
        );

        let (mut browser, mut handler) = Browser::launch(
            BrowserConfig::builder()
                // Running in full headless mode (no GUI)
                .arg("--no-sandbox")
                .arg("--disable-gpu")
                .build()?,
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
        println!("Starting capture and download (tokens expire quickly, processing one by one)...");
        println!(
            "Processing {} recordings (capture → download → next)...\n",
            files_to_download.len()
        );

        let mut cookies_captured = false;

        for (idx, file) in files_to_download.iter().enumerate() {
            println!(
                "\n[{}/{}] Processing: {}",
                idx + 1,
                files_to_download.len(),
                file.play_url
            );

            // STEP 1: Navigate to play URL
            let mut events = page
                .event_listener::<EventRequestWillBeSent>()
                .await
                .unwrap();
            page.goto(&file.play_url).await?;

            // STEP 2: Authenticate if needed
            if let Err(e) = self.handle_zoom_play_sso(&page).await {
                println!("Warning: SSO failed for {}: {:?}", file.play_url, e);
                println!("Skipping this file...");
                continue;
            }

            // STEP 3: Capture fresh cookies (first file only) and load for downloads
            let zoom_cookies = if !cookies_captured {
                println!("Capturing fresh cookies after SSO...");
                let current_cookies = page.get_cookies().await?;
                let mut fresh_cookies = Vec::new();
                for c in current_cookies {
                    if c.domain.contains("zoom.us") || c.domain.contains("cloudfront.net") {
                        fresh_cookies.push(crate::zoom::models::ZoomCookie {
                            domain: c.domain,
                            name: c.name,
                            value: c.value,
                            path: c.path,
                            expires: Some(c.expires as i64),
                            secure: c.secure,
                            http_only: c.http_only,
                        });
                    }
                }
                if !fresh_cookies.is_empty() {
                    self.db.replace_cookies(&fresh_cookies)?;
                    println!("Saved {} fresh cookies for downloads", fresh_cookies.len());
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

                                println!("✓ Captured download URL: {}", url);
                                println!("  Captured {} headers from MP4 request:", headers.len());
                                for (k, v) in &headers {
                                    // Log all headers (truncate long values like cookies)
                                    let display_val = if v.len() > 100 {
                                        format!("{}...", &v[..100])
                                    } else {
                                        v.clone()
                                    };
                                    println!("    {}: {}", k, display_val);
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
                    println!("✗ Could not capture download URL, skipping...");
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

            println!("⬇ Downloading to: {}", dest.display());
            match download_via_ffmpeg(&cfg.zoom.ffmpeg_path, &headers, &asset.download_url, &dest)
                .await
            {
                Ok(()) => println!("✓ Downloaded successfully!"),
                Err(FfmpegError::Process { .. }) => {
                    println!("✗ ffmpeg failed, trying HTTP fallback...");
                    if let Err(e) =
                        crate::zoom::download::http_download(&headers, &asset.download_url, &dest)
                            .await
                    {
                        println!("✗ HTTP download also failed: {:?}", e);
                    } else {
                        println!("✓ Downloaded via HTTP!");
                    }
                }
                Err(e) => {
                    println!("✗ Download error: {:?}", e);
                }
            }
        }

        browser.close().await?;
        handle.await?;

        println!(
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
) -> Result<std::collections::HashSet<String>, Box<dyn std::error::Error>> {
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
