"""Section 6 TRAP tests: output tables, end to end through pipeline.run and the Parquet files."""
import glob
import os

import duckdb
import pytest
from conftest import ev, make_data

from pipeline import run

T = "2026-10-03T"


def k(click_id, user, campaign, ts):
    return f"{click_id},{user},{campaign},{ts}"


def execute(tmp_path, **kw):
    data = make_data(tmp_path / "data", **kw)
    out, scratch = str(tmp_path / "out"), str(tmp_path / "scratch")
    os.makedirs(out), os.makedirs(scratch)
    messages = []
    run(data, out, scratch, 2, 4, log=messages.append)
    assert not [m for m in messages if "SELF-CHECK" in m], messages
    return out


def table(out, name, order="1, 2"):
    con = duckdb.connect()
    return con.execute(f"SELECT * FROM read_parquet('{out}/{name}/*.parquet') ORDER BY {order}").fetchall()


def users(out):
    return [(u, str(d), s, e, r, c) for u, d, s, e, r, c in table(out, "user_daily")]


def campaigns(out):
    return [(c, ch, str(d), s, p, r, a) for c, ch, d, s, p, r, a in table(out, "campaign_daily", "1, 3")]


# ------------------------------------------------------------------------------ user_daily
def test_user_daily_row_values(tmp_path):
    out = execute(tmp_path, events=[
        ev("e1", "u1", T + "10:00:00Z"), ev("e2", "u1", T + "10:05:00Z", kind="purchase", product="p1", price="1000"),
        ev("e3", "u1", T + "10:06:00Z", kind="cart", product="p2")])
    assert users(out) == [("u1", "2026-10-03", 1, 3, 1000, "shoes")]


def test_top_category_tie_goes_to_the_byte_wise_smallest(tmp_path):
    # E8: books 500 vs shoes 500 -> books.
    out = execute(tmp_path, events=[ev("e1", "u1", T + "10:00:00Z", kind="purchase", product="p1", price="500"),
                                    ev("e2", "u1", T + "10:01:00Z", kind="purchase", product="p2", price="500")])
    assert users(out)[0][5] == "books"


def test_top_category_unknown_competes_and_can_win(tmp_path):
    # E9: p9 (not in products) 2000 beats shoes 1500.
    out = execute(tmp_path, events=[ev("e1", "u1", T + "10:00:00Z", kind="purchase", product="p9", price="2000"),
                                    ev("e2", "u1", T + "10:01:00Z", kind="purchase", product="p1", price="1500")])
    assert users(out) == [("u1", "2026-10-03", 1, 2, 3500, "unknown")]


def test_unknown_sorts_by_bytes_like_any_category(tmp_path):
    # 'unknown' vs 'shoes' tie -> 'shoes' ('s' < 'u'); vs 'books' tie -> 'books'.
    out = execute(tmp_path, events=[ev("e1", "u1", T + "10:00:00Z", kind="purchase", product="p9", price="500"),
                                    ev("e2", "u1", T + "10:01:00Z", kind="purchase", product="p1", price="500")])
    assert users(out)[0][5] == "shoes"


def test_top_category_is_null_without_purchases_and_revenue_is_zero(tmp_path):
    out = execute(tmp_path, events=[ev("e1", "u1", T + "10:00:00Z"), ev("e2", "u1", T + "10:01:00Z", kind="cart")])
    assert users(out) == [("u1", "2026-10-03", 1, 2, 0, None)]


def test_top_category_sums_across_all_sessions_of_the_day(tmp_path):
    # Session 1: books 300. Session 2: shoes 200 + shoes 200. Books has the single biggest purchase, shoes wins the day.
    out = execute(tmp_path, events=[
        ev("e1", "u1", T + "08:00:00Z", kind="purchase", product="p2", price="300"),
        ev("e2", "u1", T + "14:00:00Z", kind="purchase", product="p1", price="200"),
        ev("e3", "u1", T + "14:10:00Z", kind="purchase", product="p1", price="200")])
    assert users(out) == [("u1", "2026-10-03", 2, 3, 700, "shoes")]


def test_a_session_crossing_midnight_is_counted_entirely_on_its_start_day(tmp_path):
    # E3: view Oct 3 23:50, purchase of p1 for 1000 on Oct 4 00:10.
    out = execute(tmp_path, events=[ev("e1", "u1", T + "23:50:00Z"),
                                    ev("e2", "u1", "2026-10-04T00:10:00Z", kind="purchase", price="1000")])
    assert users(out) == [("u1", "2026-10-03", 1, 2, 1000, "shoes")]


def test_day_follows_the_utc_start_with_offsets(tmp_path):
    out = execute(tmp_path, events=[ev("e1", "u1", "2026-10-04T01:30:00+02:00")])
    assert [r[1] for r in users(out)] == ["2026-10-03"]


def test_user_id_is_the_person_name(tmp_path):
    out = execute(tmp_path, events=[ev("e1", "u3", T + "10:00:00Z"), ev("e2", "u2", T + "10:05:00Z")],
                  links=["u3,u9", "u9,u2", "U7,u9"])
    assert users(out) == [("U7", "2026-10-03", 1, 2, 0, None)]


# ------------------------------------------------------------------------------ campaign_daily
def test_campaign_daily_credited_columns_and_purchasing_sessions_count_sessions(tmp_path):
    out = execute(tmp_path, events=[
        ev("e1", "u1", T + "10:00:00Z", kind="purchase", price="400"),
        ev("e2", "u1", T + "10:05:00Z", kind="purchase", product="p2", price="600"),   # same session: counts once
        ev("e3", "u2", T + "10:00:00Z"),                                                # session without purchase
    ], clicks=[k("k1", "u1", "c_1", T + "09:00:00Z"), k("k2", "u2", "c_1", T + "09:00:00Z")])
    assert campaigns(out) == [("c_1", "search", "2026-10-03", 2, 1, 1000, 2)]


def test_assist_only_campaign_still_gets_a_row_with_zero_credited_columns(tmp_path):
    # A11: u1's session has a purchase; c_1 only assists.
    out = execute(tmp_path, events=[ev("s1", "u1", T + "10:00:00Z", kind="purchase", price="1000")],
                  clicks=[k("k1", "u1", "c_1", "2026-09-26T10:00:00Z"), k("k2", "u1", "c_2", T + "09:00:00Z"),
                          k("k3", "u1", "c_2", T + "09:30:00Z")])
    assert campaigns(out) == [("c_1", "search", "2026-10-03", 0, 0, 0, 1),
                              ("c_2", "social", "2026-10-03", 1, 1, 1000, 1)]


def test_organic_row_has_channel_organic_and_zero_assists(tmp_path):
    out = execute(tmp_path, events=[ev("e1", "u1", T + "10:00:00Z", kind="purchase", price="250")])
    assert campaigns(out) == [("organic", "organic", "2026-10-03", 1, 1, 250, 0)]


def test_assists_land_on_each_sessions_own_day(tmp_path):
    out = execute(tmp_path, events=[ev("e1", "u1", "2026-10-03T09:00:00Z"), ev("e2", "u1", "2026-10-04T09:00:00Z")],
                  clicks=[k("k1", "u1", "c_1", "2026-10-03T08:00:00Z")])
    assert campaigns(out) == [("c_1", "search", "2026-10-03", 1, 0, 0, 1), ("c_1", "search", "2026-10-04", 1, 0, 0, 1)]


# ------------------------------------------------------------------------------ files and schema
def test_parquet_schema_is_exact_and_day_is_a_date(tmp_path):
    out = execute(tmp_path, events=[ev("e1", "u1", T + "10:00:00Z")], clicks=[k("k1", "u1", "c_1", T + "09:00:00Z")])
    con = duckdb.connect()
    for name, cols in (("user_daily", {"user_id": "VARCHAR", "day": "DATE", "sessions": "BIGINT", "events": "BIGINT",
                                       "revenue_cents": "BIGINT", "top_category": "VARCHAR"}),
                       ("campaign_daily", {"campaign_id": "VARCHAR", "channel": "VARCHAR", "day": "DATE",
                                           "sessions": "BIGINT", "purchasing_sessions": "BIGINT",
                                           "revenue_cents": "BIGINT", "assisted_sessions": "BIGINT"})):
        files = glob.glob(f"{out}/{name}/*")
        assert files and all(f.endswith(".parquet") for f in files)
        got = {r[0]: r[1] for r in con.execute(f"DESCRIBE SELECT * FROM read_parquet('{out}/{name}/*.parquet')").fetchall()}
        assert got == cols


def test_empty_input_still_writes_both_tables_with_the_schema(tmp_path):
    out = execute(tmp_path)
    assert users(out) == [] and campaigns(out) == []
    con = duckdb.connect()
    assert dict((r[0], r[1]) for r in con.execute(
        f"DESCRIBE SELECT * FROM read_parquet('{out}/user_daily/*.parquet')").fetchall())["day"] == "DATE"


def test_self_checks_hold_on_a_busier_dataset(tmp_path):
    rows, clicks = [], []
    for i in range(60):
        rows.append(ev(f"e{i}", f"u{i % 7}", f"2026-10-0{1 + i % 9}T{(i * 7) % 24:02d}:{(i * 13) % 60:02d}:00Z",
                       kind="purchase" if i % 5 == 0 else "view", product=["p1", "p2", "p9"][i % 3], price="150"))
    for i in range(25):
        clicks.append(k(f"k{i}", f"u{i % 7}", ["c_1", "c_2"][i % 2], f"2026-10-0{1 + i % 9}T0{i % 8}:00:00Z"))
    out = execute(tmp_path, events=rows, clicks=clicks)   # execute() fails on any SELF-CHECK message
    con = duckdb.connect()
    u = con.execute(f"SELECT sum(sessions), sum(revenue_cents) FROM read_parquet('{out}/user_daily/*.parquet')").fetchone()
    c = con.execute(f"SELECT sum(sessions), sum(revenue_cents) FROM read_parquet('{out}/campaign_daily/*.parquet')").fetchone()
    assert u == c and u[0] > 0
    keys = con.execute(f"SELECT count(*) - count(DISTINCT (campaign_id, day)) FROM read_parquet('{out}/campaign_daily/*.parquet')").fetchone()
    assert keys == (0,)
    keys = con.execute(f"SELECT count(*) - count(DISTINCT (user_id, day)) FROM read_parquet('{out}/user_daily/*.parquet')").fetchone()
    assert keys == (0,)
