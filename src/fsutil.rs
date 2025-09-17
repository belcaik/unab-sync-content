use sanitize_filename::sanitize;
use std::io;
use std::path::Path;

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

fn sanitize_stem(input: &str) -> String {
    let s1 = sanitize(input);
    let s2 = ascii_skeleton(&s1);
    s2.to_string()
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
