use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Course {
    pub id: u64,
    pub name: String,
    pub course_code: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Module {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub items: Vec<ModuleItem>,
}

#[derive(Debug, Deserialize)]
pub struct ModuleItem {
    pub id: u64,
    pub title: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub html_url: Option<String>,
    pub page_url: Option<String>,
    pub external_url: Option<String>,
    pub content_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct FileObj {
    pub id: u64,
    pub display_name: Option<String>,
    pub filename: Option<String>,
    pub size: Option<u64>,
    pub updated_at: Option<String>,
    pub url: Option<String>,
    pub download_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PageObj {
    pub title: Option<String>,
    pub body: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Assignment {
    pub id: u64,
    pub name: Option<String>,
    pub description: Option<String>,
    pub updated_at: Option<String>,
    #[serde(default)]
    pub due_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub unlock_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub lock_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub points_possible: Option<f64>,
    #[serde(default)]
    pub omit_from_final_grade: Option<bool>,
    #[serde(default)]
    pub html_url: Option<String>,
    #[serde(default)]
    pub assignment_group_id: Option<u64>,
    #[serde(default)]
    pub submission_types: Option<Vec<String>>,
    #[serde(default)]
    pub published: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct Announcement {
    pub id: u64,
    pub title: Option<String>,
    pub message: Option<String>,
    pub posted_at: Option<String>,
    pub html_url: Option<String>,
    #[serde(default)]
    pub author: Option<AnnouncementAuthor>,
    #[serde(default)]
    pub attachments: Vec<FileObj>,
}

#[derive(Debug, Deserialize)]
pub struct AnnouncementAuthor {
    pub display_name: Option<String>,
}

/// One student's submission record for one assignment, as returned by the
/// bulk `students/submissions` endpoint.
///
/// Minimal by design: the calendar-sync flow (spec D7) only needs to decide
/// "done or not", which `submitted_at` and `workflow_state` answer between
/// them. Other fields Canvas returns (`grade`, `score`, `attempt`, …) have no
/// consumer yet and are left off rather than carried speculatively.
#[derive(Debug, Clone, Deserialize)]
pub struct Submission {
    pub assignment_id: u64,
    #[serde(default)]
    pub submitted_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub workflow_state: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submission_deserializes_submitted_at_and_workflow_state() {
        let body = r#"
        {
            "assignment_id": 4501,
            "submitted_at": "2026-02-18T10:00:00Z",
            "workflow_state": "submitted"
        }
        "#;
        let submission: Submission = serde_json::from_str(body).unwrap();
        assert_eq!(submission.assignment_id, 4501);
        assert!(submission.submitted_at.is_some());
        assert_eq!(submission.workflow_state.as_deref(), Some("submitted"));
    }

    #[test]
    fn submission_without_submission_still_deserializes() {
        let body = r#"
        {
            "assignment_id": 9001,
            "submitted_at": null,
            "workflow_state": "unsubmitted"
        }
        "#;
        let submission: Submission = serde_json::from_str(body).unwrap();
        assert_eq!(submission.assignment_id, 9001);
        assert_eq!(submission.submitted_at, None);
        assert_eq!(submission.workflow_state.as_deref(), Some("unsubmitted"));
    }

    #[test]
    fn assignment_deserializes_dates_and_grading_metadata_from_a_full_response() {
        let body = r#"
        {
            "id": 4501,
            "name": "Problem Set 3",
            "description": "<p>Solve the attached problems.</p>",
            "updated_at": "2026-02-10T18:04:00Z",
            "due_at": "2026-02-20T23:59:00Z",
            "unlock_at": "2026-02-13T00:00:00Z",
            "lock_at": "2026-02-21T06:00:00Z",
            "points_possible": 25.0,
            "omit_from_final_grade": false,
            "html_url": "https://canvas.example.com/courses/1/assignments/4501",
            "assignment_group_id": 77,
            "submission_types": ["online_upload", "online_text_entry"],
            "published": true
        }
        "#;

        let assignment: Assignment = serde_json::from_str(body).unwrap();

        assert_eq!(assignment.id, 4501);
        assert_eq!(assignment.name.as_deref(), Some("Problem Set 3"));
        assert_eq!(
            assignment.due_at,
            Some(
                chrono::DateTime::parse_from_rfc3339("2026-02-20T23:59:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc)
            )
        );
        assert_eq!(
            assignment.unlock_at,
            Some(
                chrono::DateTime::parse_from_rfc3339("2026-02-13T00:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc)
            )
        );
        assert_eq!(
            assignment.lock_at,
            Some(
                chrono::DateTime::parse_from_rfc3339("2026-02-21T06:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc)
            )
        );
        assert_eq!(assignment.points_possible, Some(25.0));
        assert_eq!(assignment.omit_from_final_grade, Some(false));
        assert_eq!(
            assignment.html_url.as_deref(),
            Some("https://canvas.example.com/courses/1/assignments/4501")
        );
        assert_eq!(assignment.assignment_group_id, Some(77));
        assert_eq!(
            assignment.submission_types,
            Some(vec![
                "online_upload".to_string(),
                "online_text_entry".to_string()
            ])
        );
        assert_eq!(assignment.published, Some(true));
    }

    #[test]
    fn assignment_without_optional_fields_still_deserializes() {
        let body = r#"
        {
            "id": 9001,
            "name": "Reading Reflection",
            "description": null,
            "updated_at": "2026-01-05T09:00:00Z"
        }
        "#;

        let assignment: Assignment = serde_json::from_str(body).unwrap();

        assert_eq!(assignment.id, 9001);
        assert_eq!(assignment.due_at, None);
        assert_eq!(assignment.unlock_at, None);
        assert_eq!(assignment.lock_at, None);
        assert_eq!(assignment.points_possible, None);
        assert_eq!(assignment.omit_from_final_grade, None);
        assert_eq!(assignment.html_url, None);
        assert_eq!(assignment.assignment_group_id, None);
        assert_eq!(assignment.submission_types, None);
        assert_eq!(assignment.published, None);
    }
}
