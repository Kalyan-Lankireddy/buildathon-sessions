"""Small inputs: manifest.json window, products.csv, campaigns.csv."""
import json
import os

from csvlines import parse_csv
from timestamps import WEEK_MS, iso_to_ms


def read_window(data_dir):
    """(window_start_ms, window_end_ms) from manifest.json. Different for every dataset."""
    with open(os.path.join(data_dir, "manifest.json")) as f:
        manifest = json.load(f)
    return iso_to_ms(manifest["window_start"]), iso_to_ms(manifest["window_end"])


def click_window(window_start_ms, window_end_ms):
    """A click may be up to 7 days before the event window and still credit a session in it."""
    return window_start_ms - WEEK_MS, window_end_ms


def load_dims(con, data_dir):
    """Create products(product_id, category) and campaigns(campaign_id, channel).

    Both files are documented as clean, but they go through the same parser as everything else so
    one dialect rule covers every file.
    """
    parse_csv(con, "products", [os.path.join(data_dir, "products.csv")], ["product_id", "category"])
    parse_csv(con, "campaigns", [os.path.join(data_dir, "campaigns.csv")], ["campaign_id", "channel"])
