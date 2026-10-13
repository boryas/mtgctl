#!/usr/bin/env python3
"""Build prep/meta.json for the EW prep page from cached Challenge top-32 lists.

Usage: build_meta.py [DAYS=28]
Reads ../challenge_cache/legacy-challenge-32-*.html (+ decks/<id>.txt from
fetch_decklists.py) for the last DAYS days. Card text comes from Scryfall,
cached in card_cache.json so a refresh only asks for new cards.
"""
import collections, datetime, glob, json, os, re, sys, time, urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
CACHE = os.path.join(HERE, "..", "challenge_cache")
CARD_CACHE = os.path.join(HERE, "card_cache.json")
MY_LISTS = os.path.join(HERE, "my_lists.json")

# Goldfish names merged into one deck, or renamed. Card rules below win over this.
RENAME = {
    "Death and Taxes (Yorion) - Black/White": "BW Yorion",
    "Beanstalk Control (Non-Yorion)": "Beanstalk Control",
    "Izzet Delver": "Izzet Tempo",
    "Doomsday": "Doomsday (mirror)",
}
# (deck name, predicate over the main deck {card: qty}, goldfish name); first match wins.
# MTGGoldfish names many lists only by colour ("WU", "U", "WG"), so these key on signature cards.
CARD_RULES = [
    ("Cloudpost", lambda m, g: "Cloudpost" in m),
    ("Blue Tron", lambda m, g: g in ("Tron", "U", "UB") and "Urza's Tower" in m and "Force of Will" in m),
    ("Colorless Tron", lambda m, g: g in ("Tron", "U", "UB") and "Urza's Tower" in m),
    ("Elves", lambda m, g: "Gaea's Cradle" in m and ("Elvish Visionary" in m or "Heritage Druid" in m)),
    ("Cradle Druid", lambda m, g: "Gaea's Cradle" in m and "Devoted Druid" in m),
    ("Cradle Control", lambda m, g: "Gaea's Cradle" in m),
    ("Beanstalk Control", lambda m, g: "Up the Beanstalk" in m),
    ("Azorius Tempo", lambda m, g: g in ("WU", "Azorius Tempo") and "Brainstorm" in m),
    ("Jeskai Tempo", lambda m, g: g == "WUR" and "Brainstorm" in m),
]
# Decks the plan prepares for in depth (EW plan, 2026-10-05).
FOCUS = {"Dimir Tempo", "Izzet Tempo", "Jeskai Tempo", "Azorius Tempo",
         "Beanstalk Control", "Lands", "Cradle Control", "Blue Tron"}

def parse_list(text):
    main, side, cur = {}, {}, None
    cur = main
    for line in text.splitlines():
        line = line.strip()
        if not line:
            if main:
                cur = side
            continue
        m = re.match(r"(\d+)\s+(.+)", line)
        if m:
            cur[m.group(2)] = cur.get(m.group(2), 0) + int(m.group(1))
    return main, side

def deck_name(goldfish, main):
    for name, pred in CARD_RULES:
        if pred(main, goldfish):
            return name
    return RENAME.get(goldfish, goldfish)

def moved(p1, p2, n1, n2):
    """True when a card's share of lists differs between the halves by ≥2 standard errors (pooled two-proportion z)."""
    if p1 is None or p2 is None or n1 < 4 or n2 < 4:
        return False
    p = (p1 * n1 + p2 * n2) / (n1 + n2)
    se = (p * (1 - p) * (1 / n1 + 1 / n2)) ** 0.5
    return se > 0 and abs(p2 - p1) / se >= 2

def events(since):
    row = re.compile(r"tournament-decklist-(?:odd|event)[^>]*>.*?<td[^>]*>\s*(\w+)\s*</td>.*?"
                     r'<a href="/deck/(\d+)">([^<]+)</a>.*?<a href="/player/([^"]+)"', re.S)
    for path in sorted(glob.glob(os.path.join(CACHE, "legacy-challenge-32-*.html"))):
        m = re.search(r"(\d{4}-\d{2}-\d{2})(-1)?\.html$", path)
        if not m or m.group(1) < since:
            continue
        rows = row.findall(open(path).read())
        if rows:
            yield m.group(1) + (m.group(2) or ""), rows

def scryfall(names):
    cache = json.load(open(CARD_CACHE)) if os.path.exists(CARD_CACHE) else {}
    todo = [n for n in names if n not in cache]
    for i in range(0, len(todo), 75):
        body = json.dumps({"identifiers": [{"name": n.split(" // ")[0]} for n in todo[i:i + 75]]}).encode()
        req = urllib.request.Request("https://api.scryfall.com/cards/collection", body,
                                     {"Content-Type": "application/json", "User-Agent": "mtgctl-prep/0.1", "Accept": "application/json"})
        data = json.load(urllib.request.urlopen(req, timeout=30))
        found = {}
        for c in data["data"]:
            faces = c.get("card_faces") or [c]
            found[c["name"]] = found[c["name"].split(" // ")[0]] = {
                "cost": c.get("mana_cost") or faces[0].get("mana_cost", ""),
                "type": c.get("type_line", ""),
                "text": "\n//\n".join(f.get("oracle_text", "") for f in faces),
            }
        for n in todo[i:i + 75]:
            cache[n] = found.get(n) or found.get(n.split(" // ")[0]) or {"cost": "", "type": "", "text": ""}
        time.sleep(0.1)
    json.dump(cache, open(CARD_CACHE, "w"), indent=0, sort_keys=True)
    return cache

def main():
    days = int(sys.argv[1]) if len(sys.argv) > 1 else 28
    today = datetime.date.today()
    since = (today - datetime.timedelta(days)).isoformat()
    mid = (today - datetime.timedelta(days // 2)).isoformat()
    decks, ev_list, missing = [], [], 0
    for ev, rows in events(since):
        ev_list.append(ev)
        for pos, did, gname, player in rows:
            path = os.path.join(CACHE, "decks", f"{did}.txt")
            if not os.path.exists(path):
                missing += 1
                continue
            main, side = parse_list(open(path).read())
            decks.append({"date": ev[:10], "pos": int(re.match(r"\d+", pos).group()), "player": player,
                          "deck": deck_name(gname.strip(), main), "goldfish": gname.strip(),
                          "main": main, "side": side, "id": did})

    by = collections.defaultdict(list)
    for d in decks:
        by[d["deck"]].append(d)

    out_decks = []
    for name, ds in sorted(by.items(), key=lambda kv: -len(kv[1])):
        old = [d for d in ds if d["date"] < mid]
        new = [d for d in ds if d["date"] >= mid]
        cards = {}
        for zone in ("main", "side"):
            for c in {c for d in ds for c in d[zone]}:
                e = cards.setdefault(c, {"name": c})
                have = [d[zone][c] for d in ds if c in d[zone]]
                e[zone] = {"incl": len(have) / len(ds), "avg": sum(have) / len(have),
                           "old": sum(c in d[zone] for d in old) / len(old) if old else None,
                           "new": sum(c in d[zone] for d in new) / len(new) if new else None}
        for e in cards.values():
            for zone in ("main", "side"):
                if zone in e:
                    e[zone]["moved"] = moved(e[zone]["old"], e[zone]["new"], len(old), len(new))
        out_decks.append({
            "name": name, "focus": name in FOCUS, "n": len(ds), "n_old": len(old), "n_new": len(new),
            "share": len(ds) / len(decks), "top8": sum(d["pos"] <= 8 for d in ds),
            "goldfish": sorted({d["goldfish"] for d in ds}),
            "cards": sorted(cards.values(), key=lambda e: -max(e.get("main", {}).get("incl", 0), e.get("side", {}).get("incl", 0))),
            "lists": [{"date": d["date"], "pos": d["pos"], "player": d["player"],
                       "url": f"https://www.mtggoldfish.com/deck/{d['id']}"}
                      for d in sorted(ds, key=lambda d: (d["date"], -d["pos"]), reverse=True)[:6]],
        })

    my_lists = json.load(open(MY_LISTS)) if os.path.exists(MY_LISTS) else []
    names = {c["name"] for d in out_decks for c in d["cards"]}
    names |= {c for l in my_lists for z in ("main", "side") for c in l[z]}
    oracle = scryfall(sorted(names))
    # Mana-base churn (fetch and basic counts) is not something to study, except in Lands.
    for d in out_decks:
        if d["name"] != "Lands":
            for c in d["cards"]:
                if "Land" in oracle.get(c["name"], {}).get("type", "").split(" — ")[0]:
                    for zone in ("main", "side"):
                        if zone in c:
                            c[zone]["moved"] = False
    meta = {"built": today.isoformat(), "since": since, "mid": mid, "events": ev_list,
            "n_decks": len(decks), "decks": out_decks, "my_lists": my_lists, "oracle": oracle}
    json.dump(meta, open(os.path.join(HERE, "meta.json"), "w"), separators=(",", ":"))
    print(f"{len(ev_list)} events, {len(decks)} decks ({missing} lists not downloaded), "
          f"{len(out_decks)} deck names, {len(names)} cards -> prep/meta.json")
    for d in out_decks[:30]:
        print(f"  {'*' if d['focus'] else ' '} {d['name']:<32} {d['n']:>3}  {d['share']:5.1%}  {', '.join(d['goldfish'])[:60]}")

main()
