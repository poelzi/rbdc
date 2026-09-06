//! Behavioural test for the autocommit write-conflict retry.
//!
//! Reproduces the production race: many concurrent writers each run an
//! autocommit read-modify-write (`INSERT ... SELECT MAX(seq)+1`) against a
//! `UNIQUE`-constrained table on a shared file database. Without retry, turso's
//! MVCC surfaces `Busy`/`BusySnapshot` conflicts (or a `UNIQUE` violation if a
//! stale sequence slips through) and writers fail. With the driver-level
//! autocommit retry, every writer converges on a distinct sequence and none
//! fail.

use rbdc::db::{Connection, Driver};
use rbdc_turso::TursoDriver;

async fn connect(url: &str) -> Box<dyn Connection> {
    TursoDriver {}
        .connect(url)
        .await
        .expect("connect to turso file db")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_autocommit_inserts_converge() {
    let path =
        std::env::temp_dir().join(format!("rbdc-turso-concurrent-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite://{}", path.display());

    {
        let mut setup = connect(&url).await;
        // PRAGMA journal_mode returns a result row, so use get_rows, not exec.
        setup
            .get_rows("PRAGMA journal_mode=WAL", vec![])
            .await
            .expect("enable WAL");
        setup
            .exec(
                "CREATE TABLE items (room TEXT NOT NULL, seq INTEGER NOT NULL, \
                 UNIQUE(room, seq))",
                vec![],
            )
            .await
            .expect("create table");
    }

    const N: usize = 16;
    let mut handles = Vec::with_capacity(N);
    for i in 0..N {
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            let mut conn = connect(&url).await;
            conn.exec(
                "INSERT INTO items (room, seq) \
                 SELECT 'r', COALESCE(MAX(seq), 0) + 1 FROM items WHERE room = 'r'",
                vec![],
            )
            .await
            .map(|_| ())
            .map_err(|e| format!("writer {i} failed: {e}"))
        }));
    }

    let mut errors = Vec::new();
    for h in handles {
        if let Err(e) = h.await.expect("task join") {
            errors.push(e);
        }
    }
    assert!(
        errors.is_empty(),
        "concurrent autocommit writers should converge via retry, but got: {errors:?}"
    );

    // All N inserts landed. UNIQUE(room, seq) guarantees the sequences are
    // distinct (a duplicate would have surfaced as a non-retryable constraint
    // error above), so a row count of N proves every writer got its own slot.
    let mut verify = connect(&url).await;
    let rows = verify
        .get_rows("SELECT seq FROM items WHERE room = 'r'", vec![])
        .await
        .expect("count rows");
    assert_eq!(
        rows.len(),
        N,
        "expected {N} distinct rows, got {}",
        rows.len()
    );

    let _ = std::fs::remove_file(&path);
}
