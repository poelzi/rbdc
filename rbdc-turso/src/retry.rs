//! Transient write-conflict retry policy for the Turso adapter.
//!
//! Turso is a single-writer MVCC engine: concurrent writers can fail with
//! [`turso::Error::Busy`] ("database is locked") or
//! [`turso::Error::BusySnapshot`] ("database snapshot is stale, rollback and
//! retry the transaction"). The latter is an optimistic-concurrency conflict
//! detected at commit time; `PRAGMA busy_timeout` does NOT resolve it. The
//! contract from the engine is explicit in the message: roll back and retry.
//!
//! This module provides the classification and backoff used to retry such
//! conflicts automatically — but ONLY for autocommit statements (those
//! executed outside an application-controlled transaction). A statement inside
//! an explicit `BEGIN ... COMMIT` cannot be safely replayed at the driver
//! level, because earlier reads in that transaction may already have fed values
//! into this statement's parameters in *application* code (e.g.
//! `SELECT MAX(seq)` -> `INSERT ... seq+1`). Those transactions must be retried
//! as a whole by the caller; the driver surfaces the conflict unchanged so the
//! caller can replay the read-modify-write from a fresh snapshot.

use std::time::Duration;

/// Maximum number of *retries* (in addition to the first attempt) for an
/// autocommit statement that keeps hitting transient write conflicts.
pub(crate) const MAX_RETRIES: u32 = 10;

/// Returns `true` if `err` is a transient write conflict that is safe to retry
/// by re-executing the (autocommit) statement from scratch.
pub(crate) fn is_retryable(err: &turso::Error) -> bool {
    matches!(err, turso::Error::Busy(_) | turso::Error::BusySnapshot(_))
}

/// The effect a statement has on the connection's transaction nesting depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TxControl {
    /// Opens a transaction or savepoint (`BEGIN`, `SAVEPOINT`).
    Begin,
    /// Closes a transaction or savepoint (`COMMIT`, `ROLLBACK`, `END`, `RELEASE`).
    End,
    /// Has no effect on transaction depth.
    None,
}

/// Classify a SQL statement by its leading keyword so the connection can track
/// whether it is inside an application-controlled transaction.
///
/// rbatis issues lower-case `begin` / `commit` / `rollback` for
/// `acquire_begin`/`commit`/`rollback`, but we match case-insensitively and
/// also recognise savepoints for robustness against hand-written SQL.
pub(crate) fn classify_tx(sql: &str) -> TxControl {
    let trimmed = sql.trim_start();
    let word = trimmed
        .split(|c: char| c.is_whitespace() || c == ';' || c == '(')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match word.as_str() {
        "begin" | "savepoint" => TxControl::Begin,
        "commit" | "rollback" | "end" | "release" => TxControl::End,
        _ => TxControl::None,
    }
}

/// Exponential backoff with full jitter for the given (1-based) retry attempt.
///
/// Doubling base capped at 256ms, jittered to `[base/2, base]` to avoid a
/// thundering herd of writers retrying in lockstep. Worst-case cumulative wait
/// across `MAX_RETRIES` retries is roughly one second.
pub(crate) fn backoff_delay(attempt: u32) -> Duration {
    let shift = attempt.clamp(1, 8);
    let base_ms = (1u64 << shift).min(256);
    let jitter = next_jitter();
    let ms = (base_ms as f64) * (0.5 + 0.5 * jitter);
    Duration::from_millis(ms.max(1.0) as u64)
}

/// Cheap process-local pseudo-random value in `[0, 1)` for backoff jitter.
///
/// A `xorshift64*` step over a shared atomic seed. Not cryptographic — its only
/// job is to decorrelate concurrent retriers so they do not collide again on
/// the next attempt.
fn next_jitter() -> f64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEED: AtomicU64 = AtomicU64::new(0x9E37_79B9_7F4A_7C15);
    let mut x = SEED
        .fetch_add(0x2545_F491_4F6C_DD1D, Ordering::Relaxed)
        .wrapping_add(0x2545_F491_4F6C_DD1D);
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    let v = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
    ((v >> 11) as f64) / ((1u64 << 53) as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_transaction_control_statements() {
        assert_eq!(classify_tx("begin"), TxControl::Begin);
        assert_eq!(classify_tx("  BEGIN  "), TxControl::Begin);
        assert_eq!(classify_tx("BEGIN IMMEDIATE"), TxControl::Begin);
        assert_eq!(classify_tx("savepoint sp1"), TxControl::Begin);
        assert_eq!(classify_tx("commit"), TxControl::End);
        assert_eq!(classify_tx("COMMIT;"), TxControl::End);
        assert_eq!(classify_tx("rollback"), TxControl::End);
        assert_eq!(classify_tx("END"), TxControl::End);
        assert_eq!(classify_tx("release sp1"), TxControl::End);
        assert_eq!(classify_tx("INSERT INTO t VALUES (1)"), TxControl::None);
        assert_eq!(classify_tx("SELECT 1"), TxControl::None);
    }

    #[test]
    fn busy_and_snapshot_are_retryable_nothing_else() {
        assert!(is_retryable(&turso::Error::Busy("database is locked".into())));
        assert!(is_retryable(&turso::Error::BusySnapshot(
            "database snapshot is stale, rollback and retry the transaction".into()
        )));
        assert!(!is_retryable(&turso::Error::Constraint("UNIQUE".into())));
        assert!(!is_retryable(&turso::Error::Misuse("bad".into())));
    }

    #[test]
    fn backoff_grows_and_stays_bounded() {
        for attempt in 1..=12u32 {
            let d = backoff_delay(attempt);
            assert!(d.as_millis() >= 1, "attempt {attempt} too small: {d:?}");
            assert!(d.as_millis() <= 256, "attempt {attempt} too large: {d:?}");
        }
    }
}
