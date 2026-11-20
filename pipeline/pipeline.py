#!/usr/bin/env python3
"""
Shopping Sessions: DuckDB pipeline (correctness first).

Reads  $DATA_DIR    (default /data)
Writes $OUTPUT_DIR  (default /output): user_daily/ and campaign_daily/ as Parquet
Temp   $SCRATCH_DIR (default /scratch)

rules.md is the source of truth. The stages follow SPEC_TRAPS.md sections 1 to 6:

  1 csvlines   raw line reader and field parser (DuckDB's CSV reader is not used)
  2 timestamps ts_ms(): F1 / F2 / F3 to epoch ms
  3 links      person names via connected components
  4 events     dedup, validate, late, sessionize
  5 clicks     dedup, validate, credit, assists
  6 outputs    user_daily and campaign_daily
"""
import os
import sys
import time

import duckdb

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from clicks import attribute, load_clicks  # noqa: E402
from csvlines import csv_files  # noqa: E402
from dims import click_window, load_dims, read_window  # noqa: E402
from events import load_events, sessionize  # noqa: E402
from links import build_person_map  # noqa: E402
from outputs import build_outputs, self_check, write_outputs  # noqa: E402


THREADS = 8
MEM_LIMIT_GB = 20  # of the scorer's 24 GB; the rest is for Python, numpy and the OS


def connect(scratch, ncpu, mem_gb):
    """DuckDB: 8 threads, ~20 GB, and every spill file under /scratch (the root fs is read-only)."""
    con = duckdb.connect()  # in-memory database
    threads = min(THREADS, ncpu) if ncpu else THREADS
    mem = min(MEM_LIMIT_GB, int(mem_gb * 0.85))  # a smaller local box gets a smaller limit
    tmp = os.path.join(scratch, "duckdb_tmp")
    os.makedirs(tmp, exist_ok=True)
    con.execute(f"SET threads = {threads}")
    con.execute(f"SET memory_limit = '{mem}GB'")
    con.execute(f"SET temp_directory = '{tmp}'")
    con.execute("SET preserve_insertion_order = false")
    return con


def run(data_dir, out_dir, scratch_dir, ncpu, mem_gb, log=print):
    t0 = time.time()

    def stage(name):
        log(f"[{time.time() - t0:7.2f}s] {name}")

    con = connect(scratch_dir, ncpu, mem_gb)
    log("duckdb: threads={}, memory_limit={}, temp_directory={}".format(*(
        con.execute(f"SELECT current_setting('{k}')").fetchone()[0] for k in ("threads", "memory_limit", "temp_directory"))))
    window_start, window_end = read_window(data_dir)
    click_start, click_end = click_window(window_start, window_end)
    log(f"window: {window_start} to {window_end} (ms)")

    load_dims(con, data_dir)
    stage("products and campaigns loaded")
    build_person_map(con, csv_files(os.path.join(data_dir, "links")))
    stage("links resolved")
    load_events(con, csv_files(os.path.join(data_dir, "events")), window_start, window_end)
    stage("events parsed, deduped, validated")
    sessionize(con)
    stage("sessions built")
    load_clicks(con, csv_files(os.path.join(data_dir, "clicks")), click_start, click_end)
    stage("clicks parsed, deduped, validated")
    attribute(con)
    stage("sessions credited, assists found")
    build_outputs(con)
    for problem in self_check(con):
        log(f"SELF-CHECK FAILED: {problem}")
    write_outputs(con, out_dir)
    stage("outputs written")
    return con


def main():
    data = os.environ.get("DATA_DIR", "/data")
    out = os.environ.get("OUTPUT_DIR", "/output")
    scratch = os.environ.get("SCRATCH_DIR", "/scratch")
    ncpu = int(os.environ.get("NCPU", THREADS))
    mem_gb = int(os.environ.get("MEM_GB", "24"))
    run(data, out, scratch, ncpu, mem_gb)


if __name__ == "__main__":
    main()
