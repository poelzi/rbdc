//! Session PRAGMAs must reach every connection, not just the first one.
//!
//! `PRAGMA synchronous` and friends are per-connection session state. Issuing
//! them through a pool configures whichever connection the statement happened
//! to acquire and silently leaves the rest on turso's defaults -- which for
//! `synchronous` is FULL, an fsync on every commit. Applying them at connect
//! time is the only way a pool gets them everywhere.

use rbdc::db::{ConnectOptions, Connection, Driver};
use rbdc_turso::{TursoConnectOptions, TursoDriver};
use rbs::Value;
use std::str::FromStr;

fn temp_db(tag: &str) -> (std::path::PathBuf, String) {
    let path = std::env::temp_dir().join(format!(
        "rbdc-turso-{tag}-{}-{:?}.db",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite://{}", path.display());
    (path, url)
}

/// First column of the first row, as an integer.
async fn scalar(conn: &mut Box<dyn Connection>, sql: &str, params: Vec<Value>) -> i64 {
    let mut rows = conn.get_rows(sql, params).await.expect("query");
    let row = rows.first_mut().expect("query returns a row");
    row.get(0)
        .expect("query returns a column")
        .as_i64()
        .expect("column is an integer")
}

#[tokio::test]
async fn every_connection_gets_the_configured_session_pragmas() {
    let (path, url) = temp_db("session-pragmas");
    let with_params = format!("{url}?synchronous=normal&cache_size=-4096&foreign_keys=true");

    // Several independent connections: each must come back already configured.
    for _ in 0..4 {
        let mut opt = TursoConnectOptions::default();
        opt.set_uri(&with_params).expect("parse uri");
        let mut conn = opt.connect().await.expect("connect");

        assert_eq!(
            scalar(&mut conn, "PRAGMA synchronous", vec![]).await,
            1,
            "synchronous must be NORMAL on every connection, not turso's FULL default"
        );
        assert_eq!(scalar(&mut conn, "PRAGMA cache_size", vec![]).await, -4096);
        assert_eq!(scalar(&mut conn, "PRAGMA foreign_keys", vec![]).await, 1);
    }

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn a_connection_without_pragma_params_is_left_alone() {
    let (path, url) = temp_db("session-pragmas-default");
    let mut conn = TursoDriver {}.connect(&url).await.expect("connect");

    // turso's own default. Asserted so a driver-side default cannot be
    // introduced without this test noticing.
    assert_eq!(scalar(&mut conn, "PRAGMA synchronous", vec![]).await, 2);

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn cached_statements_rebind_their_parameters() {
    // `prepare_cached` reuses a compiled program across calls. If binding were
    // not reset per execution, a second call would silently replay the first
    // call's arguments -- a data corruption bug, not a performance one.
    let (path, url) = temp_db("cached-stmt-rebind");
    let mut conn = TursoDriver {}.connect(&url).await.expect("connect");
    conn.exec(
        "CREATE TABLE t (k TEXT PRIMARY KEY, v INTEGER NOT NULL)",
        vec![],
    )
    .await
    .expect("create table");

    for (k, v) in [("a", 1i64), ("b", 2), ("c", 3)] {
        conn.exec(
            "INSERT INTO t (k, v) VALUES (?, ?)",
            vec![Value::String(k.to_string()), Value::I64(v)],
        )
        .await
        .expect("insert");
    }

    for (k, expected) in [("a", 1i64), ("b", 2), ("c", 3)] {
        let got = scalar(
            &mut conn,
            "SELECT v FROM t WHERE k = ?",
            vec![Value::String(k.to_string())],
        )
        .await;
        assert_eq!(got, expected, "cached SELECT must rebind for key {k}");
    }

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn unknown_pragma_query_params_are_rejected() {
    assert!(TursoConnectOptions::from_str("sqlite://x.db?not_a_param=1").is_err());
}
