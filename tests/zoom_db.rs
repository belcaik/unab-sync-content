use std::error::Error;

use chrono::Utc;
use rusqlite::Connection;
use tempfile::tempdir;
use u_crawler::zoom::db::ZoomDb;
use u_crawler::zoom::models::ZoomCookie;

#[test]
fn load_cookies_filters_expired_entries() -> Result<(), Box<dyn Error>> {
    let dir = tempdir()?;
    let db = ZoomDb::new(dir.path())?;
    let now = Utc::now().timestamp();

    let cookies = vec![
        ZoomCookie {
            domain: "applications.zoom.us".into(),
            name: "valid".into(),
            value: "1".into(),
            path: "/".into(),
            expires: Some(now + 3600),
            secure: false,
            http_only: false,
        },
        ZoomCookie {
            domain: "applications.zoom.us".into(),
            name: "session".into(),
            value: "2".into(),
            path: "/".into(),
            expires: Some(0),
            secure: false,
            http_only: false,
        },
        ZoomCookie {
            domain: "applications.zoom.us".into(),
            name: "expired".into(),
            value: "0".into(),
            path: "/".into(),
            expires: Some(now - 3600),
            secure: false,
            http_only: false,
        },
    ];

    db.replace_cookies(&cookies)?;
    let loaded = db.load_cookies()?;

    let names: Vec<&str> = loaded.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"valid"));
    assert!(names.contains(&"session"));
    assert!(!names.contains(&"expired"));

    let conn = Connection::open(dir.path().join("zoom_state.sqlite"))?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM zoom_cookie WHERE name = 'expired'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count, 0);

    Ok(())
}
