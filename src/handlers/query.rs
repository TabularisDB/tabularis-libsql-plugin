//! Connection checks and arbitrary query execution.

use std::time::Instant;

use serde_json::{json, Value};

use crate::client::QueryResult;
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
    let page = params.get("page").and_then(Value::as_u64).unwrap_or(1);
    let limit = params
        .get("limit")
        .or_else(|| params.get("page_size"))
        .and_then(Value::as_u64)
        .filter(|l| *l > 0);

    let started = Instant::now();
    let trimmed = strip_trailing_semicolons(&query);

    if returns_rows(&query) {
        if let Some(size) = limit.filter(|_| is_wrappable(&query)) {
            // Fetch one row past the page: the host contract derives `has_more`
            // from the extra row (mirrors the built-in drivers' LIMIT +1 trick).
            let offset = offset_for(page, size);
            let paged = format!(
                "SELECT * FROM ({trimmed}) AS _tab_page LIMIT {} OFFSET {offset}",
                size + 1
            );
            let mut result = client.query(&paged, &[])?;
            let has_more = result.rows.len() > size as usize;
            if has_more {
                result.rows.truncate(size as usize);
            }
            let pagination = json!({
                "page": page,
                "page_size": size,
                "total_rows": Value::Null,
                "has_more": has_more,
            });
            Ok(build_payload(
                result,
                0,
                has_more,
                started,
                Some(pagination),
            ))
        } else {
            let result = client.query(trimmed, &[])?;
            Ok(build_payload(result, 0, false, started, None))
        }
    } else {
        // DML/DDL: no result set, report the affected-row count.
        let affected = client.execute(trimmed, &[])?;
        Ok(json!({
            "columns": [],
            "rows": [],
            "affected_rows": affected,
            "truncated": false,
            "pagination": Value::Null,
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
            Ok(build_payload(result, 0, false, started, None))
        })
    })
}

/// Serialise a result set into the host's `QueryResult` contract
/// (`affected_rows`/`truncated`/`pagination` are required fields).
fn build_payload(
    result: QueryResult,
    affected_rows: u64,
    truncated: bool,
    started: Instant,
    pagination: Option<Value>,
) -> Value {
    json!({
        "columns": result.columns,
        "rows": result.rows,
        "affected_rows": affected_rows,
        "truncated": truncated,
        "pagination": pagination,
        "execution_time_ms": started.elapsed().as_millis() as u64,
    })
}
