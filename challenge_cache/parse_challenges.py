#!/usr/bin/env python3
"""Parse cached Legacy Challenge HTML files and produce a CSV of top 8 results."""

import os
import re
import csv
import glob

CACHE_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT = os.path.join(os.path.dirname(CACHE_DIR), "legacy_challenge_top8s.csv")

POSITIONS = {"1st", "2nd", "3rd", "4th", "5th", "6th", "7th", "8th"}

def get_standings_key(html):
    rows = re.findall(
        r"tournament-decklist-(?:odd|event)[^>]*>.*?<td[^>]*>\s*(\w+)\s*</td>.*?<a href=\"/deck/\d+\">([^<]+)</a>.*?<a href=\"/player/([^\"]+)\"",
        html, re.DOTALL
    )
    return tuple((pos, deck, player) for pos, deck, player in rows if pos in POSITIONS)

def is_dupe(path):
    """Return True if this (1) file has identical standings to its primary counterpart."""
    if not path.endswith("-1.html"):
        return False
    primary = path.replace("-1.html", ".html")
    if not os.path.exists(primary):
        return False
    with open(path) as f:
        h1 = f.read()
    with open(primary) as f:
        h2 = f.read()
    return get_standings_key(h1) == get_standings_key(h2)

def parse_file(path):
    slug = os.path.basename(path).replace(".html", "")
    # Extract date from slug: legacy-challenge-32-YYYY-MM-DD[-1]
    m = re.search(r"(\d{4}-\d{2}-\d{2})(-1)?$", slug)
    if not m:
        return []
    date = m.group(1)
    variant = " (1)" if m.group(2) else ""
    tournament = f"Legacy Challenge 32 {date}{variant}"
    tournament_url = f"https://www.mtggoldfish.com/tournament/{slug}"

    with open(path) as f:
        html = f.read()

    # Find all tournament-decklist rows (both odd and event variants)
    # Each row contains: position, deck link, player link
    row_pattern = re.compile(
        r"tournament-decklist-(?:odd|event)[^>]*>.*?"
        r"<td[^>]*>\s*([\w]+)\s*</td>.*?"          # position (1st, 2nd, ...)
        r'<a href="(/deck/\d+)">([^<]+)</a>.*?'    # deck link + name
        r'<a href="/player/([^"]+)"',               # player
        re.DOTALL,
    )

    results = []
    for m in row_pattern.finditer(html):
        position, deck_path, deck_name, player = m.groups()
        if position not in POSITIONS:
            continue
        decklist_url = f"https://www.mtggoldfish.com{deck_path}"
        results.append({
            "date": date,
            "tournament": tournament,
            "tournament_url": tournament_url,
            "position": position,
            "player": player,
            "deck": deck_name.strip(),
            "decklist_url": decklist_url,
        })

    return results

all_results = []
files = sorted(glob.glob(os.path.join(CACHE_DIR, "legacy-challenge-32-*.html")))
print(f"Parsing {len(files)} files...")

for path in files:
    if is_dupe(path):
        print(f"  SKIPPING DUPE: {os.path.basename(path)}")
        continue
    rows = parse_file(path)
    if not rows:
        print(f"  WARNING: no results parsed from {os.path.basename(path)}")
    all_results.extend(rows)

# Sort by date descending, then position
pos_order = {"1st": 1, "2nd": 2, "3rd": 3, "4th": 4, "5th": 5, "6th": 6, "7th": 7, "8th": 8}
all_results.sort(key=lambda r: (-int(r["date"].replace("-", "")), pos_order.get(r["position"], 9)))

with open(OUTPUT, "w", newline="") as f:
    writer = csv.DictWriter(f, fieldnames=["date", "tournament", "tournament_url", "position", "player", "deck", "decklist_url"])
    writer.writeheader()
    writer.writerows(all_results)

print(f"\nWrote {len(all_results)} rows to {OUTPUT}")
print(f"Events covered: {len(files)}")
print(f"Unique players: {len(set(r['player'] for r in all_results))}")
print(f"Unique decks: {len(set(r['deck'] for r in all_results))}")
