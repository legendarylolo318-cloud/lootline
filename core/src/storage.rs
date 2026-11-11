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

/// Schema migrations, applied in order. `PRAGMA user_version` records how many have run, so
/// each runs exactly once per database. Never edit a shipped entry: append a new one.
const MIGRATIONS: &[&str] = &[
    // 1: initial schema (CREATE IF NOT EXISTS, so pre-versioning databases adopt it cleanly)
    "
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
    ",
    // 2: history page looks matches up by session
    "CREATE INDEX IF NOT EXISTS matches_session ON matches(session_id, started);",
];

pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

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

    fn init(mut conn: Connection) -> Result<Storage> {
        // A second Lootline instance (or a backup tool) holding the file must not make writes
        // fail instantly with SQLITE_BUSY: wait up to 5 s for the lock instead.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        Self::migrate(&mut conn)?;
        Ok(Storage { conn })
    }

    fn migrate(conn: &mut Connection) -> Result<()> {
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        anyhow::ensure!(
            version <= SCHEMA_VERSION,
            "database schema v{version} is newer than this Lootline (v{SCHEMA_VERSION}); update the app"
        );
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version.max(0) as usize) {
            // each step and its version bump commit together, or not at all
            let tx = conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", (i + 1) as i64)?;
            tx.commit()?;
        }
        Ok(())
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
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
    fn migrations_run_once_and_upgrade_old_databases() {
        let dir = std::env::temp_dir().join(format!("lootline-mig-{}", Uuid::new_v4()));
        let p = dir.join("l.db");
        std::fs::create_dir_all(&dir).unwrap();
        {
            // a database written by the pre-versioning build: tables exist, user_version = 0
            let c = Connection::open(&p).unwrap();
            c.execute_batch(MIGRATIONS[0]).unwrap();
            c.execute("INSERT INTO events (id, ts, json) VALUES ('x', 1, '{}')", []).unwrap();
        }
        let s = Storage::open(&p).unwrap();
        assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
        drop(s);
        let s = Storage::open(&p).unwrap(); // re-open: nothing re-runs, data kept
        assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
        let n: i64 = s.conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        drop(s);
        {
            let c = Connection::open(&p).unwrap();
            c.pragma_update(None, "user_version", SCHEMA_VERSION + 1).unwrap();
        }
        assert!(Storage::open(&p).is_err(), "a newer schema must be refused, not clobbered");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn two_connections_share_the_file() {
        let dir = std::env::temp_dir().join(format!("lootline-lock-{}", Uuid::new_v4()));
        let p = dir.join("l.db");
        let a = Storage::open(&p).unwrap();
        let mut b = Storage::open(&p).unwrap();
        a.insert_event(&Event::new_chest("a", 1, Uuid::new_v4())).unwrap();
        b.insert_events(&[Event::new_chest("b", 2, Uuid::new_v4())]).unwrap();
        assert_eq!(a.load_log().unwrap().len(), 2);
        drop((a, b));
        let _ = std::fs::remove_dir_all(dir);
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
