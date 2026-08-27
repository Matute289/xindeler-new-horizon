//! Durable backing for `oracle::ChronicleLog`, ORACLE's in-memory narrative
//! stand-in, which would otherwise be wiped by every server restart.
//!
//! Deliberately column-shaped rather than model-shaped (no `models.rs`
//! struct): the table is two scalar columns, and the resource it backs is a
//! `VecDeque<String>`. The style otherwise mirrors `character/mod.rs`'s
//! suspension helpers -- raw `rusqlite`, `prepare_cached`, `PersistenceError`
//! via `?`, transactions owned by the caller.
//!
//! The table is a **rolling window**, not an audit log: every insert prunes
//! back to `bound` rows in the same transaction, mirroring the in-memory
//! `bounds::MAX_ENTRIES` cap. See `oracle/chronicle.rs`'s module doc for why
//! this stand-in is not the eventual "real" chronicle/lore system.

extern crate rusqlite;

use super::error::PersistenceError;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, Transaction};
use std::collections::VecDeque;

/// Appends one chronicle entry and prunes the table back to its newest
/// `bound` rows, both inside the caller's transaction so the window can never
/// be observed (or left) over-long.
///
/// `bound` is `oracle::chronicle::bounds::MAX_ENTRIES` in production; the
/// parameter exists so tests can exercise pruning without writing a thousand
/// rows.
pub fn insert_and_prune_chronicle_entry(
    text: &str,
    created_at: DateTime<Utc>,
    bound: usize,
    transaction: &mut Transaction,
) -> Result<(), PersistenceError> {
    let bound = i64::try_from(bound).unwrap_or(i64::MAX);

    let mut insert = transaction.prepare_cached(
        "
        INSERT
        INTO    chronicle_log (text, created_at)
        VALUES  (?1, ?2)",
    )?;
    insert.execute(rusqlite::params![text, format_timestamp(created_at)])?;
    drop(insert);

    // `id` is AUTOINCREMENT, so descending id is descending insertion order --
    // and the write path is serialized (the "CHRONICLE_LOG" slow-job name is
    // configured with concurrency 1), so insertion order always matches
    // `created_at` order.
    let mut prune = transaction.prepare_cached(
        "
        DELETE
        FROM    chronicle_log
        WHERE   id NOT IN (SELECT id FROM chronicle_log ORDER BY id DESC LIMIT ?1)",
    )?;
    prune.execute([bound])?;
    Ok(())
}

/// The newest `bound` entries, returned **oldest-first** so the result can be
/// handed straight to `ChronicleLog` without reordering -- `ChronicleLog::iter`
/// has always been oldest-first and that contract does not change here.
pub fn load_chronicle_tail(
    bound: usize,
    connection: &Connection,
) -> Result<VecDeque<String>, PersistenceError> {
    let bound = i64::try_from(bound).unwrap_or(i64::MAX);

    let mut stmt = connection.prepare_cached(
        "
        SELECT  text
        FROM    (SELECT id, text
                 FROM   chronicle_log
                 ORDER BY id DESC
                 LIMIT  ?1)
        ORDER BY id ASC",
    )?;
    let entries = stmt
        .query_map([bound], |row| row.get::<_, String>(0))?
        .collect::<Result<VecDeque<String>, _>>()?;
    Ok(entries)
}

/// Same convention as the `character_suspensions` timestamp columns.
fn format_timestamp(at: DateTime<Utc>) -> String { at.to_rfc3339() }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::{
        ConnectionMode, DatabaseSettings, SqlLogMode, VelorenConnection, establish_connection,
    };

    struct TestDb {
        _dir: tempfile::TempDir,
        settings: DatabaseSettings,
    }

    impl TestDb {
        /// A fresh database with every migration applied, exactly as a server
        /// would produce on first start.
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("temp dir");
            let settings = DatabaseSettings {
                db_dir: dir.path().to_path_buf(),
                sql_log_mode: SqlLogMode::Disabled,
            };
            crate::persistence::run_migrations(&settings);
            Self {
                _dir: dir,
                settings,
            }
        }

        fn connection(&self) -> VelorenConnection {
            establish_connection(&self.settings, ConnectionMode::ReadWrite)
        }

        /// Appends one entry through the real path, committing on success.
        fn append(&self, text: &str, bound: usize) {
            let mut conn = self.connection();
            let mut transaction = conn.connection.transaction().expect("transaction");
            insert_and_prune_chronicle_entry(text, Utc::now(), bound, &mut transaction)
                .expect("insert");
            transaction.commit().expect("commit");
        }

        fn tail(&self, bound: usize) -> Vec<String> {
            let conn = self.connection();
            load_chronicle_tail(bound, &conn).expect("load").into()
        }
    }

    #[test]
    fn an_empty_table_loads_to_an_empty_deque() {
        let db = TestDb::new();
        assert!(
            load_chronicle_tail(1024, &db.connection())
                .expect("load")
                .is_empty()
        );
    }

    #[test]
    fn load_returns_entries_oldest_first() {
        let db = TestDb::new();
        db.append("first", 1024);
        db.append("second", 1024);
        db.append("third", 1024);

        assert_eq!(db.tail(1024), vec![
            "first".to_owned(),
            "second".to_owned(),
            "third".to_owned()
        ]);
    }

    #[test]
    fn inserting_over_the_bound_prunes_the_oldest_row() {
        let db = TestDb::new();
        for i in 0..3 {
            db.append(&format!("entry {i}"), 2);
        }

        assert_eq!(
            db.tail(1024),
            vec!["entry 1".to_owned(), "entry 2".to_owned()],
            "the table itself must be pruned to the bound, not merely read back short"
        );
    }

    #[test]
    fn load_never_returns_more_than_the_requested_bound() {
        let db = TestDb::new();
        for i in 0..5 {
            db.append(&format!("entry {i}"), 1024);
        }

        assert_eq!(db.tail(2), vec!["entry 3".to_owned(), "entry 4".to_owned()]);
    }
}
