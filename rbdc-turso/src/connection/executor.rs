//! Executor logic for the Turso connection.
//!
//! Query execution pipeline converting between `turso` result types
//! and the rbdc trait types (Row, ExecResult).

use crate::column::TursoColumn;
use crate::connection::TursoConnection;
use crate::error::TursoError;
use crate::query_result::TursoQueryResult;
use crate::row::TursoRow;
use crate::value::{value_to_turso, TursoDataType, TursoValue};
use rbdc::db::{ExecResult, Row};
use rbdc::error::Error;
use rbs::Value;
use std::sync::Arc;

impl TursoConnection {
    /// Execute a SELECT-style query and return rows.
    pub(crate) async fn execute_query(
        &mut self,
        sql: &str,
        params: Vec<Value>,
    ) -> Result<Vec<Box<dyn Row>>, Error> {
        let turso_params: Vec<turso::Value> = params
            .iter()
            .map(value_to_turso)
            .collect::<Result<Vec<_>, _>>()?;

        let mut rows_result = self
            .conn
            .query(sql, turso_params)
            .await
            .map_err(|e| {
                log::warn!("turso: query failed: {}", e);
                TursoError::from(e)
            })?;

        let column_count = rows_result.column_count();

        // Build column metadata. turso 0.5 removed column_type() from Rows,
        // so we start with Null and refine from the first row's actual values.
        let mut columns: Vec<TursoColumn> = Vec::with_capacity(column_count);
        for i in 0..column_count {
            let name = rows_result
                .column_name(i)
                .unwrap_or_default();
            columns.push(TursoColumn::new(name, i, TursoDataType::Null));
        }
        let columns = Arc::new(columns);

        let mut data: Vec<Box<dyn Row>> = Vec::new();
        while let Some(row) = rows_result.next().await.map_err(TursoError::from)? {
            let mut values = Vec::with_capacity(column_count);
            for i in 0..column_count {
                let v = row.get_value(i).map_err(TursoError::from)?;
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
    pub(crate) async fn execute_exec(
        &mut self,
        sql: &str,
        params: Vec<Value>,
    ) -> Result<ExecResult, Error> {
        let turso_params: Vec<turso::Value> = params
            .iter()
            .map(value_to_turso)
            .collect::<Result<Vec<_>, _>>()?;

        let rows_affected = self
            .conn
            .execute(sql, turso_params)
            .await
            .map_err(|e| {
                log::warn!("turso: exec failed: {}", e);
                TursoError::from(e)
            })?;

        let last_id = self.conn.last_insert_rowid();

        let result = TursoQueryResult::new(rows_affected, last_id);
        Ok(result.into())
    }
}
