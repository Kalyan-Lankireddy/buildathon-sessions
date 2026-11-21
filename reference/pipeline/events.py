"""Section 4: events, strictly in rules.md order.

  1. parse; drop malformed lines and an empty event_id          -> ev_raw
  2. dedup on event_id, keep the smallest ingest_ts             -> ev_dedup   (BEFORE validation)
  3. validate the kept copy                                     -> ev_valid
  4. drop late events (ingest_ts - ts over 24 h)                -> ev_valid
  5. map user_id to person, sessionize per person               -> ev_sess
  6. day of a session = UTC date of its first event             -> sessions
  7. category from products.csv, `unknown` when missing         -> ev_sess.category

Time values are epoch ms. `ts_ms` gives NULL for bad / unusable timestamps, so a comparison with
the window drops them.
"""
from csvlines import parse_csv
from timestamps import LATE_MS, SESSION_GAP_MS, register_macros

EVENT_COLUMNS = ["event_id", "user_id", "ts_raw", "ingest_raw", "event_type", "product_id", "price_raw"]
MAX_PRICE = 10_000_000


def load_events(con, files, window_start_ms, window_end_ms):
    """Steps 1 to 4. Creates ev_raw, ev_dedup and ev_valid."""
    register_macros(con)
    parse_csv(con, "ev_raw", files, EVENT_COLUMNS)

    # 2. Dedup on event_id, keeping the copy with the smallest ingest_ts. This happens before any
    # validation, so an invalid kept copy drops the whole event; a later valid copy never rescues it.
    # Copies with equal ingest_ts are identical, so which one survives does not matter.
    con.execute("""
        CREATE OR REPLACE TABLE ev_dedup AS
        SELECT event_id, user_id, ts_raw, ingest_ms, event_type, product_id, price_raw
        FROM (
            SELECT *, row_number() OVER (PARTITION BY event_id ORDER BY ingest_ms) AS rn
            FROM (SELECT *, ts_ms(ingest_raw) AS ingest_ms FROM ev_raw WHERE event_id IS NOT NULL)
        )
        WHERE rn = 1
    """)

    # 3 + 4. Validate the kept copy, then drop late events.
    con.execute(
        f"""
        CREATE OR REPLACE TABLE ev_valid AS
        SELECT event_id, user_id, ts, ingest_ms, event_type, product_id, price
        FROM (
            SELECT event_id, user_id, ts_ms(ts_raw) AS ts, ingest_ms, event_type, product_id,
                   digits_bigint(price_raw) AS price
            FROM ev_dedup
        )
        WHERE user_id IS NOT NULL
          AND ts >= {window_start_ms} AND ts < {window_end_ms}
          AND event_type IN ('view', 'cart', 'purchase')
          AND product_id IS NOT NULL
          -- price matters for purchases only; for view/cart it is ignored, garbage included
          AND (event_type <> 'purchase' OR price BETWEEN 1 AND {MAX_PRICE})
          -- exactly 24 h is kept, and so is an ingest_ts before ts
          AND ingest_ms - ts <= {LATE_MS}
        """
    )


def sessionize(con):
    """Steps 5 to 7. Needs person_map, products and ev_valid. Creates ev_sess and sessions."""
    con.execute("""
        CREATE OR REPLACE TABLE ev_person AS
        SELECT e.event_id, coalesce(m.person, e.user_id) AS person, e.ts, e.event_type,
               CASE WHEN p.product_id IS NULL THEN 'unknown' ELSE p.category END AS category,
               e.price
        FROM ev_valid e
        LEFT JOIN person_map m ON e.user_id = m.id
        LEFT JOIN products p ON e.product_id = p.product_id
    """)
    # A new session starts when the gap to the previous event is over 30 min. Ordering by
    # (ts, event_id) makes equal timestamps follow each other: the second has gap 0, so every
    # event sharing a ts joins the session the first of them joins.
    con.execute(
        f"""
        CREATE OR REPLACE TABLE ev_sess AS
        SELECT person, sess_no, ts, event_type, category, price,
               min(ts) OVER (PARTITION BY person, sess_no) AS sess_start
        FROM (
            SELECT *, sum(is_new) OVER (PARTITION BY person ORDER BY ts, event_id
                                        ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) AS sess_no
            FROM (
                SELECT *, CASE WHEN lag(ts) OVER w IS NULL OR ts - lag(ts) OVER w > {SESSION_GAP_MS}
                               THEN 1 ELSE 0 END AS is_new
                FROM ev_person
                WINDOW w AS (PARTITION BY person ORDER BY ts, event_id)
            )
        )
        """
    )
    con.execute("""
        CREATE OR REPLACE TABLE sessions AS
        SELECT person, sess_no, sess_start AS start, CAST(epoch_ms(sess_start) AS DATE) AS day,
               count(*) AS events,
               count(*) FILTER (WHERE event_type = 'purchase') AS purchases,
               coalesce(sum(price) FILTER (WHERE event_type = 'purchase'), 0) AS revenue
        FROM ev_sess
        GROUP BY person, sess_no, sess_start
    """)
