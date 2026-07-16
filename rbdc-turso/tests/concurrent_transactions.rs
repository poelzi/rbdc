//! Contention coverage for explicit read-modify-write transactions.
//! Ensures writer admission happens before a transaction takes its read snapshot.

use rbdc::db::{Connection, Driver};
use rbdc_turso::TursoDriver;
use rbs::Value;

async fn connect(url: &str) -> Box<dyn Connection> {
    TursoDriver {}
        .connect(url)
        .await
        .expect("connect to turso file db")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_explicit_transactions_converge() {
    let path = std::env::temp_dir().join(format!(
        "rbdc-turso-concurrent-tx-{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite://{}", path.display());

    {
        let mut setup = connect(&url).await;
        setup
            .get_rows("PRAGMA journal_mode=WAL", vec![])
            .await
            .expect("enable WAL");
        setup
            .exec(
                "CREATE TABLE items (room TEXT NOT NULL, seq INTEGER NOT NULL, UNIQUE(room, seq))",
                vec![],
            )
            .await
            .expect("create table");
    }

    const WRITERS: usize = 16;
    let mut handles = Vec::with_capacity(WRITERS);
    for writer in 0..WRITERS {
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            let mut conn = connect(&url).await;
            conn.begin()
                .await
                .map_err(|error| format!("writer {writer} failed to begin: {error}"))?;
            let mut rows = conn
                .get_rows(
                    "SELECT COALESCE(MAX(seq), 0) + 1 AS seq FROM items WHERE room = 'r'",
                    vec![],
                )
                .await
                .map_err(|error| format!("writer {writer} failed to read: {error}"))?;
            let seq = rows
                .first_mut()
                .ok_or_else(|| format!("writer {writer} got no sequence row"))?
                .get(0)
                .map_err(|error| format!("writer {writer} failed to decode sequence: {error}"))?;
            conn.exec("INSERT INTO items (room, seq) VALUES ('r', ?)", vec![seq])
                .await
                .map_err(|error| format!("writer {writer} failed to insert: {error}"))?;
            conn.commit()
                .await
                .map_err(|error| format!("writer {writer} failed to commit: {error}"))
        }));
    }

    let mut errors = Vec::new();
    for handle in handles {
        if let Err(error) = handle.await.expect("writer task") {
            errors.push(error);
        }
    }
    assert!(
        errors.is_empty(),
        "explicit transactions failed: {errors:?}"
    );

    let mut verify = connect(&url).await;
    let rows = verify
        .get_rows("SELECT seq FROM items WHERE room = 'r'", vec![])
        .await
        .expect("read inserted rows");
    assert_eq!(rows.len(), WRITERS);

    let _ = std::fs::remove_file(&path);
}
