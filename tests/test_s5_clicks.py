"""Section 5 TRAP tests: clicks, credit and assists."""
import os

import duckdb
import pytest
from conftest import ev, make_data

from clicks import attribute, load_clicks
from csvlines import csv_files
from dims import click_window, load_dims, read_window
from events import load_events, sessionize
from links import build_person_map
from timestamps import iso_to_ms

T = "2026-10-03T"
SESSION = [ev("s1", "u1", T + "10:00:00Z")]       # one session starting Oct 3 10:00:00


def build(con, tmp_path, events=SESSION, clicks=(), links=(), **kw):
    data = make_data(tmp_path, events=events, clicks=clicks, links=links, **kw)
    ws, we = read_window(data)
    cs, ce = click_window(ws, we)
    load_dims(con, data)
    build_person_map(con, csv_files(os.path.join(data, "links")))
    load_events(con, csv_files(os.path.join(data, "events")), ws, we)
    sessionize(con)
    load_clicks(con, csv_files(os.path.join(data, "clicks")), cs, ce)
    attribute(con)
    return data


def k(click_id, user, campaign, ts):
    return f"{click_id},{user},{campaign},{ts}"


def credited(con):
    return con.execute("SELECT person, campaign_id FROM sess_credit ORDER BY person, start").fetchall()


def click_ids(con):
    return sorted(r[0] for r in con.execute("SELECT click_id FROM ck_valid").fetchall())


def assisting(con):
    return sorted(con.execute("SELECT person, campaign_id FROM assists").fetchall())


# ------------------------------------------------------------------------------ steps 1 to 3
def test_basic_credit(con, tmp_path):
    build(con, tmp_path, clicks=[k("k1", "u1", "c_1", T + "09:00:00Z")])
    assert credited(con) == [("u1", "c_1")]


def test_no_click_credits_organic(con, tmp_path):
    build(con, tmp_path)
    assert credited(con) == [("u1", "organic")]
    assert assisting(con) == []


def test_malformed_and_empty_click_lines_are_dropped(con, tmp_path):
    build(con, tmp_path, clicks=[
        k("", "u1", "c_1", T + "09:00:00Z"),                 # empty click_id
        f"k2,u1,c_1",                                        # wrong field count
        f'k3,u1,c_1,{T}09:00:00Z"',                          # stray quote
        "",                                                  # empty line
        k("k4", "", "c_1", T + "09:00:00Z"),                 # empty user
        k("k5", "u1", "", T + "09:00:00Z"),                  # empty campaign
        k("ok", "u1", "c_1", T + "09:00:00Z"),
    ])
    assert click_ids(con) == ["ok"]


def test_click_dedup_quoted_differently_is_one_click(con, tmp_path):
    build(con, tmp_path, clicks=[k("k1", "u1", "c_1", T + "09:00:00Z"),
                                 k('"k1"', '"u1"', "c_1", f'"{T}09:00:00Z"'),
                                 k("k1", "u1", '"c_1"', T + "09:00:00Z")])
    assert click_ids(con) == ["k1"]


def test_unknown_campaign_drops_the_click_exact_match_only(con, tmp_path):
    # A6 + spaces (E19): `c_999`, `C_1` and ` c_1` are not in campaigns.csv.
    build(con, tmp_path, clicks=[k("k1", "u1", "c_999", T + "09:00:00Z"), k("k2", "u1", "C_1", T + "09:00:00Z"),
                                 k("k3", "u1", " c_1", T + "09:00:00Z"), k("k4", "u1", "c_1 ", T + "09:00:00Z"),
                                 k("k5", "u1", "organic", T + "09:00:00Z")])
    assert click_ids(con) == []
    assert credited(con) == [("u1", "organic")]


def test_click_user_ids_are_exact_no_case_folding_no_trim(con, tmp_path):
    # A7 + E19
    build(con, tmp_path, clicks=[k("k1", "U1", "c_1", T + "09:00:00Z"), k("k2", " u1", "c_1", T + "09:00:00Z")])
    assert credited(con) == [("u1", "organic")]


def test_click_window_edges(con, tmp_path):
    # A10: starts 7 days before the window (inclusive), ends at the window end (exclusive).
    build(con, tmp_path, clicks=[
        k("start", "u1", "c_1", "2026-09-24T00:00:00Z"),
        k("before", "u1", "c_1", str(iso_to_ms("2026-09-24T00:00:00Z") - 1)),   # 1 ms before window_start - 7 d
        k("last", "u1", "c_1", "2026-10-14T23:59:59Z"),
        k("end", "u1", "c_1", "2026-10-15T00:00:00Z"),
        k("after", "u1", "c_1", "2026-10-15T00:00:01Z"),
    ])
    assert click_ids(con) == ["last", "start"]


def test_bad_click_timestamps_are_dropped(con, tmp_path):
    bad = ["2026-10-03 09:00:00", "2026-10-03T09:00:00", "2026-10-03T09:00:00Z\r", "1791068400000.0",
           "2026-02-30T09:00:00Z", "2026-10-03T09:00:00+24:00", "", "٢٠٢٦-١٠-٠٣T09:00:00Z"]
    good = ["2026-10-03T09:00:00+00:00", "1791021600000", "0001791021600000"]
    build(con, tmp_path, clicks=[k(f"b{i}", "u1", "c_1", t) for i, t in enumerate(bad)]
          + [k(f"g{i}", "u1", "c_1", t) for i, t in enumerate(good)])
    assert click_ids(con) == ["g0", "g1", "g2"]


def test_a_click_with_cr_in_its_ts_is_bad_but_other_clicks_survive(con, tmp_path):
    # E20: `...Z\r` is a bad ts.
    build(con, tmp_path, clicks=[k("k1", "u1", "c_1", T + "09:00:00Z\r"), k("k2", "u1", "c_2", T + "08:00:00Z")])
    assert credited(con) == [("u1", "c_2")]


# ------------------------------------------------------------------------------ step 5: credit
def test_latest_click_wins(con, tmp_path):
    # A3
    build(con, tmp_path, clicks=[k("k1", "u1", "c_1", "2026-10-02T10:00:00Z"), k("k2", "u1", "c_2", T + "08:00:00Z")])
    assert credited(con) == [("u1", "c_2")]


def test_tie_on_ts_goes_to_the_smallest_click_id_byte_wise(con, tmp_path):
    # A5
    build(con, tmp_path, clicks=[k("k2", "u1", "c_2", T + "09:00:00Z"), k("k1", "u1", "c_1", T + "09:00:00Z")])
    assert credited(con) == [("u1", "c_1")]
    c2 = duckdb.connect()
    build(c2, tmp_path / "b", clicks=[k("k1", "u1", "c_1", T + "09:00:00Z"), k("K9", "u1", "c_2", T + "09:00:00Z")])
    assert credited(c2) == [("u1", "c_2")]       # 'K9' < 'k1': upper case sorts first


def test_seven_days_before_is_in_and_one_ms_more_is_out(con, tmp_path):
    # A2
    build(con, tmp_path, clicks=[k("k1", "u1", "c_1", "2026-09-26T10:00:00Z")])
    assert credited(con) == [("u1", "c_1")]
    c2 = duckdb.connect()
    build(c2, tmp_path / "b", clicks=[k("k1", "u1", "c_1", str(iso_to_ms(T + "10:00:00Z") - 604_800_000 - 1))])   # 7 d + 1 ms before
    assert credited(c2) == [("u1", "organic")]
    assert assisting(c2) == []


def test_click_at_the_session_start_counts_and_one_ms_after_does_not(con, tmp_path):
    # A4
    build(con, tmp_path, clicks=[k("k1", "u1", "c_1", T + "10:00:00Z")])
    assert credited(con) == [("u1", "c_1")]
    c2 = duckdb.connect()
    build(c2, tmp_path / "b", clicks=[k("k1", "u1", "c_1", "1791021600001")])
    assert credited(c2) == [("u1", "organic")]


def test_a_click_in_the_middle_of_a_session_credits_only_later_sessions(con, tmp_path):
    # A9: events 10:00, 10:25 and 11:30; the click is at 10:20.
    build(con, tmp_path, events=[ev("e1", "u1", T + "10:00:00Z"), ev("e2", "u1", T + "10:25:00Z"),
                                 ev("e3", "u1", T + "11:30:00Z")],
          clicks=[k("k1", "u1", "c_1", T + "10:20:00Z")])
    assert credited(con) == [("u1", "organic"), ("u1", "c_1")]


def test_one_click_credits_many_sessions(con, tmp_path):
    # A8
    build(con, tmp_path, events=[ev("e1", "u1", T + "10:00:00Z"), ev("e2", "u1", "2026-10-04T08:00:00Z")],
          clicks=[k("k1", "u1", "c_1", T + "09:00:00Z")])
    assert credited(con) == [("u1", "c_1"), ("u1", "c_1")]


def test_clicks_never_split_or_merge_sessions(con, tmp_path):
    build(con, tmp_path, events=[ev("e1", "u1", T + "10:00:00Z"), ev("e2", "u1", T + "10:20:00Z")],
          clicks=[k("k1", "u1", "c_1", T + "10:10:00Z")])
    assert con.execute("SELECT count(*) FROM sessions").fetchone() == (1,)


def test_a_click_from_another_linked_id_credits_and_the_latest_across_ids_wins(con, tmp_path):
    # L4: k1 for c_1 by u2 at 09:00; k2 for c_2 by u1 at 08:00; u1 and u2 are one person.
    build(con, tmp_path, links=["u1,u2"], clicks=[k("k1", "u2", "c_1", T + "09:00:00Z"),
                                                  k("k2", "u1", "c_2", T + "08:00:00Z")])
    assert credited(con) == [("u1", "c_1")]
    assert assisting(con) == [("u1", "c_1"), ("u1", "c_2")]


def test_a_click_by_a_person_with_no_events_does_nothing(con, tmp_path):
    build(con, tmp_path, clicks=[k("k1", "stranger", "c_1", T + "09:00:00Z")])
    assert credited(con) == [("u1", "organic")]


def test_click_in_the_prior_window_week_credits_a_session_on_the_first_day(con, tmp_path):
    # A click before window_start still counts (within 7 days).
    build(con, tmp_path, events=[ev("e1", "u1", "2026-10-01T00:00:00Z")], clicks=[k("k1", "u1", "c_1", "2026-09-24T00:00:00Z")])
    assert credited(con) == [("u1", "c_1")]


# ------------------------------------------------------------------------------ step 7: assists
def test_every_campaign_clicked_in_the_week_assists_once_including_the_credited_one(con, tmp_path):
    # A11
    build(con, tmp_path, events=[ev("s1", "u1", T + "10:00:00Z", kind="purchase", price="1000")],
          clicks=[k("k1", "u1", "c_1", "2026-09-26T10:00:00Z"),          # exactly 7 days before
                  k("k2", "u1", "c_2", T + "09:00:00Z"), k("k3", "u1", "c_2", T + "09:30:00Z"),
                  k("k4", "u1", "c_1", "1791021600001"),                 # 1 ms AFTER the start: no
                  k("k5", "u2", "c_1", T + "09:59:00Z")])                   # another person
    assert credited(con) == [("u1", "c_2")]
    assert assisting(con) == [("u1", "c_1"), ("u1", "c_2")]
    assert con.execute("SELECT count(*) FROM assists").fetchone() == (2,)   # c_2 has two clicks, assists once


def test_organic_never_assists(con, tmp_path):
    build(con, tmp_path)
    assert assisting(con) == []


def test_one_click_assists_every_session_in_the_following_7_days_on_each_sessions_day(con, tmp_path):
    # A12 (u2 is linked to u1 and holds the c_2 click)
    ev_rows = [ev("e1", "u1", "2026-10-03T09:00:00Z"), ev("e2", "u1", "2026-10-04T09:00:00Z"),
               ev("e3", "u1", "2026-10-09T09:00:00Z"), ev("e4", "u1", str(iso_to_ms("2026-10-03T09:00:00Z") + 604_800_001))]  # Oct 10 09:00:00.001
    build(con, tmp_path, events=ev_rows, links=["u1,u2"],
          clicks=[k("k1", "u1", "c_1", "2026-10-03T09:00:00Z"), k("k2", "u2", "c_2", "2026-10-04T09:00:00Z")])
    got = sorted(con.execute("SELECT strftime(day, '%m-%d'), campaign_id FROM assists").fetchall())
    # c_1 (Oct 3 09:00) assists sessions Oct 3, 4, 9 but not Oct 10 09:00:00.001 (exactly 7 d + 1 ms later).
    # c_2 (Oct 4 09:00) assists Oct 4, 9 and Oct 10 (just under 6 days later).
    assert got == [("10-03", "c_1"), ("10-04", "c_1"), ("10-04", "c_2"), ("10-09", "c_1"),
                   ("10-09", "c_2"), ("10-10", "c_2")]


def test_assists_agree_with_a_brute_force_python_check_on_random_data(con, tmp_path):
    import random
    rng = random.Random(11)
    base = 1790812800000   # 2026-10-01T00:00:00Z
    users = ["u1", "u2", "U1", "u3"]
    ev_rows, ck_rows, truth_events, truth_clicks = [], [], [], []
    for i in range(120):
        t = base + rng.randrange(0, 14 * 86400000)
        u = rng.choice(users)
        ev_rows.append(ev(f"e{i}", u, str(t)))
        truth_events.append((u, t))
    for i in range(160):
        t = base - 7 * 86400000 + rng.randrange(0, 21 * 86400000)
        # snap some clicks to existing event times to hit the boundaries and ties
        if rng.random() < 0.25:
            t = rng.choice(truth_events)[1] - rng.choice([0, 1, -1, 7 * 86400000, 7 * 86400000 + 1, 7 * 86400000 - 1])
        u = rng.choice(users)
        c = rng.choice(["c_1", "c_2"])
        ck_rows.append(k(f"k{i:03d}", u, c, str(t)))
        if base - 7 * 86400000 <= t < base + 14 * 86400000:
            truth_clicks.append((f"k{i:03d}", u, c, t))
    build(con, tmp_path, events=ev_rows, clicks=ck_rows, links=["u1,u2"])
    person = lambda u: "u1" if u in ("u1", "u2") else u  # noqa: E731
    # brute-force sessions
    by_person = {}
    for u, t in truth_events:
        by_person.setdefault(person(u), []).append(t)
    expected_credit, expected_assist = [], set()
    for p, times in by_person.items():
        times = sorted(set(times))
        starts = [times[0]] + [b for a, b in zip(times, times[1:]) if b - a > 1_800_000]
        for s in starts:
            cand = [(ts, cid, c) for cid, u, c, ts in truth_clicks if person(u) == p and s - 604_800_000 <= ts <= s]
            if cand:
                best = max(ts for ts, _, _ in cand)
                cid, c = min((cid, c) for ts, cid, c in cand if ts == best)
                expected_credit.append((p, s, c))
            else:
                expected_credit.append((p, s, "organic"))
            for c in {c for _, _, c in cand}:
                expected_assist.add((p, s, c))
    got_credit = sorted(con.execute("SELECT person, start, campaign_id FROM sess_credit").fetchall())
    got_assist = set(con.execute("SELECT s.person, s.start, a.campaign_id FROM assists a "
                                 "JOIN sessions s USING (person, sess_no)").fetchall())
    assert got_credit == sorted(expected_credit)
    assert got_assist == expected_assist
