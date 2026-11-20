"""Section 3: linked IDs -> person name.

Links are parsed with the shared CSV rules, lines with an empty side are dropped, and the rest are
unioned transitively with scipy's connected components (a recursive CTE is too slow on chains of
hundreds of links and shared-device components of thousands of people).

A person's name is the byte-wise smallest of their IDs. DuckDB compares VARCHAR with its default
binary collation (UTF-8 byte order, upper case before lower case), so we number the linked IDs in
that order; the smallest ID of a component is then its first member by number.

Result: TABLE person_map(id VARCHAR, person VARCHAR), one row per ID that appears in a valid link
line. An ID missing from it is a person on its own, named by itself: use
`COALESCE(m.person, x.user_id)` after a LEFT JOIN.
"""
import numpy as np
from scipy.sparse import coo_matrix
from scipy.sparse.csgraph import connected_components

from csvlines import parse_csv


def build_person_map(con, link_files):
    parse_csv(con, "links_raw", link_files, ["id_a", "id_b"])
    # Self-links and repeats (either way round) change nothing.
    con.execute("""
        CREATE OR REPLACE TABLE link_edges AS
        SELECT DISTINCT least(id_a, id_b) AS a, greatest(id_a, id_b) AS b
        FROM links_raw
        WHERE id_a IS NOT NULL AND id_b IS NOT NULL AND id_a <> id_b
    """)
    con.execute("""
        CREATE OR REPLACE TABLE link_ids AS
        SELECT id, (row_number() OVER (ORDER BY id) - 1)::INTEGER AS idx
        FROM (SELECT a AS id FROM link_edges UNION SELECT b FROM link_edges)
    """)
    n = con.execute("SELECT count(*) FROM link_ids").fetchone()[0]
    if n == 0:
        con.execute("CREATE OR REPLACE TABLE person_map (id VARCHAR, person VARCHAR)")
        return 0
    edges = con.execute("""
        SELECT x.idx, y.idx
        FROM link_edges e JOIN link_ids x ON e.a = x.id JOIN link_ids y ON e.b = y.id
    """).fetchnumpy()
    ia, ib = edges[list(edges)[0]], edges[list(edges)[1]]
    graph = coo_matrix((np.ones(len(ia), dtype=np.int8), (ia, ib)), shape=(n, n))
    _, labels = connected_components(graph, directed=False)
    # ids are numbered in byte order, so the first member of a component is its smallest ID.
    _, first, inverse = np.unique(labels, return_index=True, return_inverse=True)
    # The dict must not share a name with a table: an existing table would shadow the dict in the
    # replacement scan and silently read stale data.
    rep_arrays = {"idx": np.arange(n, dtype=np.int32), "rep": first[inverse].astype(np.int32)}
    con.execute("CREATE OR REPLACE TABLE link_rep AS SELECT * FROM rep_arrays")
    con.execute("""
        CREATE OR REPLACE TABLE person_map AS
        SELECT i.id AS id, r.id AS person
        FROM link_rep m JOIN link_ids i ON m.idx = i.idx JOIN link_ids r ON m.rep = r.idx
    """)
    return n
