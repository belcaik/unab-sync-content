//! Library layout v2: the single source of truth for on-disk names.
//!
//! Every function here is pure — metadata in, a relative name out — so the
//! writers (`syncer`, `announcements`, `zoom`, `calendar`) and the offline
//! migrator compute the very same paths without touching the filesystem.
//! See `docs/specs/layout-v2.md` ("API de `layout.rs`") for the contract.
//!
//! All names are lowercase kebab-case ASCII. The only `_` in a v2 name is the
//! one separating a date prefix (`YYYY-MM-DD_`) from the slug that follows it.

use crate::config;
use crate::fsutil::ascii_skeleton;
use chrono::{Datelike, NaiveDate};
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

/// Version written to `.u_crawler/library.json` by a v2 library.
pub const LAYOUT_VERSION: u32 = 2;
/// Per-library and per-course metadata directory (`library.json`, `state.json`, ...).
pub const META_DIR: &str = ".u_crawler";
/// Course subdirectory holding one directory per module.
pub const MODULES_DIR: &str = "modulos";
/// Course subdirectory holding one markdown file per assignment.
pub const ASSIGNMENTS_DIR: &str = "tareas";
/// Course subdirectory holding announcements and their `index.json`.
pub const ANNOUNCEMENTS_DIR: &str = "anuncios";
/// Course subdirectory holding Zoom recordings.
pub const RECORDINGS_DIR: &str = "grabaciones";
/// Attachments directory, under a module or under [`ANNOUNCEMENTS_DIR`].
pub const ATTACHMENTS_DIR: &str = "adjuntos";

/// Slug used when a page, assignment, announcement or attachment has no usable name.
pub const UNTITLED: &str = "sin-titulo";
/// Recording topic used when nothing is left after removing the course name.
pub const DEFAULT_RECORDING_TOPIC: &str = "clase";
/// Longest slug produced, in bytes (slugs are ASCII, so bytes = chars).
pub const MAX_SLUG_LEN: usize = 100;

/// A `[layout]` setting that cannot be used.
#[derive(Debug, Error)]
pub enum LayoutError {
    /// `course_dir` names a placeholder other than `{code}`, `{name}` or `{id}`.
    #[error(
        "layout.course_dir: unknown placeholder {{{0}}} (expected {{code}}, {{name}} or {{id}})"
    )]
    UnknownPlaceholder(String),
    /// `course_dir` has a `{` without its `}`, or the reverse.
    #[error("layout.course_dir: unbalanced brace in {0:?}")]
    UnbalancedBrace(String),
    /// One of the `*_pattern` keys is not a valid regex.
    #[error("layout.{key}: invalid regex: {source}")]
    InvalidPattern {
        /// The config key holding the pattern.
        key: &'static str,
        /// What the regex parser rejected.
        #[source]
        source: Box<regex::Error>,
    },
}

/// What [`CourseNaming`] needs to know about a course.
///
/// Filled from Canvas (`canvas::Course`) when online, or from whatever the
/// migrator recovered offline — which is why everything but the name is
/// optional.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CourseMeta {
    /// Canvas course id, when known.
    pub id: Option<u64>,
    /// Canvas `name`, as Canvas returns it.
    pub name: String,
    /// Canvas `course_code`, when present.
    pub code: Option<String>,
}

impl From<&crate::canvas::Course> for CourseMeta {
    fn from(c: &crate::canvas::Course) -> Self {
        Self {
            id: Some(c.id),
            name: c.name.clone(),
            code: c.course_code.clone(),
        }
    }
}

#[derive(Clone, Debug)]
enum Piece {
    Literal(String),
    Code,
    Name,
    Id,
}

/// The compiled `[layout]` section: how a course becomes a directory name.
#[derive(Clone, Debug)]
pub struct CourseNaming {
    template: Vec<Piece>,
    code_pattern: Option<Regex>,
    name_strip_pattern: Option<Regex>,
    section_pattern: Option<Regex>,
}

impl CourseNaming {
    /// Compile the `[layout]` settings. Fails on an unknown placeholder, an
    /// unbalanced brace or an invalid regex, so a bad config is caught at load.
    pub fn new(cfg: &config::Layout) -> Result<Self, LayoutError> {
        Ok(Self {
            template: parse_template(&cfg.course_dir)?,
            code_pattern: compile("code_pattern", &cfg.code_pattern)?,
            name_strip_pattern: compile("name_strip_pattern", &cfg.name_strip_pattern)?,
            section_pattern: compile("section_pattern", &cfg.section_pattern)?,
        })
    }

    /// The course code after `code_pattern`: capture group 1 if the pattern
    /// has one, else the whole match. No pattern, or no match, keeps the code.
    pub fn short_code(&self, course: &CourseMeta) -> String {
        let code = course.code.as_deref().unwrap_or("");
        self.code_pattern
            .as_ref()
            .and_then(|re| first_capture(re, code))
            .unwrap_or_else(|| code.to_string())
    }

    /// The course name with every match of `name_strip_pattern` removed.
    pub fn short_name(&self, course: &CourseMeta) -> String {
        match &self.name_strip_pattern {
            Some(re) => re.replace_all(&course.name, "").into_owned(),
            None => course.name.clone(),
        }
    }

    /// The section (NRC) used by [`disambiguate`]: `section_pattern` tried on
    /// the code, then on the name; capture group 1 if present, else the match.
    pub fn section(&self, course: &CourseMeta) -> Option<String> {
        let re = self.section_pattern.as_ref()?;
        let code = course.code.as_deref().unwrap_or("");
        first_capture(re, code)
            .or_else(|| first_capture(re, &course.name))
            .map(|s| slugify(&s))
            .filter(|s| !s.is_empty())
    }

    /// The course directory name, before disambiguation: `course_dir` with
    /// each placeholder replaced by its slug, then slugified as a whole (so an
    /// empty `{code}` leaves no dangling `-`). Falls back to `curso-<id>`, or
    /// `curso`, when nothing usable remains.
    pub fn course_slug(&self, course: &CourseMeta) -> String {
        let mut raw = String::new();
        for piece in &self.template {
            match piece {
                Piece::Literal(s) => raw.push_str(s),
                Piece::Code => raw.push_str(&slugify(&self.short_code(course))),
                Piece::Name => raw.push_str(&slugify(&self.short_name(course))),
                Piece::Id => {
                    if let Some(id) = course.id {
                        raw.push_str(&id.to_string());
                    }
                }
            }
        }
        let slug = slugify(&raw);
        if !slug.is_empty() {
            return slug;
        }
        match course.id {
            Some(id) => format!("curso-{id}"),
            None => "curso".to_string(),
        }
    }

    /// The Canvas name and code a v1 course directory was built from.
    ///
    /// v1 named a course `<name>_<code>`, both through
    /// `fsutil::sanitize_component` (`fsutil::course_dir`), so the split is
    /// lost in the name. This takes the rightmost `_` after which
    /// `code_pattern` matches at the very start of the rest (rightmost, so a
    /// code-shaped word of the name, like `PYTHON3`, is not taken for the
    /// code): that rest is the
    /// code, what comes before it the name. Without a pattern, or without such
    /// a split, the whole directory is the name and the code is unknown. The
    /// id is always `None` — a v1 directory name carries none.
    pub fn parse_v1_course_dir(&self, dir: &str) -> CourseMeta {
        let split = self.code_pattern.as_ref().and_then(|re| {
            dir.rmatch_indices('_').map(|(i, _)| i).find(|&i| {
                i > 0
                    && re
                        .find(&dir[i + 1..])
                        .is_some_and(|m| m.start() == 0 && !m.is_empty())
            })
        });
        match split {
            Some(i) => CourseMeta {
                id: None,
                name: dir[..i].to_string(),
                code: Some(dir[i + 1..].to_string()),
            },
            None => CourseMeta {
                id: None,
                name: dir.to_string(),
                code: None,
            },
        }
    }

    /// Directory names for `courses`, in input order, given the course
    /// directories the library already has.
    ///
    /// Names are stable over time: a course that already owns a directory
    /// keeps it, whatever else is passed, and only a newcomer whose name is
    /// taken gets a suffix. A course owns an [`ExistingCourseDir`] when both
    /// have the same Canvas id or, if the directory has no recorded id (a v1
    /// course migrated offline), the same Canvas name and code. Every other
    /// course goes through [`CourseNaming::course_slug`] and then
    /// [`disambiguate`] against all of `existing`'s names, so the writer that
    /// only sees active courses still avoids the directories of archived ones.
    pub fn course_dirs(
        &self,
        existing: &[ExistingCourseDir],
        courses: &[CourseMeta],
    ) -> Vec<String> {
        let mut owner: Vec<Option<usize>> = vec![None; courses.len()];
        let mut claimed = vec![false; existing.len()];
        let by_id =
            |e: &ExistingCourseDir, c: &CourseMeta| e.course.id.is_some() && e.course.id == c.id;
        let by_name = |e: &ExistingCourseDir, c: &CourseMeta| {
            e.course.id.is_none() && e.course.name == c.name && e.course.code == c.code
        };
        for same in [
            &by_id as &dyn Fn(&ExistingCourseDir, &CourseMeta) -> bool,
            &by_name,
        ] {
            for (ci, c) in courses.iter().enumerate() {
                if owner[ci].is_some() {
                    continue;
                }
                if let Some(ei) =
                    (0..existing.len()).find(|&ei| !claimed[ei] && same(&existing[ei], c))
                {
                    claimed[ei] = true;
                    owner[ci] = Some(ei);
                }
            }
        }

        let newcomers: Vec<usize> = (0..courses.len())
            .filter(|&ci| owner[ci].is_none())
            .collect();
        let candidates: Vec<SlugCandidate> = newcomers
            .iter()
            .map(|&ci| SlugCandidate {
                slug: self.course_slug(&courses[ci]),
                section: self.section(&courses[ci]),
                id: courses[ci].id,
            })
            .collect();
        let taken: BTreeSet<String> = existing.iter().map(|e| e.dir.clone()).collect();
        let mut fresh = disambiguate(&taken, &candidates).into_iter();

        owner
            .iter()
            .map(|o| match o {
                Some(ei) => existing[*ei].dir.clone(),
                None => fresh.next().unwrap_or_default(),
            })
            .collect()
    }
}

/// A course directory the library already has, with what is recorded about
/// its course (from its `curso.json`, or whatever the migrator recovered).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExistingCourseDir {
    /// The directory name under the library root.
    pub dir: String,
    /// The course it holds; `id` is `None` until a sync records it.
    pub course: CourseMeta,
}

fn compile(key: &'static str, pattern: &str) -> Result<Option<Regex>, LayoutError> {
    if pattern.trim().is_empty() {
        return Ok(None);
    }
    Regex::new(pattern)
        .map(Some)
        .map_err(|e| LayoutError::InvalidPattern {
            key,
            source: Box::new(e),
        })
}

fn first_capture(re: &Regex, haystack: &str) -> Option<String> {
    let caps = re.captures(haystack)?;
    caps.get(1)
        .or_else(|| caps.get(0))
        .map(|m| m.as_str().to_string())
}

fn parse_template(template: &str) -> Result<Vec<Piece>, LayoutError> {
    let mut pieces = Vec::new();
    let mut rest = template;
    while !rest.is_empty() {
        match rest.find(['{', '}']) {
            None => {
                pieces.push(Piece::Literal(rest.to_string()));
                break;
            }
            Some(i) => {
                if i > 0 {
                    pieces.push(Piece::Literal(rest[..i].to_string()));
                }
                let after = &rest[i..];
                if after.starts_with('}') {
                    return Err(LayoutError::UnbalancedBrace(template.to_string()));
                }
                let close = after
                    .find('}')
                    .ok_or_else(|| LayoutError::UnbalancedBrace(template.to_string()))?;
                let key = &after[1..close];
                pieces.push(match key {
                    "code" => Piece::Code,
                    "name" => Piece::Name,
                    "id" => Piece::Id,
                    other if other.contains('{') => {
                        return Err(LayoutError::UnbalancedBrace(template.to_string()))
                    }
                    other => return Err(LayoutError::UnknownPlaceholder(other.to_string())),
                });
                rest = &after[close + 1..];
            }
        }
    }
    Ok(pieces)
}

/// One course's input to [`disambiguate`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SlugCandidate {
    /// The undisambiguated slug, from [`CourseNaming::course_slug`].
    pub slug: String,
    /// Section / NRC, from [`CourseNaming::section`].
    pub section: Option<String>,
    /// Canvas course id, when known.
    pub id: Option<u64>,
}

/// Make course slugs unique, returning them in input order.
///
/// `taken` holds names already in use (directories of courses not in
/// `candidates`); none of the results is ever one of them. A slug that
/// collides — with another candidate or with `taken` — gets `-s<section>`
/// (`-c<id>` when the section is unknown, nothing when both are). A slug
/// that collides with nothing is left alone. Whatever still collides is
/// settled with the first free `-2`, `-3`, ... in input order, skipping any
/// name that is taken or produced for another candidate — callers wanting a
/// stable result across runs pass the candidates in a stable order (e.g.
/// sorted by id).
pub fn disambiguate(taken: &BTreeSet<String>, candidates: &[SlugCandidate]) -> Vec<String> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for c in candidates {
        *counts.entry(c.slug.as_str()).or_default() += 1;
    }
    let suffixed: Vec<String> = candidates
        .iter()
        .map(|c| {
            let collides =
                counts.get(c.slug.as_str()).copied().unwrap_or(0) > 1 || taken.contains(&c.slug);
            if !collides {
                return c.slug.clone();
            }
            match (&c.section, c.id) {
                (Some(s), _) => format!("{}-s{s}", c.slug),
                (None, Some(id)) => format!("{}-c{id}", c.slug),
                (None, None) => c.slug.clone(),
            }
        })
        .collect();

    let mut totals: BTreeMap<&str, usize> = BTreeMap::new();
    for s in &suffixed {
        *totals.entry(s.as_str()).or_default() += 1;
    }
    // Names nobody else may get: the taken ones, and every suffixed name
    // that is already unique.
    let mut used: BTreeSet<String> = taken.clone();
    used.extend(
        suffixed
            .iter()
            .filter(|s| totals.get(s.as_str()).copied().unwrap_or(0) == 1)
            .cloned(),
    );
    suffixed
        .iter()
        .map(|s| {
            let unique = totals.get(s.as_str()).copied().unwrap_or(0) == 1;
            if unique && !taken.contains(s) {
                return s.clone();
            }
            if !used.contains(s) {
                used.insert(s.clone());
                return s.clone();
            }
            let mut n = 2usize;
            while used.contains(&format!("{s}-{n}")) {
                n += 1;
            }
            let name = format!("{s}-{n}");
            used.insert(name.clone());
            name
        })
        .collect()
}

/// Lowercase kebab-case ASCII: transliterated, every run of anything but
/// `[a-z0-9]` collapsed to one `-`, trimmed, and capped at [`MAX_SLUG_LEN`]
/// on a `-` boundary when one is near. May return an empty string.
pub fn slugify(input: &str) -> String {
    let slug = ascii_skeleton(input).to_ascii_lowercase().replace('_', "-");
    if slug.len() <= MAX_SLUG_LEN {
        return slug;
    }
    // ASCII only, so any byte index is a char boundary.
    let cut = &slug[..MAX_SLUG_LEN];
    let cut = match cut.rfind('-') {
        Some(i) if i >= MAX_SLUG_LEN / 2 => &cut[..i],
        _ => cut,
    };
    cut.trim_end_matches('-').to_string()
}

fn slug_or_untitled(input: &str) -> String {
    let s = slugify(input);
    if s.is_empty() {
        UNTITLED.to_string()
    } else {
        s
    }
}

fn ymd(date: NaiveDate) -> String {
    format!("{:04}-{:02}-{:02}", date.year(), date.month(), date.day())
}

/// A module directory under [`MODULES_DIR`]: `NN-<slug>`, with `NN` the
/// module position zero-padded to two digits (`01-informacion-general`).
pub fn module_dir(position: u32, name: &str) -> String {
    format!("{position:02}-{}", slug_or_untitled(name))
}

/// A page inside its module directory: `NN-<slug>.md`, `NN` being the item's
/// position within the module.
pub fn page_file(position: u32, title: &str) -> String {
    format!("{position:02}-{}.md", slug_or_untitled(title))
}

/// An assignment under [`ASSIGNMENTS_DIR`]: `<slug>.md`.
pub fn assignment_file(title: &str) -> String {
    format!("{}.md", slug_or_untitled(title))
}

/// An announcement under [`ANNOUNCEMENTS_DIR`]: `YYYY-MM-DD_<slug>.md`.
pub fn announcement_file(posted: NaiveDate, title: &str) -> String {
    format!("{}_{}.md", ymd(posted), slug_or_untitled(title))
}

/// An attachment under [`ATTACHMENTS_DIR`]: the stem slugified, the last
/// extension lowercased and kept (`Guía 1.PDF` -> `guia-1.pdf`).
pub fn attachment_file(original: &str) -> String {
    let original = original.trim();
    let (stem, ext) = match original.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, ext),
        _ => (original, ""),
    };
    let ext: String = deunicode::deunicode(ext)
        .to_ascii_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    let stem = slug_or_untitled(stem);
    if ext.is_empty() {
        stem
    } else {
        format!("{stem}.{ext}")
    }
}

/// A recording under [`RECORDINGS_DIR`]:
/// `YYYY-MM-DD_<topic>[-parte-N].mp4`.
///
/// The course name is removed from the topic (Zoom topics usually repeat
/// it), and [`DEFAULT_RECORDING_TOPIC`] is used when nothing is left — or
/// put in front when only numbers are left (`clase-1`). Pass
/// the Canvas course name, or the v1 course directory name — both work,
/// since what is removed is the longest run of words the two share, as long
/// as it is at least two words or the whole course name. `part` is the
/// 1-based part number; `None` (the first or only part) adds no suffix.
pub fn recording_file(
    start: NaiveDate,
    topic: &str,
    part: Option<u32>,
    course_name: &str,
) -> String {
    let topic = strip_course_name(&slugify(topic), &slugify(course_name));
    let topic = if topic.is_empty() {
        DEFAULT_RECORDING_TOPIC.to_string()
    } else if topic
        .split('-')
        .all(|t| t.bytes().all(|b| b.is_ascii_digit()))
    {
        // Only a number is left (`MECANICA_1` minus the course name): keep
        // it, but say what it numbers.
        format!("{DEFAULT_RECORDING_TOPIC}-{topic}")
    } else {
        topic
    };
    match part {
        Some(n) => format!("{}_{topic}-parte-{n}.mp4", ymd(start)),
        None => format!("{}_{topic}.mp4", ymd(start)),
    }
}

fn strip_course_name(topic: &str, course: &str) -> String {
    let course: Vec<&str> = course.split('-').filter(|t| !t.is_empty()).collect();
    let mut tokens: Vec<&str> = topic.split('-').filter(|t| !t.is_empty()).collect();
    if course.is_empty() {
        return tokens.join("-");
    }
    while let Some((start, len)) = longest_shared_run(&tokens, &course) {
        if len < 2 && len < course.len() {
            break;
        }
        tokens.drain(start..start + len);
    }
    tokens.join("-")
}

/// Longest run of `a` that also appears contiguously in `b`, as
/// `(start in a, length)`; the earliest one in `a` on a tie.
fn longest_shared_run(a: &[&str], b: &[&str]) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    for i in 0..a.len() {
        for j in 0..b.len() {
            let len = a[i..]
                .iter()
                .zip(&b[j..])
                .take_while(|(x, y)| x == y)
                .count();
            if len > best.map_or(0, |(_, l)| l) {
                best = Some((i, len));
            }
        }
    }
    best
}

/// A v1 recording file name, `YYYY_MM_DD_<topic>[_N].mp4`, split up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V1Recording {
    /// The meeting date.
    pub date: NaiveDate,
    /// The topic, as it appears in the file name.
    pub topic: String,
    /// 1-based part number, `None` for the first (unsuffixed) part.
    pub part: Option<u32>,
}

/// Parse a v1 recording name (written by `zoom::headless` before v2).
///
/// v1 appended `_N` (N counting from 1) to the second, third, ... file with
/// the same name, so `_N` is part `N + 1`. A trailing `_N` is read as that
/// suffix only when `siblings` (the other names in the same directory) holds
/// the unsuffixed name — otherwise it is part of the topic, as in
/// `Sesion_sincronica_3`. Returns `None` for names without a leading date
/// (v1's `zoom-<meeting id>` fallback) or not ending in `.mp4`.
pub fn parse_v1_recording(file_name: &str, siblings: &[&str]) -> Option<V1Recording> {
    let stem = file_name.strip_suffix(".mp4")?;
    let mut parts = stem.splitn(4, '_');
    let (y, m, d, rest) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    let date = NaiveDate::from_ymd_opt(y.parse().ok()?, m.parse().ok()?, d.parse().ok()?)?;
    if y.len() != 4 || m.len() != 2 || d.len() != 2 {
        return None;
    }
    if let Some((base, n)) = rest.rsplit_once('_') {
        if let Ok(n) = n.parse::<u32>() {
            let unsuffixed = format!("{y}_{m}_{d}_{base}.mp4");
            if n > 0 && siblings.contains(&unsuffixed.as_str()) {
                return Some(V1Recording {
                    date,
                    topic: base.to_string(),
                    part: Some(n + 1),
                });
            }
        }
    }
    Some(V1Recording {
        date,
        topic: rest.to_string(),
        part: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The commented UNAB example from `assets/config.toml`, read from the
    /// template itself so the two cannot drift apart.
    fn unab() -> CourseNaming {
        let commented: String = include_str!("../assets/config.toml")
            .lines()
            .filter_map(|l| l.strip_prefix("# "))
            .filter(|l| l.contains("_pattern = '"))
            .collect::<Vec<_>>()
            .join("\n");
        let layout: config::Layout = toml::from_str(&commented).unwrap();
        assert!(!layout.section_pattern.is_empty(), "{commented}");
        CourseNaming::new(&layout).unwrap()
    }

    fn generic() -> CourseNaming {
        CourseNaming::new(&config::Layout::default()).unwrap()
    }

    fn course(id: u64, name: &str, code: &str) -> CourseMeta {
        CourseMeta {
            id: Some(id),
            name: name.into(),
            code: Some(code.into()),
        }
    }

    #[test]
    fn parse_v1_course_dir_splits_on_the_code_pattern() {
        let meta =
            unab().parse_v1_course_dir("202615_2690_MINERIA_DE_DATOS_ATDF105_202615_2690_EL_ON");
        assert_eq!(meta.id, None);
        assert_eq!(meta.name, "202615_2690_MINERIA_DE_DATOS");
        assert_eq!(meta.code.as_deref(), Some("ATDF105_202615_2690_EL_ON"));
        assert_eq!(unab().course_slug(&meta), "atdf105-mineria-de-datos");
        assert_eq!(unab().section(&meta).as_deref(), Some("2690"));
    }

    #[test]
    fn parse_v1_course_dir_skips_a_code_shaped_word_of_the_name() {
        let meta = unab()
            .parse_v1_course_dir("202615_2700_PROGRAMACION_EN_PYTHON3_ATDF200_202615_2700_EL_ON");
        assert_eq!(meta.name, "202615_2700_PROGRAMACION_EN_PYTHON3");
        assert_eq!(meta.code.as_deref(), Some("ATDF200_202615_2700_EL_ON"));
        assert_eq!(unab().course_slug(&meta), "atdf200-programacion-en-python3");
    }

    #[test]
    fn parse_v1_course_dir_without_a_split_is_all_name() {
        let meta = unab().parse_v1_course_dir("Apuntes_sueltos");
        assert_eq!(meta.name, "Apuntes_sueltos");
        assert_eq!(meta.code, None);
        let meta = generic().parse_v1_course_dir("Mineria_ATDF105");
        assert_eq!(meta.name, "Mineria_ATDF105");
        assert_eq!(meta.code, None);
        // A code that is the whole directory leaves no name, so no split.
        assert_eq!(unab().parse_v1_course_dir("ATDF105_X").code, None);
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn slugify_table() {
        let cases = [
            ("MINERIA_DE_DATOS", "mineria-de-datos"),
            ("Información General", "informacion-general"),
            ("  Semana 10: Formativa #6 ", "semana-10-formativa-6"),
            ("¡Bienvenidos al curso!", "bienvenidos-al-curso"),
            ("a--b__c  d", "a-b-c-d"),
            ("Ñandú", "nandu"),
            ("", ""),
            ("___", ""),
        ];
        for (input, want) in cases {
            assert_eq!(slugify(input), want, "input {input:?}");
        }
    }

    #[test]
    fn slugify_caps_length_on_a_dash_boundary() {
        let long = "palabra ".repeat(30);
        let got = slugify(&long);
        assert!(got.len() <= MAX_SLUG_LEN, "{got}");
        assert!(got.ends_with("palabra"), "{got}");
    }

    #[test]
    fn unab_course_slug_drops_period_and_nrc() {
        let cases = [
            (
                course(
                    1,
                    "202615_2690_MINERIA DE DATOS",
                    "ATDF105_202615_2690_EL_ON",
                ),
                "atdf105-mineria-de-datos",
            ),
            (
                course(
                    2,
                    "202615_2690_MINERIA_DE_DATOS",
                    "ATDF105_202615_2690_EL_ON",
                ),
                "atdf105-mineria-de-datos",
            ),
        ];
        for (c, want) in cases {
            assert_eq!(unab().course_slug(&c), want, "{c:?}");
        }
    }

    #[test]
    fn generic_default_is_code_then_name() {
        let n = generic();
        assert_eq!(
            n.course_slug(&course(1, "Intro to CS", "CS101")),
            "cs101-intro-to-cs"
        );
        let no_code = CourseMeta {
            id: Some(7),
            name: "Estudio Independiente".into(),
            code: None,
        };
        assert_eq!(n.course_slug(&no_code), "estudio-independiente");
        let nameless = CourseMeta {
            id: Some(7),
            ..Default::default()
        };
        assert_eq!(n.course_slug(&nameless), "curso-7");
        assert_eq!(n.course_slug(&CourseMeta::default()), "curso");
    }

    #[test]
    fn id_placeholder_and_literals_render() {
        let n = CourseNaming::new(&config::Layout {
            course_dir: "Ramo {id} - {name}".into(),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            n.course_slug(&course(42, "Álgebra", "X")),
            "ramo-42-algebra"
        );
    }

    #[test]
    fn bad_layout_settings_are_rejected() {
        let bad = |course_dir: &str, code_pattern: &str| {
            CourseNaming::new(&config::Layout {
                course_dir: course_dir.into(),
                code_pattern: code_pattern.into(),
                ..Default::default()
            })
            .unwrap_err()
        };
        assert!(matches!(bad("{codes}", ""), LayoutError::UnknownPlaceholder(k) if k == "codes"));
        assert!(matches!(bad("{code", ""), LayoutError::UnbalancedBrace(_)));
        assert!(matches!(bad("code}", ""), LayoutError::UnbalancedBrace(_)));
        assert!(matches!(
            bad("{{code}", ""),
            LayoutError::UnbalancedBrace(_)
        ));
        assert!(matches!(
            bad("{code}", "("),
            LayoutError::InvalidPattern {
                key: "code_pattern",
                ..
            }
        ));
    }

    #[test]
    fn colliding_sections_get_nrc_suffix_and_others_do_not() {
        let courses = [
            course(10, "202615_2520_ELECTIVO", "ATDF106_202615_2520_EL_ON"),
            course(11, "202615_2524_ELECTIVO", "ATDF106_202615_2524_EL_ON"),
            course(
                12,
                "202615_2690_MINERIA DE DATOS",
                "ATDF105_202615_2690_EL_ON",
            ),
        ];
        assert_eq!(
            unab().course_dirs(&[], &courses),
            [
                "atdf106-electivo-s2520",
                "atdf106-electivo-s2524",
                "atdf105-mineria-de-datos"
            ]
        );
    }

    fn existing(dir: &str, course: CourseMeta) -> ExistingCourseDir {
        ExistingCourseDir {
            dir: dir.into(),
            course,
        }
    }

    #[test]
    fn a_retaken_course_never_reuses_or_renames_the_archived_one() {
        let n = unab();
        let old = course(
            1,
            "202515_1033_MINERIA DE DATOS",
            "ATDF105_202515_1033_EL_ON",
        );
        let new = course(
            2,
            "202615_2690_MINERIA DE DATOS",
            "ATDF105_202615_2690_EL_ON",
        );

        // First run: only the 2025 course exists.
        assert_eq!(
            n.course_dirs(&[], std::slice::from_ref(&old)),
            ["atdf105-mineria-de-datos"]
        );
        let lib = [existing("atdf105-mineria-de-datos", old.clone())];

        // A later sync sees only active courses: the retake avoids the
        // archived directory instead of landing in it.
        assert_eq!(
            n.course_dirs(&lib, std::slice::from_ref(&new)),
            ["atdf105-mineria-de-datos-s2690"]
        );
        // Seeing both, the archived course keeps its name.
        assert_eq!(
            n.course_dirs(&lib, &[new.clone(), old.clone()]),
            ["atdf105-mineria-de-datos-s2690", "atdf105-mineria-de-datos"]
        );
        // And once the retake has its directory, both are stable.
        let lib = [
            existing("atdf105-mineria-de-datos", old.clone()),
            existing("atdf105-mineria-de-datos-s2690", new.clone()),
        ];
        assert_eq!(
            n.course_dirs(&lib, &[old, new]),
            ["atdf105-mineria-de-datos", "atdf105-mineria-de-datos-s2690"]
        );
    }

    #[test]
    fn a_directory_without_a_recorded_id_is_claimed_by_name_and_code() {
        let n = unab();
        let c = course(10, "202615_2520_ELECTIVO", "ATDF106_202615_2520_EL_ON");
        let unidentified = CourseMeta {
            id: None,
            ..c.clone()
        };
        let lib = [existing("atdf106-electivo-s2520", unidentified)];
        assert_eq!(n.course_dirs(&lib, &[c]), ["atdf106-electivo-s2520"]);
    }

    #[test]
    fn section_falls_back_to_the_name_with_the_shipped_pattern() {
        let n = unab();
        // No code at all: the NRC is still in the real Canvas name.
        let from_name = CourseMeta {
            id: None,
            name: "202615_2520_ELECTIVO".into(),
            code: None,
        };
        assert_eq!(n.section(&from_name).as_deref(), Some("2520"));
        assert_eq!(
            n.section(&course(1, "x", "ATDF106_202615_2524_EL_ON"))
                .as_deref(),
            Some("2524")
        );
        assert_eq!(generic().section(&course(1, "x", "y")), None);
    }

    fn cand(slug: &str, section: Option<&str>, id: Option<u64>) -> SlugCandidate {
        SlugCandidate {
            slug: slug.into(),
            section: section.map(str::to_string),
            id,
        }
    }

    fn all_distinct(names: &[String]) -> bool {
        names.iter().collect::<BTreeSet<_>>().len() == names.len()
    }

    #[test]
    fn disambiguation_falls_back_to_id_then_to_a_counter() {
        let cands = [
            cand("a", None, Some(1)),
            cand("a", None, Some(2)),
            cand("b", None, None),
            cand("b", None, None),
            cand("c", Some("9"), None),
        ];
        assert_eq!(
            disambiguate(&BTreeSet::new(), &cands),
            ["a-c1", "a-c2", "b", "b-2", "c"]
        );
    }

    #[test]
    fn the_counter_skips_names_already_produced_or_taken() {
        let none = BTreeSet::new();
        let got = disambiguate(
            &none,
            &[
                cand("b", None, None),
                cand("b", None, None),
                cand("b-2", None, None),
            ],
        );
        assert_eq!(got, ["b", "b-3", "b-2"]);

        let got = disambiguate(
            &none,
            &[
                cand("x", Some("1"), Some(1)),
                cand("x", Some("1"), Some(2)),
                cand("x-s1-2", None, Some(3)),
            ],
        );
        assert!(all_distinct(&got), "{got:?}");
        assert_eq!(got, ["x-s1", "x-s1-3", "x-s1-2"]);

        let taken: BTreeSet<String> = ["b".to_string(), "b-2".to_string()].into();
        let got = disambiguate(&taken, &[cand("b", None, None), cand("b", None, None)]);
        assert_eq!(got, ["b-3", "b-4"]);
    }

    #[test]
    fn module_page_assignment_announcement_attachment_names() {
        assert_eq!(
            module_dir(1, "Información General"),
            "01-informacion-general"
        );
        assert_eq!(module_dir(12, ""), "12-sin-titulo");
        assert_eq!(
            page_file(1, "Información General"),
            "01-informacion-general.md"
        );
        assert_eq!(
            assignment_file("Semana 10 - Formativa 6"),
            "semana-10-formativa-6.md"
        );
        assert_eq!(
            announcement_file(date(2026, 6, 17), "¡Bienvenidos al curso!"),
            "2026-06-17_bienvenidos-al-curso.md"
        );
        assert_eq!(attachment_file("Guía 1.PDF"), "guia-1.pdf");
        assert_eq!(attachment_file("Coder S3.zip"), "coder-s3.zip");
        assert_eq!(attachment_file("README"), "readme");
        assert_eq!(attachment_file(".hidden"), "hidden");
        assert_eq!(attachment_file("archivo.tar.gz"), "archivo-tar.gz");
    }

    #[test]
    fn v1_recordings_map_to_v2_names() {
        let dir = [
            "2026_03_11_202605_2333_DISRUPCION_EMPRESARIAL.mp4",
            "2026_03_11_202605_2333_DISRUPCION_EMPRESARIAL_1.mp4",
            "2026_10_14_202625_1108_RESPONSABILIDAD_SOCIAL_SEMANA_3.mp4",
            "2026_07_02_Sesion_sincronica_3.mp4",
        ];
        let cases = [
            (
                dir[0],
                "202605_2333_DISRUPCION EMPRESARIAL",
                "2026-03-11_clase.mp4",
            ),
            (
                dir[1],
                "202605_2333_DISRUPCION EMPRESARIAL",
                "2026-03-11_clase-parte-2.mp4",
            ),
            // The v1 directory name works as well as the Canvas name.
            (
                dir[1],
                "202605_2333_DISRUPCION_EMPRESARIAL_ATDF101_202605_2333_EL_ON",
                "2026-03-11_clase-parte-2.mp4",
            ),
            (
                dir[2],
                "202625_1108_RESPONSABILIDAD SOCIAL",
                "2026-10-14_semana-3.mp4",
            ),
            (
                dir[3],
                "202615_2690_MINERIA DE DATOS",
                "2026-07-02_sesion-sincronica-3.mp4",
            ),
        ];
        for (file, course_name, want) in cases {
            let rec = parse_v1_recording(file, &dir).unwrap();
            assert_eq!(
                recording_file(rec.date, &rec.topic, rec.part, course_name),
                want,
                "{file}"
            );
        }
    }

    #[test]
    fn parse_v1_recording_reads_suffix_only_when_the_base_exists() {
        let lone = parse_v1_recording("2026_07_02_Sesion_sincronica_3.mp4", &[]).unwrap();
        assert_eq!(lone.topic, "Sesion_sincronica_3");
        assert_eq!(lone.part, None);
        assert_eq!(parse_v1_recording("zoom-abc==.mp4", &[]), None);
        assert_eq!(parse_v1_recording("2026_07_02_x.mkv", &[]), None);
        assert_eq!(parse_v1_recording("2026_13_02_x.mp4", &[]), None);
    }

    #[test]
    fn recording_topic_keeps_short_shared_words() {
        // "de" alone is not the course name: one shared word is not stripped.
        assert_eq!(
            recording_file(
                date(2026, 1, 5),
                "Clase de repaso",
                None,
                "Mineria de Datos"
            ),
            "2026-01-05_clase-de-repaso.mp4"
        );
        // A one-word course name is stripped when it is the whole name.
        assert_eq!(
            recording_file(
                date(2026, 1, 5),
                "Estadística Clase 4",
                Some(3),
                "Estadística"
            ),
            "2026-01-05_clase-4-parte-3.mp4"
        );
        // Only a number left after the course name: it is a class number.
        let rec = parse_v1_recording("2025_10_01_202525_1149_MECANICA_1.mp4", &[]).unwrap();
        assert_eq!(
            recording_file(rec.date, &rec.topic, rec.part, "202525_1149_MECANICA"),
            "2025-10-01_clase-1.mp4"
        );
        assert_eq!(
            recording_file(date(2026, 1, 5), "", None, ""),
            "2026-01-05_clase.mp4"
        );
    }
}
