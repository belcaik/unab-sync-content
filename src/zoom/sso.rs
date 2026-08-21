//! Institutional single sign-on, driven through a headless browser.
//!
//! Canvas hands off to Microsoft Entra, which may present an account picker, an
//! email prompt, a password prompt and a "stay signed in" interstitial, in
//! varying combinations depending on what the browser profile remembers. Zoom
//! puts its own login page in front of the same Microsoft flow.
//!
//! These are free functions over a page and a set of credentials: none of this
//! touches the recordings database or the course being synced.

use crate::config::Config;
use chromiumoxide::Page;
use std::time::{Duration, Instant};
use tokio::time::sleep;

/// Selectors Microsoft has used for the email field, most specific first.
const EMAIL_SELECTORS: &[&str] = &["input[type='email']", "input[name='loginfmt']"];
/// Selectors Microsoft has used for the password field, most specific first.
const PASSWORD_SELECTORS: &[&str] = &["input[type='password']", "input[name='passwd']"];
/// Selectors for the button that advances a step of the Microsoft flow.
const SUBMIT_SELECTORS: &[&str] = &[
    "input[type='submit']",
    "button[type='submit']",
    // Microsoft's stable id for the primary button on every step of the flow.
    "#idSIButton9",
];

/// The credentials an SSO flow needs.
pub struct SsoCreds {
    pub email: Option<String>,
    pub password: Option<String>,
}

impl SsoCreds {
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            email: cfg.canvas.sso_email.clone(),
            password: cfg.canvas.sso_password.clone(),
        }
    }
}

/// Clicks whichever submit control this step of the flow is using.
async fn click_submit(page: &Page) -> Result<bool, Box<dyn std::error::Error>> {
    for selector in SUBMIT_SELECTORS {
        if let Ok(button) = page.find_element(*selector).await {
            button.click().await?;
            return Ok(true);
        }
    }
    Ok(false)
}

/// Types `value` into the first field matching `selectors`, then submits.
///
/// Returns whether a field was found; a missing field is normal, since the flow
/// skips whichever steps the browser profile already satisfies.
async fn fill_and_submit(
    page: &Page,
    selectors: &[&str],
    value: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    for selector in selectors {
        if let Ok(input) = page.find_element(*selector).await {
            input.click().await?.type_str(value).await?;
            click_submit(page).await?;
            sleep(Duration::from_secs(2)).await;
            return Ok(true);
        }
    }
    Ok(false)
}

pub async fn handle_sso(page: &Page, creds: &SsoCreds) -> Result<(), Box<dyn std::error::Error>> {
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

    handle_ms_account(page, creds).await?;
    Ok(())
}

async fn handle_ms_account(
    page: &Page,
    creds: &SsoCreds,
) -> Result<(), Box<dyn std::error::Error>> {
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

                let normalized_email = creds.email.as_ref().map(|email| email.to_lowercase());

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
    match &creds.email {
        Some(email) => {
            println!("Attempting to enter email...");
            fill_and_submit(page, EMAIL_SELECTORS, email).await?;
        }
        None => println!("Warning: sso_email not set; skipping email entry."),
    }

    match &creds.password {
        Some(password) => {
            println!("Attempting to enter password...");
            fill_and_submit(page, PASSWORD_SELECTORS, password).await?;
        }
        None => println!("Warning: sso_password not set; skipping password entry."),
    }

    // "Stay signed in?" prompt - poll for it since the page may still be loading after password submission.
    // Microsoft shows this as "Stay signed in?" (English) or "¿Mantener la sesión iniciada?" (Spanish).
    let kmsi_start = Instant::now();
    while kmsi_start.elapsed() < Duration::from_secs(15) {
        let url = page.url().await?.unwrap_or_default();

        // If we already left the Microsoft login domain, SSO is done
        if !url.contains("login.microsoftonline.com") {
            println!("SSO complete, redirected to: {}", url);
            break;
        }

        if let Ok(html) = page.content().await {
            let html_lower = html.to_lowercase();
            if html_lower.contains("stay signed in")
                || html_lower.contains("mantener la sesión iniciada")
                || html_lower.contains("mantener la sesion iniciada")
                || html_lower.contains("keep me signed in")
                || html_lower.contains("no volver a mostrar")
                || html_lower.contains("don't show this again")
            {
                println!("Handling 'Stay signed in' prompt...");
                if let Ok(button) = page.find_element("#idSIButton9").await {
                    button.click().await?;
                } else if let Ok(button) = page.find_element("input[type='submit']").await {
                    button.click().await?;
                }
                sleep(Duration::from_secs(3)).await;
                break;
            }
        }

        sleep(Duration::from_millis(500)).await;
    }

    // Wait for final redirects back to Canvas/Zoom
    sleep(Duration::from_secs(5)).await;

    let final_url = page.url().await?.unwrap_or_default();
    println!("Post-SSO URL: {}", final_url);
    Ok(())
}

async fn is_zoom_login_page(page: &Page) -> Result<bool, Box<dyn std::error::Error>> {
    let url = page.url().await?.unwrap_or_default();
    let html = page.content().await?;

    Ok(url.contains("zoom.us/signin")
        || html.contains("zm-login-methods__item")
        || html.contains("Sign in with Microsoft"))
}

pub async fn handle_zoom_play_sso(
    page: &Page,
    creds: &SsoCreds,
) -> Result<(), Box<dyn std::error::Error>> {
    // Step 1: Wait for page to settle after navigation
    sleep(Duration::from_secs(3)).await;

    let url = page.url().await?.unwrap_or_default();

    // Step 2: Check if already authenticated (player loaded)
    if url.contains("zoom.us/rec/play") {
        // Additional check: look for player elements, not login elements
        if let Ok(html) = page.content().await {
            if !html.contains("zm-login-methods__item") && !html.contains("Sign in with Microsoft")
            {
                println!("Zoom player already loaded, no authentication needed");
                return Ok(());
            }
        }
    }

    // Step 3: Detect Zoom login screen
    if !is_zoom_login_page(page).await.unwrap_or(false) {
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
    handle_ms_account(page, creds).await?;
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
