//! SQLite persistence: the event log (all peers' events), sessions and matches.
//! Lifetime stats are derived from the persisted log.

use crate::event::{Event, EventLog};
use anyhow::Result;
use rusqlite::{params, Connection};
use std::path::Path;
use uuid::Uuid;

pub struct Storage {
    conn: Connection,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionRow {
    pub id: i64,
    pub started: i64,
    pub ended: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatchRow {
    pub id: Uuid,
    pub session_id: i64,
    pub started: i64,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS events (
    id   TEXT PRIMARY KEY,
    ts   INTEGER NOT NULL,
    json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS events_ts ON events(ts);
CREATE TABLE IF NOT EXISTS sessions (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    started INTEGER NOT NULL,
    ended   INTEGER
);
CREATE TABLE IF NOT EXISTS matches (
    id         TEXT PRIMARY KEY,
    session_id INTEGER NOT NULL,
    started    INTEGER NOT NULL
);
";

impl Storage {
    pub fn open(path: &Path) -> Result<Storage> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn memory() -> Result<Storage> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Storage> {
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Storage { conn })
    }

    /// Persist an event; returns false if it already existed.
    pub fn insert_event(&self, e: &Event) -> Result<bool> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO events (id, ts, json) VALUES (?1, ?2, ?3)",
            params![e.id().to_string(), e.ts(), serde_json::to_string(e)?],
        )?;
        Ok(n > 0)
    }

    pub fn insert_events(&mut self, evs: &[Event]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut st = tx.prepare("INSERT OR IGNORE INTO events (id, ts, json) VALUES (?1, ?2, ?3)")?;
            for e in evs {
                st.execute(params![e.id().to_string(), e.ts(), serde_json::to_string(e)?])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn load_log(&self) -> Result<EventLog> {
        let mut st = self.conn.prepare("SELECT json FROM events ORDER BY ts")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        let mut log = EventLog::new();
        for r in rows {
            if let Ok(e) = serde_json::from_str::<Event>(&r?) {
                log.insert(e);
            }
        }
        Ok(log)
    }

    pub fn begin_session(&self, now: i64) -> Result<i64> {
        self.conn.execute("INSERT INTO sessions (started) VALUES (?1)", params![now])?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn end_session(&self, id: i64, now: i64) -> Result<()> {
        self.conn.execute("UPDATE sessions SET ended = ?2 WHERE id = ?1 AND ended IS NULL", params![id, now])?;
        Ok(())
    }

    pub fn sessions(&self) -> Result<Vec<SessionRow>> {
        let mut st = self.conn.prepare("SELECT id, started, ended FROM sessions ORDER BY id DESC")?;
        let v = st
            .query_map([], |r| Ok(SessionRow { id: r.get(0)?, started: r.get(1)?, ended: r.get(2)? }))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(v)
    }

    pub fn begin_match(&self, id: Uuid, session_id: i64, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO matches (id, session_id, started) VALUES (?1, ?2, ?3)",
            params![id.to_string(), session_id, now],
        )?;
        Ok(())
    }

    pub fn matches(&self) -> Result<Vec<MatchRow>> {
        let mut st = self.conn.prepare("SELECT id, session_id, started FROM matches ORDER BY started DESC")?;
        let v = st
            .query_map([], |r| {
                let id: String = r.get(0)?;
                Ok(MatchRow { id: Uuid::parse_str(&id).unwrap_or_default(), session_id: r.get(1)?, started: r.get(2)? })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_persist_and_dedupe() {
        let s = Storage::memory().unwrap();
        let e = Event::new_chest("a", 5, Uuid::new_v4());
        assert!(s.insert_event(&e).unwrap());
        assert!(!s.insert_event(&e).unwrap());
        let log = s.load_log().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log.get(&e.id()), Some(&e));
    }

    #[test]
    fn batch_insert_and_roundtrip_of_all_variants() {
        let mut s = Storage::memory().unwrap();
        let m = Uuid::new_v4();
        let c = Event::new_chest("a", 1, m);
        let evs = vec![
            c.clone(),
            Event::Tag {
                id: Uuid::new_v4(),
                target: c.id(),
                player: "a".into(),
                ts: 2,
                rarity: Some(crate::Rarity::Epic),
                item: Some("X".into()),
            },
            Event::Undo { id: Uuid::new_v4(), target: c.id(), player: "a".into(), ts: 3 },
            Event::Join {
                id: Uuid::new_v4(),
                player: "a".into(),
                name: "n".into(),
                color: "#fff".into(),
                detecting: true,
                ts: 0,
            },
            Event::Leave { id: Uuid::new_v4(), player: "a".into(), ts: 4 },
            Event::Rename { id: Uuid::new_v4(), player: "a".into(), name: "m".into(), ts: 5 },
        ];
        s.insert_events(&evs).unwrap();
        assert_eq!(s.load_log().unwrap().len(), evs.len());
    }

    #[test]
    fn sessions_and_matches() {
        let s = Storage::memory().unwrap();
        let a = s.begin_session(100).unwrap();
        let b = s.begin_session(200).unwrap();
        s.end_session(a, 150).unwrap();
        let rows = s.sessions().unwrap();
        assert_eq!(rows[0].id, b);
        assert_eq!(rows[1].ended, Some(150));
        let m = Uuid::new_v4();
        s.begin_match(m, b, 210).unwrap();
        s.begin_match(m, b, 999).unwrap();
        let ms = s.matches().unwrap();
        assert_eq!(ms.len(), 1);
        assert_eq!(ms[0].id, m);
    }

    #[test]
    fn file_backed_db_survives_reopen() {
        let dir = std::env::temp_dir().join(format!("lootline-db-{}", Uuid::new_v4()));
        let p = dir.join("l.db");
        {
            let s = Storage::open(&p).unwrap();
            s.insert_event(&Event::new_chest("a", 1, Uuid::new_v4())).unwrap();
        }
        assert_eq!(Storage::open(&p).unwrap().load_log().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
