//! Helpers shared by the rbdc-turso integration tests.

use futures_util::StreamExt;
use rbdc::db::{Connection, Row};
use rbdc::Error;
use rbs::Value;

/// Collects `exec_rows` into a `Vec`, the shape the pre-4.9 `get_rows` returned.
pub async fn get_rows(
    conn: &mut Box<dyn Connection>,
    sql: &str,
    params: Vec<Value>,
) -> Result<Vec<Box<dyn Row>>, Error> {
    let mut stream = conn.exec_rows(sql, params).await?;
    let mut rows = Vec::new();
    while let Some(row) = stream.next().await {
        rows.push(row?);
    }
    Ok(rows)
}
