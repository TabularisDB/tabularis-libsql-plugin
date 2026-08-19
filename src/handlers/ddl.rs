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
//! embedded libSQL fork understands the same syntax, so it works there too.
//! Renames use plain `RENAME COLUMN`, which vanilla SQLite supports too.
//!
//! `get_create_foreign_key_sql` and, when the host passes connection params
//! through, `get_alter_column_sql` receive connection params, so the column's
//! declared type and constraints can be introspected. `get_create_foreign_key_sql`
//! emits the libSQL `ALTER COLUMN col TO col <type> REFERENCES ...` form,
//! which is the only way the fork supports adding a foreign key to an existing
//! table. Dropping a foreign key (`drop_foreign_key`) also receives connection
//! params and stays supported on every backend.

use serde_json::{json, Value};

use crate::client::Client;
use crate::error::PluginError;
use crate::handlers::{cell_i64, cell_str, connect, fk_name, req_str, respond};
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
            // The host normally calls this builder without connection params,
            // but when it passes them (as it does for
            // `get_create_foreign_key_sql`) the existing column definition is
            // introspected so constraints like REFERENCES survive the rewrite.
            let statements = if params.get("params").map(Value::is_object).unwrap_or(false) {
                let client = connect(params)?;
                build_alter_column_sql_with_schema(&client, &table, old_column, new_column)?
            } else {
                build_alter_column_sql(&table, old_column, new_column)?
            };
            Ok(json!(statements))
        })(),
    )
}

/// The parts of a column definition an `ALTER COLUMN` rewrite must reproduce
/// (the fork replaces the whole definition, not just the edited bit).
#[derive(PartialEq)]
struct ColumnRewrite {
    data_type: Option<String>,
    not_null: bool,
    default: Option<String>,
    constraints: Vec<String>,
}

fn column_rewrite_from(column: &Value) -> ColumnRewrite {
    ColumnRewrite {
        data_type: col_field(column, &["data_type", "type"])
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        not_null: !col_nullable(column),
        default: col_default(column).map(str::to_string),
        constraints: constraint_clauses(column),
    }
}

/// Column-level constraint clauses the host may pass through verbatim
/// (`REFERENCES ...`, `UNIQUE`, `CHECK (...)`). They are not part of the
/// ColumnDefinition contract, but when the host round-trips them the rewrite
/// must keep them.
fn constraint_clauses(column: &Value) -> Vec<String> {
    ["references", "unique", "check", "constraints"]
        .iter()
        .filter_map(|k| col_field(column, &[k]).and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Build the statements that alter a column. Renames use vanilla
/// `RENAME COLUMN` (works on every backend). Any other change emits the
/// libSQL `ALTER COLUMN ... TO ...` form, which replaces the column's whole
/// definition, so the merged definition must reproduce every attribute. A
/// rename combined with other changes emits both statements in order.
fn build_alter_column_sql(
    table: &str,
    old_column: &Value,
    new_column: &Value,
) -> Result<Vec<String>, PluginError> {
    let old_name = col_name(old_column)?;
    let new_name = col_name(new_column)?;
    let renamed = old_name != new_name;

    let old_def = column_rewrite_from(old_column);
    let mut new_def = column_rewrite_from(new_column);
    // The dialog only sends what it edits; anything absent is carried over
    // from the old definition so the rewrite does not silently drop it.
    if new_def.data_type.is_none() {
        new_def.data_type = old_def.data_type.clone();
    }
    if col_field(new_column, &["is_nullable", "nullable"]).is_none() {
        new_def.not_null = old_def.not_null;
    }
    if col_field(new_column, &["default_value", "column_default", "default"]).is_none() {
        new_def.default = old_def.default.clone();
    }
    for clause in &old_def.constraints {
        if !new_def.constraints.contains(clause) {
            new_def.constraints.push(clause.clone());
        }
    }

    let mut statements = Vec::new();
    if renamed {
        statements.push(format!(
            "ALTER TABLE {} RENAME COLUMN {} TO {}",
            quote(table),
            quote(&old_name),
            quote(&new_name),
        ));
    }
    if new_def != old_def {
        let data_type = new_def
            .data_type
            .as_deref()
            .ok_or_else(|| PluginError::invalid_params("column definition needs a 'data_type'"))?;
        statements.push(build_column_rewrite_sql(
            table, &new_name, data_type, &new_def,
        ));
    }
    Ok(statements)
}

/// Like `build_alter_column_sql`, but with the existing column definition
/// introspected from the schema, so attributes the dialog cannot express
/// (notably REFERENCES) survive the rewrite.
fn build_alter_column_sql_with_schema(
    client: &Client,
    table: &str,
    old_column: &Value,
    new_column: &Value,
) -> Result<Vec<String>, PluginError> {
    let old_name = col_name(old_column)?;
    let new_name = col_name(new_column)?;
    let renamed = old_name != new_name;

    let existing = column_def_for(client, table, &old_name)?;
    let fk_clause = single_column_fk_clause_for(client, table, &old_name)?;

    let old_def = column_rewrite_from(old_column);
    let mut def = ColumnRewrite {
        data_type: Some(existing.data_type),
        not_null: existing.not_null,
        default: existing.default,
        constraints: old_def.constraints,
    };
    // Dialog fields override the schema where the user actually edited.
    if let Some(raw) = col_field(new_column, &["data_type", "type"])
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        def.data_type = Some(raw.to_string());
    }
    if col_field(new_column, &["is_nullable", "nullable"]).is_some() {
        def.not_null = !col_nullable(new_column);
    }
    if col_field(new_column, &["default_value", "column_default", "default"]).is_some() {
        def.default = col_default(new_column).map(str::to_string);
    }
    if let Some(clause) = fk_clause {
        def.constraints.push(clause);
    }

    let mut statements = Vec::new();
    if renamed {
        statements.push(format!(
            "ALTER TABLE {} RENAME COLUMN {} TO {}",
            quote(table),
            quote(&old_name),
            quote(&new_name),
        ));
    }
    let data_type = def
        .data_type
        .as_deref()
        .ok_or_else(|| PluginError::invalid_params("column definition needs a 'data_type'"))?;
    statements.push(build_column_rewrite_sql(table, &new_name, data_type, &def));
    Ok(statements)
}

fn build_column_rewrite_sql(
    table: &str,
    name: &str,
    data_type: &str,
    def: &ColumnRewrite,
) -> String {
    let mut sql = format!(
        "ALTER TABLE {} ALTER COLUMN {} TO {} {}",
        quote(table),
        quote(name),
        quote(name),
        data_type
    );
    if def.not_null {
        sql.push_str(" NOT NULL");
    }
    if let Some(default) = &def.default {
        sql.push_str(&format!(" DEFAULT {default}"));
    }
    for clause in &def.constraints {
        sql.push_str(&format!(" {clause}"));
    }
    sql
}

/// The REFERENCES clause of a single-column foreign key on `column`, if any.
/// Composite (table-level) constraints span several `foreign_key_list` rows
/// and cannot be expressed as a per-column rewrite, so they are skipped.
fn single_column_fk_clause_for(
    client: &Client,
    table: &str,
    column: &str,
) -> Result<Option<String>, PluginError> {
    let r = client.query(&format!("PRAGMA foreign_key_list({})", quote(table)), &[])?;
    let matches: Vec<&Vec<Value>> = r
        .rows
        .iter()
        .filter(|row| cell_str(row, 3).as_deref() == Some(column))
        .collect();
    if matches.len() != 1 {
        return Ok(None);
    }
    let row = matches[0];
    let mut clause = format!(
        "REFERENCES {} ({})",
        quote(&cell_str(row, 2).unwrap_or_default()),
        quote(&cell_str(row, 4).unwrap_or_default()),
    );
    // foreign_key_list columns: id, seq, table, from, to, on_update, on_delete, match
    if let Some(action) = cell_str(row, 6).filter(|s| s != "NO ACTION") {
        clause.push_str(&format!(" ON DELETE {action}"));
    }
    if let Some(action) = cell_str(row, 5).filter(|s| s != "NO ACTION") {
        clause.push_str(&format!(" ON UPDATE {action}"));
    }
    Ok(Some(clause))
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

/// `ALTER TABLE ... ALTER COLUMN ... REFERENCES ...` adds a foreign key on
/// the libSQL fork. Unlike the other SQL builders, the host sends connection
/// params for this method (the RpcDriver passes them through), so the
/// column's declared type can be introspected. Both remote servers and local
/// files (embedded fork) understand the syntax.
pub fn get_create_foreign_key_sql(id: Value, params: &Value) -> Value {
    respond(id, {
        (|| {
            let client = connect(params)?;
            let table = req_str(params, "table")?;
            let column = req_str(params, "column")?;
            let ref_table = req_str(params, "ref_table")?;
            let ref_column = req_str(params, "ref_column")?;
            let on_delete = params
                .get("on_delete")
                .and_then(Value::as_str)
                .map(str::to_string);
            let on_update = params
                .get("on_update")
                .and_then(Value::as_str)
                .map(str::to_string);
            let col_def = column_def_for(&client, &table, &column)?;
            Ok(json!([build_create_fk_sql(
                &table,
                &column,
                &col_def,
                &ref_table,
                &ref_column,
                on_delete.as_deref(),
                on_update.as_deref(),
            )]))
        })()
    })
}

/// Build a libSQL statement that adds a foreign key to an existing column:
/// the ALTER COLUMN form with the column's full definition plus a REFERENCES
/// clause. The constraint name is not used — SQLite foreign keys carry no
/// names of their own; the host's generated `fk_<table>_<ref>_<col>` name is
/// cosmetic and the plugin re-derives names from `PRAGMA foreign_key_list`.
fn build_create_fk_sql(
    table: &str,
    column: &str,
    def: &ColumnDef,
    ref_table: &str,
    ref_column: &str,
    on_delete: Option<&str>,
    on_update: Option<&str>,
) -> String {
    let mut sql = format!(
        "ALTER TABLE {} ALTER COLUMN {} TO {} {} REFERENCES {} ({})",
        quote(table),
        quote(column),
        quote(column),
        column_def_sql(def),
        quote(ref_table),
        quote(ref_column),
    );
    if let Some(action) = on_delete {
        sql.push_str(&format!(" ON DELETE {}", action));
    }
    if let Some(action) = on_update {
        sql.push_str(&format!(" ON UPDATE {}", action));
    }
    sql
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
/// ALTER COLUMN form without the REFERENCES clause. This mutation receives
/// connection params, so the column's type can be introspected. Works on
/// remote servers and local files alike.
pub fn drop_foreign_key(id: Value, params: &Value) -> Value {
    respond(
        id,
        (|| {
            let client = connect(params)?;
            let table = req_str(params, "table")?;
            let fk_name = req_str(params, "fk_name")?;
            if is_composite_fk(&client, &table, &fk_name)? {
                return Err(PluginError::invalid_params(format!(
                    "foreign key '{fk_name}' is a composite (multi-column) constraint; \
                     the libSQL fork can only drop per-column foreign keys — recreate the table instead"
                )));
            }
            let column = foreign_key_column_for(&client, &table, &fk_name)?;
            let col_def = column_def_for(&client, &table, &column)?;
            client.execute(&build_fk_drop_sql(&table, &column, &col_def), &[])?;
            Ok(Value::Null)
        })(),
    )
}

/// Build a libSQL statement that drops an existing column's foreign key:
/// the ALTER COLUMN form without the REFERENCES clause. The rest of the
/// column definition is reproduced so the rewrite does not strip NOT NULL,
/// DEFAULT or anything else the column carries.
fn build_fk_drop_sql(table: &str, column: &str, def: &ColumnDef) -> String {
    format!(
        "ALTER TABLE {} ALTER COLUMN {} TO {} {}",
        quote(table),
        quote(column),
        quote(column),
        column_def_sql(def)
    )
}

/// Composite (table-level) foreign keys appear as several
/// `PRAGMA foreign_key_list` rows sharing one id — one per column. The fork's
/// ALTER COLUMN rewrite is per-column and cannot remove them.
fn is_composite_fk(client: &Client, table: &str, name: &str) -> Result<bool, PluginError> {
    let r = client.query(&format!("PRAGMA foreign_key_list({})", quote(table)), &[])?;
    // Each row of a composite FK carries the same id but a different column
    // name, so the host-side name only matches one row; count the rows that
    // share the id.
    let mut target: Option<i64> = None;
    for row in &r.rows {
        let id = cell_i64(row, 0);
        let from = cell_str(row, 3).unwrap_or_default();
        if fk_name(table, &from, id) == name {
            target = Some(id);
            break;
        }
    }
    let Some(target) = target else {
        // Unknown names are reported by the caller's lookup instead.
        return Ok(false);
    };
    let count = r
        .rows
        .iter()
        .filter(|row| cell_i64(row, 0) == target)
        .count();
    Ok(count > 1)
}

/// A column's existing definition, read from `PRAGMA table_info` (cells:
/// cid, name, type, notnull, dflt_value, pk).
struct ColumnDef {
    data_type: String,
    not_null: bool,
    default: Option<String>,
}

/// Look up a column's declared type and constraints via `PRAGMA table_info`,
/// defaulting to TEXT when the type is blank (SQLite's bare-column shorthand).
fn column_def_for(client: &Client, table: &str, column: &str) -> Result<ColumnDef, PluginError> {
    let r = client.query(&format!("PRAGMA table_info({})", quote(table)), &[])?;
    for row in &r.rows {
        // table_info columns: cid, name, type, notnull, dflt_value, pk
        if cell_str(row, 1).as_deref() == Some(column) {
            let raw = cell_str(row, 2).unwrap_or_default();
            return Ok(ColumnDef {
                data_type: if raw.is_empty() {
                    "TEXT".to_string()
                } else {
                    raw
                },
                not_null: cell_i64(row, 3) != 0,
                default: cell_raw(row, 4).filter(|s| !s.is_empty()),
            });
        }
    }
    Err(PluginError::invalid_params(format!(
        "column '{column}' not found in table '{table}'"
    )))
}

/// Render a column definition the way the libSQL ALTER COLUMN form expects:
/// `TYPE [NOT NULL] [DEFAULT value]`.
fn column_def_sql(def: &ColumnDef) -> String {
    let mut sql = def.data_type.clone();
    if def.not_null {
        sql.push_str(" NOT NULL");
    }
    if let Some(default) = &def.default {
        sql.push_str(&format!(" DEFAULT {default}"));
    }
    sql
}

/// A PRAGMA cell rendered back to its SQL source text. The libSQL result
/// converts numbers to JSON numbers, so a `DEFAULT 7` would otherwise come
/// back as a number and be dropped by the string-only readers.
fn cell_raw(row: &[Value], i: usize) -> Option<String> {
    row.get(i).map(|v| match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    })
}

/// Map a host-side FK name (plugin-generated `fk_<table>_<column>_<id>`) back
/// to the SQLite column it constrains. SQLite FKs carry no names of their own,
/// so the mapping re-derives the same naming scheme from
/// `PRAGMA foreign_key_list`.
fn foreign_key_column_for(client: &Client, table: &str, name: &str) -> Result<String, PluginError> {
    let r = client.query(&format!("PRAGMA foreign_key_list({})", quote(table)), &[])?;
    for row in &r.rows {
        // foreign_key_list columns: id, seq, table, from, to, on_update, on_delete, match
        let id = cell_i64(row, 0);
        let from = cell_str(row, 3).unwrap_or_default();
        if fk_name(table, &from, id) == name {
            return Ok(from);
        }
    }
    Err(PluginError::invalid_params(format!(
        "foreign key '{name}' not found on table '{table}'"
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
            vec!["ALTER TABLE \"t\" RENAME COLUMN \"a\" TO \"b\""]
        );
    }

    #[test]
    fn alter_column_type_change() {
        let old_col = json!({ "name": "v", "data_type": "TEXT" });
        let new_col = json!({ "name": "v", "data_type": "INTEGER" });
        assert_eq!(
            build_alter_column_sql("t", &old_col, &new_col).unwrap(),
            vec!["ALTER TABLE \"t\" ALTER COLUMN \"v\" TO \"v\" INTEGER"]
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
            vec!["ALTER TABLE \"t\" ALTER COLUMN \"v\" TO \"v\" TEXT NOT NULL DEFAULT 'hai'"]
        );
    }

    #[test]
    fn alter_column_rename_with_retype_emits_both_statements() {
        let old_col = json!({ "name": "a", "data_type": "TEXT", "is_nullable": true });
        let new_col = json!({ "name": "b", "data_type": "INTEGER", "is_nullable": false });
        assert_eq!(
            build_alter_column_sql("t", &old_col, &new_col).unwrap(),
            vec![
                "ALTER TABLE \"t\" RENAME COLUMN \"a\" TO \"b\"",
                "ALTER TABLE \"t\" ALTER COLUMN \"b\" TO \"b\" INTEGER NOT NULL"
            ]
        );
    }

    #[test]
    fn alter_column_keeps_old_default_and_not_null_when_omitted() {
        let old_col = json!({
            "name": "v", "data_type": "TEXT",
            "is_nullable": false, "default_value": "'x'"
        });
        let new_col = json!({ "name": "v", "data_type": "INTEGER" });
        assert_eq!(
            build_alter_column_sql("t", &old_col, &new_col).unwrap(),
            vec!["ALTER TABLE \"t\" ALTER COLUMN \"v\" TO \"v\" INTEGER NOT NULL DEFAULT 'x'"]
        );
    }

    #[test]
    fn alter_column_inherits_type_when_dialog_omits_it() {
        let old_col = json!({ "name": "v", "data_type": "TEXT", "is_nullable": true });
        let new_col = json!({ "name": "v", "is_nullable": false });
        assert_eq!(
            build_alter_column_sql("t", &old_col, &new_col).unwrap(),
            vec!["ALTER TABLE \"t\" ALTER COLUMN \"v\" TO \"v\" TEXT NOT NULL"]
        );
    }

    #[test]
    fn alter_column_no_changes_emits_nothing() {
        let old_col = json!({ "name": "v", "data_type": "TEXT", "is_nullable": true });
        let new_col = json!({ "name": "v", "data_type": "TEXT", "is_nullable": true });
        assert!(build_alter_column_sql("t", &old_col, &new_col)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn alter_column_quotes_identifiers() {
        let old_col = json!({ "name": "a\"b", "data_type": "TEXT" });
        let new_col = json!({ "name": "c", "data_type": "TEXT" });
        assert_eq!(
            build_alter_column_sql("weird\"t", &old_col, &new_col).unwrap(),
            vec!["ALTER TABLE \"weird\"\"t\" RENAME COLUMN \"a\"\"b\" TO \"c\""]
        );
    }

    #[test]
    fn alter_column_with_schema_preserves_fk_and_attributes() {
        let client = in_memory_client();
        client
            .execute("CREATE TABLE users(id INT PRIMARY KEY)", &[])
            .expect("create users");
        client
            .execute(
                "CREATE TABLE emails(user_id INT NOT NULL DEFAULT 7 REFERENCES users(id))",
                &[],
            )
            .expect("create emails");
        let old_col = json!({ "name": "user_id", "data_type": "INT" });
        let new_col = json!({ "name": "user_id", "data_type": "BIGINT" });
        assert_eq!(
            build_alter_column_sql_with_schema(&client, "emails", &old_col, &new_col).unwrap(),
            vec!["ALTER TABLE \"emails\" ALTER COLUMN \"user_id\" TO \"user_id\" BIGINT NOT NULL DEFAULT 7 REFERENCES \"users\" (\"id\")"]
        );
    }

    #[test]
    fn alter_column_with_schema_rename_and_retype() {
        let client = in_memory_client();
        client
            .execute("CREATE TABLE users(id INT PRIMARY KEY)", &[])
            .expect("create users");
        client
            .execute(
                "CREATE TABLE emails(user_id INT NOT NULL DEFAULT 7 REFERENCES users(id))",
                &[],
            )
            .expect("create emails");
        let old_col = json!({ "name": "user_id", "data_type": "INT" });
        let new_col = json!({ "name": "owner_id", "data_type": "BIGINT" });
        assert_eq!(
            build_alter_column_sql_with_schema(&client, "emails", &old_col, &new_col).unwrap(),
            vec![
                "ALTER TABLE \"emails\" RENAME COLUMN \"user_id\" TO \"owner_id\"",
                "ALTER TABLE \"emails\" ALTER COLUMN \"owner_id\" TO \"owner_id\" BIGINT NOT NULL DEFAULT 7 REFERENCES \"users\" (\"id\")"
            ]
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
    fn create_foreign_key_builder_emits_libsql_alter_column() {
        let def = ColumnDef {
            data_type: "INT".into(),
            not_null: false,
            default: None,
        };
        assert_eq!(
            build_create_fk_sql("emails", "user_id", &def, "users", "id", None, None),
            "ALTER TABLE \"emails\" ALTER COLUMN \"user_id\" TO \"user_id\" INT REFERENCES \"users\" (\"id\")"
        );
    }

    #[test]
    fn create_foreign_key_builder_appends_referential_actions() {
        let def = ColumnDef {
            data_type: "INT".into(),
            not_null: false,
            default: None,
        };
        assert_eq!(
            build_create_fk_sql(
                "emails",
                "user_id",
                &def,
                "users",
                "id",
                Some("CASCADE"),
                Some("SET NULL"),
            ),
            "ALTER TABLE \"emails\" ALTER COLUMN \"user_id\" TO \"user_id\" INT REFERENCES \"users\" (\"id\") ON DELETE CASCADE ON UPDATE SET NULL"
        );
    }

    #[test]
    fn create_foreign_key_builder_keeps_not_null_and_default() {
        let def = ColumnDef {
            data_type: "INT".into(),
            not_null: true,
            default: Some("7".into()),
        };
        assert_eq!(
            build_create_fk_sql("emails", "user_id", &def, "users", "id", None, None),
            "ALTER TABLE \"emails\" ALTER COLUMN \"user_id\" TO \"user_id\" INT NOT NULL DEFAULT 7 REFERENCES \"users\" (\"id\")"
        );
    }

    #[test]
    fn drop_foreign_key_builder_keeps_not_null_and_default() {
        let def = ColumnDef {
            data_type: "INT".into(),
            not_null: true,
            default: Some("7".into()),
        };
        assert_eq!(
            build_fk_drop_sql("emails", "user_id", &def),
            "ALTER TABLE \"emails\" ALTER COLUMN \"user_id\" TO \"user_id\" INT NOT NULL DEFAULT 7"
        );
    }

    /// A temp-file database shared across connections (in-memory DBs are
    /// per-connection, and the handlers open their own connection).
    fn temp_file_client() -> (Client, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "libsql_plugin_test_{}_{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let cp = ConnectionParams {
            database: Some(path.to_string_lossy().to_string()),
            ..Default::default()
        };
        let client = Client::connect(&cp).expect("temp-file connection");
        (client, path)
    }

    #[test]
    fn create_foreign_key_local_uses_the_fork() {
        let (client, path) = temp_file_client();
        client
            .execute("CREATE TABLE users(id INT PRIMARY KEY)", &[])
            .expect("create users");
        client
            .execute("CREATE TABLE emails(user_id INT)", &[])
            .expect("create emails");
        let resp = get_create_foreign_key_sql(
            json!(1),
            &json!({
                "params": { "database": path },
                "table": "emails",
                "fk_name": "fk_emails_user_id_0",
                "column": "user_id",
                "ref_table": "users",
                "ref_column": "id",
                "schema": null
            }),
        );
        assert_eq!(
            resp["result"],
            json!(["ALTER TABLE \"emails\" ALTER COLUMN \"user_id\" TO \"user_id\" INT REFERENCES \"users\" (\"id\")"])
        );
    }

    #[test]
    fn drop_foreign_key_local_rewrites_the_schema() {
        let (client, path) = temp_file_client();
        client
            .execute("CREATE TABLE users(id INT PRIMARY KEY)", &[])
            .expect("create users");
        client
            .execute("CREATE TABLE emails(user_id INT REFERENCES users(id))", &[])
            .expect("create emails");
        let resp = drop_foreign_key(
            json!(1),
            &json!({
                "params": { "database": path },
                "table": "emails",
                "fk_name": "fk_emails_user_id_0"
            }),
        );
        assert!(resp.get("error").is_none(), "unexpected error: {resp}");
        let after = client
            .query("PRAGMA foreign_key_list(emails)", &[])
            .expect("pragma");
        assert!(after.rows.is_empty(), "FK should be gone");
    }

    #[test]
    fn local_alter_column_retypes_through_the_fork() {
        // The whole point of the embedded fork: local files speak ALTER COLUMN.
        let client = in_memory_client();
        client
            .execute("CREATE TABLE t(v TEXT)", &[])
            .expect("create table");
        client
            .execute("ALTER TABLE t ALTER COLUMN v TO v INTEGER", &[])
            .expect("alter column should work on local files");
        let r = client.query("PRAGMA table_info(t)", &[]).expect("pragma");
        assert_eq!(cell_str(&r.rows[0], 2), Some("INTEGER".to_string()));
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
    fn column_def_for_reads_pragma() {
        let client = in_memory_client();
        client
            .execute(
                "CREATE TABLE t(v TEXT NOT NULL DEFAULT 'x', n INTEGER)",
                &[],
            )
            .expect("create table");
        let v = column_def_for(&client, "t", "v").unwrap();
        assert_eq!(v.data_type, "TEXT");
        assert!(v.not_null);
        assert_eq!(v.default.as_deref(), Some("'x'"));
        let n = column_def_for(&client, "t", "n").unwrap();
        assert_eq!(n.data_type, "INTEGER");
        assert!(!n.not_null);
        assert_eq!(n.default, None);
    }

    #[test]
    fn column_def_for_keeps_numeric_default() {
        let client = in_memory_client();
        client
            .execute("CREATE TABLE t(v INT NOT NULL DEFAULT 7)", &[])
            .expect("create table");
        let v = column_def_for(&client, "t", "v").unwrap();
        assert!(v.not_null);
        assert_eq!(v.default.as_deref(), Some("7"));
    }

    #[test]
    fn column_def_for_defaults_blank_type_to_text() {
        let client = in_memory_client();
        client
            .execute("CREATE TABLE t(v)", &[])
            .expect("create table");
        assert_eq!(column_def_for(&client, "t", "v").unwrap().data_type, "TEXT");
    }

    #[test]
    fn column_def_for_missing_column_errors() {
        let client = in_memory_client();
        client
            .execute("CREATE TABLE t(v TEXT)", &[])
            .expect("create table");
        assert!(column_def_for(&client, "t", "nope").is_err());
    }

    #[test]
    fn drop_foreign_key_local_keeps_not_null_and_default() {
        let (client, path) = temp_file_client();
        client
            .execute("CREATE TABLE users(id INT PRIMARY KEY)", &[])
            .expect("create users");
        client
            .execute(
                "CREATE TABLE emails(user_id INT NOT NULL DEFAULT 7 REFERENCES users(id))",
                &[],
            )
            .expect("create emails");
        let resp = drop_foreign_key(
            json!(1),
            &json!({
                "params": { "database": path },
                "table": "emails",
                "fk_name": "fk_emails_user_id_0"
            }),
        );
        assert!(resp.get("error").is_none(), "unexpected error: {resp}");
        let after = client
            .query("PRAGMA foreign_key_list(emails)", &[])
            .expect("pragma");
        assert!(after.rows.is_empty(), "FK should be gone");
        let info = client
            .query("PRAGMA table_info(emails)", &[])
            .expect("pragma");
        let user_id = &info.rows[0];
        assert_eq!(cell_str(user_id, 1).as_deref(), Some("user_id"));
        assert_eq!(cell_str(user_id, 2).as_deref(), Some("INT"));
        assert_eq!(cell_i64(user_id, 3), 1, "NOT NULL must survive");
        assert_eq!(
            cell_str(user_id, 4).as_deref(),
            Some("7"),
            "DEFAULT must survive"
        );
    }

    #[test]
    fn drop_composite_foreign_key_errors_instead_of_silently_succeeding() {
        let (client, path) = temp_file_client();
        client
            .execute("CREATE TABLE p(x INT, y INT, PRIMARY KEY(x, y))", &[])
            .expect("create p");
        client
            .execute(
                "CREATE TABLE c(a INT, b INT, FOREIGN KEY(a, b) REFERENCES p(x, y))",
                &[],
            )
            .expect("create c");
        let resp = drop_foreign_key(
            json!(1),
            &json!({
                "params": { "database": path },
                "table": "c",
                "fk_name": "fk_c_a_0"
            }),
        );
        let err = resp["error"]["message"].as_str().expect("error message");
        assert!(err.contains("composite"), "unexpected error: {err}");
        let after = client
            .query("PRAGMA foreign_key_list(c)", &[])
            .expect("pragma");
        assert_eq!(after.rows.len(), 2, "FK must remain untouched");
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
