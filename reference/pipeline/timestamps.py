"""Section 2: timestamps to int64 epoch milliseconds (UTC).

`ts_ms(x)` returns BIGINT, or NULL when `x` is bad. NULL also covers an F3 number too long for
int64: rules.md calls that "outside the window", and both mean the same thing to every caller
(the event or click is dropped), so one NULL is exact.

Forms (explicit [0-9] classes everywhere; never \\d, never Unicode digits):
  F1  2026-10-03T23:58:10Z
  F2  2026-10-03T19:58:10-04:00      offset hours 00-23, minutes 00-59, UTC = local - offset
  F3  1791071890000                  digits only, leading zeros allowed
Calendar validity (month, day for the month and leap year, hour, minute, second 00-59) is checked
by DuckDB's own DATE parser plus explicit range checks on the time and offset fields.
"""
from datetime import datetime, timezone

MS_PER_DAY = 86_400_000
WEEK_MS = 7 * MS_PER_DAY
SESSION_GAP_MS = 1_800_000
LATE_MS = MS_PER_DAY


def register_macros(con):
    # F3: strip leading zeros; 19+ digits cannot be in any window and may overflow int64.
    con.execute("""
        CREATE OR REPLACE MACRO ts_f3(x) AS (
            CASE WHEN length(ltrim(x, '0')) > 18 THEN NULL
                 WHEN ltrim(x, '0') = '' THEN 0::BIGINT
                 ELSE ltrim(x, '0')::BIGINT END
        )
    """)
    # ASCII digits only -> BIGINT; NULL for anything else or for more than 18 significant digits.
    # Used for F3 timestamps and for purchase prices (range checks on the value come after).
    con.execute("""
        CREATE OR REPLACE MACRO digits_bigint(x) AS (
            CASE WHEN x IS NOT NULL AND regexp_full_match(x, '^[0-9]+$') THEN ts_f3(x) END
        )
    """)
    # F1 / F2: x has already matched the shape regex, so the substr positions are safe.
    con.execute("""
        CREATE OR REPLACE MACRO ts_iso(x) AS (
            CASE
                WHEN substr(x, 12, 2)::INTEGER > 23 OR substr(x, 15, 2)::INTEGER > 59
                     OR substr(x, 18, 2)::INTEGER > 59 THEN NULL
                WHEN length(x) = 25 AND (substr(x, 21, 2)::INTEGER > 23 OR substr(x, 24, 2)::INTEGER > 59)
                    THEN NULL
                WHEN try_cast(substr(x, 1, 10) AS DATE) IS NULL THEN NULL
                ELSE (try_cast(substr(x, 1, 10) AS DATE) - DATE '1970-01-01')::BIGINT * 86400000
                     + substr(x, 12, 2)::BIGINT * 3600000
                     + substr(x, 15, 2)::BIGINT * 60000
                     + substr(x, 18, 2)::BIGINT * 1000
                     - CASE WHEN length(x) = 20 THEN 0
                            ELSE (CASE WHEN substr(x, 20, 1) = '-' THEN -1 ELSE 1 END)
                                 * (substr(x, 21, 2)::BIGINT * 3600000 + substr(x, 24, 2)::BIGINT * 60000)
                       END
            END
        )
    """)
    con.execute("""
        CREATE OR REPLACE MACRO ts_ms(x) AS (
            CASE
                WHEN x IS NULL THEN NULL
                WHEN regexp_full_match(x, '^[0-9]+$') THEN digits_bigint(x)
                WHEN regexp_full_match(x,
                    '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(Z|[+-][0-9]{2}:[0-9]{2})$')
                    THEN ts_iso(x)
                ELSE NULL
            END
        )
    """)


def iso_to_ms(text):
    """Manifest window strings (F1) to epoch ms."""
    dt = datetime.strptime(text, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)
    return int(dt.timestamp()) * 1000
