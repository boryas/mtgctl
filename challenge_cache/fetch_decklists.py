#!/usr/bin/env python3
"""Download the text decklist of every deck in cached Challenge pages since a date.

Usage: fetch_decklists.py [SINCE=YYYY-MM-DD]   (default: 28 days ago)
Lists land in decks/<goldfish deck id>.txt (main, blank line, sideboard); a
cached list is never re-fetched, since a published Challenge list is immutable.
"""
import datetime, glob, os, re, sys, time, urllib.request

CACHE_DIR = os.path.dirname(os.path.abspath(__file__))
DECK_DIR = os.path.join(CACHE_DIR, "decks")

def event_files(since):
    for path in sorted(glob.glob(os.path.join(CACHE_DIR, "legacy-challenge-32-*.html"))):
        m = re.search(r"(\d{4}-\d{2}-\d{2})(-1)?\.html$", path)
        if m and m.group(1) >= since:
            yield path

def main():
    since = sys.argv[1] if len(sys.argv) > 1 else (datetime.date.today() - datetime.timedelta(28)).isoformat()
    os.makedirs(DECK_DIR, exist_ok=True)
    ids = []
    for path in event_files(since):
        ids += re.findall(r'<a href="/deck/(\d+)">', open(path).read())
    todo = [i for i in dict.fromkeys(ids) if not os.path.exists(os.path.join(DECK_DIR, f"{i}.txt"))]
    print(f"{len(ids)} decks since {since}, {len(todo)} to fetch")
    for n, i in enumerate(todo, 1):
        req = urllib.request.Request(f"https://www.mtggoldfish.com/deck/download/{i}", headers={"User-Agent": "Mozilla/5.0"})
        try:
            with urllib.request.urlopen(req, timeout=15) as r:
                text = r.read().decode("utf-8", errors="replace")
        except Exception as e:
            print(f"  ERROR {i}: {e}")
            continue
        if not re.match(r"\d+ ", text):
            print(f"  not a decklist: {i}")
            continue
        with open(os.path.join(DECK_DIR, f"{i}.txt"), "w") as f:
            f.write(text)
        if n % 50 == 0:
            print(f"  {n}/{len(todo)}")
        time.sleep(0.5)

main()
