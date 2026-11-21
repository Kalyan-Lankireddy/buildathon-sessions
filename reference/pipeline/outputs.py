"""Section 6: the two output tables, written as Parquet.

user_daily      one row per (person, day) with at least one session
campaign_daily  one row per (campaign_id, day) with at least one session credited OR assisted;
                a campaign that only assisted that day has 0 in the three credited columns
"""
import os


def build_outputs(con):
    """Create user_daily and campaign_daily from sessions, ev_sess, sess_credit and assists."""
    # Highest summed purchase price per (person, day); a tie goes to the byte-wise smallest category
    # (DuckDB's default VARCHAR order). `unknown` competes like any other category. No purchase,
    # no row here, so top_category is NULL.
    con.execute("""
        CREATE OR REPLACE TABLE top_cat AS
        SELECT person, day, category AS top_category
        FROM (
            SELECT person, day, category,
                   row_number() OVER (PARTITION BY person, day ORDER BY total DESC, category ASC) AS rn
            FROM (
                SELECT person, CAST(epoch_ms(sess_start) AS DATE) AS day, category, sum(price) AS total
                FROM ev_sess
                WHERE event_type = 'purchase'
                GROUP BY person, CAST(epoch_ms(sess_start) AS DATE), category
            )
        )
        WHERE rn = 1
    """)
    con.execute("""
        CREATE OR REPLACE TABLE user_daily AS
        SELECT s.person AS user_id, s.day AS day,
               count(*)::BIGINT AS sessions,
               sum(s.events)::BIGINT AS events,
               sum(s.revenue)::BIGINT AS revenue_cents,
               any_value(t.top_category) AS top_category
        FROM sessions s
        LEFT JOIN top_cat t ON s.person = t.person AND s.day = t.day
        GROUP BY s.person, s.day
    """)
    con.execute("""
        CREATE OR REPLACE TABLE campaign_daily AS
        SELECT coalesce(c.campaign_id, a.campaign_id) AS campaign_id,
               CASE WHEN coalesce(c.campaign_id, a.campaign_id) = 'organic' THEN 'organic'
                    ELSE k.channel END AS channel,
               coalesce(c.day, a.day) AS day,
               coalesce(c.sessions, 0)::BIGINT AS sessions,
               coalesce(c.purchasing_sessions, 0)::BIGINT AS purchasing_sessions,
               coalesce(c.revenue_cents, 0)::BIGINT AS revenue_cents,
               coalesce(a.assisted_sessions, 0)::BIGINT AS assisted_sessions
        FROM (
            SELECT campaign_id, day, count(*) AS sessions,
                   count(*) FILTER (WHERE purchases > 0) AS purchasing_sessions,
                   sum(revenue) AS revenue_cents
            FROM sess_credit GROUP BY campaign_id, day
        ) c
        FULL OUTER JOIN (
            SELECT campaign_id, day, count(*) AS assisted_sessions FROM assists GROUP BY campaign_id, day
        ) a ON c.campaign_id = a.campaign_id AND c.day = a.day
        LEFT JOIN campaigns k ON coalesce(c.campaign_id, a.campaign_id) = k.campaign_id
    """)


def write_outputs(con, out_dir):
    for table in ("user_daily", "campaign_daily"):
        folder = os.path.join(out_dir, table)
        os.makedirs(folder, exist_ok=True)
        path = os.path.join(folder, "part-0.parquet")
        con.execute(f"COPY {table} TO '{path}' (FORMAT parquet)")


def self_check(con):
    """The rules' own sanity checks. Returns a list of failure messages (empty when fine)."""
    problems = []
    u = con.execute("SELECT coalesce(sum(sessions), 0), coalesce(sum(revenue_cents), 0) FROM user_daily").fetchone()
    c = con.execute("SELECT coalesce(sum(sessions), 0), coalesce(sum(revenue_cents), 0), "
                    "coalesce(sum(assisted_sessions), 0), "
                    "coalesce(sum(sessions) FILTER (WHERE campaign_id <> 'organic'), 0) FROM campaign_daily").fetchone()
    if tuple(u) != (c[0], c[1]):
        problems.append(f"sessions/revenue differ: user_daily {tuple(u)} vs campaign_daily {(c[0], c[1])}")
    if c[2] < c[3]:
        problems.append(f"assisted_sessions {c[2]} < credited non-organic sessions {c[3]}")
    return problems
