import os
import sys

import duckdb
import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "pipeline"))


@pytest.fixture
def con():
    c = duckdb.connect()
    yield c
    c.close()


def write_files(folder, files):
    """files: {name: text}. Text is written byte-exact (no newline translation)."""
    os.makedirs(folder, exist_ok=True)
    paths = []
    for name, text in files.items():
        path = os.path.join(folder, name)
        with open(path, "w", encoding="utf-8", newline="") as f:
            f.write(text)
        paths.append(path)
    return sorted(paths)


# ---------------------------------------------------------------------------------------------
# Tiny dataset builder. Rows are raw CSV lines, so tests can include quotes, CR, spaces, etc.
# ---------------------------------------------------------------------------------------------
import json  # noqa: E402

DEFAULT_PRODUCTS = ["p1,shoes", "p2,books"]
DEFAULT_CAMPAIGNS = ["c_1,search", "c_2,social"]
DEFAULT_WINDOW = ("2026-10-01T00:00:00Z", "2026-10-15T00:00:00Z")
EVENT_HEADER = "event_id,user_id,ts,ingest_ts,event_type,product_id,price_cents"
CLICK_HEADER = "click_id,user_id,campaign_id,ts"
LINK_HEADER = "user_id_a,user_id_b"


def _files(spec, default_name="part-00000.csv"):
    if spec is None:
        return {}
    if isinstance(spec, dict):
        return spec
    return {default_name: list(spec)}


def make_data(root, events=(), clicks=(), links=(), products=DEFAULT_PRODUCTS,
              campaigns=DEFAULT_CAMPAIGNS, window=DEFAULT_WINDOW):
    """Write a /data layout under `root` and return its path. `events`, `clicks`, `links` are
    lists of raw lines (one file) or {filename: [lines]} (header added to each file)."""
    root = str(root)
    for folder, header, spec in (("events", EVENT_HEADER, events), ("clicks", CLICK_HEADER, clicks),
                                 ("links", LINK_HEADER, links)):
        files = _files(spec)
        if not files:
            files = {"part-00000.csv": []}
        write_files(os.path.join(root, folder),
                    {name: "\n".join([header, *lines]) + "\n" for name, lines in files.items()})
    write_files(root, {
        "products.csv": "\n".join(["product_id,category", *products]) + "\n",
        "campaigns.csv": "\n".join(["campaign_id,channel", *campaigns]) + "\n",
        "manifest.json": json.dumps({"window_start": window[0], "window_end": window[1]}),
    })
    return root


def ev(event_id, user, ts, ingest=None, kind="view", product="p1", price=""):
    """One event line. `ingest` defaults to the window start, which is never late for an in-window ts."""
    return f"{event_id},{user},{ts},{ingest or '2026-10-01T00:00:00Z'},{kind},{product},{price}"
