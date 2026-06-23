//! Connection checks and arbitrary query execution.

use std::time::Instant;

use serde_json::{json, Value};

use crate::client::{Client, QueryResult};
use crate::error::PluginError;
use crate::handlers::{connect, req_str, respond};
use crate::utils::pagination::offset_for;
use crate::utils::sql::{is_wrappable, returns_rows, strip_trailing_semicolons};

pub fn test_connection(id: Value, params: &Value) -> Value {
    respond(id, {
        connect(params).and_then(|client| {
            client.health_check()?;
            Ok(json!({ "success": true }))
        })
    })
}

pub fn ping(id: Value, params: &Value) -> Value {
    // Lightweight liveness probe: a failed health check tells the host the
    // connection is dead so it can disconnect.
    respond(id, {
        connect(params).and_then(|client| {
            client.health_check()?;
            Ok(Value::Null)
        })
    })
}

pub fn execute_query(id: Value, params: &Value) -> Value {
    respond(id, execute_query_impl(params))
}

fn execute_query_impl(params: &Value) -> Result<Value, PluginError> {
    let client = connect(params)?;
    let query = req_str(params, "query")?;
    let page = params.get("page").and_then(Value::as_u64);
    let page_size = params.get("page_size").and_then(Value::as_u64);

    let started = Instant::now();
    let trimmed = strip_trailing_semicolons(&query);

    if returns_rows(&query) {
        match (page_size, is_wrappable(&query)) {
            (Some(size), true) if size > 0 => {
                let offset = offset_for(page.unwrap_or(1), size);
                let paged =
                    format!("SELECT * FROM ({trimmed}) AS _tab_page LIMIT {size} OFFSET {offset}");
                let result = client.query(&paged, &[])?;
                let total = count_rows(&client, trimmed).unwrap_or(result.rows.len() as u64);
                Ok(build_payload(result, total, started))
            }
            _ => {
                let result = client.query(trimmed, &[])?;
                let total = result.rows.len() as u64;
                Ok(build_payload(result, total, started))
            }
        }
    } else {
        // DML/DDL: no result set, report the affected-row count.
        let affected = client.execute(trimmed, &[])?;
        Ok(json!({
            "columns": [],
            "rows": [],
            "total_count": affected,
            "execution_time_ms": started.elapsed().as_millis() as u64,
        }))
    }
}

pub fn explain_query(id: Value, params: &Value) -> Value {
    respond(id, {
        connect(params).and_then(|client| {
            let query = req_str(params, "query")?;
            let started = Instant::now();
            let sql = format!("EXPLAIN QUERY PLAN {}", strip_trailing_semicolons(&query));
            let result = client.query(&sql, &[])?;
            let total = result.rows.len() as u64;
            Ok(build_payload(result, total, started))
        })
    })
}

fn count_rows(client: &Client, inner_sql: &str) -> Option<u64> {
    let sql = format!("SELECT COUNT(*) FROM ({inner_sql}) AS _tab_count");
    let result = client.query(&sql, &[]).ok()?;
    result
        .rows
        .first()
        .and_then(|row| row.first())
        .and_then(Value::as_u64)
}

fn build_payload(result: QueryResult, total_count: u64, started: Instant) -> Value {
    json!({
        "columns": result.columns,
        "rows": result.rows,
        "total_count": total_count,
        "execution_time_ms": started.elapsed().as_millis() as u64,
    })
}
