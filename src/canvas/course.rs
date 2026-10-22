use super::models::Course;
use crate::core::fsutil::sanitize_component;
use std::path::{Path, PathBuf};

/// Select which courses a command should process.
///
/// One rule for every command (`sync`, `announcements`, `recordings`,
/// `calendar`): an explicit `--course-id` wins outright — it is an
/// instruction to process that one course, ignored or not. Without it,
/// courses listed in `ignored_courses` (matched by their string id, as
/// `canvas.ignored_courses` stores them) are dropped.
pub fn select_courses(
    courses: Vec<Course>,
    filter_course_id: Option<u64>,
    ignored_courses: &[String],
) -> Vec<Course> {
    if let Some(cid) = filter_course_id {
        return courses.into_iter().filter(|c| c.id == cid).collect();
    }
    courses
        .into_iter()
        .filter(|c| !ignored_courses.iter().any(|id| id == &c.id.to_string()))
        .collect()
}

/// Derive the on-disk directory for a course, under `root`.
///
/// The name is combined with the course code (when present) and each
/// component is sanitized and transliterated to ASCII via
/// [`sanitize_component`]. A missing/empty course code falls back to the
/// sanitized name alone.
pub fn course_dir(root: &Path, course: &Course) -> PathBuf {
    let code = course.course_code.clone().unwrap_or_default();
    root.join(if code.is_empty() {
        sanitize_component(&course.name)
    } else {
        format!(
            "{}_{}",
            sanitize_component(&course.name),
            sanitize_component(code)
        )
    })
}

/// The path of a course's `state.json`, given its on-disk directory
/// ([`course_dir`]). One spelling for every flow that loads or saves it.
pub fn state_path(course_dir: &Path) -> PathBuf {
    course_dir.join("state.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn course(name: &str, code: Option<&str>) -> Course {
        Course {
            id: 1,
            name: name.to_string(),
            course_code: code.map(str::to_string),
        }
    }

    fn course_with_id(id: u64) -> Course {
        Course {
            id,
            name: format!("Course {id}"),
            course_code: None,
        }
    }

    fn ids(courses: &[Course]) -> Vec<u64> {
        courses.iter().map(|c| c.id).collect()
    }

    #[test]
    fn state_path_is_the_state_json_under_the_course_dir() {
        assert_eq!(
            state_path(Path::new("/backup/Course_101")),
            Path::new("/backup/Course_101/state.json")
        );
    }

    #[test]
    fn explicit_id_wins_even_when_the_course_is_ignored() {
        let courses = vec![course_with_id(1), course_with_id(2)];
        let got = select_courses(courses, Some(2), &["2".to_string()]);
        assert_eq!(ids(&got), vec![2]);
    }

    #[test]
    fn explicit_id_selects_a_non_ignored_course() {
        let courses = vec![course_with_id(1), course_with_id(2), course_with_id(3)];
        let got = select_courses(courses, Some(2), &["3".to_string()]);
        assert_eq!(ids(&got), vec![2]);
    }

    #[test]
    fn explicit_id_absent_from_the_list_selects_nothing() {
        let courses = || vec![course_with_id(1), course_with_id(2)];
        assert!(select_courses(courses(), Some(9), &[]).is_empty());
        assert!(select_courses(courses(), Some(9), &["9".to_string()]).is_empty());
    }

    #[test]
    fn without_explicit_id_ignored_courses_are_dropped() {
        let courses = vec![course_with_id(1), course_with_id(2), course_with_id(3)];
        let got = select_courses(courses, None, &["2".to_string()]);
        assert_eq!(ids(&got), vec![1, 3]);
    }

    #[test]
    fn without_explicit_id_and_no_ignored_courses_everything_is_kept() {
        let courses = vec![course_with_id(1), course_with_id(2)];
        let got = select_courses(courses, None, &[]);
        assert_eq!(ids(&got), vec![1, 2]);
    }

    #[test]
    fn course_dir_joins_sanitized_name_and_code_under_root() {
        let root = Path::new("/backup");
        let c = course("Intro to CS", Some("CS101"));
        assert_eq!(course_dir(root, &c), root.join("Intro_to_CS_CS101"));
    }

    #[test]
    fn course_dir_falls_back_to_name_when_code_is_empty() {
        let root = Path::new("/backup");
        let c = course("Independent Study", Some(""));
        assert_eq!(course_dir(root, &c), root.join("Independent_Study"));

        let c_none = course("Independent Study", None);
        assert_eq!(course_dir(root, &c_none), root.join("Independent_Study"));
    }

    #[test]
    fn course_dir_transliterates_accents_and_strips_invalid_chars() {
        let root = Path::new("/backup");
        let c = course("Cálculo II: Límites/Continuidad", Some("MAT-102?"));
        let got = course_dir(root, &c);
        let name = got.file_name().unwrap().to_str().unwrap();
        assert!(name.is_ascii(), "expected ascii, got {name:?}");
        assert!(!name.contains('/'));
        assert!(!name.contains('?'));
        assert!(!name.contains(':'));
        assert_eq!(got, root.join("Calculo_II_LimitesContinuidad_MAT_102"));
    }
}
