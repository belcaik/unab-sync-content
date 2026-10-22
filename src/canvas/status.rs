//! Aggregation behind the `status` command: walks the download root, loads
//! each course's `state.json` and folds it into a
//! [`StatusReport`](crate::canvas::status::StatusReport) that `main.rs` prints.
//! Pure of terminal output by design — it returns data.

use std::path::Path;
use tracing::info;

use crate::canvas::course::state_path;
use crate::core::state::State;

/// The result of scanning the download root.
#[derive(Debug)]
pub enum StatusReport {
    /// The download root does not exist yet: no sync has ever run.
    NoBackupRoot,
    /// The root exists but contains no course directories.
    NoCourses,
    /// Per-course aggregates plus the grand totals.
    Courses(StatusSummary),
}

/// The populated case of [`StatusReport`].
#[derive(Debug)]
pub struct StatusSummary {
    pub courses: Vec<CourseStatus>,
    pub total_files: usize,
    pub total_storage: u64,
}

/// Aggregated state of one course directory.
#[derive(Debug)]
pub struct CourseStatus {
    pub name: String,
    pub file_count: usize,
    pub storage_bytes: u64,
    /// Most recent `updated_at` across the course's items, if any.
    pub last_updated: Option<String>,
    pub failed_items: Vec<FailedItem>,
}

/// One item whose last sync attempt failed.
#[derive(Debug)]
pub struct FailedItem {
    pub key: String,
    /// Recorded attempt count; items failed before `error_count` existed
    /// report `1`.
    pub error_count: u32,
    pub last_error: Option<String>,
}

/// Scans `download_root` and aggregates every course's `state.json`.
pub async fn summarize(download_root: &Path) -> anyhow::Result<StatusReport> {
    info!(path = %download_root.display(), "scanning download root for courses");

    if !download_root.exists() {
        return Ok(StatusReport::NoBackupRoot);
    }

    let mut entries = tokio::fs::read_dir(download_root).await?;
    let mut course_dirs = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.is_dir() {
            course_dirs.push(path);
        }
    }

    if course_dirs.is_empty() {
        return Ok(StatusReport::NoCourses);
    }

    info!(count = course_dirs.len(), "found course directories");

    let mut courses = Vec::with_capacity(course_dirs.len());
    let mut total_files: usize = 0;
    let mut total_storage: u64 = 0;

    for course_dir in &course_dirs {
        let state = State::load(&state_path(course_dir)).await;

        let name = course_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        let file_count = state.items.len();
        let mut storage_bytes: u64 = 0;
        let mut last_updated: Option<String> = None;
        let mut failed_items = Vec::new();

        for (key, item) in &state.items {
            if let Some(size) = item.size {
                storage_bytes += size;
            }
            if let Some(ref updated) = item.updated_at {
                match &last_updated {
                    None => last_updated = Some(updated.clone()),
                    Some(current) => {
                        if updated > current {
                            last_updated = Some(updated.clone());
                        }
                    }
                }
            }
            if item.last_error.is_some() {
                failed_items.push(FailedItem {
                    key: key.clone(),
                    error_count: item.error_count.unwrap_or(1),
                    last_error: item.last_error.clone(),
                });
            }
        }

        total_files += file_count;
        total_storage += storage_bytes;

        info!(
            course = name,
            items = file_count,
            size = storage_bytes,
            "loaded course state"
        );

        courses.push(CourseStatus {
            name,
            file_count,
            storage_bytes,
            last_updated,
            failed_items,
        });
    }

    Ok(StatusReport::Courses(StatusSummary {
        courses,
        total_files,
        total_storage,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::state::ItemState;

    fn item(size: Option<u64>, updated_at: Option<&str>) -> ItemState {
        ItemState {
            etag: None,
            updated_at: updated_at.map(str::to_string),
            size,
            content_hash: None,
            last_error: None,
            error_count: None,
        }
    }

    #[tokio::test]
    async fn missing_root_reports_no_backup_root() {
        let dir = tempfile::tempdir().unwrap();
        let report = summarize(&dir.path().join("not-there")).await.unwrap();
        assert!(matches!(report, StatusReport::NoBackupRoot));
    }

    #[tokio::test]
    async fn empty_root_reports_no_courses() {
        let dir = tempfile::tempdir().unwrap();
        let report = summarize(dir.path()).await.unwrap();
        assert!(matches!(report, StatusReport::NoCourses));
    }

    #[tokio::test]
    async fn aggregates_counts_sizes_latest_update_and_failures() {
        let dir = tempfile::tempdir().unwrap();

        let dir_a = dir.path().join("Course A_101");
        std::fs::create_dir_all(&dir_a).unwrap();
        let mut state_a = State::default();
        state_a.set("file:1".into(), item(Some(1024), Some("2026-01-01")));
        state_a.set("file:2".into(), item(Some(2048), Some("2026-03-05")));
        let mut failed = item(None, None);
        failed.last_error = Some("boom".into());
        failed.error_count = Some(3);
        state_a.set("file:3".into(), failed);
        state_a.save(&state_path(&dir_a)).await.unwrap();

        let dir_b = dir.path().join("Course B_202");
        std::fs::create_dir_all(&dir_b).unwrap();
        let mut state_b = State::default();
        state_b.set("file:9".into(), item(Some(512), Some("2026-02-02")));
        state_b.save(&state_path(&dir_b)).await.unwrap();

        let report = summarize(dir.path()).await.unwrap();
        let StatusReport::Courses(summary) = report else {
            panic!("expected a populated report");
        };

        assert_eq!(summary.courses.len(), 2);
        assert_eq!(summary.total_files, 4);
        assert_eq!(summary.total_storage, 1024 + 2048 + 512);

        let course_a = summary
            .courses
            .iter()
            .find(|c| c.name == "Course A_101")
            .unwrap();
        assert_eq!(course_a.file_count, 3);
        assert_eq!(course_a.storage_bytes, 3072);
        assert_eq!(course_a.last_updated.as_deref(), Some("2026-03-05"));
        assert_eq!(course_a.failed_items.len(), 1);
        assert_eq!(course_a.failed_items[0].key, "file:3");
        assert_eq!(course_a.failed_items[0].error_count, 3);
        assert_eq!(course_a.failed_items[0].last_error.as_deref(), Some("boom"));

        let course_b = summary
            .courses
            .iter()
            .find(|c| c.name == "Course B_202")
            .unwrap();
        assert_eq!(course_b.file_count, 1);
        assert_eq!(course_b.storage_bytes, 512);
        assert!(course_b.failed_items.is_empty());
    }

    #[tokio::test]
    async fn failed_item_without_error_count_reports_one_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let course_dir = dir.path().join("Course_1");
        std::fs::create_dir_all(&course_dir).unwrap();
        let mut state = State::default();
        let mut legacy = item(None, None);
        legacy.last_error = Some("old failure".into());
        state.set("file:1".into(), legacy);
        state.save(&state_path(&course_dir)).await.unwrap();

        let report = summarize(dir.path()).await.unwrap();
        let StatusReport::Courses(summary) = report else {
            panic!("expected a populated report");
        };
        assert_eq!(summary.courses[0].failed_items[0].error_count, 1);
    }
}
