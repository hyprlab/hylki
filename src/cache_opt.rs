/**
 * @file cache_opt.rs
 * @author Damien O'Brien (Narcarsiss) - https://github.com/narcarsiss/hylki
 * @brief SQLite optimizations: high-throughput pragmas, read pool, thread_members query, and folded paging.
 */

use rusqlite::{params, Connection, OpenFlags, Result};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const PRAGMA_STATEMENTS: &[&str] = &[
    "PRAGMA journal_mode = WAL;",
    "PRAGMA synchronous = NORMAL;",
    "PRAGMA cache_size = -64000;",      // 64MB cache
    "PRAGMA mmap_size = 268435456;",    // 256MB memory-mapped I/O
    "PRAGMA temp_store = MEMORY;",
    "PRAGMA foreign_keys = ON;",
];

/// Applies performance-critical SQLite pragmas to a connection.
pub fn apply_performance_pragmas(conn: &Connection) -> Result<()> {
    for pragma in PRAGMA_STATEMENTS {
        conn.execute_batch(pragma)?;
    }
    Ok(())
}

/// A pool of readonly connections dedicated to concurrent UI and background queries.
pub struct SqliteReadPool {
    path: PathBuf,
    connections: Mutex<Vec<Connection>>,
    max_size: usize,
}

impl SqliteReadPool {
    pub fn new<P: AsRef<Path>>(path: P, max_size: usize) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            connections: Mutex::new(Vec::with_capacity(max_size)),
            max_size,
        }
    }

    pub fn acquire(&self) -> Result<PooledConnection> {
        let mut pool = self.connections.lock().unwrap();
        if let Some(conn) = pool.pop() {
            return Ok(PooledConnection {
                conn: Some(conn),
                pool: self,
            });
        }

        let conn = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        apply_performance_pragmas(&conn)?;

        Ok(PooledConnection {
            conn: Some(conn),
            pool: self,
        })
    }

    fn release(&self, conn: Connection) {
        let mut pool = self.connections.lock().unwrap();
        if pool.len() < self.max_size {
            pool.push(conn);
        }
    }
}

pub struct PooledConnection<'a> {
    conn: Option<Connection>,
    pool: &'a SqliteReadPool,
}

impl<'a> std::ops::Deref for PooledConnection<'a> {
    type Target = Connection;
    fn deref(&self) -> &Self::Target {
        self.conn.as_ref().unwrap()
    }
}

impl<'a> Drop for PooledConnection<'a> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            self.pool.release(conn);
        }
    }
}

/// Fetches thread members in a single query using index-backed row scan.
pub fn query_thread_members(
    conn: &Connection,
    account_id: u32,
    thread_root_id: &str,
) -> Result<Vec<u32>> {
    let mut stmt = conn.prepare_cached(
        "SELECT uid FROM messages WHERE account_id = ?1 AND (message_id = ?2 OR references_ LIKE ?3) ORDER BY ts ASC"
    )?;
    let pattern = format!("%{}%", thread_root_id);
    let rows = stmt.query_map(params![account_id, thread_root_id, pattern], |row| {
        row.get(0)
    })?;

    let mut uids = Vec::new();
    for uid in rows {
        uids.push(uid?);
    }
    Ok(uids)
}

/// Folded page query: retrieves contiguous page slice with offset/limit.
pub fn query_folded_page_uids(
    conn: &Connection,
    account_id: u32,
    folder_path: &str,
    offset: u32,
    limit: u32,
) -> Result<Vec<u32>> {
    let mut stmt = conn.prepare_cached(
        "SELECT uid FROM messages WHERE account_id = ?1 AND folder_path = ?2 ORDER BY uid DESC LIMIT ?3 OFFSET ?4"
    )?;
    let rows = stmt.query_map(params![account_id, folder_path, limit, offset], |row| {
        row.get(0)
    })?;

    let mut uids = Vec::new();
    for uid in rows {
        uids.push(uid?);
    }
    Ok(uids)
}
