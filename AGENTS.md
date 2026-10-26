# AGENTS.md

Tabularis driver plugin for libSQL/Turso: a standalone binary speaking Tabularis' JSON-RPC 2.0 over stdin/stdout (one request line in, one response line out). Two backends: local files through the bundled **libSQL fork** of SQLite (`libsql` crate, `core` feature, no system lib) and remote Turso/sqld over Hrana HTTP `/v2/pipeline` with blocking `ureq` + rustls. No async runtime: `futures_executor::block_on` bridges libsql's async API. A C compiler is required to build the bundled SQLite.

## Commands

- `just test` — `cargo test`; unit tests only, inline `#[cfg(test)]`, no integration harness. Single test: `cargo test <name>`.
- `just lint` / `just fmt` — clippy with `-D warnings` / rustfmt. CI runs build + `cargo test --locked --all-targets` + RustSec audit, but not clippy or fmt, so run both locally.
- `just repl` — `cargo run --bin test_plugin`: REPL over the real dispatch. Bare method names (e.g. `get_databases`) are auto-wrapped; full JSON-RPC objects pass through; `exit` quits.
- `just dev-install` — builds debug, copies binary + `.tabularium` to the OS plugin dir: Linux `~/.local/share/tabularis/plugins/drivers/libsql`, macOS `~/Library/Application Support/tabularis/plugins/drivers/libsql`, Windows `%APPDATA%\tabularis\plugins\drivers\libsql`. Tabularis resolves kind-first (`plugins/drivers/<id>` before flat `plugins/<id>`), so the `drivers` segment is mandatory — a flat copy is shadowed by an existing installed bundle.
- Manual smoke: `echo '{"jsonrpc":"2.0","method":"get_tables","params":{"params":{"database":":memory:"}},"id":1}' | ./target/debug/libsql-plugin`
- Windows shell is PowerShell; `just` recipes use `windows-shell`, and each recipe line runs as a separate `powershell.exe`. Variable-setting recipes must be one `;`-joined line (see `dev-install`).

## Architecture

- Logic lives in the library (`src/lib.rs`); `src/main.rs` is thin (read line → `rpc::handle_line` → write line). `test_plugin` drives the same dispatch. New RPC method = handler in `src/handlers/{metadata,query,crud,ddl}.rs` plus an arm in `rpc.rs::handle_line`; handlers return `Result<Value, PluginError>` via `handlers::respond`.
- Connection params live in the nested `params.params` block; use `handlers::connect` / `models::inner_params`. A fresh `Client` is built per call; there is no shared state.
- Preview builders `get_create_table_sql`, `get_add_column_sql`, `get_create_index_sql` are pure (no connection params). `get_create_foreign_key_sql` always receives `params.params` (the host's RpcDriver passes them); `get_alter_column_sql` uses params only when present and otherwise stays pure. Do not call `connect()` unconditionally in builders. Methods that touch a database (`execute_query`, CRUD, `drop_index`, `drop_foreign_key`, schema-introspecting metadata) do receive params.
- `execute_query` must match the host's strict `QueryResult`: `affected_rows`, `truncated`, and `pagination` are required. Requests send `limit` (code also accepts `page_size`); the response `pagination` is `{page, page_size, total_rows: null, has_more}`. `has_more` comes from a LIMIT+1 fetch; there is no `total_count` field.
- `client.rs::resolve_backend`: `connection_uri` is authoritative over decomposed fields. A URL in `database`/`host` (or any non-empty `host`) is remote; otherwise `database` is a local path. `libsql://`/`turso://`/`wss://` → `https://`, `ws://` → `http://`; localhost or `ssl_mode=disable` uses `http://`; `file:` paths and `~/` expand. Auth token from the URL query (`authToken`, etc.) wins over `password`.
- sqld namespaces go in the `x-namespace` header, not the URL path: sqld 0.24.x ignores `/dev/<ns>/...` and resolves from Host. `hrana.rs::namespace_from_url_path` maps `.../dev/example` → `example`; plain Turso hosts send no header. When `host` is bare, a decomposed `database` containing `/` is kept as a URL path prefix; `.tabularium` declares `http`/`https` so those URIs reach the plugin.
- Errors must become JSON-RPC error responses, never panics; one bad request must not kill the process.

## Conventions / gotchas

- Quote identifiers with `utils::identifiers::quote` (ANSI `"name"`); bind CRUD values, never interpolate them. Booleans are stored `0`/`1`; BLOBs are returned base64-encoded.
- `get_databases` is static `["main"]`; `get_schemas` and `get_routines` return empty (libSQL has no schemas or stored routines).
- `update_record` / `delete_record` take `pk_map` (preferred, supports composite keys); legacy `pk_col`/`pk_val` is also accepted. Keep both.
- Both backends run the libSQL fork, so `ALTER TABLE ... ALTER COLUMN` (retype, FK add/drop) works locally and remotely. The ALTER/FK builders emit the libSQL form unconditionally; renames use vanilla `RENAME COLUMN`.
- FK add emits `ALTER COLUMN col TO col <type> REFERENCES ...`, reproducing type/NOT NULL/DEFAULT from `PRAGMA table_info`. Constraint names are cosmetic; names are re-derived from `PRAGMA foreign_key_list`. Composite FK drops error; FK enforcement needs `PRAGMA foreign_keys=ON` per connection.
- `.tabularium` capabilities must stay in sync with actual support (`alter_column`, `create_foreign_keys` are true).
- `get_schema_snapshot` returns the host's `Vec<TableSchema>` shape: a top-level array of `{name, columns, foreign_keys}` (empty schema → `[]`). The host deserializes it directly into the ER diagram, so an envelope or name-keyed map fails the whole load.
- Tests that need one database across connections must use a temp file: local `:memory:` databases are per-connection.

## Release

- Tag `v*` triggers `.github/workflows/release.yml`: 5 targets (Linux x64/arm64 — arm64 via `cross` — macOS x64/arm64, Windows x64), zipping the binary, `.tabularium`, and assets. `.tabularium` also ships as a standalone asset because the registry resolves the manifest from release assets, not the git ref.
- `Cargo.lock` is committed; CI and release builds use `--locked`. Keep the `.tabularium` version in sync with `Cargo.toml`.

## Demo

- `demo/tabularis_demo.db` and its `.db-wal` are committed; regenerate with `demo/make_demo_db.py` + `demo/seed.sql`. `demo/turso_demo.py` targets remote Turso.
