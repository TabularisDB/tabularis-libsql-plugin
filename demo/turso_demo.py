#!/usr/bin/env python3
"""End-to-end demo / smoke test for the libSQL plugin against a remote Turso DB.

It drives the *real* plugin binary over stdin/stdout (exactly as Tabularis does)
and runs a full battery of JSON-RPC calls: connect, create a demo table, insert
rows, inspect metadata, run a paginated query, update/delete, create and drop a
view, then clean up.

Credentials are read from the environment so they never touch disk:

    export TURSO_DATABASE_URL="libsql://my-db.turso.io"   # or https://...
    export TURSO_AUTH_TOKEN="eyJ..."                       # optional for local sqld
    python3 demo/turso_demo.py

Options:
    --keep      do not drop the demo table/view at the end
    --binary P  path to the plugin binary (default: target/debug/libsql-plugin)

The demo uses a dedicated, namespaced object so it won't clash with your data:
    table  __tabularis_libsql_demo
    view   __tabularis_libsql_demo_v
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys

TABLE = "__tabularis_libsql_demo"
VIEW = "__tabularis_libsql_demo_v"

GREEN, RED, DIM, BOLD, RESET = "\033[32m", "\033[31m", "\033[2m", "\033[1m", "\033[0m"


def build_requests(conn: dict) -> list[tuple[str, dict]]:
    """Return a list of (label, jsonrpc_request) pairs, in execution order."""

    def req(method: str, **params) -> dict:
        # Every method carries the connection block under params.params.
        params["params"] = conn
        return {"jsonrpc": "2.0", "method": method, "params": params, "id": 0}

    steps: list[tuple[str, dict]] = [
        ("test_connection", req("test_connection")),
        ("clean up any leftover view", req("drop_view", name=VIEW)),
        (
            "drop leftover demo table",
            req("execute_query", query=f'DROP TABLE IF EXISTS "{TABLE}"'),
        ),
        (
            "create demo table",
            req(
                "execute_query",
                query=(
                    f'CREATE TABLE "{TABLE}" '
                    "(id INTEGER PRIMARY KEY AUTOINCREMENT, "
                    "name TEXT NOT NULL, score REAL, active INTEGER)"
                ),
            ),
        ),
        (
            "create an index",
            req(
                "execute_query",
                query=f'CREATE INDEX "idx_{TABLE}_name" ON "{TABLE}" (name)',
            ),
        ),
        (
            "insert Alice",
            req("insert_record", table=TABLE,
                data={"name": "Alice", "score": 9.5, "active": 1}),
        ),
        (
            "insert Bob",
            req("insert_record", table=TABLE,
                data={"name": "Bob", "score": 7.0, "active": 0}),
        ),
        (
            "insert Carol",
            req("insert_record", table=TABLE,
                data={"name": "Carol", "score": 8.25, "active": 1}),
        ),
        ("list tables", req("get_tables", schema=None)),
        ("describe columns", req("get_columns", table=TABLE)),
        ("list indexes", req("get_indexes", table=TABLE)),
        (
            "paginated query (page 1, size 2)",
            req("execute_query",
                query=f'SELECT id, name, score, active FROM "{TABLE}" ORDER BY id',
                page=1, page_size=2),
        ),
        (
            "paginated query (page 2, size 2)",
            req("execute_query",
                query=f'SELECT id, name, score, active FROM "{TABLE}" ORDER BY id',
                page=2, page_size=2),
        ),
        (
            "EXPLAIN QUERY PLAN",
            req("explain_query", query=f'SELECT * FROM "{TABLE}" WHERE name = \'Alice\''),
        ),
        (
            "update Bob's score",
            req("update_record", table=TABLE,
                pk_col="id", pk_val=2, col_name="score", new_val=7.75),
        ),
        (
            "delete Carol",
            req("delete_record", table=TABLE, pk_col="id", pk_val=3),
        ),
        (
            "query after CRUD",
            req("execute_query", query=f'SELECT id, name, score FROM "{TABLE}" ORDER BY id'),
        ),
        (
            "create a view",
            req("create_view", name=VIEW,
                definition=f'SELECT name, score FROM "{TABLE}" WHERE active = 1'),
        ),
        ("list views", req("get_views", schema=None)),
        ("view definition", req("get_view_definition", view=VIEW)),
        ("CREATE TABLE SQL", req("get_create_table_sql", table=TABLE)),
        ("schema snapshot (ER diagram)", req("get_schema_snapshot", schema=None)),
    ]
    return steps


def main() -> int:
    parser = argparse.ArgumentParser(description="libSQL plugin Turso demo")
    parser.add_argument("--keep", action="store_true",
                        help="keep the demo table/view (skip cleanup)")
    parser.add_argument("--binary", default="target/debug/libsql-plugin",
                        help="path to the plugin binary")
    args = parser.parse_args()

    url = os.environ.get("TURSO_DATABASE_URL", "").strip()
    token = os.environ.get("TURSO_AUTH_TOKEN", "").strip()
    if not url:
        print(f"{RED}TURSO_DATABASE_URL is not set.{RESET}\n", file=sys.stderr)
        print(__doc__, file=sys.stderr)
        return 2

    if not os.path.exists(args.binary):
        print(f"{RED}Plugin binary not found at {args.binary}.{RESET}", file=sys.stderr)
        print("Build it first:  cargo build   (or: just build)", file=sys.stderr)
        return 2

    # Tabularis sends the URL in `database` and the token in `password`.
    conn = {"driver": "libsql", "database": url, "password": token or None}

    steps = build_requests(conn)
    if not args.keep:
        steps.append((
            "cleanup: drop view",
            {"jsonrpc": "2.0", "method": "drop_view",
             "params": {"name": VIEW, "params": conn}, "id": 0},
        ))
        steps.append((
            "cleanup: drop table",
            {"jsonrpc": "2.0", "method": "execute_query",
             "params": {"query": f'DROP TABLE IF EXISTS "{TABLE}"', "params": conn}, "id": 0},
        ))

    # Assign sequential ids and serialise one request per line.
    lines = []
    for i, (_, request) in enumerate(steps, start=1):
        request["id"] = i
        lines.append(json.dumps(request))
    stdin_blob = "\n".join(lines) + "\n"

    print(f"{BOLD}libSQL plugin — Turso demo{RESET}")
    print(f"{DIM}target: {url}{RESET}\n")

    proc = subprocess.run(
        [args.binary],
        input=stdin_blob,
        capture_output=True,
        text=True,
    )
    if proc.stderr.strip():
        print(f"{DIM}[plugin stderr]\n{proc.stderr.strip()}{RESET}\n", file=sys.stderr)

    # Map responses back to their step by id.
    responses: dict[int, dict] = {}
    for line in proc.stdout.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
            responses[obj.get("id")] = obj
        except json.JSONDecodeError:
            print(f"{RED}non-JSON output: {line}{RESET}")

    failures = 0
    for i, (label, _) in enumerate(steps, start=1):
        resp = responses.get(i)
        if resp is None:
            print(f"{RED}✗{RESET} {label}: no response")
            failures += 1
            continue
        if "error" in resp:
            print(f"{RED}✗ {label}{RESET}")
            print(f"    {RED}{resp['error'].get('message')}{RESET}")
            failures += 1
        else:
            result = json.dumps(resp.get("result"), ensure_ascii=False)
            if len(result) > 400:
                result = result[:400] + " …"
            print(f"{GREEN}✓{RESET} {label}")
            print(f"    {DIM}{result}{RESET}")

    print()
    if failures:
        print(f"{RED}{BOLD}{failures} step(s) failed.{RESET}")
        return 1
    print(f"{GREEN}{BOLD}All {len(steps)} steps passed.{RESET}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
