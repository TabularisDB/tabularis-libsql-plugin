# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.0.0] - 2026-08-25

First stable release. Local libSQL / SQLite files via the embedded libSQL fork
(bundled, no system dependency) and remote Turso/sqld via Hrana HTTP (`/v2/pipeline`)
over a blocking `ureq` + `rustls` client. Standalone JSON-RPC 2.0 binary over
stdin/stdout (one request line in, one response line out), no async runtime
(`futures-executor::block_on` bridge).

Release notes: https://github.com/TabularisDB/tabularis-libsql-plugin/releases/tag/v1.0.0

### Added

- Initial plugin implementation — `src/main.rs` stdio loop + `src/lib.rs` shared
  dispatch, `rpc.rs` routing, `client.rs` backend resolution, `hrana.rs` Hrana
  pipeline client, `handlers/{metadata,query,crud,ddl}` and `utils` helpers
  ([#1](https://github.com/TabularisDB/tabularis-libsql-plugin/pull/1), [`bef9bc2`](https://github.com/TabularisDB/tabularis-libsql-plugin/commit/bef9bc2280f077ad1b47eb0348e50ecbdcd8d5cb)).
- CRUD with `pk_map` contract — `update_record` / `delete_record` accept `pk_map`
  fixing cell updates failing with `missing 'pk_col' parameter`
  ([#1](https://github.com/TabularisDB/tabularis-libsql-plugin/pull/1), [`bd7a65d`](https://github.com/TabularisDB/tabularis-libsql-plugin/commit/bd7a65d2b9f1110fd88e728035abdba06e6caa23)).
- `.tabularium` manifest for the Tabularium registry, replacing the legacy
  plugin manifest — includes `create_foreign_keys`, `alter_column`,
  `connection_uri` / `connection_uri_schemes` capabilities, driver `kind` contract
  and `icon.svg`
  ([#2](https://github.com/TabularisDB/tabularis-libsql-plugin/pull/2), [#3](https://github.com/TabularisDB/tabularis-libsql-plugin/pull/3), [`b9b7fe2`](https://github.com/TabularisDB/tabularis-libsql-plugin/commit/b9b7fe2d80ea336ada26d2e90ccefd6d94fdf79a), [`3a4f459`](https://github.com/TabularisDB/tabularis-libsql-plugin/commit/3a4f45997e0eff63321bbff2a721f342ea05af9b), [`d03374c`](https://github.com/TabularisDB/tabularis-libsql-plugin/commit/d03374c74ae99f81369ad06bb87b0d39a9b6ba33)).
- Remote libSQL DDL support — `CREATE TABLE`, add column, create/drop index,
  `ALTER TABLE ... ALTER COLUMN` (type change, FK add/drop via libSQL fork
  extension) and refactored DDL builders
  ([#4](https://github.com/TabularisDB/tabularis-libsql-plugin/pull/4), [`5190ab8`](https://github.com/TabularisDB/tabularis-libsql-plugin/commit/5190ab8e1bbf20e6dc405ee0f8f6ca1294524455), [`694483e`](https://github.com/TabularisDB/tabularis-libsql-plugin/commit/694483e12bf4a1037930ae459929f0157e4dae2e)).
- `get_triggers` RPC method and `create_foreign_keys` capability; `get_create_foreign_key_sql`
  now receives connection params and emits libSQL `ALTER COLUMN ... REFERENCES ...`
  form via `PRAGMA table_info` introspection
  ([`9584e64`](https://github.com/TabularisDB/tabularis-libsql-plugin/commit/9584e64aef797b4aace0e798e19fd6a169296b15)).
- `connection_uri` field on `ConnectionParams` — raw URI is authoritative over
  decomposed `host`/`password` fields; `resolve_backend` precedence, URL rewrites
  (`libsql://`/`wss://`/`turso://` → `https://`, `ws://` → `http://`,
  `localhost:8080` → `http://`), `?authToken=` preservation, and
  `.tabularium` `connection_uri_schemes` declaration
  ([`55880de`](https://github.com/TabularisDB/tabularis-libsql-plugin/commit/55880de360fd781dfe8e40a656406b2390f6d28c)).
- `get_create_foreign_key_sql` / `get_alter_column_sql` / trigger-metadata
  helpers — `fk_name` shared helper, trigger `timing`/`event` parsed from header
  only, column metadata keys aligned to host contract
  ([`d556c38`](https://github.com/TabularisDB/tabularis-libsql-plugin/commit/d556c383a13a34ec0b4021f4267f91842d45a2ac), [`4b494d3`](https://github.com/TabularisDB/tabularis-libsql-plugin/commit/4b494d32515e64ccf740bc30f395cffe3b462b47), [`4c16ae7`](https://github.com/TabularisDB/tabularis-libsql-plugin/commit/4c16ae7e3756b6711901d954155d9faeccb2db86)).

### Changed

- Replaced `rusqlite` with the embedded **libSQL fork of SQLite** (`libsql` crate,
  `core` feature, bundled) — local files now support the same `ALTER COLUMN` /
  FK extensions as remote Turso; client rewritten to use `libsql::Builder::new_local`
  + `futures-executor::block_on` without `tokio`
  ([`900bd8f`](https://github.com/TabularisDB/tabularis-libsql-plugin/commit/900bd8f2c4ae3baa68107d0b41f373c1d7b80d5b)).
- Switched from the `futures` facade to `futures-executor` directly
  ([`54afbb8`](https://github.com/TabularisDB/tabularis-libsql-plugin/commit/54afbb8cf60a4699a8ccb537a1c50490e97e2aab)).
- Unified row-fetch loop under a single `block_on` for the entire fetch
  ([`58c63ed`](https://github.com/TabularisDB/tabularis-libsql-plugin/commit/58c63ed10bffd1470ac996a51e610929ee5b8d75)).
- Documentation — corrected feature coverage for local `ALTER COLUMN` / FK support
  and plugin installation paths (`ProjectDirs::from("com","debba","tabularis")`)
  ([`43eaac5`](https://github.com/TabularisDB/tabularis-libsql-plugin/commit/43eaac5e8c0239f635ca022b5990d2439b7a5cc6), [`aa279b5`](https://github.com/TabularisDB/tabularis-libsql-plugin/commit/aa279b530e096b94737e1fd106114fac31679013)).

### Fixed

- `update_record` / `delete_record` contract — accept `pk_map` (host deserializes
  strictly) ([#1](https://github.com/TabularisDB/tabularis-libsql-plugin/pull/1)).
- Plugin validation — `name` must be lowercase (`.tabularium` / `Cargo.toml`)
  ([`7223305`](https://github.com/TabularisDB/tabularis-libsql-plugin/commit/72233056f2838e94dbd7b1709fe05a73efacc898)).
- CRUD `insert` — return `affected_rows` count instead of `null`
  ([`431695a`](https://github.com/TabularisDB/tabularis-libsql-plugin/commit/431695abeb3f9120c5feb71370a5ec30a683e13e)).
- Column metadata keys to match host contract
  ([`d556c38`](https://github.com/TabularisDB/tabularis-libsql-plugin/commit/d556c383a13a34ec0b4021f4267f91842d45a2ac)).
- Trigger parsing — `timing`/`event` extracted from header only
  ([`4b494d3`](https://github.com/TabularisDB/tabularis-libsql-plugin/commit/4b494d32515e64ccf740bc30f395cffe3b462b47)).
- `dev-install` path for `.tabularium` on Windows PowerShell
  ([`4969932`](https://github.com/TabularisDB/tabularis-libsql-plugin/commit/496993217608c990a93b740d674a234cec7230b9)).
- PR feedback fixes for `feat/alter`
  ([#4](https://github.com/TabularisDB/tabularis-libsql-plugin/pull/4), [`90bf265`](https://github.com/TabularisDB/tabularis-libsql-plugin/commit/90bf265801ac6354664fe315c7f36682d48685bf)).

### Contributors

Thanks to @debba, @NewtTheWolf and @jonaspm — see the
[v1.0.0 release notes](https://github.com/TabularisDB/tabularis-libsql-plugin/releases/tag/v1.0.0)
for the full list.

[Unreleased]: https://github.com/TabularisDB/tabularis-libsql-plugin/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/TabularisDB/tabularis-libsql-plugin/releases/tag/v1.0.0
