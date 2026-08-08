//! DDL generation for the host's schema dialogs.
//!
//! The `get_*_sql` methods return SQL statements (as a JSON array, matching
//! the host's `Vec<String>` contract) the host may show before running them
//! through `execute_query`. The host calls these without any connection
//! params — they are pure builders over `ColumnDefinition` objects
//! (`{name, data_type, is_nullable, is_pk, is_auto_increment, default_value}`)
//! — so none of them can open a connection or introspect the schema.
//!
//! Vanilla SQLite cannot retype columns or add/drop foreign keys on an
//! existing table. The libSQL fork used by remote Turso / sqld servers adds
//! `ALTER TABLE ... ALTER COLUMN col TO col <type> [DEFAULT ...] [REFERENCES ...]`,
//! which covers both. Statement *builders* cannot tell local from remote (no
//! connection params arrive), so `get_alter_column_sql` emits the libSQL form
//! unconditionally: it works on remote connections and, on local files, the
//! follow-up `execute_query` surfaces SQLite's own parse error. Renames use
//! plain `RENAME COLUMN`, which vanilla SQLite supports too.
//!
//! `get_create_foreign_key_sql` cannot be implemented at all: the libSQL
//! `ALTER COLUMN` rewrite replaces the column's whole definition (type
//! included), so the builder needs the column's declared type — which the
//! host does not provide for this method (only its name), and without
//! connection params there is no way to look it up. It returns a clear
//! unsupported error; the `.tabularium` `create_foreign_keys` capability is
//! off so the host hides the add-FK dialog. Dropping a foreign key
//! (`drop_foreign_key`) does receive connection params and stays supported
//! on remote servers.

use serde_json::{json, Value};

use crate::client::Client;
use crate::error::PluginError;
use crate::handlers::{cell_i64, cell_str, connect, req_str, respond};
use crate::utils::identifiers::quote;

// ---------------------------------------------------------------------------
// Column definitions (host `ColumnDefinition` contract)
// ---------------------------------------------------------------------------

fn col_field<'a>(column: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| column.get(*k))
}

fn col_name(column: &Value) -> Result<String, PluginError> {
    col_field(column, &["name"])
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| PluginError::invalid_params("column definition needs a 'name'"))
}

fn col_type(column: &Value) -> Result<String, PluginError> {
    col_field(column, &["data_type", "type"])
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| PluginError::invalid_params("column definition needs a 'data_type'"))
}

fn col_bool(column: &Value, key: &str) -> bool {
    col_field(column, &[key])
        .map(|v| match v {
            Value::Bool(b) => *b,
            Value::Number(n) => n.as_u64() == Some(1),
            _ => false,
        })
        .unwrap_or(false)
}

/// Columns default to nullable when the flag is absent (SQLite semantics).
fn col_nullable(column: &Value) -> bool {
    col_field(column, &["is_nullable", "nullable"])
        .map(|v| match v {
            Value::Bool(b) => *b,
            Value::Number(n) => n.as_u64() == Some(1),
            _ => true,
        })
        .unwrap_or(true)
}

/// The host sends `default_value` as a ready-to-embed SQL literal (the raw
/// text the user typed in the dialog, e.g. `'active'` or `CURRENT_TIMESTAMP`).
fn col_default(column: &Value) -> Option<&str> {
    col_field(column, &["default_value", "column_default", "default"])
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// CREATE TABLE
// ---------------------------------------------------------------------------

pub fn get_create_table_sql(id: Value, params: &Value) -> Value {
    respond(
        id,
        (|| {
            let table_name = req_str(params, "table_name").or_else(|_| req_str(params, "table"))?;
            let columns = params
                .get("columns")
                .and_then(Value::as_array)
                .ok_or_else(|| PluginError::invalid_params("missing 'columns' definition"))?;
            Ok(json!([build_create_table_sql(&table_name, columns)?]))
        })(),
    )
}

/// Mirror of the host's built-in SQLite driver: a single PK column gets an
/// inline `PRIMARY KEY [AUTOINCREMENT]`, multiple PK columns become a
/// table-level `PRIMARY KEY (a, b)` constraint.
fn build_create_table_sql(table_name: &str, columns: &[Value]) -> Result<String, PluginError> {
    let single_pk = columns.iter().filter(|c| col_bool(c, "is_pk")).count() == 1;
    let mut defs = Vec::new();
    let mut pk_cols = Vec::new();
    for column in columns {
        let name = col_name(column)?;
        let data_type = col_type(column)?;
        let is_pk = col_bool(column, "is_pk");
        let mut def = format!("{} {}", quote(&name), data_type);
        if is_pk && single_pk {
            def.push_str(" PRIMARY KEY");
            if col_bool(column, "is_auto_increment") {
                def.push_str(" AUTOINCREMENT");
            }
        }
        if !col_nullable(column) && !(is_pk && single_pk) {
            def.push_str(" NOT NULL");
        }
        if let Some(default) = col_default(column) {
            def.push_str(&format!(" DEFAULT {default}"));
        }
        defs.push(def);
        if is_pk && !single_pk {
            pk_cols.push(quote(&name));
        }
    }
    if !pk_cols.is_empty() {
        defs.push(format!("PRIMARY KEY ({})", pk_cols.join(", ")));
    }
    Ok(format!(
        "CREATE TABLE {} (\n  {}\n)",
        quote(table_name),
        defs.join(",\n  ")
    ))
}

// ---------------------------------------------------------------------------
// ADD COLUMN
// ---------------------------------------------------------------------------

pub fn get_add_column_sql(id: Value, params: &Value) -> Value {
    respond(
        id,
        (|| {
            let table = req_str(params, "table")?;
            let column = params
                .get("column")
                .ok_or_else(|| PluginError::invalid_params("missing 'column' definition"))?;
            Ok(json!([build_add_column_sql(&table, column)?]))
        })(),
    )
}

fn build_add_column_sql(table: &str, column: &Value) -> Result<String, PluginError> {
    let name = col_name(column)?;
    let data_type = col_type(column)?;
    let mut sql = format!(
        "ALTER TABLE {} ADD COLUMN {} {}",
        quote(table),
        quote(&name),
        data_type
    );
    let default = col_default(column);
    if let Some(default) = default {
        sql.push_str(&format!(" DEFAULT {default}"));
    }
    // SQLite only accepts NOT NULL on ADD COLUMN when a default is provided.
    if !col_nullable(column) && default.is_some() {
        sql.push_str(" NOT NULL");
    }
    Ok(sql)
}

// ---------------------------------------------------------------------------
// ALTER COLUMN
// ---------------------------------------------------------------------------

pub fn get_alter_column_sql(id: Value, params: &Value) -> Value {
    respond(
        id,
        (|| {
            let table = req_str(params, "table")?;
            let old_column = params
                .get("old_column")
                .ok_or_else(|| PluginError::invalid_params("missing 'old_column' definition"))?;
            let new_column = params
                .get("new_column")
                .ok_or_else(|| PluginError::invalid_params("missing 'new_column' definition"))?;
            Ok(json!([build_alter_column_sql(
                &table, old_column, new_column
            )?]))
        })(),
    )
}

/// A rename uses vanilla `RENAME COLUMN` (works on every backend). Any other
/// change emits the libSQL `ALTER COLUMN ... TO ...` form, which replaces the
/// column's whole definition — the new `data_type` is mandatory for it.
fn build_alter_column_sql(
    table: &str,
    old_column: &Value,
    new_column: &Value,
) -> Result<String, PluginError> {
    let old_name = col_name(old_column)?;
    let new_name = col_name(new_column)?;
    if old_name != new_name {
        return Ok(format!(
            "ALTER TABLE {} RENAME COLUMN {} TO {}",
            quote(table),
            quote(&old_name),
            quote(&new_name),
        ));
    }
    let data_type = col_type(new_column)?;
    let mut sql = format!(
        "ALTER TABLE {} ALTER COLUMN {} TO {} {}",
        quote(table),
        quote(&old_name),
        quote(&new_name),
        data_type
    );
    if let Some(default) = col_default(new_column) {
        sql.push_str(&format!(" DEFAULT {default}"));
    }
    if !col_nullable(new_column) {
        sql.push_str(" NOT NULL");
    }
    Ok(sql)
}

// ---------------------------------------------------------------------------
// CREATE INDEX
// ---------------------------------------------------------------------------

pub fn get_create_index_sql(id: Value, params: &Value) -> Value {
    respond(
        id,
        (|| {
            let table = req_str(params, "table")?;
            let name = req_str(params, "index_name").or_else(|_| req_str(params, "name"))?;
            let columns: Vec<String> = params
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
            Ok(json!([build_create_index_sql(
                &table,
                &name,
                &columns,
                col_bool(params, "is_unique"),
            )]))
        })(),
    )
}

fn build_create_index_sql(table: &str, name: &str, columns: &[String], unique: bool) -> String {
    format!(
        "CREATE {}INDEX {} ON {} ({})",
        if unique { "UNIQUE " } else { "" },
        quote(name),
        quote(table),
        columns.join(", "),
    )
}

// ---------------------------------------------------------------------------
// Foreign keys
// ---------------------------------------------------------------------------

/// Not implementable over the host protocol: the libSQL `ALTER COLUMN` form
/// replaces the column's full definition, so it needs the column's declared
/// type, but the host passes only the column *name* for this method and no
/// connection params to look it up against.
pub fn get_create_foreign_key_sql(id: Value, _params: &Value) -> Value {
    respond(
        id,
        Err(PluginError::unsupported(
            "cannot add a foreign key to an existing table: the host does not provide the column's type to the SQL builder and libSQL needs the full column definition; define the foreign key in the CREATE TABLE statement instead",
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

/// Dropping a foreign key is a schema rewrite on the libSQL fork: the same
/// ALTER COLUMN form without the REFERENCES clause. Unlike the SQL builders
/// above, this mutation *does* receive connection params, so the local/remote
/// split can be decided here and the column's type introspected.
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

/// Build a libSQL statement that drops an existing column's foreign key:
/// the ALTER COLUMN form without the REFERENCES clause.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Client;
    use crate::models::ConnectionParams;
    use serde_json::json;

    // -----------------------------------------------------------------------
    // CREATE TABLE builder
    // -----------------------------------------------------------------------

    #[test]
    fn create_table_single_pk_autoincrement() {
        let cols = vec![
            json!({ "name": "id", "data_type": "INTEGER", "is_pk": true, "is_auto_increment": true, "is_nullable": false }),
            json!({ "name": "title", "data_type": "TEXT", "is_nullable": false, "default_value": "'untitled'" }),
            json!({ "name": "body", "data_type": "TEXT" }),
        ];
        assert_eq!(
            build_create_table_sql("blog", &cols).unwrap(),
            "CREATE TABLE \"blog\" (\n  \"id\" INTEGER PRIMARY KEY AUTOINCREMENT,\n  \"title\" TEXT NOT NULL DEFAULT 'untitled',\n  \"body\" TEXT\n)"
        );
    }

    #[test]
    fn create_table_composite_pk() {
        let cols = vec![
            json!({ "name": "a", "data_type": "INTEGER", "is_pk": true }),
            json!({ "name": "b", "data_type": "INTEGER", "is_pk": true }),
        ];
        assert_eq!(
            build_create_table_sql("t", &cols).unwrap(),
            "CREATE TABLE \"t\" (\n  \"a\" INTEGER,\n  \"b\" INTEGER,\n  PRIMARY KEY (\"a\", \"b\")\n)"
        );
    }

    #[test]
    fn create_table_requires_name_and_type() {
        assert!(build_create_table_sql("t", &[json!({ "name": "x" })]).is_err());
        assert!(build_create_table_sql("t", &[json!({ "data_type": "TEXT" })]).is_err());
    }

    #[test]
    fn create_table_quotes_identifiers() {
        let cols = vec![json!({ "name": "we\"ird", "data_type": "TEXT" })];
        assert_eq!(
            build_create_table_sql("my\"t", &cols).unwrap(),
            "CREATE TABLE \"my\"\"t\" (\n  \"we\"\"ird\" TEXT\n)"
        );
    }

    #[test]
    fn create_table_sql_handler_uses_host_payload_shape() {
        let resp = get_create_table_sql(
            json!(1),
            &json!({
                "table_name": "blog",
                "columns": [
                    { "name": "id", "data_type": "INTEGER", "is_pk": true, "is_auto_increment": true, "is_nullable": false, "default_value": null },
                    { "name": "name", "data_type": "TEXT", "is_nullable": true, "is_pk": false, "is_auto_increment": false, "default_value": null }
                ],
                "schema": null
            }),
        );
        assert_eq!(
            resp["result"],
            json!(["CREATE TABLE \"blog\" (\n  \"id\" INTEGER PRIMARY KEY AUTOINCREMENT,\n  \"name\" TEXT\n)"])
        );
    }

    // -----------------------------------------------------------------------
    // ADD COLUMN builder
    // -----------------------------------------------------------------------

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
            "is_nullable": false, "default_value": "'active'"
        });
        assert_eq!(
            build_add_column_sql("t", &col).unwrap(),
            "ALTER TABLE \"t\" ADD COLUMN \"status\" TEXT DEFAULT 'active' NOT NULL"
        );
    }

    #[test]
    fn add_column_default_is_verbatim_literal() {
        let col =
            json!({ "name": "ts", "data_type": "TEXT", "default_value": "CURRENT_TIMESTAMP" });
        assert_eq!(
            build_add_column_sql("t", &col).unwrap(),
            "ALTER TABLE \"t\" ADD COLUMN \"ts\" TEXT DEFAULT CURRENT_TIMESTAMP"
        );
    }

    #[test]
    fn add_column_not_null_without_default_drops_not_null() {
        let col = json!({ "name": "x", "data_type": "INTEGER", "is_nullable": false });
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

    // -----------------------------------------------------------------------
    // ALTER COLUMN builder (libSQL fork extension)
    // -----------------------------------------------------------------------

    #[test]
    fn alter_column_rename_uses_vanilla_sqlite() {
        let old_col = json!({ "name": "a", "data_type": "TEXT" });
        let new_col = json!({ "name": "b", "data_type": "TEXT" });
        assert_eq!(
            build_alter_column_sql("t", &old_col, &new_col).unwrap(),
            "ALTER TABLE \"t\" RENAME COLUMN \"a\" TO \"b\""
        );
    }

    #[test]
    fn alter_column_type_change() {
        let old_col = json!({ "name": "v", "data_type": "TEXT" });
        let new_col = json!({ "name": "v", "data_type": "INTEGER" });
        assert_eq!(
            build_alter_column_sql("t", &old_col, &new_col).unwrap(),
            "ALTER TABLE \"t\" ALTER COLUMN \"v\" TO \"v\" INTEGER"
        );
    }

    #[test]
    fn alter_column_with_default_and_not_null() {
        let old_col = json!({ "name": "v", "data_type": "TEXT" });
        let new_col = json!({
            "name": "v", "data_type": "TEXT",
            "default_value": "'hai'", "is_nullable": false
        });
        assert_eq!(
            build_alter_column_sql("t", &old_col, &new_col).unwrap(),
            "ALTER TABLE \"t\" ALTER COLUMN \"v\" TO \"v\" TEXT DEFAULT 'hai' NOT NULL"
        );
    }

    #[test]
    fn alter_column_requires_new_type() {
        let old_col = json!({ "name": "v", "data_type": "TEXT" });
        assert!(build_alter_column_sql("t", &old_col, &json!({ "name": "v" })).is_err());
    }

    #[test]
    fn alter_column_quotes_identifiers() {
        let old_col = json!({ "name": "a\"b", "data_type": "TEXT" });
        let new_col = json!({ "name": "c", "data_type": "TEXT" });
        assert_eq!(
            build_alter_column_sql("weird\"t", &old_col, &new_col).unwrap(),
            "ALTER TABLE \"weird\"\"t\" RENAME COLUMN \"a\"\"b\" TO \"c\""
        );
    }

    // -----------------------------------------------------------------------
    // CREATE INDEX builder
    // -----------------------------------------------------------------------

    #[test]
    fn create_index_unique_multi_column() {
        let cols = vec![quote("a"), quote("b")];
        assert_eq!(
            build_create_index_sql("t", "idx_a_b", &cols, true),
            "CREATE UNIQUE INDEX \"idx_a_b\" ON \"t\" (\"a\", \"b\")"
        );
    }

    #[test]
    fn create_index_plain() {
        let cols = vec![quote("email")];
        assert_eq!(
            build_create_index_sql("users", "idx_email", &cols, false),
            "CREATE INDEX \"idx_email\" ON \"users\" (\"email\")"
        );
    }

    #[test]
    fn create_index_handler_reads_host_payload_shape() {
        let resp = get_create_index_sql(
            json!(1),
            &json!({ "table": "users", "index_name": "idx_email", "columns": ["email"], "is_unique": false, "schema": null }),
        );
        assert_eq!(
            resp["result"],
            json!(["CREATE INDEX \"idx_email\" ON \"users\" (\"email\")"])
        );
    }

    // -----------------------------------------------------------------------
    // Foreign keys
    // -----------------------------------------------------------------------

    #[test]
    fn create_foreign_key_sql_is_unsupported() {
        let resp = get_create_foreign_key_sql(
            json!(1),
            &json!({
                "table": "emails",
                "fk_name": "fk_emails_user_id_0",
                "column": "user_id",
                "ref_table": "users",
                "ref_column": "id",
                "schema": null
            }),
        );
        assert_eq!(resp["error"]["code"], -32601);
        assert!(resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("foreign key"));
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
}
