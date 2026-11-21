#!/usr/bin/env python3
"""Check the Rust pipeline against the DuckDB reference, stage by stage, on one dataset.

  stage_check.py DATA_DIR [--rust BIN] [--keep DIR]

Runs reference/pipeline in-process (its intermediate tables stay in the DuckDB connection), runs
the Rust binary with DUMP_DIR set (it then also writes its intermediate tables), and compares:

  links     person_map                     vs  persons.parquet     (pairs where id <> person)
  events    ev_person (surviving events)   vs  events.parquet
  sessions  sess_credit                    vs  sessions.parquet    (start, day, counts, revenue, credited campaign)
  final     user_daily, campaign_daily     vs  the Rust output tables
  checks    rules.md self-checks on the Rust output

Exit status 0 only if every stage matches. All comparisons are exact, in both directions.
"""
import argparse
import os
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(ROOT, "reference", "pipeline"))
import pipeline as ref  # noqa: E402


def diff(con, name, a, b):
    """Row-set diff of two SQL relations, both ways. Returns True when equal."""
    ca = con.execute(f"SELECT count(*) FROM ({a})").fetchone()[0]
    cb = con.execute(f"SELECT count(*) FROM ({b})").fetchone()[0]
    only_a = con.execute(f"SELECT count(*) FROM (({a}) EXCEPT ALL ({b}))").fetchone()[0]
    only_b = con.execute(f"SELECT count(*) FROM (({b}) EXCEPT ALL ({a}))").fetchone()[0]
    ok = only_a == 0 and only_b == 0 and ca == cb
    print(f"  {'ok  ' if ok else 'FAIL'} {name}: reference {ca} rows, rust {cb} rows, only-ref {only_a}, only-rust {only_b}")
    if not ok:
        for label, x, y in (("ref only", a, b), ("rust only", b, a)):
            print(f"    first {label}:", con.execute(f"SELECT * FROM (({x}) EXCEPT ALL ({y})) LIMIT 3").fetchall())
    return ok


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("data")
    ap.add_argument("--rust", default=os.path.join(ROOT, "target", "release", "sessions"))
    ap.add_argument("--keep", help="keep outputs and dumps in this folder")
    args = ap.parse_args()
    work = args.keep or tempfile.mkdtemp(prefix="stagecheck_")
    for sub in ("ref_out", "rust_out", "dump", "scratch"):
        os.makedirs(os.path.join(work, sub), exist_ok=True)
    ref_out, rust_out, dump = (os.path.join(work, s) for s in ("ref_out", "rust_out", "dump"))

    t = time.time()
    con = ref.run(args.data, ref_out, os.path.join(work, "scratch"), 8, 24, log=lambda *_: None)
    t_ref = time.time() - t

    env = dict(os.environ, DATA_DIR=args.data, OUTPUT_DIR=rust_out, DUMP_DIR=dump, QUIET="1")
    t = time.time()
    p = subprocess.run([args.rust], env=env, capture_output=True, text=True)
    t_rust = time.time() - t
    if p.returncode != 0:
        print("RUST FAILED", p.returncode, p.stderr[-2000:])
        return 1
    print(f"{args.data}: reference {t_ref:.1f}s, rust {t_rust:.2f}s (with stage dumps)")

    pq = lambda name: f"read_parquet('{dump}/{name}.parquet')"
    ok = True
    ok &= diff(con, "links: person_map (id <> person)",
               "SELECT id, person FROM person_map WHERE id <> person",
               f"SELECT id, person FROM {pq('persons')} WHERE id <> person")
    ok &= diff(con, "events: surviving events",
               "SELECT person, ts, CASE event_type WHEN 'view' THEN 0 WHEN 'cart' THEN 1 ELSE 2 END::BIGINT AS kind, "
               "category, CASE WHEN event_type = 'purchase' THEN price ELSE 0 END::BIGINT AS price FROM ev_person",
               f"SELECT person, ts, kind, category, price FROM {pq('events')}")
    ok &= diff(con, "sessions: start, day, counts, revenue, credited campaign",
               "SELECT person, start, day, events::BIGINT, purchases::BIGINT, revenue::BIGINT, campaign_id FROM sess_credit",
               f"SELECT person, start, day, events, purchases, revenue, campaign FROM {pq('sessions')}")
    ok &= diff(con, "final: user_daily",
               "SELECT user_id, day, sessions, events, revenue_cents, top_category FROM user_daily",
               f"SELECT user_id, day, sessions, events, revenue_cents, top_category FROM read_parquet('{rust_out}/user_daily/*.parquet')")
    ok &= diff(con, "final: campaign_daily",
               "SELECT campaign_id, channel, day, sessions, purchasing_sessions, revenue_cents, assisted_sessions FROM campaign_daily",
               f"SELECT campaign_id, channel, day, sessions, purchasing_sessions, revenue_cents, assisted_sessions "
               f"FROM read_parquet('{rust_out}/campaign_daily/*.parquet')")

    # rules.md self-checks on the Rust output
    u = con.execute(f"SELECT coalesce(sum(sessions),0), coalesce(sum(revenue_cents),0) FROM read_parquet('{rust_out}/user_daily/*.parquet')").fetchone()
    c = con.execute(f"SELECT coalesce(sum(sessions),0), coalesce(sum(revenue_cents),0), coalesce(sum(assisted_sessions),0), "
                    f"coalesce(sum(sessions) FILTER (WHERE campaign_id <> 'organic'),0) FROM read_parquet('{rust_out}/campaign_daily/*.parquet')").fetchone()
    s1 = tuple(u) == (c[0], c[1])
    s2 = c[2] >= c[3]
    dates = con.execute(f"SELECT typeof(day) FROM read_parquet('{rust_out}/user_daily/*.parquet') LIMIT 1").fetchone()
    print(f"  {'ok  ' if s1 else 'FAIL'} self-check: sessions and revenue equal across tables {tuple(u)} vs {(c[0], c[1])}")
    print(f"  {'ok  ' if s2 else 'FAIL'} self-check: assisted {c[2]} >= credited non-organic {c[3]}")
    print(f"  day column type: {dates[0] if dates else 'n/a (empty)'}")
    ok &= s1 and s2
    print("STAGES MATCH" if ok else "STAGES DIFFER")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
