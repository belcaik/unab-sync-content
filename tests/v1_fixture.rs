//! The synthetic layout-v1 library. See `tests/fixtures/v1/mod.rs`.
#[path = "fixtures/v1/mod.rs"]
mod v1;

use std::error::Error;
use std::fs;
use std::io;

use tempfile::tempdir;
use u_crawler::layout;

fn written() -> Result<(tempfile::TempDir, v1::V1Library), Box<dyn Error>> {
    let dir = tempdir()?;
    let lib = v1::write_v1_library(dir.path())?;
    Ok((dir, lib))
}

fn course<'a>(lib: &'a v1::V1Library, code: &str) -> Result<&'a v1::V1Course, Box<dyn Error>> {
    lib.course_by_code(code)
        .ok_or_else(|| format!("missing course {code}").into())
}

fn recording<'a>(
    course: &'a v1::V1Course,
    file_name: &str,
) -> Result<&'a v1::V1RecordingFile, Box<dyn Error>> {
    course
        .zoom
        .as_ref()
        .and_then(|zoom| zoom.recordings.iter().find(|r| r.file_name == file_name))
        .ok_or_else(|| format!("missing recording {file_name}").into())
}

#[test]
fn write_v1_library_is_deterministic() -> Result<(), Box<dyn Error>> {
    let left_dir = tempdir()?;
    let right_dir = tempdir()?;
    let left = v1::write_v1_library(left_dir.path())?;
    let right = v1::write_v1_library(right_dir.path())?;
    assert_eq!(left, right);
    Ok(())
}

#[test]
fn manifest_matches_a_fresh_hash_of_the_tree() -> Result<(), Box<dyn Error>> {
    let (dir, lib) = written()?;
    assert_eq!(v1::hash_tree(dir.path())?, lib.manifest);
    assert!(lib.manifest.values().all(|hash| hash.len() == 40));
    Ok(())
}

#[test]
fn write_v1_library_refuses_a_non_empty_root() -> Result<(), Box<dyn Error>> {
    let dir = tempdir()?;
    v1::write_v1_library(dir.path())?;
    let err = v1::write_v1_library(dir.path())
        .err()
        .ok_or("write succeeded")?;
    assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    Ok(())
}

#[test]
fn two_sections_share_a_slug_and_one_has_no_index() -> Result<(), Box<dyn Error>> {
    let (_dir, lib) = written()?;
    let without = course(&lib, "ATDF106_202615_2520_EL_ON")?;
    let with = course(&lib, "ATDF106_202615_2524_EL_ON")?;

    assert!(without.has_announcements_dir);
    assert!(!without.has_index_json);
    assert_eq!(without.meta.id, None);
    assert!(without.zoom.is_none());
    assert_eq!(without.v2_dir, "atdf106-electivo-s2520");

    assert!(with.has_index_json);
    assert_eq!(with.meta.id, Some(9002));
    assert_eq!(with.zoom.as_ref().map(|zoom| zoom.folder_id), Some(9002));
    assert_eq!(with.v2_dir, "atdf106-electivo-s2524");
    Ok(())
}

#[test]
fn a_same_day_suffix_is_part_two_and_keeps_distinct_bytes() -> Result<(), Box<dyn Error>> {
    let (_dir, lib) = written()?;
    let course = course(&lib, "ATDF101_202605_2333_EL_ON")?;
    let base = recording(course, "2026_03_11_202605_2333_DISRUPCION_EMPRESARIAL.mp4")?;
    let second = recording(
        course,
        "2026_03_11_202605_2333_DISRUPCION_EMPRESARIAL_1.mp4",
    )?;
    assert_eq!(base.v2_file_name.as_deref(), Some("2026-03-11_clase.mp4"));
    assert_eq!(
        second.v2_file_name.as_deref(),
        Some("2026-03-11_clase-parte-2.mp4")
    );
    assert_ne!(lib.manifest[&base.rel], lib.manifest[&second.rel]);
    Ok(())
}

#[test]
fn sesion_sincronica_keeps_its_topic_when_no_base_file_exists() -> Result<(), Box<dyn Error>> {
    let (_dir, lib) = written()?;
    let course = course(&lib, "ATDF105_202615_2690_EL_ON")?;
    assert_eq!(course.v2_dir, "atdf105-mineria-de-datos");
    let rec = recording(course, "2026_07_02_Sesion_sincronica_1.mp4")?;
    assert_eq!(
        rec.v2_file_name.as_deref(),
        Some("2026-07-02_sesion-sincronica-1.mp4")
    );
    Ok(())
}

#[test]
fn index_json_carries_the_canvas_course_id() -> Result<(), Box<dyn Error>> {
    let (dir, lib) = written()?;
    let course = course(&lib, "ATDF105_202615_2690_EL_ON")?;
    let rel = course.index_rel.as_deref().ok_or("mineria has no index")?;
    let text = fs::read_to_string(dir.path().join(rel))?;
    assert!(text.contains("https://canvas.example.edu/courses/9001/discussion_topics/501"));
    assert_eq!(course.zoom.as_ref().map(|zoom| zoom.folder_id), Some(9001));
    Ok(())
}

#[test]
fn a_user_note_sits_inside_a_module() -> Result<(), Box<dyn Error>> {
    let (_dir, lib) = written()?;
    let course = course(&lib, "ATDF105_202615_2690_EL_ON")?;
    let rel = course
        .user_note_rel
        .as_deref()
        .ok_or("mineria has no user note")?;
    assert!(rel.contains("/Modules/"));
    assert!(rel.ends_with(&format!("/{}", v1::USER_NOTE_FILE)));
    assert!(!rel.contains("ASSIGN-"));
    assert!(lib.manifest.contains_key(rel));
    Ok(())
}

#[test]
fn capitalised_announcement_media_keeps_its_stem() -> Result<(), Box<dyn Error>> {
    let (_dir, lib) = written()?;
    let course = course(&lib, "ATDF105_202615_2690_EL_ON")?;
    let rel = course
        .capital_media_rel
        .as_deref()
        .ok_or("mineria has no capitalised media")?;
    assert!(rel.ends_with(&format!("/announcements/media/{}", v1::CAPITAL_MEDIA_FILE)));
    assert_eq!(
        course.capital_media_v2_name.as_deref(),
        Some(layout::attachment_file(v1::CAPITAL_MEDIA_FILE).as_str())
    );
    assert_eq!(
        course.capital_media_v2_name.as_deref(),
        Some("coder-s3.zip")
    );
    assert!(lib
        .manifest
        .keys()
        .any(|path| path.ends_with("/Attachments/Guia_1.pdf")));
    Ok(())
}

#[test]
fn courses_without_announcements_carry_the_2025_recording_names() -> Result<(), Box<dyn Error>> {
    let (_dir, lib) = written()?;
    let bare: Vec<_> = lib
        .courses
        .iter()
        .filter(|c| !c.has_announcements_dir)
        .collect();
    assert_eq!(bare.len(), 4);
    assert!(bare.iter().all(|c| c.meta.id.is_none() && c.zoom.is_some()));

    let names = [
        "2025_06_23_202515_1033_ELEMENTOS_DE_FISICA_Y_MEDICION.mp4",
        "2025_06_18_Clase_FMMA015_1226_18_06_2025.mp4",
        "2025_10_06_Sesion_01_FMMA115_NRC_1332_0610.mp4",
        "2025_10_01_202525_1149_MECANICA_1.mp4",
    ];
    for name in names {
        let owner = bare
            .iter()
            .find(|c| recording(c, name).is_ok())
            .ok_or_else(|| format!("no 2025 course holds {name}"))?;
        assert!(owner.meta.id.is_none());
    }

    let mecanica = course(&lib, "MECA100_202525_1149_EL_ON")?;
    let rec = recording(mecanica, "2025_10_01_202525_1149_MECANICA_1.mp4")?;
    assert_eq!(rec.v2_file_name.as_deref(), Some("2025-10-01_clase-1.mp4"));
    Ok(())
}

#[test]
fn catalog_v2_names_come_from_layout() -> Result<(), Box<dyn Error>> {
    let (_dir, lib) = written()?;
    let naming = v1::unab_naming()?;
    let metas: Vec<_> = lib.courses.iter().map(|c| c.meta.clone()).collect();
    let dirs = naming.course_dirs(&[], &metas);
    let got: Vec<&str> = lib.courses.iter().map(|c| c.v2_dir.as_str()).collect();
    assert_eq!(got, dirs.iter().map(String::as_str).collect::<Vec<_>>());

    for course in &lib.courses {
        let Some(zoom) = &course.zoom else {
            continue;
        };
        let names: Vec<&str> = zoom
            .recordings
            .iter()
            .map(|r| r.file_name.as_str())
            .collect();
        for rec in &zoom.recordings {
            let parsed = layout::parse_v1_recording(&rec.file_name, &names)
                .ok_or_else(|| format!("unparsed {}", rec.file_name))?;
            let want =
                layout::recording_file(parsed.date, &parsed.topic, parsed.part, &course.meta.name);
            assert_eq!(rec.v2_file_name.as_deref(), Some(want.as_str()));
        }
    }
    Ok(())
}

#[test]
fn root_foreign_files_and_an_orphan_zoom_stay_unassigned() -> Result<(), Box<dyn Error>> {
    let (dir, lib) = written()?;
    assert_eq!(
        lib.foreign,
        vec![
            v1::OBSIDIAN_REL.to_string(),
            v1::GIT_HEAD_REL.to_string(),
            v1::LOOSE_FILE_REL.to_string(),
        ]
    );
    for rel in &lib.foreign {
        assert!(lib.manifest.contains_key(rel));
        assert!(dir.path().join(rel).is_file());
        assert!(!rel.starts_with("Zoom/"));
        assert!(lib
            .courses
            .iter()
            .all(|course| !rel.starts_with(&format!("{}/", course.v1_dir))));
    }
    assert!(dir.path().join(".obsidian").is_dir());
    assert!(!dir.path().join(".u_crawler").exists());
    assert!(!lib
        .manifest
        .keys()
        .any(|path| path.contains("library.json")));

    assert_eq!(lib.orphan_zoom.folder_id, v1::ORPHAN_ZOOM_ID);
    assert!(lib
        .courses
        .iter()
        .all(|c| { c.zoom.as_ref().map(|zoom| zoom.folder_id) != Some(v1::ORPHAN_ZOOM_ID) }));
    assert!(lib
        .orphan_zoom
        .recordings
        .iter()
        .all(|rec| rec.v2_file_name.is_none()));
    assert!(lib
        .orphan_zoom
        .recordings
        .iter()
        .any(|rec| rec.file_name == "zoom-424242.mp4"));
    assert!(layout::parse_v1_recording("zoom-424242.mp4", &[]).is_none());
    Ok(())
}

#[test]
fn the_tree_has_the_v1_signals() -> Result<(), Box<dyn Error>> {
    let (dir, lib) = written()?;
    assert!(lib.manifest.keys().any(|path| path.contains("/Modules/")));
    assert!(lib.manifest.keys().any(|path| path.contains("-ASSIGN-")));
    assert!(dir.path().join("Zoom/9001").is_dir());
    assert!(dir.path().join("Zoom/424242").is_dir());
    let without = course(&lib, "ATDF106_202615_2520_EL_ON")?;
    assert!(!dir
        .path()
        .join(&without.v1_dir)
        .join("announcements/index.json")
        .exists());
    let bare = course(&lib, "FMMA015_202515_1033_EL_ON")?;
    assert!(!dir.path().join(&bare.v1_dir).join("announcements").exists());
    Ok(())
}
