use super::models::Course;
use crate::core::fsutil::sanitize_component;
use std::path::{Path, PathBuf};

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
