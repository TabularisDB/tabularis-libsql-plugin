//! DDL generation and the few DDL mutations libSQL/SQLite supports.
//!
//! The `get_*_sql` methods return SQL statements (as a JSON array, matching
//! the host's `Vec<String>` contract) the host may show before running them
//! through `execute_query`.
//!
//! Vanilla SQLite cannot retype columns or add/drop foreign keys on an
//! existing table. The libSQL fork used by remote Turso / sqld servers adds
//! `ALTER TABLE ... ALTER COLUMN col TO col <type> [REFERENCES ...]`, which
//! covers all three. Local files keep returning an explicit unsupported
//! error rather than faking success; remote connections build and run the
//! libSQL statements.

use serde_json::{json, Value};

use crate::client::Client;
use crate::error::PluginError;
use crate::handlers::{cell_i64, cell_str, connect, req_str, respond};
use crate::utils::identifiers::{quote, quote_literal};

pub fn get_create_table_sql(id: Value, params: &Value) -> Value {
    respond(id, {
        connect(params).and_then(|client| {
            let table = req_str(params, "table").or_else(|_| req_str(params, "table_name"))?;
            let r = client.query(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                &[json!(table)],
            )?;
            let sql = r
                .rows
                .first()
                .and_then(|row| cell_str(row, 0))
                .ok_or_else(|| PluginError::internal(format!("table '{table}' not found")))?;
            Ok(json!([sql]))
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
            Ok(json!([build_add_column_sql(&table, column)?]))
        })
    })
}

pub fn get_alter_column_sql(id: Value, params: &Value) -> Value {
    respond(
        id,
        (|| {
            let client = connect(params)?;
            require_remote(&client)?;
            let table = req_str(params, "table")?;
            let old_column = params
                .get("old_column")
                .ok_or_else(|| PluginError::invalid_params("missing 'old_column' definition"))?;
            let new_column = params
                .get("new_column")
                .ok_or_else(|| PluginError::invalid_params("missing 'new_column' definition"))?;
            let old_name = column_name(old_column)?;
            let new_name = column_name(new_column)?;
            let data_type = column_field(new_column, &["data_type", "type"])
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("TEXT");
            let default = column_field(new_column, &["default_value", "column_default", "default"])
                .and_then(render_default);
            let nullable = column_field(new_column, &["is_nullable", "nullable"])
                .and_then(Value::as_bool)
                .unwrap_or(true);
            Ok(json!([build_alter_column_sql(
                &table, &old_name, &new_name, data_type, default, nullable
            )]))
        })(),
    )
}

pub fn get_create_index_sql(id: Value, params: &Value) -> Value {
    respond(id, {
        let table = req_str(params, "table");
        table.and_then(|table| {
            let index = params
                .get("index")
                .ok_or_else(|| PluginError::invalid_params("missing 'index' definition"))?;
            Ok(json!([build_create_index_sql(&table, index)?]))
        })
    })
}

pub fn get_create_foreign_key_sql(id: Value, params: &Value) -> Value {
    respond(
        id,
        (|| {
            let client = connect(params)?;
            require_remote(&client)?;
            let table = req_str(params, "table")?;
            let column = req_str(params, "column")?;
            let ref_table = req_str(params, "ref_table")?;
            let ref_column = req_str(params, "ref_column")?;
            let col_type = column_type_for(&client, &table, &column)?;
            let on_delete = params.get("on_delete").and_then(Value::as_str);
            let on_update = params.get("on_update").and_then(Value::as_str);
            Ok(json!([build_fk_add_sql(
                &table,
                &column,
                &col_type,
                &ref_table,
                &ref_column,
                on_delete,
                on_update,
            )]))
        })(),
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

pub fn drop_foreign_key(id: Value, params: &Value) -> Value {
    respond(
        id,
        (|| {
            let client = connect(params)?;
            require_remote(&client)?;
            let table = req_str(params, "table")?;
            let fk_name = req_str(params, "fk_name")?;
            let column = foreign_key_column_for(&client, &table, &fk_name)?;
            let col_type = column_type_for(&client, &table, &column)?;
            client.execute(&build_fk_drop_sql(&table, &column, &col_type), &[])?;
            Ok(Value::Null)
        })(),
    )
}

/// `ALTER TABLE ... ALTER COLUMN` exists only in the libSQL fork (remote
/// Turso / sqld). Local bundled SQLite rejects the syntax, so fail early
/// with a clear message instead of a confusing parse error.
fn require_remote(client: &Client) -> Result<(), PluginError> {
    if client.is_remote() {
        Ok(())
    } else {
        Err(PluginError::unsupported(
            "local SQLite cannot alter an existing column's type or foreign key; this requires the libSQL fork used by remote Turso / sqld servers",
        ))
    }
}

// ---------------------------------------------------------------------------
// Pure SQL builders
// ---------------------------------------------------------------------------

fn column_field<'a>(column: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| column.get(*k))
}

fn column_name(column: &Value) -> Result<String, PluginError> {
    column
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| PluginError::invalid_params("column definition needs a 'name'"))
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

    let default = column_field(column, &["default_value", "column_default", "default"])
        .and_then(render_default);
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

/// Build a libSQL `ALTER TABLE ... ALTER COLUMN` statement (type change and,
/// optionally, a new DEFAULT / NOT NULL). Only available on the libSQL fork
/// used by remote Turso / sqld servers; local SQLite has no equivalent.
///
/// `default` is an already-rendered SQL literal (e.g. `'hai'`). NOT NULL is
/// only applied alongside a default, mirroring the ADD COLUMN restriction.
fn build_alter_column_sql(
    table: &str,
    old_name: &str,
    new_name: &str,
    data_type: &str,
    default: Option<String>,
    nullable: bool,
) -> String {
    let mut sql = format!(
        "ALTER TABLE {} ALTER COLUMN {} TO {} {}",
        quote(table),
        quote(old_name),
        quote(new_name),
        data_type
    );
    if let Some(default) = &default {
        sql.push_str(&format!(" DEFAULT {default}"));
        if !nullable {
            sql.push_str(" NOT NULL");
        }
    }
    sql
}

/// Build a libSQL `ALTER TABLE ... ALTER COLUMN ... REFERENCES` statement
/// that adds a foreign key to an existing column. `col_type` is the column's
/// current declared type (the TO clause requires a full column definition).
fn build_fk_add_sql(
    table: &str,
    column: &str,
    col_type: &str,
    ref_table: &str,
    ref_column: &str,
    on_delete: Option<&str>,
    on_update: Option<&str>,
) -> String {
    let mut sql = format!(
        "ALTER TABLE {} ALTER COLUMN {} TO {} {} REFERENCES {}({})",
        quote(table),
        quote(column),
        quote(column),
        col_type,
        quote(ref_table),
        quote(ref_column),
    );
    if let Some(action) = on_delete.filter(|a| !a.is_empty()) {
        sql.push_str(&format!(" ON DELETE {action}"));
    }
    if let Some(action) = on_update.filter(|a| !a.is_empty()) {
        sql.push_str(&format!(" ON UPDATE {action}"));
    }
    sql
}

/// Build a libSQL statement that drops an existing column's foreign key:
/// the same ALTER COLUMN form without the REFERENCES clause.
fn build_fk_drop_sql(table: &str, column: &str, col_type: &str) -> String {
    format!(
        "ALTER TABLE {} ALTER COLUMN {} TO {} {}",
        quote(table),
        quote(column),
        quote(column),
        col_type
    )
}

/// Look up a column's declared type via `PRAGMA table_info`, defaulting to
/// TEXT when the type is blank (SQLite's bare-column shorthand).
fn column_type_for(client: &Client, table: &str, column: &str) -> Result<String, PluginError> {
    let r = client.query(&format!("PRAGMA table_info({})", quote(table)), &[])?;
    for row in &r.rows {
        // table_info columns: cid, name, type, notnull, dflt_value, pk
        if cell_str(row, 1).as_deref() == Some(column) {
            let raw = cell_str(row, 2).unwrap_or_default();
            return Ok(if raw.is_empty() {
                "TEXT".to_string()
            } else {
                raw
            });
        }
    }
    Err(PluginError::invalid_params(format!(
        "column '{column}' not found in table '{table}'"
    )))
}

/// Map a host-side FK name (plugin-generated `fk_<table>_<column>_<id>`) back
/// to the SQLite column it constrains. SQLite FKs carry no names of their own,
/// so the mapping re-derives the same naming scheme from
/// `PRAGMA foreign_key_list`.
fn foreign_key_column_for(
    client: &Client,
    table: &str,
    fk_name: &str,
) -> Result<String, PluginError> {
    let r = client.query(&format!("PRAGMA foreign_key_list({})", quote(table)), &[])?;
    for row in &r.rows {
        // foreign_key_list columns: id, seq, table, from, to, on_update, on_delete, match
        let id = cell_i64(row, 0);
        let from = cell_str(row, 3).unwrap_or_default();
        if format!("fk_{table}_{from}_{id}") == fk_name {
            return Ok(from);
        }
    }
    Err(PluginError::invalid_params(format!(
        "foreign key '{fk_name}' not found on table '{table}'"
    )))
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
    use crate::client::Client;
    use crate::models::ConnectionParams;
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

    // -----------------------------------------------------------------------
    // ALTER COLUMN (libSQL fork extension; remote Turso/sqld only)
    // -----------------------------------------------------------------------

    #[test]
    fn alter_column_type_change() {
        assert_eq!(
            build_alter_column_sql("t", "v", "v", "TEXT", None, true),
            "ALTER TABLE \"t\" ALTER COLUMN \"v\" TO \"v\" TEXT"
        );
    }

    #[test]
    fn alter_column_rename_and_retype() {
        assert_eq!(
            build_alter_column_sql("t", "a", "b", "INTEGER", None, true),
            "ALTER TABLE \"t\" ALTER COLUMN \"a\" TO \"b\" INTEGER"
        );
    }

    #[test]
    fn alter_column_with_default() {
        assert_eq!(
            build_alter_column_sql("t", "v", "v", "TEXT", Some("'hai'".into()), true),
            "ALTER TABLE \"t\" ALTER COLUMN \"v\" TO \"v\" TEXT DEFAULT 'hai'"
        );
    }

    #[test]
    fn alter_column_not_null_requires_default() {
        // NOT NULL is only applied with a default (SQLite-style restriction).
        assert_eq!(
            build_alter_column_sql("t", "v", "v", "TEXT", None, false),
            "ALTER TABLE \"t\" ALTER COLUMN \"v\" TO \"v\" TEXT"
        );
        assert_eq!(
            build_alter_column_sql("t", "v", "v", "TEXT", Some("'x'".into()), false),
            "ALTER TABLE \"t\" ALTER COLUMN \"v\" TO \"v\" TEXT DEFAULT 'x' NOT NULL"
        );
    }

    #[test]
    fn alter_column_quotes_identifiers() {
        assert_eq!(
            build_alter_column_sql("weird\"t", "a\"b", "c", "TEXT", None, true),
            "ALTER TABLE \"weird\"\"t\" ALTER COLUMN \"a\"\"b\" TO \"c\" TEXT"
        );
    }

    // -----------------------------------------------------------------------
    // Foreign keys via ALTER COLUMN (libSQL fork extension)
    // -----------------------------------------------------------------------

    #[test]
    fn fk_add_basic() {
        assert_eq!(
            build_fk_add_sql("emails", "user_id", "INT", "users", "id", None, None),
            "ALTER TABLE \"emails\" ALTER COLUMN \"user_id\" TO \"user_id\" INT REFERENCES \"users\"(\"id\")"
        );
    }

    #[test]
    fn fk_add_with_actions() {
        assert_eq!(
            build_fk_add_sql("emails", "user_id", "INT", "users", "id", Some("CASCADE"), Some("SET NULL")),
            "ALTER TABLE \"emails\" ALTER COLUMN \"user_id\" TO \"user_id\" INT REFERENCES \"users\"(\"id\") ON DELETE CASCADE ON UPDATE SET NULL"
        );
    }

    #[test]
    fn fk_add_skips_empty_actions() {
        assert_eq!(
            build_fk_add_sql("emails", "user_id", "INT", "users", "id", Some(""), Some("")),
            "ALTER TABLE \"emails\" ALTER COLUMN \"user_id\" TO \"user_id\" INT REFERENCES \"users\"(\"id\")"
        );
    }

    #[test]
    fn fk_drop_basic() {
        assert_eq!(
            build_fk_drop_sql("emails", "user_id", "INT"),
            "ALTER TABLE \"emails\" ALTER COLUMN \"user_id\" TO \"user_id\" INT"
        );
    }

    // -----------------------------------------------------------------------
    // Introspection against a real in-memory database
    // -----------------------------------------------------------------------

    fn in_memory_client() -> Client {
        let cp = ConnectionParams {
            database: Some(":memory:".into()),
            ..Default::default()
        };
        Client::connect(&cp).expect("in-memory connection")
    }

    #[test]
    fn column_type_for_reads_pragma() {
        let client = in_memory_client();
        client
            .execute("CREATE TABLE t(v TEXT, n INTEGER)", &[])
            .expect("create table");
        assert_eq!(column_type_for(&client, "t", "v").unwrap(), "TEXT");
        assert_eq!(column_type_for(&client, "t", "n").unwrap(), "INTEGER");
    }

    #[test]
    fn column_type_for_defaults_blank_type_to_text() {
        let client = in_memory_client();
        client
            .execute("CREATE TABLE t(v)", &[])
            .expect("create table");
        assert_eq!(column_type_for(&client, "t", "v").unwrap(), "TEXT");
    }

    #[test]
    fn column_type_for_missing_column_errors() {
        let client = in_memory_client();
        client
            .execute("CREATE TABLE t(v TEXT)", &[])
            .expect("create table");
        assert!(column_type_for(&client, "t", "nope").is_err());
    }

    #[test]
    fn foreign_key_column_for_matches_constraint_name() {
        let client = in_memory_client();
        client
            .execute("CREATE TABLE users(id INT PRIMARY KEY)", &[])
            .expect("create users");
        client
            .execute("CREATE TABLE emails(user_id INT REFERENCES users(id))", &[])
            .expect("create emails");
        assert_eq!(
            foreign_key_column_for(&client, "emails", "fk_emails_user_id_0").unwrap(),
            "user_id"
        );
    }

    #[test]
    fn foreign_key_column_for_unknown_errors() {
        let client = in_memory_client();
        client
            .execute("CREATE TABLE users(id INT PRIMARY KEY)", &[])
            .expect("create users");
        client
            .execute("CREATE TABLE emails(user_id INT REFERENCES users(id))", &[])
            .expect("create emails");
        assert!(foreign_key_column_for(&client, "emails", "fk_emails_nope_0").is_err());
    }

    // -----------------------------------------------------------------------
    // Handler-level backend gating: local SQLite -> clear unsupported error
    // -----------------------------------------------------------------------

    #[test]
    fn alter_column_sql_local_backend_is_unsupported() {
        let resp = get_alter_column_sql(
            json!(1),
            &json!({
                "params": { "database": ":memory:" },
                "table": "t",
                "old_column": { "name": "v", "data_type": "TEXT" },
                "new_column": { "name": "v", "data_type": "INTEGER" }
            }),
        );
        assert_eq!(resp["error"]["code"], -32601);
        assert!(resp["error"]["message"].as_str().unwrap().contains("Turso"));
    }

    #[test]
    fn alter_column_sql_remote_reports_missing_params() {
        let resp = get_alter_column_sql(
            json!(1),
            &json!({
                "params": { "database": "libsql://db.turso.io" },
                "table": "t"
            }),
        );
        assert_eq!(resp["error"]["code"], -32602);
    }

    #[test]
    fn create_foreign_key_sql_local_is_unsupported() {
        let resp = get_create_foreign_key_sql(
            json!(1),
            &json!({
                "params": { "database": ":memory:" },
                "table": "emails",
                "column": "user_id",
                "ref_table": "users",
                "ref_column": "id"
            }),
        );
        assert_eq!(resp["error"]["code"], -32601);
        assert!(resp["error"]["message"].as_str().unwrap().contains("Turso"));
    }

    #[test]
    fn drop_foreign_key_local_is_unsupported() {
        let resp = drop_foreign_key(
            json!(1),
            &json!({
                "params": { "database": ":memory:" },
                "table": "emails",
                "fk_name": "fk_emails_user_id_0"
            }),
        );
        assert_eq!(resp["error"]["code"], -32601);
        assert!(resp["error"]["message"].as_str().unwrap().contains("Turso"));
    }

    // -----------------------------------------------------------------------
    // Host-contract return shapes: SQL arrays, `table_name` key
    // -----------------------------------------------------------------------

    #[test]
    fn add_column_sql_returns_array() {
        let resp = get_add_column_sql(
            json!(1),
            &json!({ "table": "users", "column": { "name": "age", "data_type": "INTEGER" } }),
        );
        assert_eq!(
            resp["result"],
            json!(["ALTER TABLE \"users\" ADD COLUMN \"age\" INTEGER"])
        );
    }

    #[test]
    fn create_table_sql_uses_host_table_name_key_and_returns_array() {
        let dir = std::env::temp_dir().join(format!("tab_libsql_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let db = dir.join("create_table_test.db");
        let _ = std::fs::remove_file(&db);
        {
            let conn = rusqlite::Connection::open(&db).expect("open temp db");
            conn.execute_batch("CREATE TABLE users(id INTEGER PRIMARY KEY)")
                .expect("create");
        }
        let resp = get_create_table_sql(
            json!(1),
            &json!({
                "params": { "database": db.to_str().unwrap() },
                "table_name": "users"
            }),
        );
        assert_eq!(
            resp["result"],
            json!(["CREATE TABLE users(id INTEGER PRIMARY KEY)"])
        );
        let _ = std::fs::remove_file(&db);
    }
}
