use crate::canvas::{CanvasClient, Module};
use crate::config::{load_config_from_path, ConfigPaths};
use crate::http::build_http_client;
use regex::Regex;
use tracing::info;

pub async fn run_discovery(
    filter_course_id: Option<u64>,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let paths = ConfigPaths::default()?;
    let mut cfg = load_config_from_path(&paths.config_file)
        .await
        .unwrap_or_default();
    cfg.expand_paths();

    let canvas = CanvasClient::from_config().await?;
    let _http = build_http_client(&cfg);
    let mut courses = canvas.list_courses().await?;

    if let Some(cid) = filter_course_id {
        courses.retain(|c| c.id == cid);
        if courses.is_empty() {
            println!("No active course with id {} found.", cid);
            return Ok(());
        }
    }

    let mut total = 0usize;
    for course in courses {
        info!(course_id = course.id, name = %course.name, "scan recordings");
        let modules: Vec<Module> = canvas
            .list_modules_with_items(course.id)
            .await
            .unwrap_or_default();
        for module in modules {
            for item in module.items {
                if let Some(page_url) = item.page_url.as_deref() {
                    if let Ok(page) = canvas.get_page(course.id, page_url).await {
                        let html = page.body.unwrap_or_default();
                        for url in extract_zoom_links(&html) {
                            total += 1;
                            println!(
                                "{}[course:{}] {:<40} | module:{} | page:{} | {}",
                                if dry_run { "DRY-RUN " } else { "" },
                                course.id,
                                course.name,
                                module.id,
                                page_url,
                                url
                            );
                        }
                    }
                }

                if let Some(u) = item.external_url.as_deref().or(item.html_url.as_deref()) {
                    for url in extract_zoom_links(u) {
                        total += 1;
                        println!(
                            "{}[course:{}] {:<40} | module:{} | item:{} | {}",
                            if dry_run { "DRY-RUN " } else { "" },
                            course.id,
                            course.name,
                            module.id,
                            item.id,
                            url
                        );
                    }
                }
            }
        }

        let assignments = canvas.list_assignments(course.id).await.unwrap_or_default();
        for assignment in assignments {
            if let Some(desc) = assignment.description.as_deref() {
                for url in extract_zoom_links(desc) {
                    total += 1;
                    println!(
                        "{}[course:{}] {:<40} | assignment:{} | {}",
                        if dry_run { "DRY-RUN " } else { "" },
                        course.id,
                        course.name,
                        assignment.id,
                        url
                    );
                }
            }
        }
    }

    println!(
        "{}Discovered {} Zoom link(s).",
        if dry_run { "DRY-RUN: " } else { "" },
        total
    );
    Ok(())
}

fn extract_zoom_links(input: &str) -> Vec<String> {
    static PATTERN: &str = r#"https?://[A-Za-z0-9-]+\.zoom\.(us|com\.cn)/[A-Za-z0-9_/\-?&=%#\.]+"#;
    let regex = Regex::new(PATTERN).expect("valid regex");
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for cap in regex.captures_iter(input) {
        if let Some(m) = cap.get(0) {
            let url = m
                .as_str()
                .trim_end_matches(&[',', ';', ')', ']', '}'][..])
                .to_string();
            if seen.insert(url.clone()) {
                out.push(url);
            }
        }
    }
    out
}
