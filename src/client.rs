//! Connection layer: route a Tabularis connection to the right backend and
//! expose a single `query`/`execute` surface the handlers can use without
//! caring whether the database is a local file or a remote Turso server.

use rusqlite::types::Value as SqliteValue;
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::error::PluginError;
use crate::hrana::HranaClient;
use crate::models::ConnectionParams;

/// Where a connection actually points.
#[derive(Debug, PartialEq, Eq)]
pub enum Backend {
    /// Local libSQL/SQLite file (or `:memory:`).
    Local(String),
    /// Remote Turso / sqld server reachable over Hrana HTTP.
    Remote { url: String, token: Option<String> },
}

/// A uniform result for both backends.
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    pub affected: u64,
}

/// An open connection to one of the two backends. A fresh one is built per RPC
/// call: opening a local SQLite file is cheap, and the remote backend is
/// stateless (each call is an independent HTTP request), so there is no shared
/// mutable state to manage.
pub enum Client {
    Local(Connection),
    Remote(HranaClient),
}

impl Client {
    pub fn connect(params: &ConnectionParams) -> Result<Self, PluginError> {
        match resolve_backend(params)? {
            Backend::Local(path) => {
                let conn = Connection::open(&path).map_err(|e| {
                    PluginError::internal(format!("cannot open libSQL file '{path}': {e}"))
                })?;
                Ok(Client::Local(conn))
            }
            Backend::Remote { url, token } => Ok(Client::Remote(HranaClient::new(url, token))),
        }
    }

    /// Run a row-returning statement (SELECT/PRAGMA/...).
    pub fn query(&self, sql: &str, args: &[Value]) -> Result<QueryResult, PluginError> {
        match self {
            Client::Local(conn) => local_query(conn, sql, args),
            Client::Remote(client) => {
                let r = client.execute(sql, args)?;
                Ok(QueryResult {
                    columns: r.columns,
                    rows: r.rows,
                    affected: r.affected,
                })
            }
        }
    }

    /// Run a non-row statement and return the affected-row count.
    pub fn execute(&self, sql: &str, args: &[Value]) -> Result<u64, PluginError> {
        match self {
            Client::Local(conn) => {
                let sqlite_args = to_sqlite_params(args);
                let mut stmt = conn.prepare(sql)?;
                let affected = stmt.execute(rusqlite::params_from_iter(sqlite_args.iter()))?;
                Ok(affected as u64)
            }
            Client::Remote(client) => Ok(client.execute(sql, args)?.affected),
        }
    }

    /// Cheap connectivity check used by `test_connection` and `ping`.
    pub fn health_check(&self) -> Result<(), PluginError> {
        self.query("SELECT 1", &[]).map(|_| ())
    }
}

fn local_query(conn: &Connection, sql: &str, args: &[Value]) -> Result<QueryResult, PluginError> {
    let sqlite_args = to_sqlite_params(args);
    let mut stmt = conn.prepare(sql)?;
    let columns: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let ncol = columns.len();

    let mut out_rows = Vec::new();
    let mut rows = stmt.query(rusqlite::params_from_iter(sqlite_args.iter()))?;
    while let Some(row) = rows.next()? {
        let mut cells = Vec::with_capacity(ncol);
        for i in 0..ncol {
            let value: SqliteValue = row.get(i)?;
            cells.push(sqlite_to_json(value));
        }
        out_rows.push(cells);
    }

    Ok(QueryResult {
        columns,
        rows: out_rows,
        affected: 0,
    })
}

fn to_sqlite_params(args: &[Value]) -> Vec<SqliteValue> {
    args.iter().map(json_to_sqlite).collect()
}

fn json_to_sqlite(value: &Value) -> SqliteValue {
    match value {
        Value::Null => SqliteValue::Null,
        Value::Bool(b) => SqliteValue::Integer(if *b { 1 } else { 0 }),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                SqliteValue::Integer(i)
            } else if let Some(f) = n.as_f64() {
                SqliteValue::Real(f)
            } else {
                SqliteValue::Text(n.to_string())
            }
        }
        Value::String(s) => SqliteValue::Text(s.clone()),
        other => SqliteValue::Text(other.to_string()),
    }
}

fn sqlite_to_json(value: SqliteValue) -> Value {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    match value {
        SqliteValue::Null => Value::Null,
        SqliteValue::Integer(i) => json!(i),
        SqliteValue::Real(f) => json!(f),
        SqliteValue::Text(s) => Value::String(s),
        SqliteValue::Blob(b) => Value::String(STANDARD.encode(b)),
    }
}

// ---------------------------------------------------------------------------
// Backend resolution (pure logic, unit-tested)
// ---------------------------------------------------------------------------

const URL_SCHEMES: [&str; 6] = [
    "libsql://",
    "https://",
    "http://",
    "wss://",
    "ws://",
    "turso://",
];

fn is_url(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    URL_SCHEMES.iter().any(|scheme| lower.starts_with(scheme))
}

/// Decide which backend a set of connection params points at.
pub fn resolve_backend(params: &ConnectionParams) -> Result<Backend, PluginError> {
    let database = params.database.clone().unwrap_or_default();
    let database = database.trim();

    // 1. A URL in the database field is always remote.
    if is_url(database) {
        let (url, token_from_url) = normalize_remote_url(database);
        let token = token_from_url.or_else(|| params.password.clone());
        return Ok(Backend::Remote { url, token });
    }

    // 2. A host means remote too. The host may itself be a full URL.
    if let Some(host) = params.host.as_deref() {
        let host = host.trim();
        if !host.is_empty() {
            if is_url(host) {
                let (url, token_from_url) = normalize_remote_url(host);
                let token = token_from_url.or_else(|| params.password.clone());
                return Ok(Backend::Remote { url, token });
            }
            let url = build_url_from_host(host, params.port, params.ssl_mode.as_deref());
            return Ok(Backend::Remote {
                url,
                token: params.password.clone(),
            });
        }
    }

    // 3. Otherwise treat the database field as a local file path.
    if !database.is_empty() {
        return Ok(Backend::Local(expand_path(database)));
    }

    Err(PluginError::invalid_params(
        "no connection target: provide a local file path, or a libsql:// / https:// URL (with an auth token for Turso)",
    ))
}

/// Normalise a remote URL to http(s) and pull out the auth token query param.
/// Returns the clean base URL (no query string, no trailing slash) plus any
/// token found in the URL.
pub fn normalize_remote_url(raw: &str) -> (String, Option<String>) {
    let (before_query, query) = match raw.split_once('?') {
        Some((a, b)) => (a, Some(b)),
        None => (raw, None),
    };

    // Swap the scheme for the HTTP equivalent Hrana expects.
    let lower = before_query.to_ascii_lowercase();
    let normalised = if let Some(rest) = strip_scheme(&lower, before_query, "libsql://") {
        format!("https://{rest}")
    } else if let Some(rest) = strip_scheme(&lower, before_query, "turso://") {
        format!("https://{rest}")
    } else if let Some(rest) = strip_scheme(&lower, before_query, "wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = strip_scheme(&lower, before_query, "ws://") {
        format!("http://{rest}")
    } else {
        before_query.to_string()
    };

    let token = query.and_then(token_from_query);
    (normalised.trim_end_matches('/').to_string(), token)
}

/// If `lower` starts with `scheme`, return the remainder of the *original*
/// (case-preserving) string after the scheme.
fn strip_scheme(lower: &str, original: &str, scheme: &str) -> Option<String> {
    if lower.starts_with(scheme) {
        Some(original[scheme.len()..].to_string())
    } else {
        None
    }
}

fn token_from_query(query: &str) -> Option<String> {
    for pair in query.split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            let key = key.to_ascii_lowercase();
            let is_token_key = matches!(key.as_str(), "authtoken" | "auth_token" | "jwt" | "token");
            if is_token_key && !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn build_url_from_host(host: &str, port: Option<u16>, ssl_mode: Option<&str>) -> String {
    let is_local = host == "localhost" || host == "127.0.0.1" || host == "[::1]";
    let scheme = if ssl_mode == Some("disable") || (is_local && ssl_mode != Some("require")) {
        "http"
    } else {
        "https"
    };
    match port {
        Some(p) => format!("{scheme}://{host}:{p}"),
        None => format!("{scheme}://{host}"),
    }
}

fn expand_path(path: &str) -> String {
    let path = path.strip_prefix("file:").unwrap_or(path);
    if path == ":memory:" {
        return path.to_string();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(
        database: Option<&str>,
        host: Option<&str>,
        password: Option<&str>,
    ) -> ConnectionParams {
        ConnectionParams {
            database: database.map(String::from),
            host: host.map(String::from),
            password: password.map(String::from),
            ..Default::default()
        }
    }

    #[test]
    fn local_path_in_database_field() {
        let p = params(Some("/data/app.db"), None, None);
        assert_eq!(
            resolve_backend(&p).unwrap(),
            Backend::Local("/data/app.db".into())
        );
    }

    #[test]
    fn libsql_url_becomes_remote_https() {
        let p = params(Some("libsql://my-db.turso.io"), None, Some("tok"));
        assert_eq!(
            resolve_backend(&p).unwrap(),
            Backend::Remote {
                url: "https://my-db.turso.io".into(),
                token: Some("tok".into())
            }
        );
    }

    #[test]
    fn auth_token_from_url_query_wins_and_is_stripped() {
        let (url, token) = normalize_remote_url("libsql://db.turso.io?authToken=abc123");
        assert_eq!(url, "https://db.turso.io");
        assert_eq!(token.as_deref(), Some("abc123"));
    }

    #[test]
    fn websocket_schemes_map_to_http() {
        assert_eq!(
            normalize_remote_url("wss://x.turso.io").0,
            "https://x.turso.io"
        );
        assert_eq!(
            normalize_remote_url("ws://localhost:8080").0,
            "http://localhost:8080"
        );
    }

    #[test]
    fn host_field_builds_remote_url() {
        let p = params(None, Some("db.turso.io"), Some("tok"));
        assert_eq!(
            resolve_backend(&p).unwrap(),
            Backend::Remote {
                url: "https://db.turso.io".into(),
                token: Some("tok".into())
            }
        );
    }

    #[test]
    fn localhost_host_uses_http() {
        assert_eq!(
            build_url_from_host("localhost", Some(8080), None),
            "http://localhost:8080"
        );
        assert_eq!(
            build_url_from_host("db.turso.io", None, None),
            "https://db.turso.io"
        );
    }

    #[test]
    fn empty_params_are_an_error() {
        let p = params(None, None, None);
        assert!(resolve_backend(&p).is_err());
    }

    #[test]
    fn tilde_is_expanded_when_home_is_set() {
        std::env::set_var("HOME", "/home/tester");
        assert_eq!(expand_path("~/db.sqlite"), "/home/tester/db.sqlite");
        assert_eq!(expand_path("file:/tmp/a.db"), "/tmp/a.db");
        assert_eq!(expand_path(":memory:"), ":memory:");
    }
}
