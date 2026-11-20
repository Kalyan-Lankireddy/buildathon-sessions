"""Section 5: clicks and attribution.

  1. parse; drop malformed lines and an empty click_id                   -> ck_raw
  2. dedup on click_id (copies are identical after quote stripping)      -> ck_dedup
  3. validate: user non-empty, ts in the CLICK window, campaign in campaigns.csv -> ck_valid
  4. map user_id to person                                               -> ck_valid.person
  5. credit each session to the person's latest click with
        session_start - 7 d <= click.ts <= session_start,  ties -> smallest click_id
  6. no qualifying click -> campaign 'organic'
  7. assists: every campaign with at least one such click assists the session once

Correctness first: assists use the plain range join. It is exact and obviously right, but a whale
person with many sessions and clicks can make it large. Speeding that up is a later step.
"""
from csvlines import parse_csv
from timestamps import WEEK_MS, register_macros

CLICK_COLUMNS = ["click_id", "user_id", "campaign_id", "ts_raw"]
ORGANIC = "organic"


def load_clicks(con, files, click_start_ms, window_end_ms):
    """Steps 1 to 4. Needs campaigns and person_map. Creates ck_raw, ck_dedup, ck_valid."""
    register_macros(con)
    parse_csv(con, "ck_raw", files, CLICK_COLUMNS)

    con.execute("""
        CREATE OR REPLACE TABLE ck_dedup AS
        SELECT click_id, user_id, campaign_id, ts_raw
        FROM (
            SELECT *, row_number() OVER (PARTITION BY click_id ORDER BY user_id, campaign_id, ts_raw) AS rn
            FROM ck_raw WHERE click_id IS NOT NULL
        )
        WHERE rn = 1
    """)

    con.execute(
        f"""
        CREATE OR REPLACE TABLE ck_valid AS
        SELECT c.click_id, coalesce(m.person, c.user_id) AS person, c.campaign_id, c.ts
        FROM (SELECT click_id, user_id, campaign_id, ts_ms(ts_raw) AS ts FROM ck_dedup) c
        JOIN campaigns k ON c.campaign_id = k.campaign_id
        LEFT JOIN person_map m ON c.user_id = m.id
        WHERE c.user_id IS NOT NULL
          AND c.ts >= {click_start_ms} AND c.ts < {window_end_ms}
        """
    )


def attribute(con):
    """Steps 5 to 7. Needs sessions and ck_valid. Creates sess_credit and assists."""
    # Several clicks at the same ms: only the smallest click_id can win, so keep just that one.
    con.execute("""
        CREATE OR REPLACE TABLE ck_latest AS
        SELECT person, ts, min(click_id) AS click_id, arg_min(campaign_id, click_id) AS campaign_id
        FROM ck_valid
        GROUP BY person, ts
    """)
    # ASOF picks, per session, the person's click with the largest ts <= session start. If that
    # one is more than 7 days old, every other click is older still, so the session is organic.
    con.execute(f"""
        CREATE OR REPLACE TABLE sess_credit AS
        SELECT s.person, s.sess_no, s.start, s.day, s.events, s.purchases, s.revenue,
               CASE WHEN c.ts IS NOT NULL AND s.start - c.ts <= {WEEK_MS}
                    THEN c.campaign_id ELSE '{ORGANIC}' END AS campaign_id
        FROM sessions s
        ASOF LEFT JOIN ck_latest c ON s.person = c.person AND s.start >= c.ts
    """)
    con.execute(f"""
        CREATE OR REPLACE TABLE assists AS
        SELECT DISTINCT s.person, s.sess_no, s.day, c.campaign_id
        FROM sessions s
        JOIN (SELECT DISTINCT person, campaign_id, ts FROM ck_valid) c
          ON s.person = c.person AND c.ts <= s.start AND c.ts >= s.start - {WEEK_MS}
    """)
