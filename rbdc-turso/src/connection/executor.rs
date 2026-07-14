//! Executor logic for the Turso connection.
//!
//! Query execution pipeline converting between `turso` result types
//! and the rbdc trait types (Row, ExecResult).

use crate::column::TursoColumn;
use crate::connection::TursoConnection;
use crate::error::TursoError;
use crate::query_result::TursoQueryResult;
use crate::retry::{backoff_delay, classify_tx, is_retryable, TxControl, MAX_RETRIES};
use crate::row::TursoRow;
use crate::value::{value_to_turso, TursoDataType, TursoValue};
use rbdc::db::{ExecResult, Row};
use rbdc::error::Error;
use rbs::Value;
use std::sync::Arc;

impl TursoConnection {
    /// Execute a SELECT-style query and return rows.
    ///
    /// When executed in autocommit mode (no open application transaction), a
    /// transient write conflict ([`turso::Error::Busy`] /
    /// [`turso::Error::BusySnapshot`]) re-runs the whole query from scratch
    /// with backoff. Inside an explicit transaction the conflict is surfaced
    /// unchanged so the caller can replay the transaction as a unit.
    pub(crate) async fn execute_query(
        &mut self,
        sql: &str,
        params: Vec<Value>,
    ) -> Result<Vec<Box<dyn Row>>, Error> {
        let tx_control = classify_tx(sql);
        let allow_retry = self.tx_depth == 0;

        let mut attempt: u32 = 0;
        let outcome = loop {
            // Re-map params each attempt. `turso::Value` is `Clone`, but
            // re-mapping keeps the retry path self-contained and is cheap since
            // retries are rare.
            let turso_params: Vec<turso::Value> = params
                .iter()
                .map(value_to_turso)
                .collect::<Result<Vec<_>, _>>()?;
            match self.run_query_once(sql, turso_params).await {
                Ok(rows) => break Ok(rows),
                Err(e) => {
                    if allow_retry && attempt < MAX_RETRIES && is_retryable(&e) {
                        attempt += 1;
                        let delay = backoff_delay(attempt);
                        log::debug!(
                            "turso: query hit transient conflict ({e}); retry {attempt}/{MAX_RETRIES} in {delay:?}"
                        );
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    break Err(e);
                }
            }
        };

        self.apply_tx_control(tx_control, outcome.is_ok());

        outcome.map_err(|e| {
            log::warn!("turso: query failed: {}", e);
            TursoError::from(e).into()
        })
    }

    /// Run a single SELECT and materialise its rows, returning the raw
    /// [`turso::Error`] on failure so the caller can classify retryability.
    async fn run_query_once(
        &self,
        sql: &str,
        turso_params: Vec<turso::Value>,
    ) -> Result<Vec<Box<dyn Row>>, turso::Error> {
        let mut rows_result = self.conn.query(sql, turso_params).await?;

        let column_count = rows_result.column_count();

        // Build column metadata. turso 0.5 removed column_type() from Rows,
        // so we start with Null and refine from the first row's actual values.
        let mut columns: Vec<TursoColumn> = Vec::with_capacity(column_count);
        for i in 0..column_count {
            let name = rows_result.column_name(i).unwrap_or_default();
            columns.push(TursoColumn::new(name, i, TursoDataType::Null));
        }
        let columns = Arc::new(columns);

        let mut data: Vec<Box<dyn Row>> = Vec::new();
        while let Some(row) = rows_result.next().await? {
            let mut values = Vec::with_capacity(column_count);
            for i in 0..column_count {
                let v = row.get_value(i)?;
                // Infer type from the actual value variant.
                // Fall back to Null for null values.
                let data_type = match &v {
                    turso::Value::Null => columns[i].type_info,
                    other => TursoDataType::from(other),
                };
                values.push(Some(TursoValue::with_type(v, data_type)));
            }
            data.push(Box::new(TursoRow {
                values,
                columns: columns.clone(),
                json_detect: self.json_detect,
            }));
        }
        Ok(data)
    }

    /// Execute a non-SELECT statement and return the result.
    ///
    /// Autocommit statements are retried on transient write conflicts (see
    /// [`execute_query`](Self::execute_query)); statements inside an explicit
    /// transaction are surfaced unchanged. `BEGIN`/`COMMIT`/`ROLLBACK` also
    /// drive the connection's transaction-depth tracking.
    pub(crate) async fn execute_exec(
        &mut self,
        sql: &str,
        params: Vec<Value>,
    ) -> Result<ExecResult, Error> {
        let tx_control = classify_tx(sql);
        let allow_retry = self.tx_depth == 0;

        let mut attempt: u32 = 0;
        let outcome = loop {
            let turso_params: Vec<turso::Value> = params
                .iter()
                .map(value_to_turso)
                .collect::<Result<Vec<_>, _>>()?;
            match self.conn.execute(sql, turso_params).await {
                Ok(rows_affected) => break Ok(rows_affected),
                Err(e) => {
                    if allow_retry && attempt < MAX_RETRIES && is_retryable(&e) {
                        attempt += 1;
                        let delay = backoff_delay(attempt);
                        log::debug!(
                            "turso: exec hit transient conflict ({e}); retry {attempt}/{MAX_RETRIES} in {delay:?}"
                        );
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    break Err(e);
                }
            }
        };

        self.apply_tx_control(tx_control, outcome.is_ok());

        match outcome {
            Ok(rows_affected) => {
                let last_id = self.conn.last_insert_rowid();
                Ok(TursoQueryResult::new(rows_affected, last_id).into())
            }
            Err(e) => {
                log::warn!("turso: exec failed: {}", e);
                Err(TursoError::from(e).into())
            }
        }
    }

    /// Update the tracked transaction nesting depth after a statement runs.
    ///
    /// A `BEGIN`/`SAVEPOINT` only counts once it actually succeeded; a
    /// `COMMIT`/`ROLLBACK` always steps the depth down because it ends the
    /// transaction even when it fails (a failed commit aborts).
    fn apply_tx_control(&mut self, control: TxControl, succeeded: bool) {
        match control {
            TxControl::Begin => {
                if succeeded {
                    self.tx_depth += 1;
                }
            }
            TxControl::End => {
                self.tx_depth = self.tx_depth.saturating_sub(1);
            }
            TxControl::None => {}
        }
    }
}
