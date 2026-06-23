//! DDL generation and the few DDL mutations libSQL/SQLite supports.
//!
//! The `get_*_sql` methods return SQL strings the host may show before running
//! them through `execute_query`. SQLite cannot retype columns or add foreign
//! keys to an existing table, so those methods return an explicit unsupported
//! error rather than faking success.

use serde_json::{json, Value};

use crate::error::PluginError;
use crate::handlers::{cell_str, connect, req_str, respond};
use crate::utils::identifiers::{quote, quote_literal};

pub fn get_create_table_sql(id: Value, params: &Value) -> Value {
    respond(id, {
        connect(params).and_then(|client| {
            let table = req_str(params, "table")?;
            let r = client.query(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                &[json!(table)],
            )?;
            let sql = r
                .rows
                .first()
                .and_then(|row| cell_str(row, 0))
                .ok_or_else(|| PluginError::internal(format!("table '{table}' not found")))?;
            Ok(Value::String(sql))
        })
    })
}

pub fn get_add_column_sql(id: Value, params: &Value) -> Value {
    respond(id, {
        let table = req_str(params, "table");
        table.and_then(|table| {
            let column = params
                .get("column")
                .ok_or_else(|| PluginError::invalid_params("missing 'column' definition"))?;
            Ok(Value::String(build_add_column_sql(&table, column)?))
        })
    })
}

pub fn get_alter_column_sql(id: Value, _params: &Value) -> Value {
    respond(
        id,
        Err(PluginError::unsupported(
            "libSQL/SQLite cannot alter an existing column's type or constraints",
        )),
    )
}

pub fn get_create_index_sql(id: Value, params: &Value) -> Value {
    respond(id, {
        let table = req_str(params, "table");
        table.and_then(|table| {
            let index = params
                .get("index")
                .ok_or_else(|| PluginError::invalid_params("missing 'index' definition"))?;
            Ok(Value::String(build_create_index_sql(&table, index)?))
        })
    })
}

pub fn get_create_foreign_key_sql(id: Value, _params: &Value) -> Value {
    respond(
        id,
        Err(PluginError::unsupported(
            "libSQL/SQLite cannot add a foreign key to an existing table; define it at CREATE TABLE time",
        )),
    )
}

pub fn drop_index(id: Value, params: &Value) -> Value {
    respond(id, {
        connect(params).and_then(|client| {
            let index_name = req_str(params, "index_name")?;
            client.execute(&format!("DROP INDEX IF EXISTS {}", quote(&index_name)), &[])?;
            Ok(Value::Null)
        })
    })
}

pub fn drop_foreign_key(id: Value, _params: &Value) -> Value {
    respond(
        id,
        Err(PluginError::unsupported(
            "libSQL/SQLite cannot drop a foreign key constraint from an existing table",
        )),
    )
}

// ---------------------------------------------------------------------------
// Pure SQL builders
// ---------------------------------------------------------------------------

fn column_field<'a>(column: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| column.get(*k))
}

/// Render a default value for inclusion in DDL. String values are treated as
/// literals (quoted); numbers/booleans pass through.
fn render_default(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(s) => Some(quote_literal(s)),
        Value::Bool(b) => Some(if *b { "1".into() } else { "0".into() }),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn build_add_column_sql(table: &str, column: &Value) -> Result<String, PluginError> {
    let name = column
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| PluginError::invalid_params("column definition needs a 'name'"))?;
    let data_type = column_field(column, &["data_type", "type"])
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("TEXT");

    let mut sql = format!(
        "ALTER TABLE {} ADD COLUMN {} {}",
        quote(table),
        quote(name),
        data_type
    );

    let default = column_field(column, &["column_default", "default"]).and_then(render_default);
    if let Some(default) = &default {
        sql.push_str(&format!(" DEFAULT {default}"));
    }

    let nullable = column_field(column, &["is_nullable", "nullable"])
        .and_then(Value::as_bool)
        .unwrap_or(true);
    // SQLite only accepts NOT NULL on ADD COLUMN when a default is provided.
    if !nullable && default.is_some() {
        sql.push_str(" NOT NULL");
    }

    Ok(sql)
}

fn build_create_index_sql(table: &str, index: &Value) -> Result<String, PluginError> {
    let name = column_field(index, &["index_name", "name"])
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| PluginError::invalid_params("index definition needs a name"))?;

    let columns: Vec<String> = index
        .get("columns")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(quote)
                .collect()
        })
        .unwrap_or_default();

    if columns.is_empty() {
        return Err(PluginError::invalid_params(
            "index definition needs at least one column",
        ));
    }

    let unique = index
        .get("is_unique")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(format!(
        "CREATE {}INDEX {} ON {} ({})",
        if unique { "UNIQUE " } else { "" },
        quote(name),
        quote(table),
        columns.join(", "),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn add_column_basic() {
        let col = json!({ "name": "age", "data_type": "INTEGER" });
        assert_eq!(
            build_add_column_sql("users", &col).unwrap(),
            "ALTER TABLE \"users\" ADD COLUMN \"age\" INTEGER"
        );
    }

    #[test]
    fn add_column_with_default_and_not_null() {
        let col = json!({
            "name": "status", "data_type": "TEXT",
            "is_nullable": false, "column_default": "active"
        });
        assert_eq!(
            build_add_column_sql("t", &col).unwrap(),
            "ALTER TABLE \"t\" ADD COLUMN \"status\" TEXT DEFAULT 'active' NOT NULL"
        );
    }

    #[test]
    fn add_column_not_null_without_default_drops_not_null() {
        let col = json!({ "name": "x", "type": "INTEGER", "is_nullable": false });
        // No default => NOT NULL is omitted (SQLite would otherwise reject it).
        assert_eq!(
            build_add_column_sql("t", &col).unwrap(),
            "ALTER TABLE \"t\" ADD COLUMN \"x\" INTEGER"
        );
    }

    #[test]
    fn add_column_requires_name() {
        assert!(build_add_column_sql("t", &json!({ "data_type": "TEXT" })).is_err());
    }

    #[test]
    fn create_index_unique_multi_column() {
        let idx = json!({ "index_name": "idx_a_b", "columns": ["a", "b"], "is_unique": true });
        assert_eq!(
            build_create_index_sql("t", &idx).unwrap(),
            "CREATE UNIQUE INDEX \"idx_a_b\" ON \"t\" (\"a\", \"b\")"
        );
    }

    #[test]
    fn create_index_plain() {
        let idx = json!({ "name": "idx_email", "columns": ["email"] });
        assert_eq!(
            build_create_index_sql("users", &idx).unwrap(),
            "CREATE INDEX \"idx_email\" ON \"users\" (\"email\")"
        );
    }

    #[test]
    fn create_index_requires_columns() {
        assert!(build_create_index_sql("t", &json!({ "name": "i", "columns": [] })).is_err());
    }
}
