//! SQLite rolling-window buy history. Proxy-only.
//!
//! The DB lives on the host / in the proxy container's volume. The
//! script / test container has no mount for it at all.
//!
//! Rules:
//! - Only a `201` from upstream *plus* a confirmed
//!   `GET /orders/{id}` writes a **buy** row.
//! - `previewOrder`, a `400`, and a timeout write an **attempt** row,
//!   not a buy. An attempt never blocks a later real buy.
//! - If `(account_hash, symbol)` already has a buy inside the
//!   rolling window, the proxy returns `409` and does not forward.

use rusqlite::{params, Connection};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Db {
    conn: Mutex<Connection>,
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

impl Db {
    pub fn open(path: &str) -> Result<Self, rusqlite::Error> {
        let conn = if path == ":memory:" {
            Connection::open_in_memory()?
        } else {
            if let Some(parent) = std::path::Path::new(path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            Connection::open(path)?
        };
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS buys (
                id INTEGER PRIMARY KEY,
                account_hash TEXT NOT NULL,
                symbol TEXT NOT NULL,
                order_id TEXT NOT NULL,
                placed_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_buys_account_symbol ON buys(account_hash, symbol);
            CREATE TABLE IF NOT EXISTS attempts (
                id INTEGER PRIMARY KEY,
                account_hash TEXT NOT NULL,
                symbol TEXT NOT NULL,
                kind TEXT NOT NULL,
                attempted_at INTEGER NOT NULL
            );",
        )?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    /// True if this (account, symbol) was bought inside the window.
    pub fn duplicate_in_window(&self, account_hash: &str, symbol: &str, window_days: i64) -> bool {
        let cutoff = now_secs() - window_days * 86_400;
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM buys WHERE account_hash = ?1 AND symbol = ?2 AND placed_at >= ?3",
                params![account_hash, symbol, cutoff],
                |r| r.get(0),
            )
            .unwrap_or(0);
        count > 0
    }

    pub fn record_buy(&self, account_hash: &str, symbol: &str, order_id: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT INTO buys (account_hash, symbol, order_id, placed_at) VALUES (?1, ?2, ?3, ?4)",
            params![account_hash, symbol, order_id, now_secs()],
        );
    }

    pub fn record_attempt(&self, account_hash: &str, symbol: &str, kind: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT INTO attempts (account_hash, symbol, kind, attempted_at) VALUES (?1, ?2, ?3, ?4)",
            params![account_hash, symbol, kind, now_secs()],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_blocks_second_buy_but_not_attempts() {
        let db = Db::open(":memory:").unwrap();
        assert!(!db.duplicate_in_window("h1", "VOO", 28));
        db.record_attempt("h1", "VOO", "preview");
        assert!(!db.duplicate_in_window("h1", "VOO", 28));
        db.record_buy("h1", "VOO", "order-1");
        assert!(db.duplicate_in_window("h1", "VOO", 28));
        assert!(!db.duplicate_in_window("h1", "SCHD", 28));
        assert!(!db.duplicate_in_window("h2", "VOO", 28));
    }
}
