#!/usr/bin/env python3
"""Download Legacy Challenge 32 pages from SINCE (YYYY-MM-DD, default 2025-09-01) through today."""

import os
import sys
import time
import datetime
import urllib.request

CACHE_DIR = os.path.dirname(os.path.abspath(__file__))
START_DATE = datetime.date.fromisoformat(sys.argv[1]) if len(sys.argv) > 1 else datetime.date(2025, 9, 1)
END_DATE = datetime.date.today()
DELAY = 0.5  # seconds between requests

def date_range(start, end):
    d = start
    while d <= end:
        yield d
        d += datetime.timedelta(days=1)

def download(url, path):
    req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0"})
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            html = resp.read().decode("utf-8", errors="replace")
        with open(path, "w") as f:
            f.write(html)
        return html
    except Exception as e:
        print(f"  ERROR: {e}")
        return None

def is_valid_tournament(html):
    """Check if the page has actual tournament data (not a redirect/404)."""
    return html and "Legacy Challenge 32" in html and "tournament-decklist" in html

skipped = 0
downloaded = 0
already_cached = 0

for d in date_range(START_DATE, END_DATE):
    # Challenges run on Wednesday (2), Friday (4), Saturday (5), Sunday (6)
    if d.weekday() not in (2, 4, 5, 6):
        continue

    for suffix in ("", "-1"):
        slug = f"legacy-challenge-32-{d.isoformat()}{suffix}"
        fname = os.path.join(CACHE_DIR, f"{slug}.html")

        if os.path.exists(fname):
            already_cached += 1
            continue

        url = f"https://www.mtggoldfish.com/tournament/{slug}"
        print(f"Fetching {url}")
        html = download(url, fname + ".tmp")
        time.sleep(DELAY)

        if is_valid_tournament(html):
            os.rename(fname + ".tmp", fname)
            print(f"  -> saved ({len(html)} bytes)")
            downloaded += 1
        else:
            if os.path.exists(fname + ".tmp"):
                os.remove(fname + ".tmp")
            print(f"  -> no data, skipped")
            skipped += 1

print(f"\nDone. Downloaded: {downloaded}, Already cached: {already_cached}, Skipped (no data): {skipped}")
