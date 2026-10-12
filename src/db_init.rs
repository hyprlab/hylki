/**
 * @file db_init.rs
 * @author Damien O'Brien (Narcarsiss) - https://github.com/narcarsiss/hylki
 * @brief Configures SQLite connection with hyper-tuned mmap, RAM cache, and SMB/NFS fallback.
 * @param conn Mutable reference to an active rusqlite::Connection (units: N/A).
 * @param db_path File system path to the SQLite database file (units: N/A).
 * @return Result indicating success or SQLite execution failure.
 * @throws rusqlite::Error if PRAGMA execution or index creation fails.
 * @warning Safety Disclaimer: Executing WAL mode on SMB/NFS mounts will cause database corruption due to shm locking failures.
 * @pre Valid rusqlite connection must be established and in autocommit mode.
 * @post Database operates with 256MB mmap and 64MB RAM page cache on local disks, or TRUNCATE on network shares.
 */
use rusqlite::{Connection, Result};
use std::path::Path;

/// Applies commercial-grade SQLite PRAGMAs for zero-latency lookups.
/// G'day mate! On local drives we map 256MB right into kernel memory space.
pub fn optimize_sqlite_connection(conn: &mut Connection, db_path: &Path) -> Result<()> {
    assert!(conn.is_autocommit(), "Connection must be in autocommit mode, mate.");
    assert!(db_path.exists(), "Database path must exist before tuning.");

    let is_network_share = detect_network_filesystem(db_path);

    // She'll be right: local disks get blazing fast mmap + WAL; network shares fall back safely.
    let pragma_query = if is_network_share {
        "PRAGMA journal_mode=TRUNCATE; \
         PRAGMA synchronous=FULL; \
         PRAGMA cache_size=-64000; \
         PRAGMA temp_store=MEMORY;"
    } else {
        "PRAGMA journal_mode=WAL; \
         PRAGMA synchronous=NORMAL; \
         PRAGMA mmap_size=268435456; \
         PRAGMA cache_size=-64000; \
         PRAGMA temp_store=MEMORY;"
    };

    conn.execute_batch(pragma_query)?;

    // Compound covering index for instant mailbox pagination queries
    let index_migration = "
        CREATE INDEX IF NOT EXISTS idx_mail_folder_date 
        ON messages(folder_id, date_received) 
        INCLUDE(subject, sender_name, snippet);
    ";
    conn.execute_batch(index_migration)?;

    Ok(())
}

/// Detects if database resides on a remote network mount (NFS/SMB)
fn detect_network_filesystem(path: &Path) -> bool {
    let path_str = path.to_string_lossy().to_lowercase();
    if path_str.contains("/mnt/smb") || path_str.contains("/mnt/nfs") || path_str.contains("/run/user") {
        return true;
    }
    false
}
