//! Which layout a library is in, from its [`Inventory`] alone.
//!
//! A library with `.u_crawler/library.json` is v2, whatever else it holds.
//! Without it, any v1 signal makes it v1: a `Zoom/<digits>/` folder, a
//! `Modules/<id>_<Name>/` directory, an `NN-ASSIGN-<Title>.md` file or an
//! `announcements/` directory inside a course. A library with neither (new,
//! or holding only foreign files) is [`Detected::Empty`]: nothing to migrate.

use super::{library_json_rel, Inventory, V1_ANNOUNCEMENTS_DIR, V1_MODULES_DIR, V1_ZOOM_DIR};
use std::collections::BTreeSet;

/// The layout a library is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Detected {
    /// `.u_crawler/library.json` is present.
    V2,
    /// No `library.json`, and at least one v1 signal.
    V1(V1Signals),
    /// No `library.json` and no v1 signal.
    Empty,
}

/// What gave a v1 library away. Counts are of distinct directories or files.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct V1Signals {
    /// The numeric folder names under `Zoom/`, ascending.
    pub zoom_dirs: Vec<u64>,
    /// `<course>/Modules/<id>_<Name>/` directories.
    pub module_dirs: usize,
    /// `NN-ASSIGN-<Title>.md` files inside a module directory.
    pub assignment_files: usize,
    /// `<course>/announcements/` directories.
    pub announcement_dirs: usize,
}

impl V1Signals {
    fn any(&self) -> bool {
        !self.zoom_dirs.is_empty()
            || self.module_dirs > 0
            || self.assignment_files > 0
            || self.announcement_dirs > 0
    }
}

/// Tell the layout of the library `inv` was scanned from.
pub fn detect(inv: &Inventory) -> Detected {
    if inv.files.contains_key(&library_json_rel()) {
        return Detected::V2;
    }
    let courses = inv.v1_course_dirs();
    let mut zoom = BTreeSet::new();
    let mut modules = BTreeSet::new();
    let mut announcements = BTreeSet::new();
    let mut assignments = 0;
    for path in inv.files.keys() {
        let parts: Vec<&str> = path.split('/').collect();
        match parts.as_slice() {
            [V1_ZOOM_DIR, id, _, ..] => {
                if let Some(id) = numeric(id) {
                    zoom.insert(id);
                }
            }
            [course, V1_MODULES_DIR, module, rest @ ..]
                if courses.contains(course) && module_id(module).is_some() =>
            {
                modules.insert((*course, *module));
                if let [file] = rest {
                    assignments += usize::from(is_assignment_file(file));
                }
            }
            [course, V1_ANNOUNCEMENTS_DIR, _, ..] if courses.contains(course) => {
                announcements.insert(*course);
            }
            _ => {}
        }
    }
    let signals = V1Signals {
        zoom_dirs: zoom.into_iter().collect(),
        module_dirs: modules.len(),
        assignment_files: assignments,
        announcement_dirs: announcements.len(),
    };
    if signals.any() {
        Detected::V1(signals)
    } else {
        Detected::Empty
    }
}

/// A folder name made of ASCII digits only, as a number.
pub(crate) fn numeric(name: &str) -> Option<u64> {
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    name.parse().ok()
}

/// The module id of a v1 module directory, `<id>_<Name>`.
pub(crate) fn module_id(dir: &str) -> Option<u64> {
    let (id, _) = dir.split_once('_')?;
    numeric(id)
}

/// `NN-ASSIGN-<Title>.md`, the v1 assignment file name.
pub(crate) fn is_assignment_file(name: &str) -> bool {
    name.split_once('-')
        .is_some_and(|(nn, rest)| numeric(nn).is_some() && rest.starts_with("ASSIGN-"))
        && name.ends_with(".md")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inv(paths: &[&str]) -> Inventory {
        let mut inv = Inventory::default();
        for p in paths {
            inv.files.insert((*p).to_string(), 1);
        }
        inv
    }

    #[test]
    fn library_json_wins() {
        let inv = inv(&[".u_crawler/library.json", "Zoom/1/a.mp4", "C_X/state.json"]);
        assert_eq!(detect(&inv), Detected::V2);
    }

    #[test]
    fn each_v1_signal_is_counted() {
        let inv = inv(&[
            "C_X/state.json",
            "C_X/Modules/10_Uno/01-Pagina.md",
            "C_X/Modules/10_Uno/02-ASSIGN-Tarea.md",
            "C_X/Modules/20_Dos/Attachments/a.pdf",
            "C_X/Modules/sin-id/nota.md",
            "C_X/announcements/2026-01-01_Hola_1.md",
            "Zoom/9001/a.mp4",
            "Zoom/notas/a.mp4",
            "Zoom/suelto.mp4",
        ]);
        assert_eq!(
            detect(&inv),
            Detected::V1(V1Signals {
                zoom_dirs: vec![9001],
                module_dirs: 2,
                assignment_files: 1,
                announcement_dirs: 1,
            })
        );
    }

    #[test]
    fn a_v2_or_foreign_tree_is_empty() {
        let v2 = inv(&[
            "atdf105-x/.u_crawler/state.json",
            "atdf105-x/modulos/01-a/01-a.md",
            "atdf105-x/tareas/t.md",
            "INDEX.md",
            ".obsidian/app.json",
        ]);
        assert_eq!(detect(&v2), Detected::Empty);
        assert_eq!(detect(&Inventory::default()), Detected::Empty);
    }

    #[test]
    fn v1_name_helpers() {
        assert_eq!(module_id("123_Semana_1"), Some(123));
        assert_eq!(module_id("Semana_1"), None);
        assert_eq!(module_id("_x"), None);
        assert!(is_assignment_file("02-ASSIGN-Tarea_1.md"));
        assert!(!is_assignment_file("02-Pagina.md"));
        assert!(!is_assignment_file("xx-ASSIGN-Tarea.md"));
        assert!(!is_assignment_file("02-ASSIGN-Tarea.pdf"));
        assert_eq!(numeric("0042"), Some(42));
        assert_eq!(numeric(""), None);
        assert_eq!(numeric("4a"), None);
    }
}
