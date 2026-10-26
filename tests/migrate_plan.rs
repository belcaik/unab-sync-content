//! `migrate::detect` and `migrate::plan` over the synthetic v1 library (T1).
#[path = "fixtures/v1/mod.rs"]
mod v1;

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::path::Path;

use tempfile::tempdir;
use u_crawler::migrate::detect::{detect, Detected};
use u_crawler::migrate::plan::{plan, EditKind, Plan, Reason, Root};
use u_crawler::migrate::{scan, Inventory};

const GOLDEN: &str = "tests/golden/migrate_plan_v1.txt";

fn fixture() -> Result<(tempfile::TempDir, v1::V1Library, Inventory, Plan), Box<dyn Error>> {
    let dir = tempdir()?;
    let lib = v1::write_v1_library(dir.path())?;
    let inv = scan(dir.path(), None)?;
    let plan = plan(&inv, &v1::unab_naming()?, &[]);
    Ok((dir, lib, inv, plan))
}

#[test]
fn plan_matches_the_golden_snapshot() -> Result<(), Box<dyn Error>> {
    let (_dir, _lib, _inv, plan) = fixture()?;
    let got = plan.to_string();
    if std::env::var_os("U_CRAWLER_BLESS").is_some() {
        fs::write(Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN), &got)?;
    }
    let want = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN))?;
    assert_eq!(
        got, want,
        "plan differs from {GOLDEN}; rerun with U_CRAWLER_BLESS=1 to accept"
    );
    Ok(())
}

#[test]
fn the_fixture_is_detected_as_v1() -> Result<(), Box<dyn Error>> {
    let (_dir, lib, inv, _plan) = fixture()?;
    let Detected::V1(signals) = detect(&inv) else {
        return Err("fixture not detected as v1".into());
    };
    let mut zoom: Vec<u64> = lib
        .courses
        .iter()
        .filter_map(|c| c.zoom.as_ref().map(|z| z.folder_id))
        .chain([lib.orphan_zoom.folder_id])
        .collect();
    zoom.sort_unstable();
    assert_eq!(signals.zoom_dirs, zoom);
    assert_eq!(signals.module_dirs, 9);
    assert_eq!(signals.assignment_files, 3);
    assert_eq!(signals.announcement_dirs, 4);
    Ok(())
}

#[test]
fn courses_get_the_catalog_directories_and_ids() -> Result<(), Box<dyn Error>> {
    let (_dir, lib, _inv, plan) = fixture()?;
    assert_eq!(plan.courses.len(), lib.courses.len());
    for want in &lib.courses {
        let got = plan
            .courses
            .iter()
            .find(|c| c.v1_dir == want.v1_dir)
            .ok_or_else(|| format!("course {} not planned", want.v1_dir))?;
        assert_eq!(got.dir, want.v2_dir, "{}", want.v1_dir);
        assert_eq!(got.course.id, want.meta.id, "{}", want.v1_dir);
        let zoom: Vec<u64> = want.zoom.iter().map(|z| z.folder_id).collect();
        assert_eq!(got.zoom_ids, zoom, "{}", want.v1_dir);
    }
    Ok(())
}

#[test]
fn recordings_get_the_catalog_names() -> Result<(), Box<dyn Error>> {
    let (_dir, lib, _inv, plan) = fixture()?;
    let moves = library_moves(&plan);
    for course in &lib.courses {
        for rec in course.zoom.iter().flat_map(|z| &z.recordings) {
            let name = rec
                .v2_file_name
                .as_deref()
                .ok_or("catalog recording without v2 name")?;
            let want = format!("{}/grabaciones/{name}", course.v2_dir);
            assert_eq!(
                moves.get(rec.rel.as_str()),
                Some(&want.as_str()),
                "{}",
                rec.rel
            );
        }
    }
    let orphan = plan
        .unresolved
        .iter()
        .find(|u| u.path == lib.orphan_zoom.rel)
        .ok_or("orphan zoom folder not reported")?;
    assert_eq!(orphan.reason, Reason::OrphanZoom);
    for rec in &lib.orphan_zoom.recordings {
        assert!(!moves.contains_key(rec.rel.as_str()), "{}", rec.rel);
    }
    Ok(())
}

#[test]
fn foreign_files_travel_with_their_folder_or_stay() -> Result<(), Box<dyn Error>> {
    let (_dir, lib, _inv, plan) = fixture()?;
    let moves = library_moves(&plan);
    for foreign in &lib.foreign {
        assert!(!moves.contains_key(foreign.as_str()), "{foreign} moved");
        assert!(!plan.unresolved.iter().any(|u| &u.path == foreign));
    }
    let mineria = lib
        .course_by_code("ATDF105_202615_2690_EL_ON")
        .ok_or("missing mineria")?;
    let note = mineria.user_note_rel.as_deref().ok_or("no user note")?;
    assert_eq!(
        moves.get(note),
        Some(&"atdf105-mineria-de-datos/modulos/01-informacion-general/nota-del-usuario.md")
    );
    let media = mineria.capital_media_rel.as_deref().ok_or("no media")?;
    let media_name = mineria
        .capital_media_v2_name
        .as_deref()
        .ok_or("no v2 media")?;
    assert_eq!(
        moves.get(media),
        Some(&format!("atdf105-mineria-de-datos/anuncios/adjuntos/{media_name}").as_str())
    );
    // The user note is not u_crawler's: it is moved, never rewritten.
    assert!(!plan
        .edits
        .iter()
        .any(|e| e.path.ends_with(v1::USER_NOTE_FILE)));
    assert!(plan.edits.iter().any(|e| e.kind == EditKind::Assignment
        && e.path == "atdf105-mineria-de-datos/tareas/semana-10-formativa-6.md"));
    Ok(())
}

/// Two Canvas items that want one v2 name are numbered in id / `NN` order,
/// never reported as a duplicate or conflict.
#[test]
fn distinct_items_with_one_name_are_numbered() -> Result<(), Box<dyn Error>> {
    let (_dir, lib, _inv, plan) = fixture()?;
    let moves = library_moves(&plan);
    let disrupcion = lib
        .course_by_code("ATDF101_202605_2333_EL_ON")
        .ok_or("missing disrupcion")?;
    let mineria = lib
        .course_by_code("ATDF105_202615_2690_EL_ON")
        .ok_or("missing mineria")?;
    let v1 = |course: &v1::V1Course, rest: &str| format!("{}/{rest}", course.v1_dir);
    let v2 = |course: &v1::V1Course, rest: &str| format!("{}/{rest}", course.v2_dir);
    let twin = v1::TWIN_ANNOUNCEMENT_ID;
    let want = [
        (
            v1(
                disrupcion,
                "announcements/2026-03-01_Inicio_de_clases_801.md",
            ),
            v2(disrupcion, "anuncios/2026-03-01_inicio-de-clases.md"),
        ),
        (
            v1(
                disrupcion,
                &format!("announcements/2026-03-01_Inicio_de_clases_{twin}.md"),
            ),
            v2(disrupcion, "anuncios/2026-03-01_inicio-de-clases-2.md"),
        ),
        (
            v1(
                mineria,
                "Modules/200_Semana_1/04-ASSIGN-Semana_11_Sumativa_3_Informe_de_caso_3.md",
            ),
            v2(mineria, "tareas/semana-11-sumativa-3-informe-de-caso-3.md"),
        ),
        (
            v1(
                mineria,
                "Modules/200_Semana_1/06-ASSIGN-Semana_11_Sumativa_3_Informe_de_caso_3.md",
            ),
            v2(
                mineria,
                "tareas/semana-11-sumativa-3-informe-de-caso-3-2.md",
            ),
        ),
    ];
    for (from, to) in &want {
        assert!(lib.manifest.contains_key(from), "fixture lacks {from}");
        assert_eq!(moves.get(from.as_str()), Some(&to.as_str()), "{from}");
        assert!(plan.edits.iter().any(|e| &e.path == to), "no edit for {to}");
    }
    assert!(plan.duplicates.is_empty() && plan.conflicts.is_empty());
    Ok(())
}

#[test]
fn every_file_is_accounted_for_and_no_destination_is_shared() -> Result<(), Box<dyn Error>> {
    let (_dir, lib, _inv, plan) = fixture()?;
    assert!(plan.duplicates.is_empty() && plan.conflicts.is_empty());
    let moves = library_moves(&plan);
    let foreign: BTreeSet<&str> = lib.foreign.iter().map(String::as_str).collect();
    for path in lib.manifest.keys() {
        let left = plan
            .unresolved
            .iter()
            .any(|u| path.starts_with(&format!("{}/", u.path)));
        assert!(
            moves.contains_key(path.as_str()) || foreign.contains(path.as_str()) || left,
            "{path} is neither moved, foreign nor unresolved"
        );
    }
    for from in moves.keys() {
        assert!(
            lib.manifest.contains_key(*from),
            "{from} is not in the library"
        );
    }
    let targets: BTreeSet<&str> = moves.values().copied().collect();
    assert_eq!(targets.len(), moves.len(), "two moves share a destination");
    for to in &targets {
        assert!(!lib.manifest.contains_key(*to), "{to} already exists");
    }
    Ok(())
}

#[test]
fn the_plan_is_deterministic() -> Result<(), Box<dyn Error>> {
    let (_a, _, _, left) = fixture()?;
    let (_b, _, _, right) = fixture()?;
    assert_eq!(left, right);
    assert_eq!(left.to_string(), right.to_string());
    Ok(())
}

/// Carry the plan out with plain renames, in the temp dir only, then plan again.
#[test]
fn a_migrated_library_plans_nothing_and_lost_nothing() -> Result<(), Box<dyn Error>> {
    let (dir, lib, _inv, plan) = fixture()?;
    for m in &plan.moves {
        assert_eq!(m.root, Root::Library);
        let to = dir.path().join(&m.to);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        assert!(!to.exists(), "{} exists", m.to);
        fs::rename(dir.path().join(&m.from), &to)?;
    }

    let after = v1::hash_tree(dir.path())?;
    assert_eq!(
        multiset(&after),
        multiset(&lib.manifest),
        "content was lost"
    );
    for foreign in &lib.foreign {
        assert_eq!(after.get(foreign), lib.manifest.get(foreign), "{foreign}");
    }

    let inv = scan(dir.path(), None)?;
    assert_eq!(detect(&inv), Detected::V1(orphan_only(&lib)));
    let again = plan_of(&inv)?;
    assert!(again.is_empty(), "second plan is not empty:\n{again}");
    assert!(again.courses.is_empty());
    assert_eq!(again.unresolved.len(), 1);

    fs::create_dir_all(dir.path().join(".u_crawler"))?;
    fs::write(
        dir.path().join(".u_crawler/library.json"),
        "{\"layout_version\":2}\n",
    )?;
    assert_eq!(detect(&scan(dir.path(), None)?), Detected::V2);
    Ok(())
}

fn plan_of(inv: &Inventory) -> Result<Plan, Box<dyn Error>> {
    Ok(plan(inv, &v1::unab_naming()?, &[]))
}

fn orphan_only(lib: &v1::V1Library) -> u_crawler::migrate::detect::V1Signals {
    u_crawler::migrate::detect::V1Signals {
        zoom_dirs: vec![lib.orphan_zoom.folder_id],
        ..Default::default()
    }
}

fn library_moves(plan: &Plan) -> BTreeMap<&str, &str> {
    plan.moves
        .iter()
        .filter(|m| m.root == Root::Library)
        .map(|m| (m.from.as_str(), m.to.as_str()))
        .collect()
}

fn multiset(manifest: &BTreeMap<String, String>) -> BTreeMap<&str, usize> {
    let mut out = BTreeMap::new();
    for hash in manifest.values() {
        *out.entry(hash.as_str()).or_insert(0) += 1;
    }
    out
}
