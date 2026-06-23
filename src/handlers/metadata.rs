//! Schema discovery and view management, implemented on top of SQLite's
//! `sqlite_master` table and `PRAGMA` introspection (libSQL is SQLite-compatible
//! on the wire, so the same queries work locally and remotely).

use serde_json::{json, Value};

use crate::client::Client;
use crate::error::PluginError;
use crate::handlers::{cell, cell_i64, cell_str, connect, req_str, respond};
use crate::utils::identifiers::quote;

// ---------------------------------------------------------------------------
// Reusable extractors
// ---------------------------------------------------------------------------

fn list_table_names(client: &Client) -> Result<Vec<String>, PluginError> {
    let r = client.query(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        &[],
    )?;
    Ok(r.rows.iter().filter_map(|row| cell_str(row, 0)).collect())
}

fn columns_for(client: &Client, table: &str) -> Result<Vec<Value>, PluginError> {
    let r = client.query(&format!("PRAGMA table_info({})", quote(table)), &[])?;
    let mut columns = Vec::with_capacity(r.rows.len());
    for row in &r.rows {
        // table_info columns: cid, name, type, notnull, dflt_value, pk
        let name = cell_str(row, 1).unwrap_or_default();
        let raw_type = cell_str(row, 2).unwrap_or_default();
        let not_null = cell_i64(row, 3) != 0;
        let default = cell(row, 4);
        let pk = cell_i64(row, 5) != 0;
        // INTEGER PRIMARY KEY is a rowid alias and behaves as auto-increment.
        let auto_increment = pk && raw_type.to_ascii_uppercase().contains("INT");
        let data_type = if raw_type.is_empty() {
            "TEXT".to_string()
        } else {
            raw_type
        };

        columns.push(json!({
            "name": name,
            "data_type": data_type,
            "is_nullable": !not_null,
            "column_default": default,
            "is_primary_key": pk,
            "is_auto_increment": auto_increment,
            "comment": Value::Null,
        }));
    }
    Ok(columns)
}

fn foreign_keys_for(client: &Client, table: &str) -> Result<Vec<Value>, PluginError> {
    let r = client.query(&format!("PRAGMA foreign_key_list({})", quote(table)), &[])?;
    let mut fks = Vec::with_capacity(r.rows.len());
    for row in &r.rows {
        // foreign_key_list columns: id, seq, table, from, to, on_update, on_delete, match
        let id = cell_i64(row, 0);
        let ref_table = cell_str(row, 2).unwrap_or_default();
        let from = cell_str(row, 3).unwrap_or_default();
        let to = cell_str(row, 4).unwrap_or_default();
        fks.push(json!({
            "constraint_name": format!("fk_{table}_{from}_{id}"),
            "column_name": from,
            "referenced_table": ref_table,
            "referenced_column": to,
            "on_update": cell(row, 5),
            "on_delete": cell(row, 6),
        }));
    }
    Ok(fks)
}

fn indexes_for(client: &Client, table: &str) -> Result<Vec<Value>, PluginError> {
    let list = client.query(&format!("PRAGMA index_list({})", quote(table)), &[])?;
    let mut indexes = Vec::with_capacity(list.rows.len());
    for row in &list.rows {
        // index_list columns: seq, name, unique, origin, partial
        let index_name = match cell_str(row, 1) {
            Some(n) => n,
            None => continue,
        };
        let is_unique = cell_i64(row, 2) == 1;
        let origin = cell_str(row, 3).unwrap_or_default();

        let info = client.query(&format!("PRAGMA index_info({})", quote(&index_name)), &[])?;
        let columns: Vec<Value> = info
            .rows
            .iter()
            // index_info columns: seqno, cid, name
            .filter_map(|r| cell_str(r, 2))
            .map(Value::String)
            .collect();

        indexes.push(json!({
            "index_name": index_name,
            "columns": columns,
            "is_unique": is_unique,
            "is_primary": origin == "pk",
        }));
    }
    Ok(indexes)
}

fn list_views(client: &Client) -> Result<Vec<Value>, PluginError> {
    let r = client.query(
        "SELECT name FROM sqlite_master WHERE type = 'view' ORDER BY name",
        &[],
    )?;
    Ok(r.rows
        .iter()
        .filter_map(|row| cell_str(row, 0))
        .map(|name| json!({ "name": name, "schema": Value::Null }))
        .collect())
}

// ---------------------------------------------------------------------------
// RPC handlers
// ---------------------------------------------------------------------------

pub fn get_databases(id: Value, _params: &Value) -> Value {
    // SQLite/libSQL exposes a single primary database namespace.
    respond(id, Ok(json!(["main"])))
}

pub fn get_schemas(id: Value, _params: &Value) -> Value {
    respond(id, Ok(json!([])))
}

pub fn get_tables(id: Value, params: &Value) -> Value {
    respond(id, get_tables_impl(params))
}

fn get_tables_impl(params: &Value) -> Result<Value, PluginError> {
    let client = connect(params)?;
    let tables: Vec<Value> = list_table_names(&client)?
        .into_iter()
        .map(|name| json!({ "name": name, "schema": Value::Null, "comment": Value::Null }))
        .collect();
    Ok(json!(tables))
}

pub fn get_columns(id: Value, params: &Value) -> Value {
    respond(id, {
        let client = connect(params).and_then(|c| {
            let table = req_str(params, "table")?;
            Ok((c, table))
        });
        client.and_then(|(c, table)| Ok(json!(columns_for(&c, &table)?)))
    })
}

pub fn get_foreign_keys(id: Value, params: &Value) -> Value {
    respond(id, {
        connect(params).and_then(|c| {
            let table = req_str(params, "table")?;
            Ok(json!(foreign_keys_for(&c, &table)?))
        })
    })
}

pub fn get_indexes(id: Value, params: &Value) -> Value {
    respond(id, {
        connect(params).and_then(|c| {
            let table = req_str(params, "table")?;
            Ok(json!(indexes_for(&c, &table)?))
        })
    })
}

pub fn get_views(id: Value, params: &Value) -> Value {
    respond(id, connect(params).and_then(|c| Ok(json!(list_views(&c)?))))
}

pub fn get_view_definition(id: Value, params: &Value) -> Value {
    respond(id, {
        connect(params).and_then(|c| {
            let view = req_str(params, "view")?;
            let r = c.query(
                "SELECT sql FROM sqlite_master WHERE type = 'view' AND name = ?1",
                &[json!(view)],
            )?;
            let def = r
                .rows
                .first()
                .and_then(|row| cell_str(row, 0))
                .unwrap_or_default();
            Ok(Value::String(def))
        })
    })
}

pub fn get_view_columns(id: Value, params: &Value) -> Value {
    respond(id, {
        connect(params).and_then(|c| {
            let view = req_str(params, "view")?;
            Ok(json!(columns_for(&c, &view)?))
        })
    })
}

pub fn get_routines(id: Value, _params: &Value) -> Value {
    respond(id, Ok(json!([])))
}

pub fn get_routine_parameters(id: Value, _params: &Value) -> Value {
    respond(id, Ok(json!([])))
}

pub fn get_routine_definition(id: Value, _params: &Value) -> Value {
    respond(id, Ok(Value::String(String::new())))
}

pub fn create_view(id: Value, params: &Value) -> Value {
    respond(id, {
        connect(params).and_then(|c| {
            let name = req_str(params, "name")?;
            let definition = req_str(params, "definition")?;
            c.execute(
                &format!("CREATE VIEW {} AS {}", quote(&name), definition),
                &[],
            )?;
            Ok(Value::Null)
        })
    })
}

pub fn alter_view(id: Value, params: &Value) -> Value {
    // SQLite has no ALTER VIEW; emulate with DROP + CREATE.
    respond(id, {
        connect(params).and_then(|c| {
            let name = req_str(params, "name")?;
            let definition = req_str(params, "definition")?;
            c.execute(&format!("DROP VIEW IF EXISTS {}", quote(&name)), &[])?;
            c.execute(
                &format!("CREATE VIEW {} AS {}", quote(&name), definition),
                &[],
            )?;
            Ok(Value::Null)
        })
    })
}

pub fn drop_view(id: Value, params: &Value) -> Value {
    respond(id, {
        connect(params).and_then(|c| {
            let name = req_str(params, "name")?;
            c.execute(&format!("DROP VIEW IF EXISTS {}", quote(&name)), &[])?;
            Ok(Value::Null)
        })
    })
}

pub fn get_schema_snapshot(id: Value, params: &Value) -> Value {
    respond(id, get_schema_snapshot_impl(params))
}

fn get_schema_snapshot_impl(params: &Value) -> Result<Value, PluginError> {
    let client = connect(params)?;
    let names = list_table_names(&client)?;

    let mut tables = Vec::with_capacity(names.len());
    let mut columns = serde_json::Map::new();
    let mut foreign_keys = serde_json::Map::new();

    for name in &names {
        tables.push(json!({ "name": name, "schema": Value::Null, "comment": Value::Null }));
        columns.insert(name.clone(), json!(columns_for(&client, name)?));
        foreign_keys.insert(name.clone(), json!(foreign_keys_for(&client, name)?));
    }

    Ok(json!({
        "tables": tables,
        "columns": Value::Object(columns),
        "foreign_keys": Value::Object(foreign_keys),
    }))
}

pub fn get_all_columns_batch(id: Value, params: &Value) -> Value {
    respond(id, batch_impl(params, columns_for))
}

pub fn get_all_foreign_keys_batch(id: Value, params: &Value) -> Value {
    respond(id, batch_impl(params, foreign_keys_for))
}

fn batch_impl(
    params: &Value,
    extract: fn(&Client, &str) -> Result<Vec<Value>, PluginError>,
) -> Result<Value, PluginError> {
    let client = connect(params)?;
    let requested: Vec<String> = params
        .get("tables")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let tables = if requested.is_empty() {
        list_table_names(&client)?
    } else {
        requested
    };

    let mut out = serde_json::Map::new();
    for table in tables {
        out.insert(table.clone(), json!(extract(&client, &table)?));
    }
    Ok(Value::Object(out))
}
