use crate::zoom::models::{RecordingListResponse, ZoomCookie, ZoomRecordingFile};
use chrono::Utc;
use rusqlite::{params, Connection};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

pub struct ZoomDb {
    path: PathBuf,
}

impl ZoomDb {
    pub fn new(config_dir: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let path = config_dir.join("zoom_state.sqlite");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let db = Self { path };
        db.init()?;
        Ok(db)
    }

    fn init(&self) -> Result<(), Box<dyn std::error::Error>> {
        let conn = self.connection()?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS zoom_course_scid (
                course_id TEXT PRIMARY KEY,
                scid TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS zoom_cookie (
                host TEXT NOT NULL,
                name TEXT NOT NULL,
                value TEXT NOT NULL,
                path TEXT NOT NULL,
                expires INTEGER,
                secure INTEGER NOT NULL,
                http_only INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY(host, name, path)
            );
            CREATE TABLE IF NOT EXISTS zoom_meetings (
                meeting_id TEXT PRIMARY KEY,
                course_id TEXT NOT NULL,
                payload TEXT NOT NULL,
                fetched_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS zoom_files (
                meeting_id TEXT NOT NULL,
                play_url TEXT NOT NULL,
                payload TEXT NOT NULL,
                fetched_at INTEGER NOT NULL,
                PRIMARY KEY(meeting_id, play_url)
            );
            "#,
        )?;
        Ok(())
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        Connection::open(&self.path)
    }

    pub fn save_scid(&self, course_id: u64, scid: &str) -> Result<(), Box<dyn std::error::Error>> {
        let conn = self.connection()?;
        conn.execute(
            "REPLACE INTO zoom_course_scid(course_id, scid, updated_at) VALUES (?1, ?2, ?3)",
            params![course_id.to_string(), scid, Utc::now().timestamp()],
        )?;
        Ok(())
    }

    pub fn get_scid(&self, course_id: u64) -> Result<Option<String>, Box<dyn std::error::Error>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare("SELECT scid FROM zoom_course_scid WHERE course_id = ?1")?;
        let mut rows = stmt.query(params![course_id.to_string()])?;
        if let Some(row) = rows.next()? {
            let scid: String = row.get(0)?;
            Ok(Some(scid))
        } else {
            Ok(None)
        }
    }

    pub fn replace_cookies(
        &self,
        cookies: &[ZoomCookie],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM zoom_cookie", [])?;
        for cookie in cookies {
            tx.execute(
                "INSERT INTO zoom_cookie(host, name, value, path, expires, secure, http_only, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    cookie.domain,
                    cookie.name,
                    cookie.value,
                    cookie.path,
                    cookie.expires,
                    if cookie.secure { 1 } else { 0 },
                    if cookie.http_only { 1 } else { 0 },
                    Utc::now().timestamp(),
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn load_cookies(&self) -> Result<Vec<ZoomCookie>, Box<dyn std::error::Error>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT host, name, value, path, expires, secure, http_only FROM zoom_cookie",
        )?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(ZoomCookie {
                domain: row.get(0)?,
                name: row.get(1)?,
                value: row.get(2)?,
                path: row.get(3)?,
                expires: row.get::<_, Option<i64>>(4)?,
                secure: row.get::<_, i64>(5)? != 0,
                http_only: row.get::<_, i64>(6)? != 0,
            });
        }
        Ok(out)
    }

    pub fn save_meetings(
        &self,
        course_id: u64,
        response: &RecordingListResponse,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        if let Some(result) = &response.result {
            if let Some(list) = &result.list {
                tx.execute(
                    "DELETE FROM zoom_meetings WHERE course_id = ?1",
                    params![course_id.to_string()],
                )?;
                for summary in list {
                    let payload = serde_json::to_string(summary)?;
                    tx.execute(
                        "REPLACE INTO zoom_meetings(meeting_id, course_id, payload, fetched_at)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![
                            summary.meeting_id,
                            course_id.to_string(),
                            payload,
                            Utc::now().timestamp(),
                        ],
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn load_meeting_payloads(
        &self,
        course_id: u64,
    ) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare("SELECT payload FROM zoom_meetings WHERE course_id = ?1")?;
        let mut rows = stmt.query(params![course_id.to_string()])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let payload: String = row.get(0)?;
            out.push(serde_json::from_str(&payload)?);
        }
        Ok(out)
    }

    pub fn save_files(
        &self,
        _course_id: u64,
        meeting_id: &str,
        files: &[ZoomRecordingFile],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM zoom_files WHERE meeting_id = ?1",
            params![meeting_id],
        )?;
        for file in files {
            let mut enriched = file.clone();
            // ensure meeting_id matches
            enriched.meeting_id = meeting_id.to_string();
            let payload = serde_json::to_string(&enriched)?;
            tx.execute(
                "INSERT OR REPLACE INTO zoom_files(meeting_id, play_url, payload, fetched_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    meeting_id,
                    enriched.play_url,
                    payload,
                    Utc::now().timestamp(),
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn load_files(
        &self,
        course_id: u64,
    ) -> Result<Vec<ZoomRecordingFile>, Box<dyn std::error::Error>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT f.payload FROM zoom_files f
             JOIN zoom_meetings m ON m.meeting_id = f.meeting_id
             WHERE m.course_id = ?1",
        )?;
        let mut rows = stmt.query(params![course_id.to_string()])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let payload: String = row.get(0)?;
            out.push(serde_json::from_str(&payload)?);
        }
        Ok(out)
    }
}
