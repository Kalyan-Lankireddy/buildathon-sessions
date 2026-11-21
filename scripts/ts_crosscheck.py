#!/usr/bin/env python3
"""Cross-check the Rust timestamp parser against the reference's DuckDB ts_ms().

Writes random and mutated timestamp strings with the reference's answer to a file, then runs
`cargo test against_reference` with TS_CASES pointing at it. Usage: ts_crosscheck.py [N] [SEED]
"""
import os
import random
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(ROOT, "reference", "pipeline"))
import duckdb  # noqa: E402
from timestamps import register_macros  # noqa: E402


def make_cases(n, rnd):
    def digits(k):
        return "".join(rnd.choice("0123456789") for _ in range(k))

    def valid_iso():
        y, mo, d = rnd.choice([2024, 2025, 2026, 2027, 2028, 2029]), rnd.randint(1, 12), rnd.randint(1, 31)
        base = f"{y:04d}-{mo:02d}-{d:02d}T{rnd.randint(0, 23):02d}:{rnd.randint(0, 59):02d}:{rnd.randint(0, 59):02d}"
        if rnd.random() < 0.5:
            return base + "Z"
        return base + rnd.choice("+-") + f"{rnd.randint(0, 23):02d}:{rnd.randint(0, 59):02d}"

    alphabet = "0123456789-:TZz+ .e٢２"
    out = []
    for _ in range(n):
        r = rnd.random()
        if r < 0.35:
            s = valid_iso()
        elif r < 0.55:
            s = ("0" * rnd.randint(0, 6)) + str(rnd.randint(0, 2 * 10**12))
        elif r < 0.65:
            s = digits(rnd.randint(1, 24))
        else:
            s = valid_iso() if rnd.random() < 0.5 else str(rnd.randint(0, 2 * 10**12))
            for _ in range(rnd.randint(1, 3)):
                i = rnd.randrange(len(s) + 1)
                op = rnd.random()
                if op < 0.4 and i < len(s):
                    s = s[:i] + rnd.choice(alphabet) + s[i + 1:]
                elif op < 0.7:
                    s = s[:i] + rnd.choice(alphabet) + s[i:]
                elif i < len(s):
                    s = s[:i] + s[i + 1:]
        if "\t" in s or "\n" in s:
            continue
        out.append(s)
    # hand-picked edges
    out += ["2028-02-29T00:00:00Z", "2026-02-29T00:00:00Z", "2100-02-29T00:00:00Z", "2000-02-29T00:00:00Z",
            "2026-10-03T23:59:59Z", "2026-10-03T00:00:00+23:59", "2026-10-03T00:00:00-23:59",
            "9" * 18, "9" * 19, "0" * 30, "0", "000000000000000000000001", "2026-10-03T00:00:00-00:00"]
    return out


def main():
    n = int(sys.argv[1]) if len(sys.argv) > 1 else 200000
    seed = int(sys.argv[2]) if len(sys.argv) > 2 else 1
    cases = make_cases(n, random.Random(seed))
    con = duckdb.connect()
    register_macros(con)
    con.execute("CREATE TABLE c(s VARCHAR)")
    con.executemany("INSERT INTO c VALUES (?)", [(s,) for s in cases])
    rows = con.execute("SELECT s, ts_ms(s) FROM c").fetchall()
    valid = sum(1 for _, v in rows if v is not None)
    with tempfile.NamedTemporaryFile("w", suffix=".tsv", delete=False, encoding="utf-8") as f:
        for s, v in rows:
            f.write(f"{s}\t{'' if v is None else v}\n")
        path = f.name
    print(f"{len(rows)} cases, {valid} valid, {len(rows) - valid} bad")
    env = dict(os.environ, TS_CASES=path)
    r = subprocess.run(["cargo", "test", "--release", "against_reference", "--", "--nocapture"], cwd=ROOT, env=env)
    os.unlink(path)
    sys.exit(r.returncode)


if __name__ == "__main__":
    main()
