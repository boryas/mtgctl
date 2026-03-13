# legacy-challenge-stats

Fetch and cache any missing MTGO Legacy Challenge results, then print a breakdown by deck archetype.

## Invocation

`/legacy-challenge-stats [time window]`

`$ARGUMENTS` is an optional English time window description. Examples:
- (empty) — default: since the latest cached event, up to 6 months back
- `last 2 weeks`
- `since october`
- `last 6 months`
- `2025-11-01 to 2025-12-31`

---

## Step 1 — Determine the date range

The cache directory is `/home/bo/repos/mtgctl/challenge_cache/`.
The parse script is `/home/bo/repos/mtgctl/challenge_cache/parse_challenges.py`.
The download script is `/home/bo/repos/mtgctl/challenge_cache/download_challenges.py`.
The output CSV is `/home/bo/repos/mtgctl/legacy_challenge_top8s.csv`.

Determine `START_DATE` and `END_DATE` (inclusive, as `datetime.date` values):

- `END_DATE` is always today.
- If `$ARGUMENTS` is empty or not provided:
  - Find the most recently dated cached HTML file matching `legacy-challenge-32-*.html`.
  - `START_DATE` = that date (so only new events are fetched).
  - Cap: if that date is more than 6 months ago, use 6 months ago instead.
- If `$ARGUMENTS` is provided, parse it as a natural-language window:
  - "last N weeks/days/months" → `START_DATE` = today minus that duration
  - "since <month>" → `START_DATE` = first of that month in the most recent applicable year
  - "YYYY-MM-DD to YYYY-MM-DD" → use literally

Use Python to compute this. Print the resolved range before proceeding.

---

## Step 2 — Download missing events

Run the downloader inline with Python rather than re-running the full download script (which would re-check all already-cached dates). Use this logic:

```python
import os, re, time, datetime, urllib.request

CACHE_DIR = "/home/bo/repos/mtgctl/challenge_cache"
DELAY = 0.5

def download(url, path):
    req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0"})
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            html = resp.read().decode("utf-8", errors="replace")
        with open(path, "w") as f:
            f.write(html)
        return html
    except Exception as e:
        print(f"  ERROR {url}: {e}")
        return None

def is_valid(html):
    return html and "Legacy Challenge 32" in html and "tournament-decklist" in html

# Iterate over dates in range, Wed/Fri/Sat/Sun only, try primary and -1 variants
for d in date_range(START_DATE, END_DATE):
    if d.weekday() not in (2, 4, 5, 6):
        continue
    for suffix in ("", "-1"):
        slug = f"legacy-challenge-32-{d.isoformat()}{suffix}"
        fname = os.path.join(CACHE_DIR, f"{slug}.html")
        if os.path.exists(fname):
            continue
        url = f"https://www.mtggoldfish.com/tournament/{slug}"
        print(f"Fetching {url}")
        html = download(url, fname + ".tmp")
        time.sleep(DELAY)
        if is_valid(html):
            os.rename(fname + ".tmp", fname)
            print(f"  -> saved")
        else:
            if os.path.exists(fname + ".tmp"):
                os.remove(fname + ".tmp")
```

Report how many new events were downloaded.

---

## Step 3 — Re-parse

Run the parse script to regenerate the CSV:

```bash
cd /home/bo/repos/mtgctl/challenge_cache && python3 parse_challenges.py
```

---

## Step 4 — Print deck breakdown

Run this Python snippet against the CSV and print the results:

```python
import csv
from collections import defaultdict

pos_order = {"1st":1,"2nd":2,"3rd":3,"4th":4,"5th":5,"6th":6,"7th":7,"8th":8}

def score(pos):
    p = pos_order[pos]
    if p <= 2: return 10
    if p <= 4: return 9
    return 8

def summarize(positions):
    pts  = sum(score(p) for p in positions)
    t8s  = len(positions)
    t8   = sum(1 for p in positions if pos_order[p] >= 5)  # 5th-8th only
    t4   = sum(1 for p in positions if pos_order[p] in (3, 4))
    t2   = positions.count("2nd")
    wins = positions.count("1st")
    return pts, t8s, t8, t4, t2, wins

decks = defaultdict(list)
players = defaultdict(list)
with open("/home/bo/repos/mtgctl/legacy_challenge_top8s.csv") as f:
    for row in csv.DictReader(f):
        # filter to date range before appending
        decks[row["deck"]].append(row["position"])
        players[row["player"]].append(row["position"])

deck_summary   = sorted([(summarize(pos), deck)   for deck,   pos in decks.items()],   reverse=True)
player_summary = sorted([(summarize(pos), player) for player, pos in players.items()], reverse=True)

hdr = f"{'Deck':<35} {'T8s':>4} {'T8':>4} {'T4':>4} {'T2':>4} {'W':>4} {'Score':>7}"
print(hdr)
print("-" * len(hdr))
for (pts, t8s, t8, t4, t2, wins), deck in deck_summary:
    print(f"{deck:<35} {t8s:>4} {t8:>4} {t4:>4} {t2:>4} {wins:>4} {pts:>7}")

print()

hdr2 = f"{'Player':<25} {'T8s':>4} {'T8':>4} {'T4':>4} {'T2':>4} {'W':>4} {'Score':>7}"
print(hdr2)
print("-" * len(hdr2))
for (pts, t8s, t8, t4, t2, wins), player in player_summary[:10]:
    print(f"{player:<25} {t8s:>4} {t8:>4} {t4:>4} {t2:>4} {wins:>4} {pts:>7}")
```

Columns: **T8s** = total top 8 appearances; **T8/T4/T2/W** = exclusive buckets (5th–8th, 3rd–4th, 2nd, 1st) that sum to T8s. Score: 1st/2nd=10, top4=9, top8=8.

Also print a summary line: total events in the CSV, date range covered, and total unique players.
