//! JSON-RPC method handlers, split by concern.

pub mod crud;
pub mod ddl;
pub mod metadata;
pub mod query;

use serde_json::Value;

use crate::client::Client;
use crate::error::PluginError;
use crate::models::{inner_params, ConnectionParams};
use crate::rpc::{error_response, ok_response};

/// Open a connection from the `params.params` block every method carries.
pub fn connect(params: &Value) -> Result<Client, PluginError> {
    let cp = ConnectionParams::from_value(inner_params(params));
    Client::connect(&cp)
}

/// Turn a handler result into a JSON-RPC response.
pub fn respond(id: Value, result: Result<Value, PluginError>) -> Value {
    match result {
        Ok(value) => ok_response(id, value),
        Err(err) => error_response(id, err.code, &err.message),
    }
}

/// Read a required string parameter from the top-level params object.
pub fn req_str(params: &Value, key: &str) -> Result<String, PluginError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| PluginError::invalid_params(format!("missing '{key}' parameter")))
}

/// A cell from a result row, defaulting to JSON null when out of range.
pub fn cell(row: &[Value], i: usize) -> Value {
    row.get(i).cloned().unwrap_or(Value::Null)
}

pub fn cell_str(row: &[Value], i: usize) -> Option<String> {
    row.get(i).and_then(Value::as_str).map(str::to_string)
}

pub fn cell_i64(row: &[Value], i: usize) -> i64 {
    row.get(i).and_then(Value::as_i64).unwrap_or(0)
}
