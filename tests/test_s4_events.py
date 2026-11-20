"""Section 4 TRAP tests: events, in rules.md order. Each test builds a tiny dataset and reads the tables."""
import random

import pytest
from conftest import ev, make_data

from dims import load_dims, read_window
from events import load_events, sessionize
from csvlines import csv_files
from links import build_person_map
import os


def build(con, tmp_path, events, links=(), products=None, **kw):
    kwargs = dict(events=events, links=links)
    if products is not None:
        kwargs["products"] = products
    data = make_data(tmp_path, **kwargs, **kw)
    ws, we = read_window(data)
    load_dims(con, data)
    build_person_map(con, csv_files(os.path.join(data, "links")))
    load_events(con, csv_files(os.path.join(data, "events")), ws, we)
    sessionize(con)
    return data


def valid_ids(con):
    return sorted(r[0] for r in con.execute("SELECT event_id FROM ev_valid").fetchall())


def sessions(con):
    return con.execute(
        "SELECT person, strftime(day, '%Y-%m-%d'), events, purchases, revenue FROM sessions ORDER BY 1, start").fetchall()


T = "2026-10-03T"


# ------------------------------------------------------------------------------ steps 1 and 2: dedup
def test_dedup_keeps_the_smallest_ingest_ts_even_across_users(con, tmp_path):
    # E4: the earliest copy wins, including its user.
    build(con, tmp_path, [
        ev("e1", "u2", T + "10:00:00Z", ingest=T + "10:05:00Z"),
        ev("e1", "u1", T + "10:00:00Z", ingest=T + "10:01:00Z"),
    ])
    assert con.execute("SELECT user_id FROM ev_valid").fetchall() == [("u1",)]


def test_invalid_earliest_copy_kills_the_event_a_later_valid_copy_does_not_rescue_it(con, tmp_path):
    # TRAP E5: dedup BEFORE validation. The valid copy is listed first and ingested later.
    build(con, tmp_path, [
        ev("e2", "u1", T + "10:00:00Z", ingest=T + "10:05:00Z"),
        ev("e2", "u1", "2026-10-03 10:00:00", ingest=T + "10:01:00Z"),
    ])
    assert valid_ids(con) == []


@pytest.mark.parametrize("bad_first", [
    ev("e1", "", T + "10:00:00Z", ingest=T + "10:01:00Z"),                       # empty user
    ev("e1", "u1", T + "10:00:00Z", ingest=T + "10:01:00Z", kind="Purchase", price="100"),  # bad type
    ev("e1", "u1", T + "10:00:00Z", ingest=T + "10:01:00Z", product=""),         # empty product
    ev("e1", "u1", T + "10:00:00Z", ingest=T + "10:01:00Z", kind="purchase", price="0"),  # bad price
    ev("e1", "u1", "2026-11-03T10:00:00Z", ingest=T + "10:01:00Z"),              # outside the window
])
def test_any_invalid_kept_copy_drops_the_event(con, tmp_path, bad_first):
    build(con, tmp_path, [ev("e1", "u1", T + "10:00:00Z", ingest=T + "10:05:00Z"), bad_first])
    assert valid_ids(con) == []


def test_a_malformed_copy_was_already_dropped_so_it_is_never_the_kept_copy(con, tmp_path):
    build(con, tmp_path, [
        f"e1,u1,{T}10:00:00Z,{T}10:01:00Z,view",           # 5 fields: malformed, dropped in step 1
        ev("e1", "u1", T + "10:00:00Z", ingest=T + "10:05:00Z"),
    ])
    assert valid_ids(con) == ["e1"]


def test_empty_event_id_is_dropped_and_ids_compare_exactly(con, tmp_path):
    # E13 + E19 + E20: empty id dropped; `e1` vs `e1 ` vs `e3` vs `e3\r` are different events.
    build(con, tmp_path, [
        ev("", "u1", T + "10:00:00Z"),
        ev("e1", "u1", T + "10:00:00Z"), ev("e1 ", "u2", T + "10:00:00Z"),
        ev("E1", "u3", T + "10:00:00Z"),
        ev("e3", "u4", T + "10:00:00Z"), ev("e3\r", "u5", T + "10:00:00Z"),
        ev('"e7"', "u6", T + "10:00:00Z"), ev("e7", "u7", T + "10:30:00Z", ingest=T + "11:00:00Z"),
    ])
    assert valid_ids(con) == ["E1", "e1", "e1 ", "e3", "e3\r", "e7"]
    assert con.execute("SELECT user_id FROM ev_valid WHERE event_id = 'e7'").fetchall() == [("u6",)]


def test_dedup_copies_with_equal_ingest_are_identical_so_any_is_fine(con, tmp_path):
    line = ev("e1", "u1", T + "10:00:00Z", ingest=T + "10:01:00Z")
    build(con, tmp_path, [line, line, line])
    assert valid_ids(con) == ["e1"]


# ------------------------------------------------------------------------------ step 3: validation
def test_event_type_is_exact_and_case_sensitive(con, tmp_path):
    rows = [ev(f"e{i}", f"u{i}", T + "10:00:00Z", kind=k, price="100")
            for i, k in enumerate(["view", "cart", "purchase", "Purchase", "cart ", "VIEW", " view", "click", ""])]
    build(con, tmp_path, rows)
    assert valid_ids(con) == ["e0", "e1", "e2"]


def test_empty_user_and_empty_product_are_dropped_but_spaces_are_values(con, tmp_path):
    build(con, tmp_path, [
        ev("e1", "", T + "10:00:00Z"), ev("e2", '""', T + "10:00:00Z"),
        ev("e3", " ", T + "10:00:00Z"), ev("e4", "u1", T + "10:00:00Z", product=""),
        ev("e5", "u1", T + "10:00:00Z", product=" "),
    ])
    assert valid_ids(con) == ["e3", "e5"]


@pytest.mark.parametrize("price,ok", [
    ("100", True), ("0250", True), ("1", True), ("10000000", True), ("010000000", True),
    ("0000000000000000000000001", True),                  # many leading zeros are still the number 1
    ("0", False), ("00", False), ("", False), ("10000001", False),
    ("100000000000000000000000", False),                  # TRAP: far too long to fit any integer
    ("99999999999999999999", False), ("9223372036854775808", False),
    ("19.99", False), ("-5", False), ("+5", False), ("1e3", False), ("1,000", False),
    ("1000\r", False),                                    # TRAP: CR is part of the field
    (" 100", False), ("100 ", False),
    ("٢٥٠", False), ("２５０", False),                       # TRAP: Unicode digits are not digits
])
def test_purchase_price_rules(con, tmp_path, price, ok):
    build(con, tmp_path, [ev("e1", "u1", T + "10:00:00Z", kind="purchase", price=f'"{price}"' if "," in price else price)])
    assert (valid_ids(con) == ["e1"]) is ok


def test_view_and_cart_ignore_the_price_completely(con, tmp_path):
    # E10: a view priced `-5` stays; garbage of any kind is ignored.
    build(con, tmp_path, [
        ev("e1", "u1", T + "10:00:00Z", kind="view", price="-5"),
        ev("e2", "u1", T + "10:01:00Z", kind="cart", price="٢٥٠"),
        ev("e3", "u1", T + "10:02:00Z", kind="view", price="99999999999999999999999"),
        ev("e4", "u1", T + "10:03:00Z", kind="view", price=""),
    ])
    assert valid_ids(con) == ["e1", "e2", "e3", "e4"]


def test_quoted_price_is_stripped_before_it_is_judged(con, tmp_path):
    build(con, tmp_path, [ev("e1", "u1", T + "10:00:00Z", kind="purchase", price='"0250"')])
    assert con.execute("SELECT price FROM ev_valid").fetchall() == [(250,)]


def test_event_window_includes_start_and_excludes_end(con, tmp_path):
    # E12
    build(con, tmp_path, [
        ev("before", "u1", "2026-09-30T23:59:59Z"), ev("start", "u1", "2026-10-01T00:00:00Z"),
        ev("last", "u1", "2026-10-14T23:59:59Z"), ev("end", "u1", "2026-10-15T00:00:00Z"),
        ev("f3end", "u1", "1792022400000"), ev("f3last", "u1", "1792022399999"),
        ev("f2", "u1", "2026-10-14T23:59:59-00:01", ingest="2026-10-15T00:00:00Z"),   # = 00:00:59 on the 15th
    ])
    assert valid_ids(con) == ["f3last", "last", "start"]


def test_bad_timestamp_forms_are_dropped_in_events(con, tmp_path):
    bad = ["2026-10-03 10:00:00", "2026-10-03T10:00:00", "2026-10-03T10:00:00z", "1791071890000.0",
           "2026-10-03T10:00:60Z", "2026-10-03T24:00:00Z", "2026-02-30T10:00:00Z", "2026-10-03T10:00:00+24:00",
           "2026-10-03T10:00:00+0200", "2026-10-03T10:00:00.5Z", "", "86400000", "179102160000000000000",
           "٢٠٢٦-١٠-٠٣T10:00:00Z", "2026-10-03T10:00:00Z\r"]
    good = ["2026-10-03T10:00:00-00:00", "2026-10-03T10:00:00+23:59", "0001791021600000", "1791021600000"]
    build(con, tmp_path, [ev(f"b{i}", "u1", t) for i, t in enumerate(bad)]
          + [ev(f"g{i}", "u1", t) for i, t in enumerate(good)])
    assert valid_ids(con) == ["g0", "g1", "g2", "g3"]


# ------------------------------------------------------------------------------ step 4: late events
def test_late_cutoff_is_strictly_over_24_hours(con, tmp_path):
    # E6, plus ingest before ts.
    build(con, tmp_path, [
        ev("exact", "u1", T + "10:00:00Z", ingest="2026-10-04T10:00:00Z"),
        ev("late", "u2", T + "10:00:00Z", ingest="2026-10-04T10:00:01Z"),
        ev("early", "u3", T + "10:00:00Z", ingest=T + "09:00:00Z"),
        ev("f3exact", "u4", "1791021600000", ingest="2026-10-04T10:00:00Z"),
        ev("f3late", "u5", "1791021599999", ingest="2026-10-04T10:00:00Z"),
    ])
    assert valid_ids(con) == ["early", "exact", "f3exact"]


def test_late_is_judged_on_the_kept_copy_only(con, tmp_path):
    # The earliest copy is on time and is the kept one; the later copy would be 5 days late.
    build(con, tmp_path, [ev("e1", "u1", T + "10:00:00Z", ingest=T + "10:01:00Z"),
                          ev("e1", "u1", T + "10:00:00Z", ingest="2026-10-08T10:00:00Z")])
    assert valid_ids(con) == ["e1"]


# ------------------------------------------------------------------------------ step 5: sessions
def run_times(con, tmp_path, times, user="u1", **kw):
    build(con, tmp_path, [ev(f"e{i}", user, t) for i, t in enumerate(times)], **kw)
    return con.execute("SELECT count(DISTINCT sess_no) FROM ev_sess").fetchone()[0]


def test_gap_of_exactly_30_minutes_stays_in_the_session_and_1ms_more_splits(con, tmp_path):
    # E1 and E2
    assert run_times(con, tmp_path / "a", [T + "10:00:00Z", T + "10:30:00Z"]) == 1
    con2 = __import__("duckdb").connect()
    assert run_times(con2, tmp_path / "b", ["1791021600000", "1791023400001"]) == 2
    con3 = __import__("duckdb").connect()
    assert run_times(con3, tmp_path / "c", ["1791021600000", "1791023400000"]) == 1


def test_gap_is_measured_from_the_previous_event_not_the_session_start(con, tmp_path):
    # 10:00, 10:25, 10:50, 11:15 each 25 min apart: one session of 75 minutes.
    assert run_times(con, tmp_path, [T + "10:00:00Z", T + "10:25:00Z", T + "10:50:00Z", T + "11:15:00Z"]) == 1


def test_equal_timestamps_share_a_session_on_either_side_of_a_gap(con, tmp_path):
    # TRAP E16: ties at 10:00:00.000 x2, at 10:30:00.001 x3 (gap 30:00.001 splits), at 11:00:00.001.
    ms = lambda h, m, s, milli: str(1791021600000 + ((h - 10) * 3600 + m * 60 + s) * 1000 + milli)  # noqa: E731
    times = [ms(10, 0, 0, 0)] * 2 + [ms(10, 30, 0, 1)] * 3 + [ms(11, 0, 0, 1)]
    build(con, tmp_path, [ev(f"e{i}", "u1", t) for i, t in enumerate(times)])
    got = con.execute("SELECT events FROM sessions ORDER BY start").fetchall()
    assert got == [(2,), (4,)]  # the 11:00:00.001 event is exactly 30:00.000 after 10:30:00.001, so it joins


def test_session_result_does_not_depend_on_read_order(con, tmp_path):
    rng = random.Random(5)
    base = [(f"e{i}", "u1", t) for i, t in enumerate(
        ["1791021600000"] * 3 + ["1791023400001"] * 2 + ["1791025200001"] * 2 + ["1791040000000"])]
    results = set()
    for trial in range(6):
        rows = base[:]
        rng.shuffle(rows)
        c = __import__("duckdb").connect()
        build(c, tmp_path / f"t{trial}", [ev(e, u, t) for e, u, t in rows])
        results.add(tuple(c.execute("SELECT start, events FROM sessions ORDER BY start").fetchall()))
    assert len(results) == 1


def test_linked_ids_share_sessions_and_gaps_are_measured_across_them(con, tmp_path):
    # L1: u1 at 10:00 and 10:50, u2 buys at 10:25. Separately there are three sessions; linked it is one.
    build(con, tmp_path, [
        ev("e1", "u1", T + "10:00:00Z"), ev("e2", "u1", T + "10:50:00Z"),
        ev("e3", "u2", T + "10:25:00Z", kind="purchase", product="p2", price="800"),
        ev("e4", "u3", T + "10:00:00Z"),
    ], links=["u2,u1"])
    assert sessions(con) == [("u1", "2026-10-03", 3, 1, 800), ("u3", "2026-10-03", 1, 0, 0)]


def test_users_are_different_without_a_link_and_no_case_folding(con, tmp_path):
    build(con, tmp_path, [ev("e1", "u1", T + "10:00:00Z"), ev("e2", "U1", T + "10:01:00Z"),
                          ev("e3", " u1", T + "10:02:00Z"), ev("e4", "u1 ", T + "10:03:00Z")])
    assert sorted(r[0] for r in sessions(con)) == [" u1", "U1", "u1", "u1 "]


def test_person_name_is_the_smallest_id_even_if_no_event_uses_it(con, tmp_path):
    # L2
    build(con, tmp_path, [ev("e1", "u3", T + "10:00:00Z"), ev("e2", "u2", T + "10:05:00Z"),
                          ev("e3", "u10", T + "10:10:00Z")],
          links=["u3,u9", "u9,u2", "U7,u9", "u10,u10"])
    # u3, u9, u2 and U7 are one person; 'U7' sorts before every lower-case ID and names them.
    assert sessions(con) == [("U7", "2026-10-03", 2, 0, 0), ("u10", "2026-10-03", 1, 0, 0)]


# ------------------------------------------------------------------------------ step 6: day
def test_session_day_is_the_start_day_even_after_midnight(con, tmp_path):
    # E3
    build(con, tmp_path, [ev("e1", "u1", "2026-10-03T23:50:00Z"),
                          ev("e2", "u1", "2026-10-04T00:10:00Z", kind="purchase", price="1000")])
    assert sessions(con) == [("u1", "2026-10-03", 2, 1, 1000)]


def test_offset_timestamps_change_the_day(con, tmp_path):
    # E7: 01:30+02:00 on Oct 4 is 23:30 on Oct 3 UTC.
    build(con, tmp_path, [ev("e1", "u1", "2026-10-04T01:30:00+02:00")])
    assert sessions(con) == [("u1", "2026-10-03", 1, 0, 0)]


def test_several_sessions_a_day_are_counted_by_start_day(con, tmp_path):
    # E14
    build(con, tmp_path, [ev("e1", "u1", T + "09:00:00Z"), ev("e2", "u1", T + "09:10:00Z"),
                          ev("e3", "u1", T + "14:00:00Z"), ev("e4", "u1", "2026-10-04T08:00:00Z")])
    assert sessions(con) == [("u1", "2026-10-03", 2, 0, 0), ("u1", "2026-10-03", 1, 0, 0),
                             ("u1", "2026-10-04", 1, 0, 0)]


def test_leap_day_2028_is_a_real_day(con, tmp_path):
    build(con, tmp_path, [ev("e1", "u1", "2028-02-29T10:00:00Z")], window=("2028-02-25T00:00:00Z", "2028-03-10T00:00:00Z"))
    assert sessions(con) == [("u1", "2028-02-29", 1, 0, 0)]


# ------------------------------------------------------------------------------ step 7: categories
def test_missing_product_is_unknown_including_case_and_space_variants(con, tmp_path):
    build(con, tmp_path, [ev("e1", "u1", T + "10:00:00Z", kind="purchase", product="p9", price="1"),
                          ev("e2", "u1", T + "10:01:00Z", kind="purchase", product="P1", price="1"),
                          ev("e3", "u1", T + "10:02:00Z", kind="purchase", product=" p1", price="1"),
                          ev("e4", "u1", T + "10:03:00Z", kind="purchase", product="p1", price="1")])
    cats = sorted(con.execute("SELECT category FROM ev_sess").fetchall())
    assert cats == [("shoes",), ("unknown",), ("unknown",), ("unknown",)]
