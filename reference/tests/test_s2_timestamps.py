"""Section 2 TRAP tests: timestamps."""
import random
import re
from datetime import date

import pytest

from timestamps import iso_to_ms, register_macros

F1 = "2026-10-03T23:58:10Z"
F1_MS = 1791071890000  # the rules' own example: F1, F2 and F3 are the same instant


@pytest.fixture
def ts(con):
    register_macros(con)

    def run(values):
        con.execute("CREATE OR REPLACE TABLE v(i INTEGER, x VARCHAR)")
        con.executemany("INSERT INTO v VALUES (?, ?)", list(enumerate(values)))
        return [r[0] for r in con.execute("SELECT ts_ms(x) FROM v ORDER BY i").fetchall()]

    return run


def test_the_rules_example_is_the_same_instant_in_all_three_forms(ts):
    assert ts([F1, "2026-10-03T19:58:10-04:00", "1791071890000", "0001791071890000"]) == [F1_MS] * 4


def test_manifest_window_parser_agrees(ts):
    assert iso_to_ms(F1) == F1_MS


def test_offsets_that_change_the_date(ts):
    assert ts(["2026-10-04T01:30:00+02:00", "2026-10-03T23:30:00Z", "2026-10-03T10:00:00-12:00"]) == [
        ts(["2026-10-03T23:30:00Z"])[0], ts(["2026-10-03T23:30:00Z"])[0], ts(["2026-10-03T22:00:00Z"])[0]]
    assert ts(["2026-10-03T22:00:00+12:00"])[0] == ts(["2026-10-03T10:00:00Z"])[0]


def test_valid_offset_extremes_and_minus_zero(ts):
    z = ts(["2026-10-03T10:00:00Z"])[0]
    assert ts(["2026-10-03T10:00:00-00:00", "2026-10-03T10:00:00+00:00"]) == [z, z]
    assert ts(["2026-10-03T10:00:00+23:59"])[0] == z - (23 * 60 + 59) * 60000


@pytest.mark.parametrize("bad", [
    "2026-10-03T10:00:00+24:00",   # TRAP: offset hour 24
    "2026-10-03T10:00:00+05:60",   # TRAP: offset minute 60
    "2026-10-03T10:00:00+0200",    # TRAP: no colon
    "2026-10-03T10:00:00+02",      # hour only
    "2026-10-03T10:00:00",         # no zone
    "2026-10-03 10:00:00",         # space instead of T
    "2026-10-03t10:00:00Z",        # lowercase t
    "2026-10-03T10:00:00z",        # lowercase z
    "2026-10-03T10:00:00.123Z",    # fractional seconds
    "2026-10-03T10:00:00.000Z",
    "2026-10-03T10:00:60Z",        # TRAP: second 60
    "2026-10-03T24:00:00Z",        # TRAP: hour 24
    "2026-10-03T10:60:00Z",        # minute 60
    "2026-13-03T10:00:00Z",        # month 13
    "2026-00-03T10:00:00Z",        # month 0
    "2026-10-00T10:00:00Z",        # day 0
    "2026-10-32T10:00:00Z",
    "2026-04-31T10:00:00Z",        # 30-day month
    "2026-02-30T10:00:00Z",        # TRAP: Feb 30
    "2026-02-29T10:00:00Z",        # TRAP: Feb 29, not a leap year
    "2027-02-29T10:00:00Z",
    "2100-02-29T10:00:00Z",        # century, not a leap year
    "2026-10-03T10:00:00ZZ",
    " 2026-10-03T10:00:00Z",       # spaces are not trimmed
    "2026-10-03T10:00:00Z ",
    "2026-10-03T10:00:00Z\r",      # TRAP: CR is a character
    "1791071890000\r",
    "1791071890000.0",             # F3 with a decimal
    "1.79e12",
    "-5",
    "+5",
    "",
    "٢٠٢٦-١٠-٠٣T10:00:00Z",        # TRAP: Arabic-Indic digits
    "２０２６-10-03T10:00:00Z",     # TRAP: full-width digits
    "١٧٩١٠٢١٦٠٠٠٠٠",
    "２５０",
    "abc",
    "2026-10-03",
    "20261003T100000Z",
])
def test_bad_forms_are_null(ts, bad):
    assert ts([bad]) == [None]


def test_null_input_is_null(ts):
    assert ts([None]) == [None]


def test_leap_day_2028_is_valid_and_is_a_real_day(ts):
    feb29, mar1 = ts(["2028-02-29T00:00:00Z", "2028-03-01T00:00:00Z"])
    assert feb29 is not None and mar1 - feb29 == 86_400_000
    assert ts(["2024-02-29T12:00:00Z", "2000-02-29T12:00:00Z"])[0] is not None
    assert ts(["2000-02-29T12:00:00Z"])[0] is not None


def test_f3_leading_zeros_and_extremes_are_numbers_not_malformed(ts):
    # Valid numbers: short ones are just outside a window, still a value.
    assert ts(["86400000", "0", "000", "0000000000000000000000001"]) == [86_400_000, 0, 0, 1]
    # TRAP: too long for int64 gives NULL (outside every window) and must not raise.
    assert ts(["179102160000000000000", "9223372036854775808", "99999999999999999999999999"]) == [None] * 3
    # 18 significant digits still fit.
    assert ts(["999999999999999999"]) == [999_999_999_999_999_999]
    # Leading zeros do not count toward the length.
    assert ts(["0000000000000000000001791071890000"]) == [F1_MS]


def test_second_boundaries_and_ms_precision(ts):
    a, b, c = ts(["2026-10-03T10:00:00Z", "1791021600000", "1791021600001"])
    assert a == 1791021600000 and b == a and c == a + 1


# Independent reference: plain Python, written from the rules text, used on random strings.
def reference(x):
    if x is None:
        return None
    if re.fullmatch(r"[0-9]+", x, re.ASCII):
        n = int(x)
        return n if n < 2**63 and len(x.lstrip("0")) <= 18 else None
    m = re.fullmatch(r"([0-9]{4})-([0-9]{2})-([0-9]{2})T([0-9]{2}):([0-9]{2}):([0-9]{2})"
                     r"(?:Z|([+-])([0-9]{2}):([0-9]{2}))", x, re.ASCII)
    if not m:
        return None
    y, mo, d, h, mi, s = (int(m.group(i)) for i in range(1, 7))
    if not (1 <= mo <= 12 and h <= 23 and mi <= 59 and s <= 59):
        return None
    leap = y % 4 == 0 and (y % 100 != 0 or y % 400 == 0)
    dim = [31, 29 if leap else 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][mo - 1]
    if not (1 <= d <= dim) or y < 1:
        return None
    off = 0
    if m.group(7):
        oh, om = int(m.group(8)), int(m.group(9))
        if oh > 23 or om > 59:
            return None
        off = (1 if m.group(7) == "+" else -1) * (oh * 3600 + om * 60)
    days = (date(y, mo, d) - date(1970, 1, 1)).days
    return (days * 86400 + h * 3600 + mi * 60 + s - off) * 1000


def test_random_strings_match_an_independent_python_reference(ts):
    rng = random.Random(7)
    vals = []
    for _ in range(4000):
        y = rng.choice([2025, 2026, 2027, 2028, 2100, 2000, 1999, 2024])
        mo = rng.choice([1, 2, 2, 2, 4, 9, 12, 0, 13])
        d = rng.choice([1, 28, 29, 30, 31, 0, 32, 15])
        h = rng.choice([0, 12, 23, 24, 25])
        mi = rng.choice([0, 30, 59, 60])
        s = rng.choice([0, 30, 59, 60])
        zone = rng.choice(["Z", "Z", "+05:30", "-04:00", "-00:00", "+23:59", "+24:00", "+05:60", "+0200", "z", ""])
        vals.append(f"{y:04d}-{mo:02d}-{d:02d}T{h:02d}:{mi:02d}:{s:02d}{zone}")
    vals += [str(rng.randrange(0, 10**rng.randint(1, 22))).zfill(rng.randint(1, 25)) for _ in range(1000)]
    got = ts(vals)
    for v, g in zip(vals, got):
        # year 0000 is never in a window; both sides call it bad or far out, but compare exactly
        assert g == reference(v), v
