#!/usr/bin/env python3
"""Build prep/ranges.json: what opponents actually did in the first rounds of the user's Doomsday games.

Usage: build_ranges.py DB [SINCE=2026-06-29]
DB is a copy of the stats site's pooled mtgctl.db. Only cards an opponent cast, played or
showed are visible, so every rate is a lower bound on how often they had the card.
Rounds are MTGO turn numbers: round N = my turn N and their turn N; the active player of each
line comes from the log's "Turn N: player" markers, so a flash threat on my turn is told apart
from a sorcery-speed one on theirs.
"""
import collections, json, os, re, sqlite3, sys

HERE = os.path.dirname(os.path.abspath(__file__))
# Opponent deck names in the logs → deck names on the prep page (meta.json).
DECK = {"Dimir Tempo (Moonshadow)": "Dimir Tempo", "Sewer-Cam": "Sewer-Cam Combo",
        "UB Tempo DD": "Doomsday (mirror)", "Turbo DD": "Doomsday (mirror)", "Esper DD": "Doomsday (mirror)",
        "Sultai DD": "Doomsday (mirror)", "Alurentell": "Sultai Aluren"}
ROUNDS = 3          # turn-by-turn tables
TIMING_ROUNDS = 6   # first-cast histogram; the last bucket is "6 or later"
MIN_GAMES = 10
MIN_COND = 5        # games a tell needs before it is reported
SHOWN = ("cast", "land", "activated", "revealed", "discarded", "in_hand", "opening_hand", "exiled", "milled", "boarded_in")
KEEP_LANDS = {"Wasteland"}
TARGET_RE = re.compile(r"targeting (.*?)\.(?: \(|$)")
REF_RE = re.compile(r"@\[([^@]*?)@:[0-9,]*:@\]")


BASIC_COLOR = {"Plains": "W", "Island": "U", "Swamp": "B", "Mountain": "R", "Forest": "G"}


def classify_land(name, o):
    """(label for land-pattern tells, colours it taps for). Fetches are grouped; their find is not logged."""
    text, typ = o.get("text", ""), o.get("type", "")
    if "Search your library for" in text and "Sacrifice this land" in text and "onto the battlefield" in text:
        return "a fetchland", ""
    if name == "Wasteland":
        return name, ""
    colors = {BASIC_COLOR[t] for t in BASIC_COLOR if t in typ}
    colors |= set(re.findall(r"\{([WUBRG])\}", " ".join(re.findall(r"Add ([^.\n]*)", text))))
    if "mana of any color" in text:
        colors.add("*")
    return name, "".join(sorted(colors, key="WUBRG*".index))


def color_label(colors):
    cs = set("".join(colors))
    if not cs:
        return "none (fetches only)"
    return "".join(c for c in "WUBRG*" if c in cs)


def main():
    db, since = sys.argv[1], sys.argv[2] if len(sys.argv) > 2 else "2026-06-29"
    con = sqlite3.connect(db)
    games = {gid: dict(deck=DECK.get(d, d), play=play, mull=(mull or 0) > 0, g1=gn == 1, won=bool(won), players={me, opp})
             for gid, d, play, mull, gn, won, me, opp in con.execute(
                 "SELECT game_id, opp_deck, on_play, opp_mulligans, game_number, won, player, opponent FROM mtgo_game_view "
                 "WHERE my_archetype = 'Doomsday' AND opp_deck IS NOT NULL AND date >= ?", (since,))}
    lands = {c for (c,) in con.execute("SELECT DISTINCT card FROM mtgo_card_events WHERE verb = 'land'")}
    hidden = lambda c: c in lands and c not in KEEP_LANDS
    try:
        oracle = json.load(open(os.path.join(HERE, "card_cache.json")))
    except OSError:
        oracle = {}
    land_kind = {c: classify_land(c, oracle.get(c) or oracle.get(c.split(" // ")[0]) or {}) for c in lands}

    # active player per line, from the turn markers
    marks = collections.defaultdict(list)
    for gid, seq, turn, side in con.execute("SELECT game_id, seq, turn, side FROM mtgo_events WHERE kind = 'Turn' ORDER BY game_id, seq"):
        if gid in games:
            marks[gid].append((seq, turn, side))
    maxround = {gid: m[-1][1] for gid, m in marks.items()}

    def active(gid, seq):
        side = None
        for s, _, sd in marks[gid]:
            if s > seq:
                break
            side = sd
        return side

    # targets of cast spells, from the raw line when the event table has none
    raw_targets = {}
    for gid, seq, text in con.execute("SELECT game_id, seq, text FROM mtgo_events WHERE kind = 'Casts' AND side = 'opp' AND text LIKE '%targeting%'"):
        if gid not in games:
            continue
        m = TARGET_RE.search(text)
        if not m:
            continue
        names = []
        for part in re.split(r",\s*(?:and\s+)?|\s+and\s+", REF_RE.sub(r"\1", m.group(1))):
            part = part.strip()
            if not part:
                continue
            part = re.sub(r"^Triggered [Aa]bility from (.*)$", r"\1 trigger", part)
            names.append(part)
        raw_targets[(gid, seq)] = names

    # per game: ordered opponent actions (round, on_my_turn, verb, card, targets) and every card shown
    acts = collections.defaultdict(list)
    shown = collections.defaultdict(set)
    for gid, seq, turn, verb, card, other in con.execute(
            "SELECT game_id, seq, turn, verb, card, other FROM mtgo_card_events WHERE side = 'opp' AND actor = 'opp' ORDER BY game_id, seq"):
        if gid not in games:
            continue
        if verb in SHOWN:
            shown[gid].add(card)
        if verb in ("cast", "activated", "land"):
            tg = [other] if other else raw_targets.get((gid, seq), []) if verb == "cast" else []
            tg = [t for t in tg if t not in games[gid]["players"]]
            acts[gid].append((turn, active(gid, seq) == "me", verb, card, tg))

    def spells(gid, rnd, my_turn=None):
        """Nonland spells cast and nonland abilities activated in a round, optionally on one player's turn."""
        return [c for t, mine, v, c, _ in acts[gid] if t == rnd and (my_turn is None or mine == my_turn)
                and (v == "cast" or (v == "activated" and c not in lands))]

    def first_land(gid):
        return next((c for t, _, v, c, _ in acts[gid] if v == "land"), None)

    by_deck = collections.defaultdict(list)
    for gid, g in games.items():
        by_deck[g["deck"]].append(gid)
    out = {"since": since, "rounds": ROUNDS, "timing_rounds": TIMING_ROUNDS, "decks": {}}
    for deck, gids in sorted(by_deck.items(), key=lambda kv: -len(kv[1])):
        n = len(gids)
        if n < MIN_GAMES:
            continue
        base = collections.Counter()
        for gid in gids:
            base.update(c for c in shown[gid] if not hidden(c))
        cards = [c for c, k in base.most_common() if k >= max(3, 0.1 * n)]
        d = {"n": n, "play": sum(1 for g in gids if games[g]["play"] == 1), "draw": sum(1 for g in gids if games[g]["play"] == 0),
             "g1": sum(1 for g in gids if games[g]["g1"]), "post": sum(1 for g in gids if not games[g]["g1"]),
             "mull": sum(1 for g in gids if games[g]["mull"]), "won": sum(1 for g in gids if games[g]["won"]),
             "reached": [sum(1 for g in gids if maxround.get(g, 0) >= r) for r in range(1, TIMING_ROUNDS + 2)],
             "shown": {c: base[c] for c in cards}}

        # turn-by-turn, split by who was on the play ("me_play": they are on the draw)
        splits = {}
        for key, sel in (("all", gids), ("me_play", [g for g in gids if games[g]["play"] == 1]), ("me_draw", [g for g in gids if games[g]["play"] == 0])):
            if not sel:
                continue
            turns = []
            for r in range(1, ROUNDS + 1):
                reached = [g for g in sel if maxround.get(g, 0) >= r]
                own, mine = collections.Counter(), collections.Counter()
                for g in reached:
                    own.update(set(spells(g, r, False)))
                    mine.update(set(spells(g, r, True)))
                turns.append({"reached": len(reached), "own": own.most_common(), "mine": mine.most_common()})
            t1 = collections.Counter(" + ".join(sorted(set(spells(g, 1, False)))) or "" for g in sel)
            splits[key] = {"n": len(sel), "t1": t1.most_common(), "turns": turns}
        d["splits"] = splits

        # tells: an early observable that moves the chance of a card showing up at all
        conds = [("Cast nothing on their turn 1", [g for g in gids if not spells(g, 1, False)], None),
                 ("No spell in round 1 at all (their turn or yours)", [g for g in gids if not spells(g, 1)], None)]
        t1_cards = collections.Counter(c for g in gids for c in set(spells(g, 1, False)))
        for c, k in t1_cards.most_common():
            if k >= MIN_COND:
                conds.append((f"Their turn 1: {c}", [g for g in gids if c in spells(g, 1, False)], c))
        fl = collections.Counter(first_land(g) for g in gids)
        for c, k in fl.most_common():
            if c and k >= MIN_COND:
                conds.append((f"First land: {c}", [g for g in gids if first_land(g) == c], c))
        mulls = [g for g in gids if games[g]["mull"]]
        if len(mulls) >= MIN_COND:
            conds.append(("They mulliganed", mulls, None))
        # land patterns: which lands they chose to play, and the colours those lands show
        def land_plays(g, upto):
            return [c for t, mine, v, c, _ in acts[g] if v == "land" and t <= upto and not mine]
        fk = collections.Counter(land_kind[first_land(g)][0] for g in gids if first_land(g))
        for lab, k in fk.most_common():
            if lab not in fl and k >= MIN_COND:
                conds.append((f"First land: {lab}", [g for g in gids if first_land(g) and land_kind[first_land(g)][0] == lab], None))
        def land_set(g):
            return " + ".join(sorted(land_kind[c][0] for c in land_plays(g, 2)))
        ls = collections.Counter(land_set(g) for g in gids if len(land_plays(g, 2)) >= 2)
        for lab, k in ls.most_common():
            if k >= MIN_COND:
                conds.append((f"Lands after their turn 2: {lab}", [g for g in gids if land_set(g) == lab and len(land_plays(g, 2)) >= 2], set(lab.split(" + "))))
        def colors2(g):
            return color_label(land_kind[c][1] for c in land_plays(g, 2))
        cl = collections.Counter(colors2(g) for g in gids if len(land_plays(g, 2)) >= 2)
        for lab, k in cl.most_common():
            if k >= MIN_COND:
                conds.append((f"Colours shown by their turn 2: {lab}", [g for g in gids if len(land_plays(g, 2)) >= 2 and colors2(g) == lab], None))
        waste2 = [g for g in gids if "Wasteland" in land_plays(g, 2)]
        if len(waste2) >= MIN_COND:
            conds.append(("Wasteland played by their turn 2", waste2, "Wasteland"))
        # early interaction on my turn (counters, flash)
        early_mine = collections.Counter(c for g in gids for c in set(spells(g, 1, True) + spells(g, 2, True)))
        for c, k in early_mine.most_common():
            if k >= MIN_COND:
                conds.append((f"Cast {c} on your turn 1 or 2", [g for g in gids if c in spells(g, 1, True) + spells(g, 2, True)], c))
        tells = []
        for label, sel, own in conds:
            if len(sel) < MIN_COND or len(sel) == n:
                continue
            own = own if isinstance(own, set) else {own}
            rows = []
            for c in cards:
                if c in own:
                    continue
                k = sum(1 for g in sel if c in shown[g])
                if abs(k / len(sel) - base[c] / n) >= 0.10:
                    rows.append([c, k])
            rows.sort(key=lambda r: -abs(r[1] / len(sel) - base[r[0]] / n))
            if rows:
                tells.append({"cond": label, "n": len(sel), "cards": rows})
        d["tells"] = tells

        # first cast per game, by round and whose turn; lands by the turn they were played
        first = collections.defaultdict(dict)
        first_act = collections.defaultdict(dict)
        for g in gids:
            for t, mine, v, c, _ in acts[g]:
                if v == "cast" or (v == "land" and c in KEEP_LANDS):
                    first[g].setdefault(c, (t, mine))
                elif v == "activated" and c in KEEP_LANDS:
                    first_act[g].setdefault(c, (t, mine))
        timing = []
        for c in cards:
            hist = [[0, 0] for _ in range(TIMING_ROUNDS)]
            for g in gids:
                if c in first[g]:
                    t, mine = first[g][c]
                    hist[min(t, TIMING_ROUNDS) - 1][1 if mine else 0] += 1
            if any(a or b for a, b in hist):
                timing.append({"card": c, "first": hist})
            if c in KEEP_LANDS:
                hist = [[0, 0] for _ in range(TIMING_ROUNDS)]
                for g in gids:
                    if c in first_act[g]:
                        t, mine = first_act[g][c]
                        hist[min(t, TIMING_ROUNDS) - 1][1 if mine else 0] += 1
                if any(a or b for a, b in hist):
                    timing.append({"card": f"{c} (activated)", "first": hist})
        d["timing"] = timing

        # what their interaction is pointed at
        targets = []
        for c in cards:
            tc = collections.Counter()
            casts = 0
            for g in gids:
                for t, mine, v, cc, tg in acts[g]:
                    if cc == c and v == "cast" and tg:
                        casts += 1
                        tc.update(tg)
            if casts >= 3:
                targets.append({"card": c, "casts": casts, "targets": tc.most_common(8)})
        d["targets"] = targets

        # game 1 vs postboard
        g1 = [g for g in gids if games[g]["g1"]]
        pb = [g for g in gids if not games[g]["g1"]]
        rows = []
        if g1 and pb:
            allc = collections.Counter()
            for g in gids:
                allc.update(c for c in shown[g] if not hidden(c))
            for c, k in allc.most_common():
                if k < 3:
                    continue
                a, b = sum(1 for g in g1 if c in shown[g]), sum(1 for g in pb if c in shown[g])
                if abs(b / len(pb) - a / len(g1)) >= 0.1:
                    rows.append([c, a, b, sum(1 for g in pb if c in spells(g, 1, False))])
            rows.sort(key=lambda r: -(r[2] / len(pb) - r[1] / len(g1)))
        d["board"] = {"g1": len(g1), "post": len(pb), "rows": rows}
        out["decks"][deck] = d

    json.dump(out, open(os.path.join(HERE, "ranges.json"), "w"), separators=(",", ":"))
    for deck, d in out["decks"].items():
        t1 = d["splits"]["all"]["t1"]
        quiet = next((k for lab, k in t1 if lab == ""), 0)
        print(f"{deck:<22} {d['n']:>4} games  won {d['won']/d['n']:.0%}  mull {d['mull']/d['n']:.0%}  quiet T1 {quiet/d['n']:.0%}  "
              f"tells {len(d['tells'])}  top: " + ", ".join(f"{c} {k/d['n']:.0%}" for c, k in list(d["shown"].items())[:4]))


main()
