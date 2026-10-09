// Local SQLite cache: library lists and track lists as JSON, so the window fills instantly.
// Stale-while-revalidate: callers show what is cached right away and refresh it in the
// background; `age` tells how old an entry is, to skip refreshes that are not due.
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};

pub struct Cache {
    conn: Option<Connection>,
}

impl Cache {
    pub fn open(path: &Path) -> Self {
        let conn = Connection::open(path)
            .and_then(|c| {
                c.execute_batch(
                    // NORMAL: WAL commits without an fsync each time (no UI stalls on writes);
                    // the cache can always be refetched, so losing the last write is fine.
                    "PRAGMA journal_mode=WAL;
                     PRAGMA synchronous=NORMAL;
                     PRAGMA busy_timeout=2000;
                     CREATE TABLE IF NOT EXISTS kv (
                         key TEXT PRIMARY KEY,
                         value TEXT NOT NULL,
                         updated INTEGER NOT NULL
                     );",
                )?;
                Ok(c)
            })
            .map_err(|e| eprintln!("cache disabled: {e}"))
            .ok();
        Self { conn }
    }

    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        let conn = self.conn.as_ref()?;
        let json: String = conn
            .query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))
            .optional()
            .ok()??;
        serde_json::from_str(&json).ok()
    }

    /// Seconds since `key` was last stored or confirmed unchanged.
    pub fn age(&self, key: &str) -> Option<u64> {
        let conn = self.conn.as_ref()?;
        let updated: i64 = conn
            .query_row("SELECT updated FROM kv WHERE key = ?1", [key], |r| r.get(0))
            .optional()
            .ok()??;
        Some((now() - updated).max(0) as u64)
    }

    pub fn put<T: Serialize>(&self, key: &str, value: &T) {
        let Some(conn) = self.conn.as_ref() else { return };
        let Ok(json) = serde_json::to_string(value) else { return };
        let now = now();
        // Unchanged content: only touch the timestamp.
        let same = conn
            .query_row("SELECT value = ?2 FROM kv WHERE key = ?1", params![key, json], |r| r.get::<_, bool>(0))
            .optional()
            .ok()
            .flatten()
            .unwrap_or(false);
        if same {
            let _ = conn.execute("UPDATE kv SET updated = ?2 WHERE key = ?1", params![key, now]);
            return;
        }
        if let Err(e) = conn.execute(
            "INSERT INTO kv (key, value, updated) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated = excluded.updated",
            params![key, json, now],
        ) {
            eprintln!("cache write {key}: {e}");
        }
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
