use crate::zoom::models::{RecordingListResponse, ReplayHeader, ZoomCookie, ZoomRecordingFile};
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
            CREATE TABLE IF NOT EXISTS zoom_request_headers (
                course_id TEXT NOT NULL,
                request_path TEXT NOT NULL,
                header_name TEXT NOT NULL,
                header_value TEXT NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY(course_id, request_path, header_name)
            );
            CREATE TABLE IF NOT EXISTS zoom_replay_headers (
                course_id TEXT NOT NULL,
                referer TEXT NOT NULL,
                download_url TEXT NOT NULL,
                headers TEXT NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY(course_id, referer)
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
        println!("DB: Saved scid for course {}", course_id);
        Ok(())
    }

    pub fn get_scid(&self, course_id: u64) -> Result<Option<String>, Box<dyn std::error::Error>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare("SELECT scid FROM zoom_course_scid WHERE course_id = ?1")?;
        let mut rows = stmt.query(params![course_id.to_string()])?;
        if let Some(row) = rows.next()? {
            let scid: String = row.get(0)?;
            println!("DB: Found scid for course {}", course_id);
            Ok(Some(scid))
        } else {
            println!("DB: No scid found for course {}", course_id);
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
        let mut conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT host, name, value, path, expires, secure, http_only FROM zoom_cookie",
        )?;
        let rows = stmt.query_map([], |row| -> Result<ZoomCookie, rusqlite::Error> {
            Ok(ZoomCookie {
                domain: row.get(0)?,
                name: row.get(1)?,
                value: row.get(2)?,
                path: row.get(3)?,
                expires: row.get::<_, Option<i64>>(4)?,
                secure: row.get::<_, i64>(5)? != 0,
                http_only: row.get::<_, i64>(6)? != 0,
            })
        })?;

        let now = Utc::now().timestamp();
        let mut valid = Vec::new();
        let mut expired = Vec::new();

        for cookie in rows {
            let cookie = cookie?;
            let is_expired = match cookie.expires {
                Some(ts) if ts > 0 => ts <= now,
                Some(_) | None => false,
            };
            if is_expired {
                expired.push((
                    cookie.domain.clone(),
                    cookie.name.clone(),
                    cookie.path.clone(),
                ));
            } else {
                valid.push(cookie);
            }
        }
        drop(stmt);

        if !expired.is_empty() {
            let tx = conn.transaction()?;
            for (domain, name, path) in expired {
                tx.execute(
                    "DELETE FROM zoom_cookie WHERE host = ?1 AND name = ?2 AND path = ?3",
                    params![domain, name, path],
                )?;
            }
            tx.commit()?;
        }

        println!("DB: Loaded {} valid cookies", valid.len());
        Ok(valid)
    }

    pub fn save_request_headers(
        &self,
        course_id: u64,
        request_path: &str,
        headers: &[(String, String)],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM zoom_request_headers WHERE course_id = ?1 AND request_path = ?2",
            params![course_id.to_string(), request_path],
        )?;
        for (name, value) in headers {
            tx.execute(
                "INSERT INTO zoom_request_headers(course_id, request_path, header_name, header_value, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    course_id.to_string(),
                    request_path,
                    name,
                    value,
                    Utc::now().timestamp(),
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_all_request_headers(
        &self,
        course_id: u64,
    ) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT header_name, header_value FROM zoom_request_headers WHERE course_id = ?1",
        )?;
        let rows = stmt.query_map(params![course_id.to_string()], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;

        let mut headers = Vec::new();
        for row in rows {
            headers.push(row?);
        }
        Ok(headers)
    }

    pub fn save_replay_headers(
        &self,
        course_id: u64,
        entries: &std::collections::HashMap<String, ReplayHeader>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM zoom_replay_headers WHERE course_id = ?1",
            params![course_id.to_string()],
        )?;
        for (referer, entry) in entries {
            let headers_json = serde_json::to_string(&entry.headers)?;
            tx.execute(
                "INSERT INTO zoom_replay_headers(course_id, referer, download_url, headers, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    course_id.to_string(),
                    referer,
                    entry.download_url,
                    headers_json,
                    Utc::now().timestamp(),
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn load_replay_headers(
        &self,
        course_id: u64,
    ) -> Result<std::collections::HashMap<String, ReplayHeader>, Box<dyn std::error::Error>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT referer, download_url, headers FROM zoom_replay_headers WHERE course_id = ?1",
        )?;
        let mut rows = stmt.query(params![course_id.to_string()])?;

        let mut map = std::collections::HashMap::new();
        while let Some(row) = rows.next()? {
            let referer: String = row.get(0)?;
            let download_url: String = row.get(1)?;
            let headers_json: String = row.get(2)?;
            let headers: std::collections::HashMap<String, String> =
                serde_json::from_str(&headers_json)?;
            map.insert(
                referer,
                ReplayHeader {
                    download_url,
                    headers,
                },
            );
        }
        Ok(map)
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


}
