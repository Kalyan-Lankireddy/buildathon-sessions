"""Section 1: CSV parsing, done by hand.

DuckDB's CSV reader is not usable here. It ends a line at a bare `\\r` (even with
new_line='\\n' and strict_mode=false), repairs or rejects bad quotes its own way, and reads `""`
as an empty string instead of null. rules.md says only `\\n` ends a line and `\\r` is an ordinary
character, so we read whole files with read_text, split them on `\\n` ourselves, and parse fields
with explicit rules:

* the first line of every file is the header and is skipped;
* a line is split on `,`; a wrong field count drops the line;
* a field is valid when it has no `"` at all or is exactly `"..."` with no inner quote; any other
  `"` placement drops the whole line;
* wrapping quotes are removed; an empty field (quoted or not) is NULL;
* no trimming, no case folding, `\\r` is kept.
"""
import os


def register_macros(con):
    # A field is valid if it holds no quote, or is wrapped in exactly one pair of quotes.
    con.execute("""
        CREATE OR REPLACE MACRO csv_field_ok(x) AS
            (NOT contains(x, '"')) OR regexp_full_match(x, '^"[^"]*"$')
    """)
    # Strip the wrapping quotes (only call on a valid field); empty becomes NULL.
    con.execute("""
        CREATE OR REPLACE MACRO csv_field(x) AS
            NULLIF(CASE WHEN starts_with(x, '"') THEN substr(x, 2, length(x) - 2) ELSE x END, '')
    """)


def csv_files(folder):
    """Every *.csv file directly in `folder`, sorted. A missing folder holds no files."""
    if not os.path.isdir(folder):
        return []
    return sorted(
        os.path.join(folder, name)
        for name in os.listdir(folder)
        if name.endswith(".csv") and os.path.isfile(os.path.join(folder, name))
    )


def parse_csv(con, table, files, columns):
    """Create TABLE `table` with one VARCHAR column per name in `columns`.

    Malformed lines are dropped. Empty fields are NULL. Nothing is trimmed or normalized.
    """
    register_macros(con)
    n = len(columns)
    if not files:
        con.execute(f"CREATE OR REPLACE TABLE {table} ({', '.join(c + ' VARCHAR' for c in columns)})")
        return
    cols = ", ".join(f"csv_field(f[{i + 1}]) AS {c}" for i, c in enumerate(columns))
    con.execute(
        f"""
        CREATE OR REPLACE TABLE {table} AS
        SELECT {cols}
        FROM (
            SELECT string_split(line, ',') AS f
            FROM (
                -- [2:] skips the header, the first line of every file
                SELECT unnest(string_split(content, chr(10))[2:]) AS line FROM read_text(?)
            )
        )
        WHERE len(f) = {n} AND list_bool_and(list_transform(f, x -> csv_field_ok(x)))
        """,
        [files],
    )
