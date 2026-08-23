use crate::canvas::Course;
use sanitize_filename::sanitize;
use std::io;
use std::path::{Path, PathBuf};

fn ascii_skeleton(input: &str) -> String {
    // Transliterate to ASCII, then replace any non [A-Za-z0-9_] with '_'
    let s = deunicode::deunicode(input);
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            // map hyphens to underscore to avoid mixed separators
            out.push(ch);
        } else {
            // Treat hyphen as underscore and all others as underscore
            out.push('_');
        }
    }
    // Collapse multiple underscores
    let mut collapsed = String::with_capacity(out.len());
    let mut prev_us = false;
    for ch in out.chars() {
        if ch == '_' {
            if !prev_us {
                collapsed.push(ch);
            }
            prev_us = true;
        } else {
            collapsed.push(ch);
            prev_us = false;
        }
    }
    collapsed.trim_matches('_').to_string()
}

pub fn sanitize_component<S: AsRef<str>>(s: S) -> String {
    let name = s.as_ref().trim();
    if name.is_empty() {
        return "untitled".into();
    }
    // First pass: remove OS-invalid chars via sanitize-filename
    let s1 = sanitize(name);
    // Second pass: strict ASCII and restricted charset
    let s2 = ascii_skeleton(&s1);
    let final_s = if s2.is_empty() { "untitled".into() } else { s2 };
    // Optional max length
    const MAX_LEN: usize = 120;
    if final_s.len() > MAX_LEN {
        final_s[..MAX_LEN].to_string()
    } else {
        final_s
    }
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

fn sanitize_stem(input: &str) -> String {
    ascii_skeleton(&sanitize(input))
}

/// Sanitize a filename but preserve the last extension (lowercased).
pub fn sanitize_filename_preserve_ext<S: AsRef<str>>(s: S) -> String {
    let name = s.as_ref().trim();
    if name.is_empty() {
        return "untitled".into();
    }

    // Find last dot that is not the first char
    let mut parts = name.rsplitn(2, '.');
    let ext_part = parts.next().unwrap_or("");
    let stem_part = parts.next();

    let (stem_raw, ext_raw) = match stem_part {
        Some(stem) if !stem.is_empty() => (stem, ext_part),
        _ => (name, ""), // no extension
    };

    let mut stem = sanitize_stem(stem_raw);
    if stem.is_empty() {
        stem = "untitled".into();
    }

    let mut out = stem;
    if !ext_raw.is_empty() {
        // sanitize extension: transliterate and keep alphanumeric only
        let ext_ascii = deunicode::deunicode(ext_raw).to_lowercase();
        let ext_clean: String = ext_ascii
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
        if !ext_clean.is_empty() {
            out.push('.');
            out.push_str(&ext_clean);
        }
    }

    const MAX_LEN: usize = 180;
    if out.len() > MAX_LEN {
        out[..MAX_LEN].to_string()
    } else {
        out
    }
}

// Intentionally left out join_sanitized until needed to avoid dead code warnings.

pub async fn ensure_dir(path: &Path) -> io::Result<()> {
    tokio::fs::create_dir_all(path).await
}

pub async fn atomic_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let tmp = path.with_extension("part");
    tokio::fs::write(&tmp, contents).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perm = std::fs::Permissions::from_mode(0o644);
        std::fs::set_permissions(&tmp, perm)?;
    }
    tokio::fs::rename(&tmp, path).await
}

pub async fn atomic_rename(src: &Path, dest: &Path) -> io::Result<()> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::rename(src, dest).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Course;

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

    #[test]
    fn sanitize_component_strips_path_separators() {
        assert!(!sanitize_component("a/b\\c").contains('/'));
        assert!(!sanitize_component("a/b\\c").contains('\\'));
    }

    #[test]
    fn sanitize_component_transliterates_accents_to_ascii() {
        let got = sanitize_component("Semana Nº1 — Introducción");
        assert!(got.is_ascii(), "expected ascii, got {got:?}");
        assert!(got.to_lowercase().contains("introduccion"), "got {got:?}");
    }

    #[test]
    fn filename_extension_is_preserved_and_lowercased() {
        assert!(sanitize_filename_preserve_ext("Taller.IPYNB").ends_with(".ipynb"));
    }

    #[test]
    fn filename_without_extension_is_left_alone() {
        assert!(!sanitize_filename_preserve_ext("README").contains('.'));
    }
}
