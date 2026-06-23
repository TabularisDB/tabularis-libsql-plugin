#!/usr/bin/env python3
"""Generate a populated demo SQLite/libSQL database from demo/seed.sql.

The resulting file is a standard SQLite database, which is exactly what Turso
accepts when importing:

    python3 demo/make_demo_db.py                 # writes demo/tabularis_demo.db
    turso db create my-demo --from-file demo/tabularis_demo.db

You can also open the file directly with the plugin's local backend (point the
Tabularis "Database" field at it), no Turso required.
"""

from __future__ import annotations

import os
import sqlite3
import sys

HERE = os.path.dirname(os.path.abspath(__file__))


def main() -> int:
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "tabularis_demo.db")
    seed = os.path.join(HERE, "seed.sql")

    with open(seed, "r", encoding="utf-8") as fh:
        script = fh.read()

    if os.path.exists(out):
        os.remove(out)

    conn = sqlite3.connect(out)
    try:
        conn.executescript(script)
        conn.commit()
        tables = [
            r[0]
            for r in conn.execute(
                "SELECT name FROM sqlite_master WHERE type='table' "
                "AND name NOT LIKE 'sqlite_%' ORDER BY name"
            )
        ]
        counts = {t: conn.execute(f'SELECT COUNT(*) FROM "{t}"').fetchone()[0] for t in tables}
    finally:
        conn.close()

    size = os.path.getsize(out)
    print(f"Wrote {out} ({size} bytes)")
    print("Row counts:")
    for table, n in counts.items():
        print(f"  {table:<12} {n}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
