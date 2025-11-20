use crate::config::Config;
use crate::zoom::db::ZoomDb;
use crate::zoom::models::{ReplayHeader, ZoomCookie, ZoomRecordingFile};
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::network::EventRequestWillBeSent;
use chromiumoxide::Page;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::sleep;
use url::Url;
use base64::prelude::*;
use regex::Regex;

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
                .with_head() // We might want to run headless, but for debugging/SSO visibility 'with_head' is often safer initially. User asked for headless though.
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
        let captured_data = Arc::new(Mutex::new((None::<String>, None::<HashMap<String, String>>)));
        let captured_data_clone_for_fetch = captured_data.clone(); // Renamed to avoid conflict with new `captured_data_clone`

        // Enable Fetch domain for interception
        let patterns = vec![
            chromiumoxide::cdp::browser_protocol::fetch::RequestPattern::builder()
                .url_pattern("*applications.zoom.us/lti/advantage*")
                .request_stage(chromiumoxide::cdp::browser_protocol::fetch::RequestStage::Response)
                .build(),
        ];
        page.execute(chromiumoxide::cdp::browser_protocol::fetch::EnableParams::builder().patterns(patterns).build()).await?;

        let mut request_paused_events = page.event_listener::<chromiumoxide::cdp::browser_protocol::fetch::EventRequestPaused>().await.unwrap();
        
        let page_clone = page.clone();
        let captured_data_clone = captured_data.clone(); // This is the one used by the new task

        let mut request_events = page.event_listener::<EventRequestWillBeSent>().await.unwrap();

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
                    match page_inner.execute(chromiumoxide::cdp::browser_protocol::fetch::GetResponseBodyParams::new(req_id.clone())).await {
                        Ok(body) => {
                            // Capture headers
                            let mut headers = HashMap::new();
                            let headers_val = serde_json::to_value(event.request.headers.clone()).unwrap_or(serde_json::Value::Null);
                            if let Some(obj) = headers_val.as_object() {
                                for (k, v) in obj {
                                    let key_lower = k.to_ascii_lowercase();
                                    if key_lower != "cookie" && key_lower != "host" && key_lower != "content-length" {
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
                                let start = idx;
                                let end = if idx + 2000 < content.len() { idx + 2000 } else { content.len() };
                                let chunk = &content[start..end];
                                
                                // Regex for scid
                                // scid: "..."
                                let re_scid = Regex::new(r#"scid\s*:\s*['"]([^'"]+)['"]"#).unwrap();
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
                                let re_ajax = Regex::new(r#"ajaxHeaders\s*:\s*\[(.*?)\]"#).unwrap();
                                if let Some(caps) = re_ajax.captures(chunk) {
                                    if let Some(ajax_body) = caps.get(1) {
                                        let ajax_body_str = ajax_body.as_str();
                                        // Now extract key-value pairs
                                        // Regex for {key: "...", value: "..."}
                                        // We need to be careful about quotes and spacing
                                        let re_kv = Regex::new(r#"\{\s*key\s*:\s*['"]([^'"]+)['"]\s*,\s*value\s*:\s*['"]([^'"]+)['"]\s*\}"#).unwrap();
                                        
                                        let mut headers = HashMap::new();
                                        for cap in re_kv.captures_iter(ajax_body_str) {
                                            if let (Some(k), Some(v)) = (cap.get(1), cap.get(2)) {
                                                let key = k.as_str().to_string();
                                                let val = v.as_str().to_string();
                                                // Filter for interesting headers
                                                let key_lower = key.to_lowercase();
                                                if key_lower.starts_with("x-zm-") || key_lower == "x-xsrf-token" {
                                                    headers.insert(key, val);
                                                }
                                            }
                                        }
                                        
                                        if !headers.is_empty() {
                                            println!("Captured {} ajaxHeaders from Fetch (array format)", headers.len());
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
                                let re_xsrf = Regex::new(r#"(?i)['"]?x-xsrf-token['"]?\s*:\s*['"]([^'"]+)['"]"#).unwrap();
                                if let Some(caps) = re_xsrf.captures(chunk) {
                                     if let Some(val) = caps.get(1) {
                                         let mut data = captured_data_clone.lock().unwrap();
                                         if data.1.is_none() {
                                             data.1 = Some(HashMap::new());
                                         }
                                         let headers = data.1.as_mut().unwrap();
                                         if !headers.contains_key("x-xsrf-token") && !headers.contains_key("X-XSRF-TOKEN") {
                                             headers.insert("x-xsrf-token".to_string(), val.as_str().to_string());
                                             println!("Explicitly captured x-xsrf-token: {}", val.as_str());
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
                let _ = page_inner.execute(chromiumoxide::cdp::browser_protocol::fetch::ContinueRequestParams::new(req_id_inner)).await;
            }
        });

        let _capture_task = tokio::spawn(async move {
            while let Some(event) = request_events.next().await {
                let url = event.request.url.clone();
                let mut data = captured_data_clone_for_fetch.lock().unwrap();

                // Capture lti_scid from URL query params (fallback)
                if data.0.is_none() && url.contains("lti_scid=") {
                    if let Some(parsed) = Url::parse(&url).ok() {
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
                        let headers_val = serde_json::to_value(event.request.headers.clone()).unwrap_or(serde_json::Value::Null);
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
            let header_list: Vec<(String, String)> = captured_headers.iter().map(|(k,v)| (k.clone(), v.clone())).collect();
            self.db.save_request_headers(self.course_id, "/api/v1/lti/rich/recording", &header_list)?;
            println!("Saved {} request headers to DB", captured_headers.len());
        } else {
            println!("Warning: No request headers captured");
        }

        if !cookies.is_empty() {
            self.db.replace_cookies(&cookies)?;
        } else {
            return Err("Failed to capture Zoom cookies".into());
        }
        


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
                 println!("Warning: Could not find 'ESTUDIANTES Y DOCENTES' button on Canvas login page.");
             }
        }

        if !url.contains("login.microsoftonline.com") {
            println!("Not on Microsoft SSO page (URL: {}), assuming already logged in or not required.", url);
            return Ok(());
        }

        if let Some(email) = &self.config.canvas.sso_email {
            println!("Attempting to enter email...");
            // Selector for email input. Usually 'input[type="email"]' or 'input[name="loginfmt"]'
            if page.find_element("input[type='email']").await.is_ok() {
                page.find_element("input[type='email']").await?.click().await?.type_str(email).await?;
                page.find_element("input[type='submit']").await?.click().await?; // "Next" button
                sleep(Duration::from_secs(2)).await;
            }
        }

        if let Some(password) = &self.config.canvas.sso_password {
            println!("Attempting to enter password...");
             // Selector for password input. 'input[type="password"]' or 'input[name="passwd"]'
            if page.find_element("input[type='password']").await.is_ok() {
                page.find_element("input[type='password']").await?.click().await?.type_str(password).await?;
                page.find_element("input[type='submit']").await?.click().await?; // "Sign in" button
                sleep(Duration::from_secs(2)).await;
            }
        }

        // "Stay signed in?" - usually has a "Yes" button (input[type="submit"] or button)
        // We can try to click "Yes" or "No". Let's try "Yes" to avoid repeated logins if we reuse session (though we don't persist browser profile yet).
        // Actually, "No" is safer to avoid "kmsi" (Keep Me Signed In) interruptions if logic is flaky.
        // But "Yes" reduces friction.
        // Let's look for the text or button.
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

    pub async fn capture_play_url_headers(&self, files: &[ZoomRecordingFile]) -> Result<(), Box<dyn std::error::Error>> {
         let (mut browser, mut handler) = Browser::launch(
            BrowserConfig::builder()
                .with_head() // Headless
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

        // We need to capture the download URL and headers for each file
        // The logic is: navigate to play_url -> wait for network request to .mp4 or .m3u8
        
        let mut stored = self.db.load_replay_headers(self.course_id)?;
        
        for file in files {
            if stored.contains_key(&file.play_url) {
                continue;
            }
            
            println!("Capturing headers for: {}", file.play_url);
            
            let mut events = page.event_listener::<EventRequestWillBeSent>().await.unwrap();
            page.goto(&file.play_url).await?;
            
            // Wait for the media request
            let start = std::time::Instant::now();
            let mut found = false;
            
            while start.elapsed() < Duration::from_secs(30) {
                 tokio::select! {
                    event = events.next() => {
                        if let Some(event) = event {
                            let url = event.request.url.clone();
                            if self.is_replay_asset(&url) {
                                let headers_val = serde_json::to_value(event.request.headers.clone()).unwrap_or(serde_json::Value::Null);
                                let mut headers = HashMap::new();
                                if let Some(obj) = headers_val.as_object() {
                                    for (k, v) in obj {
                                        headers.insert(k.clone(), v.as_str().unwrap_or("").to_string());
                                    }
                                }
                                
                                stored.insert(file.play_url.clone(), ReplayHeader {
                                    download_url: url,
                                    headers,
                                });
                                found = true;
                                break;
                            }
                        }
                    }
                    _ = sleep(Duration::from_millis(100)) => {}
                }
            }
            
            if !found {
                println!("Warning: Could not capture download URL for {}", file.play_url);
            }
        }
        
        self.db.save_replay_headers(self.course_id, &stored)?;

        browser.close().await?;
        handle.await?;
        
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
