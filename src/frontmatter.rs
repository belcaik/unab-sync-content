//! YAML frontmatter for notes u_crawler generated.
//!
//! The block is the Obsidian properties list on a page, assignment,
//! announcement or recording. Rendering and parsing are pure. [`upsert`]
//! rewrites an existing block in place: known keys are updated, unknown keys
//! and the body are left as they were, and a second call with the same value
//! changes nothing.
//!
//! Dates (`publicado`, `vence`, `disponible`) are opaque strings. Callers
//! pass RFC 3339 when they have it; a value that is already written is not
//! dropped because this module could not parse it.

use thiserror::Error;

const OUR_KEYS: [&str; 7] = [
    "ramo",
    "tipo",
    "canvas_id",
    "url",
    "publicado",
    "vence",
    "disponible",
];

/// What a generated note is. The YAML spelling is [`DocKind::as_str`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocKind {
    /// A module page. YAML: `page`.
    Page,
    /// An assignment. YAML: `tarea`.
    Assignment,
    /// An announcement. YAML: `anuncio`.
    Announcement,
    /// A recording transcript. YAML: `grabacion`.
    Recording,
}

impl DocKind {
    /// The `tipo` value written into the frontmatter.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Page => "page",
            Self::Assignment => "tarea",
            Self::Announcement => "anuncio",
            Self::Recording => "grabacion",
        }
    }

    /// Parse a `tipo` value. Only the four spellings in [`DocKind::as_str`].
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "page" => Some(Self::Page),
            "tarea" => Some(Self::Assignment),
            "anuncio" => Some(Self::Announcement),
            "grabacion" => Some(Self::Recording),
            _ => None,
        }
    }
}

/// The properties u_crawler owns on a generated note.
///
/// `ramo` and `tipo` are always written. The other fields are omitted when
/// they are [`None`] or blank, and an update removes a key that became blank.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frontmatter {
    /// Course slug (`layout`'s course directory name), not the Canvas title.
    pub ramo: String,
    /// Which kind of note this is.
    pub tipo: DocKind,
    /// Canvas id of the page, assignment, announcement or recording, when known.
    pub canvas_id: Option<u64>,
    /// Canvas `html_url` for the note. Not a `/files/<id>` download link.
    pub url: Option<String>,
    /// When the note was published or posted.
    pub publicado: Option<String>,
    /// When an assignment is due.
    pub vence: Option<String>,
    /// When an assignment becomes available.
    pub disponible: Option<String>,
}

/// A frontmatter block that mentions our keys but cannot be read back.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum FrontmatterError {
    /// `ramo` is absent, or explicitly null.
    #[error("frontmatter is missing ramo")]
    MissingRamo,
    /// `tipo` is absent, or explicitly null.
    #[error("frontmatter is missing tipo")]
    MissingTipo,
    /// `tipo` is not `page`, `tarea`, `anuncio` or `grabacion`.
    #[error("frontmatter tipo {0:?} is not page, tarea, anuncio or grabacion")]
    BadTipo(String),
    /// `canvas_id` is present and is not an integer.
    #[error("frontmatter canvas_id is not an integer")]
    BadCanvasId,
}

/// The fenced YAML block for `fm`, including the closing newline.
///
/// The result is what [`upsert`] prepends to a note that has no frontmatter.
pub fn render(fm: &Frontmatter) -> String {
    let mut lines = Vec::new();
    for key in OUR_KEYS {
        if let Some(line) = render_field(key, fm) {
            lines.push(line);
        }
    }
    format!("---\n{}\n---\n", lines.join("\n"))
}

/// Read our keys from a leading frontmatter block.
///
/// Returns [`None`] when the note has no frontmatter, or the block has none
/// of our keys (a user's own properties). Unknown keys are ignored.
///
/// # Errors
///
/// [`FrontmatterError`] when the block has some of our keys but `ramo` or
/// `tipo` is missing or null, `tipo` is not one of the four kinds, or
/// `canvas_id` is not an integer.
pub fn parse(markdown: &str) -> Result<Option<Frontmatter>, FrontmatterError> {
    let (_, markdown) = strip_bom(markdown);
    let Some((yaml, _)) = split_frontmatter(markdown) else {
        return Ok(None);
    };
    parse_yaml(yaml)
}

/// Insert `fm`, or refresh our keys inside the frontmatter the note already has.
///
/// A note that already starts with a closed `---` block keeps every key this
/// module does not own, and keeps the body byte for byte. Our keys are
/// rewritten to the canonical spelling; a missing one is appended; one whose
/// value is now blank is removed. Calling `upsert` again with the same `fm`
/// returns the same string.
///
/// # Examples
///
/// ```
/// use u_crawler::frontmatter::{upsert, DocKind, Frontmatter};
///
/// let fm = Frontmatter {
///     ramo: "atdf105-mineria-de-datos".into(),
///     tipo: DocKind::Page,
///     canvas_id: None,
///     url: None,
///     publicado: None,
///     vence: None,
///     disponible: None,
/// };
/// let once = upsert("Hola\n", &fm);
/// assert_eq!(upsert(&once, &fm), once);
/// assert!(once.starts_with("---\n"));
/// ```
pub fn upsert(markdown: &str, fm: &Frontmatter) -> String {
    let (bom, markdown) = strip_bom(markdown);
    let rewritten = match split_frontmatter(markdown) {
        Some((yaml, body)) => {
            let yaml = update_yaml(yaml, fm);
            join_frontmatter(&yaml, body)
        }
        None => format!("{}{markdown}", render(fm)),
    };
    format!("{bom}{rewritten}")
}

pub(crate) fn strip_bom(markdown: &str) -> (&str, &str) {
    match markdown.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", markdown),
    }
}

/// Split a leading `---` block into `(yaml interior, body)`.
///
/// The interior keeps the newline that ended its last line. The body starts
/// on the first byte after the closing fence's newline. A missing closer
/// means this is not frontmatter.
pub(crate) fn split_frontmatter(markdown: &str) -> Option<(&str, &str)> {
    let rest = if let Some(rest) = markdown.strip_prefix("---\r\n") {
        rest
    } else {
        markdown.strip_prefix("---\n")?
    };
    let mut offset = 0;
    loop {
        if offset >= rest.len() {
            return None;
        }
        let tail = &rest[offset..];
        let (line_len, nl_len) = line_ending(tail);
        let mut line = &tail[..line_len];
        if nl_len == 0 {
            if let Some(stripped) = line.strip_suffix('\r') {
                line = stripped;
            }
        }
        if line == "---" {
            let body_at = offset + line_len + nl_len;
            return Some((&rest[..offset], &rest[body_at..]));
        }
        if nl_len == 0 {
            return None;
        }
        offset += line_len + nl_len;
    }
}

pub(crate) fn join_frontmatter(yaml: &str, body: &str) -> String {
    if yaml.is_empty() || yaml.ends_with('\n') {
        format!("---\n{yaml}---\n{body}")
    } else {
        format!("---\n{yaml}\n---\n{body}")
    }
}

fn line_ending(s: &str) -> (usize, usize) {
    match s.find('\n') {
        Some(n) if n > 0 && s.as_bytes()[n - 1] == b'\r' => (n - 1, 2),
        Some(n) => (n, 1),
        None => (s.len(), 0),
    }
}

fn update_yaml(yaml: &str, fm: &Frontmatter) -> String {
    let lines = yaml_lines(yaml);
    let mut seen = [false; OUR_KEYS.len()];
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if let Some(key) = key_of(lines[i]) {
            if let Some(idx) = OUR_KEYS.iter().position(|candidate| *candidate == key) {
                i += 1;
                while i < lines.len() && is_indented(lines[i]) {
                    i += 1;
                }
                if !seen[idx] {
                    seen[idx] = true;
                    if let Some(line) = render_field(key, fm) {
                        out.push(line);
                    }
                }
                continue;
            }
        }
        out.push(lines[i].to_string());
        i += 1;
    }
    for (idx, key) in OUR_KEYS.iter().enumerate() {
        if !seen[idx] {
            if let Some(line) = render_field(key, fm) {
                out.push(line);
            }
        }
    }
    if out.is_empty() {
        "\n".to_string()
    } else {
        format!("{}\n", out.join("\n"))
    }
}

fn yaml_lines(yaml: &str) -> Vec<&str> {
    let yaml = yaml.trim_end_matches(['\r', '\n']);
    if yaml.is_empty() {
        Vec::new()
    } else {
        yaml.split('\n')
            .map(|line| line.trim_end_matches('\r'))
            .collect()
    }
}

fn render_field(key: &str, fm: &Frontmatter) -> Option<String> {
    match key {
        "ramo" => Some(format!("ramo: {}", quote(&fm.ramo))),
        "tipo" => Some(format!("tipo: {}", fm.tipo.as_str())),
        "canvas_id" => fm.canvas_id.map(|id| format!("canvas_id: {id}")),
        "url" => optional_line("url", &fm.url),
        "publicado" => optional_line("publicado", &fm.publicado),
        "vence" => optional_line("vence", &fm.vence),
        "disponible" => optional_line("disponible", &fm.disponible),
        _ => None,
    }
}

fn optional_line(key: &str, value: &Option<String>) -> Option<String> {
    let value = value.as_deref()?;
    if value.trim().is_empty() {
        None
    } else {
        Some(format!("{key}: {}", quote(value)))
    }
}

fn quote(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn parse_yaml(yaml: &str) -> Result<Option<Frontmatter>, FrontmatterError> {
    let lines = yaml_lines(yaml);
    let mut saw = false;
    let mut ramo = None;
    let mut tipo = None;
    let mut canvas_id = None;
    let mut url = None;
    let mut publicado = None;
    let mut vence = None;
    let mut disponible = None;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        let Some(key) = key_of(line) else {
            continue;
        };
        if !OUR_KEYS.contains(&key) {
            while i < lines.len() && is_indented(lines[i]) {
                i += 1;
            }
            continue;
        }
        saw = true;
        while i < lines.len() && is_indented(lines[i]) {
            i += 1;
        }
        let scalar = parse_scalar(value_of(line));
        match key {
            "ramo" => match scalar {
                Scalar::Null => return Err(FrontmatterError::MissingRamo),
                other => ramo = Some(string_value(other)),
            },
            "tipo" => match scalar {
                Scalar::Null => return Err(FrontmatterError::MissingTipo),
                other => {
                    let text = string_value(other);
                    tipo = Some(DocKind::parse(&text).ok_or(FrontmatterError::BadTipo(text))?);
                }
            },
            "canvas_id" => {
                canvas_id = match scalar {
                    Scalar::Null => None,
                    Scalar::Int(id) => Some(id),
                    Scalar::Str(text) => {
                        let text = text.trim();
                        if text.is_empty() {
                            None
                        } else {
                            Some(
                                text.parse::<u64>()
                                    .map_err(|_| FrontmatterError::BadCanvasId)?,
                            )
                        }
                    }
                };
            }
            "url" => url = optional_string(scalar),
            "publicado" => publicado = optional_string(scalar),
            "vence" => vence = optional_string(scalar),
            "disponible" => disponible = optional_string(scalar),
            _ => {}
        }
    }
    if !saw {
        return Ok(None);
    }
    Ok(Some(Frontmatter {
        ramo: ramo.ok_or(FrontmatterError::MissingRamo)?,
        tipo: tipo.ok_or(FrontmatterError::MissingTipo)?,
        canvas_id,
        url,
        publicado,
        vence,
        disponible,
    }))
}

enum Scalar {
    Null,
    Str(String),
    Int(u64),
}

fn parse_scalar(raw: &str) -> Scalar {
    let raw = raw.trim();
    if raw.is_empty() {
        return Scalar::Null;
    }
    if let Some(text) = unquote(raw) {
        return Scalar::Str(text);
    }
    if is_null(raw) {
        return Scalar::Null;
    }
    let plain = cut_comment(raw);
    if plain.is_empty() || is_null(plain) {
        return Scalar::Null;
    }
    if plain.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(n) = plain.parse::<u64>() {
            return Scalar::Int(n);
        }
    }
    Scalar::Str(plain.to_string())
}

fn string_value(scalar: Scalar) -> String {
    match scalar {
        Scalar::Null => String::new(),
        Scalar::Int(n) => n.to_string(),
        Scalar::Str(text) => text,
    }
}

fn optional_string(scalar: Scalar) -> Option<String> {
    let text = string_value(scalar);
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

fn is_null(value: &str) -> bool {
    value == "~" || value.eq_ignore_ascii_case("null")
}

fn cut_comment(value: &str) -> &str {
    let mut prev_ws = false;
    for (idx, c) in value.char_indices() {
        if c == '#' && prev_ws {
            return value[..idx].trim_end();
        }
        prev_ws = c.is_whitespace();
    }
    value.trim()
}

fn unquote(raw: &str) -> Option<String> {
    match raw.chars().next() {
        Some('"') => unquote_double(raw),
        Some('\'') => unquote_single(raw),
        _ => None,
    }
}

fn unquote_double(raw: &str) -> Option<String> {
    let mut chars = raw[1..].chars();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        if c == '"' {
            return Some(out);
        }
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            '\\' => out.push('\\'),
            '"' => out.push('"'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            '/' => out.push('/'),
            'u' => {
                let hex: String = chars.by_ref().take(4).collect();
                if hex.len() != 4 {
                    return None;
                }
                let code = u32::from_str_radix(&hex, 16).ok()?;
                out.push(char::from_u32(code)?);
            }
            other => out.push(other),
        }
    }
    None
}

fn unquote_single(raw: &str) -> Option<String> {
    let mut chars = raw[1..].chars();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        if c == '\'' {
            if chars.as_str().starts_with('\'') {
                chars.next();
                out.push('\'');
                continue;
            }
            return Some(out);
        }
        out.push(c);
    }
    None
}

fn key_of(line: &str) -> Option<&str> {
    if line.is_empty() || is_indented(line) || line.starts_with('#') {
        return None;
    }
    let (key, _) = line.split_once(':')?;
    if is_yaml_key(key) {
        Some(key)
    } else {
        None
    }
}

fn is_yaml_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn value_of(line: &str) -> &str {
    match line.split_once(':') {
        Some((_, value)) => value,
        None => "",
    }
}

fn is_indented(line: &str) -> bool {
    line.starts_with(' ') || line.starts_with('\t')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Frontmatter {
        Frontmatter {
            ramo: "atdf105-mineria-de-datos".into(),
            tipo: DocKind::Assignment,
            canvas_id: Some(42),
            url: Some("https://canvas.example.com/courses/1/assignments/42".into()),
            publicado: Some("2026-06-01T00:00:00Z".into()),
            vence: Some("2026-06-20T23:59:00Z".into()),
            disponible: Some("2026-06-01T00:00:00Z".into()),
        }
    }

    #[test]
    fn render_writes_the_canonical_block_and_parses_back() {
        let fm = sample();
        let text = render(&fm);
        assert_eq!(
            text,
            "\
---
ramo: \"atdf105-mineria-de-datos\"
tipo: tarea
canvas_id: 42
url: \"https://canvas.example.com/courses/1/assignments/42\"
publicado: \"2026-06-01T00:00:00Z\"
vence: \"2026-06-20T23:59:00Z\"
disponible: \"2026-06-01T00:00:00Z\"
---
"
        );
        assert_eq!(parse(&text).unwrap(), Some(fm));
    }

    #[test]
    fn render_omits_blank_optional_keys() {
        let fm = Frontmatter {
            ramo: "curso".into(),
            tipo: DocKind::Page,
            canvas_id: None,
            url: Some("  ".into()),
            publicado: None,
            vence: None,
            disponible: None,
        };
        let text = render(&fm);
        assert_eq!(text, "---\nramo: \"curso\"\ntipo: page\n---\n");
        assert_eq!(parse(&text).unwrap().unwrap().url, None);
    }

    #[test]
    fn each_kind_round_trips() {
        for kind in [
            DocKind::Page,
            DocKind::Assignment,
            DocKind::Announcement,
            DocKind::Recording,
        ] {
            assert_eq!(DocKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(DocKind::parse("pagina"), None);
        assert_eq!(DocKind::parse("Page"), None);
    }

    #[test]
    fn upsert_on_a_plain_body_is_idempotent() {
        let fm = sample();
        let once = upsert("Cuerpo\n", &fm);
        assert_eq!(upsert(&once, &fm), once);
        assert_eq!(upsert(&upsert(&once, &fm), &fm), once);
        assert!(once.ends_with("Cuerpo\n"));
        assert_eq!(parse(&once).unwrap(), Some(fm));
    }

    #[test]
    fn upsert_updates_our_keys_and_keeps_user_keys_and_the_body_rule() {
        let input = "\
---
tags:
  - examen
ramo: \"viejo\"
ramo: \"mas viejo\"
---
Cuerpo
---
fin
";
        let fm = Frontmatter {
            ramo: "atdf105-mineria-de-datos".into(),
            tipo: DocKind::Page,
            canvas_id: Some(7),
            url: None,
            publicado: None,
            vence: None,
            disponible: None,
        };
        let out = upsert(input, &fm);
        assert_eq!(out.lines().filter(|line| *line == "---").count(), 3);
        assert!(out.contains("tags:\n  - examen\n"));
        assert_eq!(out.matches("ramo:").count(), 1);
        assert!(out.contains("ramo: \"atdf105-mineria-de-datos\"\n"));
        assert!(out.contains("tipo: page\n"));
        assert!(out.contains("canvas_id: 7\n"));
        assert!(!out.contains("viejo"));
        assert!(out.contains("Cuerpo\n---\nfin\n"));
        assert_eq!(upsert(&out, &fm), out);
    }

    #[test]
    fn upsert_removes_an_optional_key_that_is_now_blank() {
        let input = "\
---
ramo: \"curso\"
tipo: tarea
vence: \"2026-06-20T23:59:00Z\"
---
Body
";
        let fm = Frontmatter {
            ramo: "curso".into(),
            tipo: DocKind::Assignment,
            canvas_id: None,
            url: None,
            publicado: None,
            vence: None,
            disponible: None,
        };
        let out = upsert(input, &fm);
        assert!(!out.contains("vence:"));
        assert_eq!(parse(&out).unwrap().unwrap().vence, None);
        assert_eq!(upsert(&out, &fm), out);
    }

    #[test]
    fn upsert_keeps_a_crlf_body_and_normalizes_the_block_once() {
        let input = "---\r\ntipo: page\r\nramo: \"old\"\r\n---\r\nHola\r\n";
        let fm = Frontmatter {
            ramo: "curso".into(),
            tipo: DocKind::Page,
            canvas_id: None,
            url: None,
            publicado: None,
            vence: None,
            disponible: None,
        };
        let out = upsert(input, &fm);
        assert!(out.contains("ramo: \"curso\"\n"));
        assert!(out.ends_with("Hola\r\n"));
        assert!(!out.contains("old"));
        assert_eq!(upsert(&out, &fm), out);
    }

    #[test]
    fn a_leading_rule_without_a_closer_is_body() {
        let fm = Frontmatter {
            ramo: "curso".into(),
            tipo: DocKind::Announcement,
            canvas_id: None,
            url: None,
            publicado: None,
            vence: None,
            disponible: None,
        };
        let out = upsert("---\nno close\n", &fm);
        assert_eq!(out.lines().filter(|line| *line == "---").count(), 3);
        assert!(out.contains("no close\n"));
        assert_eq!(parse("---\nno close\n").unwrap(), None);
    }

    #[test]
    fn user_only_frontmatter_parses_as_absent_until_we_add_our_keys() {
        let input = "---\ntags: examen\n---\nBody\n";
        assert_eq!(parse(input).unwrap(), None);
        let fm = Frontmatter {
            ramo: "curso".into(),
            tipo: DocKind::Recording,
            canvas_id: None,
            url: None,
            publicado: None,
            vence: None,
            disponible: None,
        };
        let out = upsert(input, &fm);
        assert!(out.contains("tags: examen\n"));
        assert_eq!(parse(&out).unwrap().unwrap().tipo, DocKind::Recording);
    }

    #[test]
    fn parse_reports_a_partial_or_broken_block() {
        assert_eq!(
            parse("---\ntipo: page\n---\n").unwrap_err(),
            FrontmatterError::MissingRamo
        );
        assert_eq!(
            parse("---\nramo: \"curso\"\n---\n").unwrap_err(),
            FrontmatterError::MissingTipo
        );
        assert_eq!(
            parse("---\nramo: \"curso\"\ntipo: pagina\n---\n").unwrap_err(),
            FrontmatterError::BadTipo("pagina".into())
        );
        assert_eq!(
            parse("---\nramo: \"curso\"\ntipo: page\ncanvas_id: abc\n---\n").unwrap_err(),
            FrontmatterError::BadCanvasId
        );
    }

    #[test]
    fn parse_accepts_quoted_ids_comments_and_null_optionals() {
        let text = "\
---
# nota
ramo: \"curso\"
tipo: \"anuncio\" # kind
canvas_id: \"20411861\"
url: null
publicado: \"\"
vence: ~
---
";
        let fm = parse(text).unwrap().unwrap();
        assert_eq!(fm.tipo, DocKind::Announcement);
        assert_eq!(fm.canvas_id, Some(20_411_861));
        assert_eq!(fm.url, None);
        assert_eq!(fm.publicado, None);
        assert_eq!(fm.vence, None);
    }

    #[test]
    fn quotes_and_newlines_in_ramo_round_trip() {
        let fm = Frontmatter {
            ramo: "say \"hola\"\nlinea".into(),
            tipo: DocKind::Page,
            canvas_id: None,
            url: None,
            publicado: None,
            vence: None,
            disponible: None,
        };
        assert_eq!(parse(&render(&fm)).unwrap(), Some(fm));
    }

    #[test]
    fn bom_stays_in_front_of_the_block() {
        let fm = Frontmatter {
            ramo: "curso".into(),
            tipo: DocKind::Page,
            canvas_id: None,
            url: None,
            publicado: None,
            vence: None,
            disponible: None,
        };
        let out = upsert("\u{feff}Hola\n", &fm);
        assert!(out.starts_with('\u{feff}'));
        assert_eq!(parse(&out).unwrap().unwrap().ramo, "curso");
        assert_eq!(upsert(&out, &fm), out);
    }
}
