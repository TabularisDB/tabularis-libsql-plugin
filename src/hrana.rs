//! Minimal Hrana-over-HTTP client for remote libSQL servers (Turso, sqld).
//!
//! We use the stateless `/v2/pipeline` endpoint: each call POSTs a single
//! `execute` request and reads the typed result back. We deliberately do *not*
//! send a trailing `close` request — without a baton the server closes the
//! implicit stream on its own, and omitting `close` keeps us compatible with
//! servers (e.g. tursodb) whose pipeline parser only accepts `execute`/`batch`.
//! This keeps the plugin's stdio loop synchronous with no persistent socket.

use serde_json::{json, Value};

use crate::error::PluginError;
use crate::utils::values::{hrana_value_to_json, json_to_hrana_arg};

/// Result of one remote statement execution.
#[derive(Debug)]
pub struct HranaResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    pub affected: u64,
}

pub struct HranaClient {
    /// Base URL without a trailing slash, already normalised to http(s).
    base_url: String,
    auth_token: Option<String>,
}

impl HranaClient {
    pub fn new(base_url: String, auth_token: Option<String>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            auth_token,
        }
    }

    /// Run a single statement through the pipeline endpoint.
    pub fn execute(&self, sql: &str, args: &[Value]) -> Result<HranaResult, PluginError> {
        let hrana_args: Vec<Value> = args.iter().map(json_to_hrana_arg).collect();
        let body = json!({
            "requests": [
                {
                    "type": "execute",
                    "stmt": { "sql": sql, "args": hrana_args, "want_rows": true }
                }
            ]
        });

        let url = format!("{}/v2/pipeline", self.base_url);
        let mut req = ureq::post(&url);
        if let Some(token) = &self.auth_token {
            req = req.set("Authorization", &format!("Bearer {token}"));
        }

        let response = req.send_json(body).map_err(map_ureq_err)?;
        let value: Value = response.into_json().map_err(|e| {
            PluginError::internal(format!("invalid response body from server: {e}"))
        })?;

        parse_pipeline(&value)
    }
}

fn map_ureq_err(err: ureq::Error) -> PluginError {
    match err {
        ureq::Error::Status(code, resp) => {
            let detail = resp.into_string().unwrap_or_default();
            let detail = detail.trim();
            if detail.is_empty() {
                PluginError::internal(format!("server returned HTTP {code}"))
            } else {
                PluginError::internal(format!("server returned HTTP {code}: {detail}"))
            }
        }
        other => PluginError::internal(format!("could not reach server: {other}")),
    }
}

/// Extract the first statement result from a pipeline response, turning any
/// error envelope into a `PluginError`.
fn parse_pipeline(value: &Value) -> Result<HranaResult, PluginError> {
    let results = value
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| PluginError::internal("malformed pipeline response: missing 'results'"))?;

    let first = results
        .first()
        .ok_or_else(|| PluginError::internal("malformed pipeline response: empty 'results'"))?;

    if first.get("type").and_then(Value::as_str) == Some("error") {
        let msg = first
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("remote statement failed");
        return Err(PluginError::internal(msg.to_string()));
    }

    let result = first
        .get("response")
        .and_then(|r| r.get("result"))
        .ok_or_else(|| PluginError::internal("malformed pipeline response: missing 'result'"))?;

    let columns = result
        .get("cols")
        .and_then(Value::as_array)
        .map(|cols| {
            cols.iter()
                .map(|c| {
                    c.get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default();

    let mut rows = Vec::new();
    if let Some(raw_rows) = result.get("rows").and_then(Value::as_array) {
        for raw in raw_rows {
            if let Some(cells) = raw.as_array() {
                rows.push(cells.iter().map(hrana_value_to_json).collect());
            }
        }
    }

    let affected = result
        .get("affected_row_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    Ok(HranaResult {
        columns,
        rows,
        affected,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_successful_execute() {
        let resp = json!({
            "results": [
                {
                    "type": "ok",
                    "response": {
                        "type": "execute",
                        "result": {
                            "cols": [{"name": "id"}, {"name": "name"}],
                            "rows": [
                                [{"type":"integer","value":"1"}, {"type":"text","value":"Alice"}]
                            ],
                            "affected_row_count": 0
                        }
                    }
                },
                { "type": "ok", "response": { "type": "close" } }
            ]
        });
        let parsed = parse_pipeline(&resp).expect("should parse");
        assert_eq!(parsed.columns, vec!["id", "name"]);
        assert_eq!(parsed.rows, vec![vec![json!(1), json!("Alice")]]);
        assert_eq!(parsed.affected, 0);
    }

    #[test]
    fn surfaces_remote_errors() {
        let resp = json!({
            "results": [
                { "type": "error", "error": { "message": "no such table: ghosts" } }
            ]
        });
        let err = parse_pipeline(&resp).unwrap_err();
        assert!(err.message.contains("no such table"));
    }

    #[test]
    fn reports_affected_rows() {
        let resp = json!({
            "results": [
                {
                    "type": "ok",
                    "response": {
                        "type": "execute",
                        "result": { "cols": [], "rows": [], "affected_row_count": 3 }
                    }
                }
            ]
        });
        let parsed = parse_pipeline(&resp).expect("should parse");
        assert_eq!(parsed.affected, 3);
        assert!(parsed.columns.is_empty());
    }
}
