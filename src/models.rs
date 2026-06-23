//! Shared request shapes.
//!
//! `ConnectionParams` mirrors the values the user typed into the Tabularis
//! connection form. All fields are optional because libSQL is dual-mode: a
//! local connection only fills `database` (a file path), while a remote Turso
//! connection fills `host`/`database` with a URL and `password` with the auth
//! token.

use serde_json::Value;

#[derive(Debug, Clone, Default)]
pub struct ConnectionParams {
    pub driver: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub database: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub ssl_mode: Option<String>,
}

impl ConnectionParams {
    pub fn from_value(value: &Value) -> Self {
        let obj = value.as_object();
        let get_str = |k: &str| {
            obj.and_then(|o| o.get(k))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let port = obj
            .and_then(|o| o.get("port"))
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .and_then(|p| u16::try_from(p).ok());

        Self {
            driver: get_str("driver"),
            host: get_str("host"),
            port,
            database: get_str("database"),
            username: get_str("username"),
            password: get_str("password"),
            ssl_mode: get_str("ssl_mode"),
        }
    }
}

/// Extract the nested `params` object every RPC method receives. Tabularis
/// wraps the connection params in `params.params`.
pub fn inner_params(value: &Value) -> &Value {
    value.get("params").unwrap_or(&Value::Null)
}
