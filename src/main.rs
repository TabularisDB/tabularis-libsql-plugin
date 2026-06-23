//! Entry point: read JSON-RPC lines from stdin, dispatch, write responses.
//!
//! One JSON-RPC request per line in, one response per line out. Everything
//! that can fail is turned into a JSON-RPC error response rather than a panic,
//! so a single bad request never takes the plugin down.

use std::io::{self, BufRead, Write};

use libsql_plugin::rpc;

fn main() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let response = rpc::handle_line(trimmed);
        let mut body = match serde_json::to_string(&response) {
            Ok(s) => s,
            Err(err) => format!(
                "{{\"jsonrpc\":\"2.0\",\"error\":{{\"code\":-32603,\"message\":\"serialization failed: {err}\"}},\"id\":null}}",
            ),
        };
        body.push('\n');
        if out.write_all(body.as_bytes()).is_err() {
            break;
        }
        let _ = out.flush();
    }
}
