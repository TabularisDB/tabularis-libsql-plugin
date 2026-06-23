# Tabularis libSQL / Turso driver

> ⚠️ **Work in progress** — this plugin is under active development. APIs,
> behavior and feature coverage may change, and things may break. Use at your
> own risk.

A [Tabularis](https://github.com/TabularisDB/tabularis) database driver plugin
for **libSQL**. It connects to:

- **Local libSQL / SQLite files** on disk (via bundled SQLite — nothing to
  install), and
- **Remote Turso / sqld servers** over the **Hrana HTTP** protocol.

The plugin is a standalone executable that speaks Tabularis' JSON-RPC protocol
over stdin/stdout. It is cross-platform (Linux x64/arm64, macOS x64/arm64,
Windows x64) and ships those binaries from a single GitHub Actions release
workflow.

## Connecting

The driver picks a backend from the connection form automatically:

| You enter | Backend |
|-----------|---------|
| A file path in **Database** (e.g. `/data/app.db`, `~/notes.db`, `:memory:`) | Local SQLite file |
| A URL in **Database** or **Host** (e.g. `libsql://my-db.turso.io`) | Remote Hrana HTTP |
| A bare host in **Host** (e.g. `db.turso.io`) | Remote Hrana HTTP (`https://`) |

For Turso, put the **auth token** in the **Password** field, or append it to the
URL as `?authToken=...`. `libsql://`, `wss://` and `turso://` URLs are
automatically rewritten to `https://` (and `ws://` to `http://`) for the Hrana
HTTP endpoint. A self-hosted sqld on `localhost:8080` uses `http://`
automatically.

Connection-string import is supported, e.g.:

```
libsql://my-db.turso.io?authToken=eyJ...
```

## Feature coverage

| Area | Status |
|------|--------|
| `test_connection`, `ping` | ✅ |
| Tables, columns, indexes, foreign keys | ✅ |
| Views: list, definition, columns, create/alter/drop | ✅ |
| Query execution with server-side LIMIT/OFFSET pagination + total count | ✅ |
| `EXPLAIN QUERY PLAN` | ✅ |
| Insert / update / delete rows (bound parameters) | ✅ |
| Schema snapshot + batch columns/FKs (ER diagram) | ✅ |
| `CREATE TABLE` SQL, add column, create/drop index | ✅ |
| Schemas, stored routines | ❌ (not a SQLite concept) |
| Alter column type, add/drop foreign key on existing table | ❌ (SQLite limitation — returns a clear error) |

Identifiers are quoted ANSI-style (`"name"`). Booleans are stored as `0`/`1` and
BLOBs are returned base64-encoded.

## Build & test

Requires a Rust toolchain (and a C compiler for the bundled SQLite).

```bash
just test          # cargo test — unit tests for SQL builders, parsing, RPC
just build         # debug build
just release       # optimized release build
just lint          # clippy -D warnings
just dev-install   # build + copy binary + manifest into the Tabularis plugins dir
just repl          # local JSON-RPC REPL over stdio
```

Or directly with cargo:

```bash
cargo test
cargo build --release
```

### Manual JSON-RPC smoke test

```bash
echo '{"jsonrpc":"2.0","method":"get_tables","params":{"params":{"database":"/tmp/app.db"},"schema":null},"id":1}' \
  | ./target/debug/libsql-plugin
```

## Installing

`just dev-install` copies `libsql-plugin` and `manifest.json` into the Tabularis
plugins folder:

- **Linux:** `~/.local/share/tabularis/plugins/libsql/`
- **macOS:** `~/Library/Application Support/tabularis/plugins/libsql/`
- **Windows:** `%APPDATA%\tabularis\plugins\libsql\`

Restart Tabularis (or toggle the plugin in Settings) and **libSQL** appears in
the Database Type list.

## Architecture

```
src/
├── main.rs              # stdio JSON-RPC loop
├── rpc.rs               # method routing + response helpers
├── client.rs            # backend resolution (local vs remote) + unified query/execute
├── hrana.rs             # Hrana-over-HTTP pipeline client (remote)
├── models.rs            # ConnectionParams
├── error.rs             # PluginError
├── handlers/            # metadata / query / crud / ddl
└── utils/               # identifiers, pagination, SQL classification, value conversion
```

Local files use [`rusqlite`](https://crates.io/crates/rusqlite) with the bundled
SQLite. Remote connections use a small synchronous [`ureq`](https://crates.io/crates/ureq)
client (pure-Rust rustls TLS) speaking the stateless Hrana `/v2/pipeline`
endpoint, so the same `query`/`execute` surface serves both backends with no
async runtime.

## License

Apache-2.0
