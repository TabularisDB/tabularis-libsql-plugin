# Tabularis libSQL / Turso driver

[![CI](https://github.com/TabularisDB/tabularis-libsql-plugin/actions/workflows/ci.yml/badge.svg)](https://github.com/TabularisDB/tabularis-libsql-plugin/actions/workflows/ci.yml)

> ⚠️ **Work in progress** — this plugin is under active development. APIs,
> behavior and feature coverage may change, and things may break. Use at your
> own risk.

A [Tabularis](https://github.com/TabularisDB/tabularis) database driver plugin
for **libSQL**. It connects to:

- **Local libSQL / SQLite files** on disk (via the bundled libSQL fork — nothing
  to install), and
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
| A `file:` URI in **Database** / connection string (e.g. `file:///data/app.db`) | Local SQLite file |
| A URL in **Database** or **Host** (e.g. `libsql://my-db.turso.io`, `http://localhost:8080/dev/example/`) | Remote Hrana HTTP |
| A bare host in **Host** (e.g. `db.turso.io`) | Remote Hrana HTTP (`https://`) |

A connection URI (`connection_uri`) is authoritative over the decomposed
`host`/`port`/`database` fields. `libsql://`, `wss://` and `turso://` URLs are
automatically rewritten to `https://` (and `ws://` to `http://`) for the Hrana
HTTP endpoint; `http://` and `https://` URLs are used as-is.

For a decomposed `host`, the scheme is chosen from `ssl_mode`:

- `ssl_mode=disable` forces `http://` for any host.
- A loopback host (`localhost`, `127.0.0.1`, `[::1]`) without
  `ssl_mode=require` also uses `http://`.
- Everything else uses `https://`.

For Turso, put the **auth token** in the **Password** field, or append it to the
URL as `?authToken=...`.

Self-hosted sqld namespaces: the namespace is derived from the URL path
(`http://localhost:8080/dev/example/` → `example`) and sent as the `x-namespace`
header, because sqld 0.24.x ignores the path and would otherwise resolve the
namespace from the Host header. This also covers decomposed `host`/`port`/
`database` params — a multi-segment database value (`dev/example`) is kept as a
path prefix. Turso database names contain no `/`, so plain names stay pathless.

Connection-string import is supported, e.g.:

```
libsql://my-db.turso.io?authToken=eyJ...
http://localhost:8080/dev/example/
```

## Feature coverage

| Area | Status |
|------|--------|
| `initialize`, `ping`, `test_connection` | ✅ |
| Connection-string import (`connection_uri` + `turso`/`http`/`https`/`ws`/`wss`/`file` schemes) | ✅ |
| Tables, columns, indexes, foreign keys | ✅ |
| Triggers: list + definition | ✅ |
| Views: list, definition, columns, create/alter/drop | ✅ |
| Query execution with LIMIT/OFFSET pagination for wrappable `SELECT`s (`has_more` from a one-row-ahead fetch; `total_rows` is always `null`) | ✅ |
| `EXPLAIN QUERY PLAN` | ✅ |
| Insert / update / delete rows (bound parameters) | ✅ |
| Schema snapshot + batch columns/FKs — powers the ER diagram | ✅ |
| `CREATE TABLE` SQL, add column, create/drop index | ✅ |
| Schemas, stored routines | ❌ (not a SQLite concept — Turso has no stored procedures, and its multi-database model is separate databases, not schemas) |
| Rename column | ✅ everywhere (vanilla `RENAME COLUMN`) |
| Alter column type / default | ✅ everywhere (libSQL `ALTER COLUMN` extension) |
| Drop foreign key on existing table | ✅ everywhere |
| Add foreign key to existing table | ✅ everywhere (see below) |

Identifiers are quoted ANSI-style (`"name"`). Booleans are stored as `0`/`1` and
BLOBs are returned base64-encoded.

`get_schema_snapshot` returns the host's `TableSchema` array shape — one
`{name, columns, foreign_keys}` entry per table — which is what Tabularis
consumes for the ER diagram. An empty schema returns `[]`.

### Schema changes via ALTER COLUMN

Both local files and remote Turso / sqld servers run the **libSQL fork** of
SQLite, which adds
`ALTER TABLE ... ALTER COLUMN col TO col <type> [DEFAULT ...] [REFERENCES ...]`.
The plugin uses it to:

- change a column's type (and optionally its DEFAULT / NOT NULL),
- add or drop a foreign key on an existing column (the same statement with or
  without the `REFERENCES` clause).

The `get_create_foreign_key_sql` builder always receives connection params from
the host, and `get_alter_column_sql` does too when the host provides them, so
the plugin introspects the column's declared type and constraints (NOT NULL,
DEFAULT, REFERENCES) and reproduces them in the rewrite — altering a column
does not silently strip its foreign key or default. Note that libSQL applies
constraint changes to newly inserted/updated rows only — existing rows are not
rewritten or revalidated — and foreign key *enforcement* requires
`PRAGMA foreign_keys=ON`.

Composite (multi-column) foreign keys cannot be dropped: the fork only rewrites
per-column definitions, so `drop_foreign_key` reports a clear error instead of
silently doing nothing.

## Build & test

Requires a Rust toolchain (`rust-toolchain.toml` pins the stable channel with
`rustfmt` and `clippy`) and a C compiler for the bundled SQLite. `just` is
convenient but optional — every recipe is a thin wrapper over cargo.

```bash
just test          # cargo test — unit tests for SQL builders, parsing, RPC
just build         # debug build
just release       # optimized release build
just lint          # clippy -D warnings
just fmt           # rustfmt
just dev-install   # build + copy binary + manifest into plugins/drivers/libsql
just repl          # local JSON-RPC REPL over stdio
```

CI runs the same suite with the lockfile pinned:

```bash
cargo build --locked
cargo test --locked --all-targets
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

### Release archive

Download `libsql-plugin-<platform>.zip` from the
[releases page](https://github.com/TabularisDB/tabularis-libsql-plugin/releases)
and unpack it so the driver folder contains the executable, `.tabularium` and
`assets/`:

- **Linux:** `~/.local/share/tabularis/plugins/drivers/libsql/`
- **macOS:** `~/Library/Application Support/tabularis/plugins/drivers/libsql/`
- **Windows:** `%APPDATA%\tabularis\plugins\drivers\libsql\`

The root is the Tabularis config directory
(`ProjectDirs::from("com", "debba", "tabularis")`). Tabularis resolves
kind-scoped bundles (`plugins/drivers/<id>`) before flat `plugins/<id>` bundles,
so keep the `drivers` segment — a flat copy is shadowed by an installed bundle.
Each release also publishes `.tabularium` as a standalone asset.

### From source

`just dev-install` builds a debug binary and copies it plus `.tabularium` into
the same `plugins/drivers/libsql` folder.

Either way, restart Tabularis (or toggle the plugin in Settings) and **libSQL**
appears in the Database Type list.

## Architecture

```
src/
├── main.rs              # stdio JSON-RPC loop (one request line in, one response line out)
├── lib.rs               # shared dispatch, used by main.rs and the REPL
├── rpc.rs               # method routing + response helpers
├── client.rs            # backend resolution (local vs remote) + unified query/execute
├── hrana.rs             # Hrana-over-HTTP pipeline client (remote)
├── models.rs            # ConnectionParams
├── error.rs             # PluginError
├── handlers/            # metadata / query / crud / ddl
├── utils/               # identifiers, pagination, SQL classification, value conversion
└── bin/test_plugin.rs   # `just repl` driver
```

Local files use the embedded **libSQL fork of SQLite** (bundled — nothing to
install). Remote connections use a small synchronous [`ureq`](https://crates.io/crates/ureq)
client (pure-Rust rustls TLS) speaking the stateless Hrana `/v2/pipeline`
endpoint, so the same `query`/`execute` surface serves both backends with no
async runtime.

## License

Apache-2.0
