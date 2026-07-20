use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;

const JOB_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS __aq_jobs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL,
    table_name TEXT,
    state TEXT NOT NULL,
    progress REAL NOT NULL DEFAULT 0,
    result_json TEXT,
    error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS __aq_audit (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    occurred_at TEXT NOT NULL,
    key_id TEXT,
    method TEXT NOT NULL,
    path TEXT NOT NULL,
    status INTEGER NOT NULL,
    duration_ms REAL NOT NULL
);
"#;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Job {
    pub(crate) id: i64,
    pub(crate) kind: String,
    pub(crate) table: Option<String>,
    pub(crate) state: String,
    pub(crate) progress: f64,
    pub(crate) result: Option<Value>,
    pub(crate) error: Option<String>,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct AuditRecord {
    pub(crate) id: i64,
    pub(crate) occurred_at: String,
    pub(crate) key_id: Option<String>,
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) status: u16,
    pub(crate) duration_ms: f64,
}

pub(crate) struct JobStore {
    connection: Mutex<Connection>,
    root: PathBuf,
}

impl JobStore {
    pub(crate) fn open(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root.join("jobs"))?;
        let connection = Connection::open(root.join("data.sqlite3"))?;
        connection.execute_batch(JOB_SQL)?;
        let now = now();
        connection.execute(
            "UPDATE __aq_jobs SET state = 'interrupted', updated_at = ?1 WHERE state IN ('receiving', 'queued', 'running')",
            [&now],
        )?;
        Ok(Self {
            connection: Mutex::new(connection),
            root: root.to_path_buf(),
        })
    }

    pub(crate) fn create(&self, kind: &str, table: Option<String>) -> Result<Job> {
        let connection = self.lock()?;
        let now = now();
        connection.execute(
            "INSERT INTO __aq_jobs(kind, table_name, state, created_at, updated_at) VALUES (?1, ?2, 'queued', ?3, ?3)",
            params![kind, table, now],
        )?;
        self.get_with(&connection, connection.last_insert_rowid())?
            .context("created job disappeared")
    }

    pub(crate) fn list(&self) -> Result<Vec<Job>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare("SELECT id, kind, table_name, state, progress, result_json, error, created_at, updated_at FROM __aq_jobs ORDER BY id DESC")?;
        statement
            .query_map([], read_job)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub(crate) fn get(&self, id: i64) -> Result<Option<Job>> {
        let connection = self.lock()?;
        self.get_with(&connection, id)
    }

    pub(crate) fn running(&self, id: i64) -> Result<()> {
        self.update(id, "running", 0.0, None, None)
    }

    pub(crate) fn complete(&self, id: i64, result: Value) -> Result<()> {
        self.update(id, "completed", 1.0, Some(result), None)
    }

    pub(crate) fn fail(&self, id: i64, error: String) -> Result<()> {
        self.update(id, "failed", 0.0, None, Some(error))
    }

    pub(crate) fn progress(&self, id: i64, progress: f64) -> Result<()> {
        let connection = self.lock()?;
        connection.execute(
            "UPDATE __aq_jobs SET progress=?1, updated_at=?2 WHERE id=?3 AND state='running'",
            params![progress.clamp(0.0, 1.0), now(), id],
        )?;
        Ok(())
    }

    pub(crate) fn cancel(&self, id: i64) -> Result<bool> {
        Ok(self.lock()?.execute(
            "UPDATE __aq_jobs SET state='cancelled', updated_at=?1 WHERE id=?2 AND state IN ('queued','running')",
            params![now(), id],
        )? == 1)
    }

    pub(crate) fn is_cancelled(&self, id: i64) -> Result<bool> {
        Ok(self
            .lock()?
            .query_row(
                "SELECT state='cancelled' FROM __aq_jobs WHERE id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(false))
    }

    pub(crate) fn operational_counts(&self) -> Result<(u64, u64)> {
        let connection = self.lock()?;
        let jobs = connection.query_row(
            "SELECT COUNT(*) FROM __aq_jobs WHERE state IN ('queued','running')",
            [],
            |row| row.get(0),
        )?;
        let outbox =
            connection.query_row("SELECT COUNT(*) FROM __aq_outbox", [], |row| row.get(0))?;
        Ok((jobs, outbox))
    }

    pub(crate) fn artifact_path(&self, id: i64, extension: &str) -> PathBuf {
        self.root.join("jobs").join(format!("{id}.{extension}"))
    }

    pub(crate) fn audit(
        &self,
        key_id: Option<&str>,
        method: &str,
        path: &str,
        status: u16,
        duration_ms: f64,
    ) -> Result<()> {
        self.lock()?.execute(
            "INSERT INTO __aq_audit(occurred_at,key_id,method,path,status,duration_ms) VALUES (?1,?2,?3,?4,?5,?6)",
            params![now(), key_id, method, path, status, duration_ms],
        )?;
        Ok(())
    }

    pub(crate) fn audit_records(&self, limit: usize) -> Result<Vec<AuditRecord>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT id,occurred_at,key_id,method,path,status,duration_ms FROM __aq_audit ORDER BY id DESC LIMIT ?1",
        )?;
        statement
            .query_map([limit.min(1_000) as i64], |row| {
                Ok(AuditRecord {
                    id: row.get(0)?,
                    occurred_at: row.get(1)?,
                    key_id: row.get(2)?,
                    method: row.get(3)?,
                    path: row.get(4)?,
                    status: row.get(5)?,
                    duration_ms: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    fn update(
        &self,
        id: i64,
        state: &str,
        progress: f64,
        result: Option<Value>,
        error: Option<String>,
    ) -> Result<()> {
        self.lock()?.execute(
            "UPDATE __aq_jobs SET state=?1, progress=?2, result_json=?3, error=?4, updated_at=?5 WHERE id=?6 AND state!='cancelled'",
            params![state, progress, result.map(|v| v.to_string()), error, now(), id],
        )?;
        Ok(())
    }

    fn get_with(&self, connection: &Connection, id: i64) -> Result<Option<Job>> {
        connection
            .query_row(
                "SELECT id, kind, table_name, state, progress, result_json, error, created_at, updated_at FROM __aq_jobs WHERE id=?1",
                [id],
                read_job,
            )
            .optional()
            .map_err(Into::into)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| anyhow::anyhow!("job store lock was poisoned"))
    }
}

fn read_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<Job> {
    let result: Option<String> = row.get(5)?;
    Ok(Job {
        id: row.get(0)?,
        kind: row.get(1)?,
        table: row.get(2)?,
        state: row.get(3)?,
        progress: row.get(4)?,
        result: result.and_then(|json| serde_json::from_str(&json).ok()),
        error: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_jobs_become_interrupted_after_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let database = frankensteindb::Database::open(directory.path()).unwrap();
        drop(database);
        let store = JobStore::open(directory.path()).unwrap();
        let job = store.create("reindex", Some("items".into())).unwrap();
        store.running(job.id).unwrap();
        drop(store);
        let reopened = JobStore::open(directory.path()).unwrap();
        assert_eq!(reopened.get(job.id).unwrap().unwrap().state, "interrupted");
    }
}
