#!/usr/bin/env python3
"""Sideboard maps: one TOML per 75, with the in/out plan per opposing deck.

  sbmap.py import GRID.{csv,org} -o MAP.toml [--from MAP.toml] [--list NAME] [--source URL]
      Read a grid (rows = cards, columns = opposing decks, a cell = copies
      swapped; main-deck rows are outs, sideboard rows ins): Eureka's sheet
      exported as CSV, or an Org table written by `grid --org`. --from carries
      aliases and roles over from an existing map.
  sbmap.py grid MAP.toml [--org FILE | --csv FILE]
      The same map as a grid: on the terminal, or written as an Org table /
      CSV to edit and `import` back.
  sbmap.py check MAP.toml [--lists my_lists.json --label "UB Tempo DD"]
      Validate every plan and print cuts per matchup, where each main card is
      cut, which matchups each sideboard card and role covers, and the diff
      between the map's 75 and one of my registered lists.
  sbmap.py rebase MAP.toml --list LIST.txt -o NEW.toml [--name NAME]
      Start a map for a different 75 from an existing one: every plan is
      clipped to the cards the new list has, and each plan carries a comment
      listing what it lost and by how much in/out now differ. LIST.txt is
      "N Card" lines with a "SIDEBOARD:" line between main and side.
  sbmap.py set MAP.toml [--plan DECK --out "Card=N" ... --in "Card=N" ...] [--swap-in "Old=New"] [--drop DECK ...]
      Edit plans in place (N=0 removes a card; --swap-in renames a sideboard
      card in every plan). The file is rewritten; a "# TODO" line marks each
      plan whose in and out counts differ.
  sbmap.py coverage MAP.toml [--meta sbmaps/expected-meta.csv]
      Every deck of the expected meta against the map's [meta] table: which
      plan covers it, which decks have no plan, and which plans cover nothing
      in the meta.
  sbmap.py export MAP.toml [MAP2.toml ...] -o sbmaps.json
      The maps as one JSON file for the prep page (plans plus validation issues).
  sbmap.py whatif MAP.toml --remove "Card=n" ... --add "Card=n" ...
      Apply a sideboard change and list, per matchup, the slots that lose their
      card, the plans that cut more than they can now bring in, and which new
      cards share a role with what left.

TOML shape:
  list = "..."            source = "..."
  [main] / [side]         card = copies
  [aliases]               sheet deck name = prep/stats deck name
  [codes]                 deck name = short column header for grids (optional)
  [meta]                  expected-meta deck name = plan deck name ("" = no plan yet)
  [roles]                 role = [sideboard cards]   (a card may have several)
  [plans."Deck".out] / [plans."Deck".in]   card = copies
"""
import argparse, collections, csv, json, sys, tomllib

TOTAL_FIELD_ROWS = {"Lands", "Main Deck Total", "Card Count", "Sideboard Plan Check"}


def q(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def emit_table(name, d):
    out = [f"[{name}]"]
    for k, v in d.items():
        out.append(f"{q(k)} = {q(v) if isinstance(v, str) else v}")
    return "\n".join(out) + "\n"


def read_grid_rows(path):
    if path.endswith(".org"):
        rows = []
        for line in open(path, encoding="utf-8"):
            line = line.strip()
            if not line.startswith("|") or line.startswith("|-"):
                continue
            rows.append([c.strip() for c in line.strip("|").split("|")])
        return rows
    return list(csv.reader(open(path, newline="", encoding="utf-8")))


def import_grid(path, list_name, source, base):
    rows = read_grid_rows(path)
    header = next(r for r in rows if r[0] == "" and sum(1 for c in r[2:] if c.strip()) > 1)
    title = next((r[2] for r in rows if r[0] == "" and r[2] and not any(r[3:])), None)
    rev = {v: k for k, v in (base or {}).get("codes", {}).items()}
    decks = [rev.get(c.strip(), c.strip()) for c in header[2:] if c.strip()]
    ncol = len(decks)
    main, side, plans = {}, {}, {d: {"out": {}, "in": {}} for d in decks}
    zone = "main"
    for r in rows:
        name = r[0].strip()
        if not name or name in TOTAL_FIELD_ROWS or r[0] == "":
            if name == "Main Deck Total":
                zone = "side"
            continue
        copies = int(r[1])
        (main if zone == "main" else side)[name] = copies
        for d, cell in zip(decks, r[2 : 2 + ncol]):
            if cell.strip():
                plans[d]["out" if zone == "main" else "in"][name] = int(cell)
    if base:
        for zone, d in (("main", main), ("side", side)):
            for c, n in base[zone].items():
                d.setdefault(c, n)
        main = {c: main[c] for c in list(base["main"]) + [c for c in main if c not in base["main"]] if c in main}
    text = [f"list = {q(list_name or title or 'map')}"]
    if source:
        text.append(f"source = {q(source)}")
    text.append("")
    text.append(emit_table("main", main))
    text.append(emit_table("side", side))
    aliases = {d: (base or {}).get("aliases", {}).get(d, d) for d in decks}
    roles = (base or {}).get("roles") or {"role" + str(i + 1): [c] for i, c in enumerate(side)}
    text.append(emit_table("aliases", aliases))
    for k in ("codes", "meta"):
        if (base or {}).get(k):
            text.append(emit_table(k, base[k]))
    text.append("[roles]\n" + "\n".join(f"{q(r)} = [" + ", ".join(q(c) for c in cs) + "]" for r, cs in roles.items()) + "\n")
    for d in decks:
        text.append(emit_table(f"plans.{q(d)}.out", plans[d]["out"]))
        text.append(emit_table(f"plans.{q(d)}.in", plans[d]["in"]))
    return "\n".join(text)


def load(path):
    m = tomllib.load(open(path, "rb"))
    m.setdefault("aliases", {})
    m.setdefault("roles", {})
    m.setdefault("codes", {})
    m.setdefault("meta", {})
    return m


def validate(m):
    errs = []
    if sum(m["main"].values()) != 60:
        errs.append(f"main deck has {sum(m['main'].values())} cards")
    if sum(m["side"].values()) != 15:
        errs.append(f"sideboard has {sum(m['side'].values())} cards")
    for d, p in m["plans"].items():
        o, i = p.get("out", {}), p.get("in", {})
        if sum(o.values()) != sum(i.values()):
            errs.append(f"{d}: out {sum(o.values())} != in {sum(i.values())}")
        for c, n in o.items():
            if n > m["main"].get(c, 0):
                errs.append(f"{d}: cuts {n} {c}, main has {m['main'].get(c, 0)}")
        for c, n in i.items():
            if n > m["side"].get(c, 0):
                errs.append(f"{d}: brings {n} {c}, side has {m['side'].get(c, 0)}")
    return errs


def roles_of(m, card):
    return [r for r, cs in m["roles"].items() if card in cs] or ["(no role)"]


def check(m, lists_path, label):
    errs = validate(m)
    print("== validation ==")
    print("\n".join("  " + e for e in errs) if errs else "  ok")
    plans = m["plans"]
    print("\n== cuts per matchup ==")
    for d, p in sorted(plans.items(), key=lambda kv: -sum(kv[1].get("out", {}).values())):
        o, i = p.get("out", {}), p.get("in", {})
        mix = collections.Counter()
        for c, n in i.items():
            for r in roles_of(m, c):
                mix[r] += n
        stay = {c: n - i.get(c, 0) for c, n in m["side"].items() if n - i.get(c, 0) > 0}
        print(f"  {d:20s} {sum(o.values()):2d}  out: " + ", ".join(f"{n} {c}" for c, n in o.items()))
        print(f"  {'':20s}     in:  " + ", ".join(f"{n} {c}" for c, n in i.items()) + f"   [{', '.join(f'{r} {n}' for r, n in mix.most_common())}]")
        print(f"  {'':20s}     stays out: " + ", ".join(f"{n} {c}" for c, n in stay.items()))
    print("\n== main cards: matchups where they are cut ==")
    for c, n in m["main"].items():
        hits = [(d, p["out"][c]) for d, p in plans.items() if c in p.get("out", {})]
        if hits:
            full = sum(1 for _, k in hits if k == n)
            print(f"  {c:28s} {n} | cut in {len(hits):2d} of {len(plans)} (all copies in {full:2d}): " + ", ".join(f"{d}{'' if k == n else f' ({k})'}" for d, k in hits))
    print("\n== sideboard cards: matchups where they come in ==")
    for c, n in m["side"].items():
        hits = [(d, p["in"][c]) for d, p in plans.items() if c in p.get("in", {})]
        print(f"  {c:28s} {n} | {', '.join(roles_of(m, c)):18s} | in {len(hits):2d} of {len(plans)}: " + ", ".join(f"{d}{'' if k == n else f' ({k})'}" for d, k in hits))
    print("\n== roles: copies brought in per matchup ==")
    roles = list(m["roles"]) + (["(no role)"] if any(roles_of(m, c) == ["(no role)"] for c in m["side"]) else [])
    print(f"  {'':20s}" + "".join(f"{r[:12]:>13s}" for r in roles))
    for d, p in plans.items():
        mix = collections.Counter()
        for c, n in p.get("in", {}).items():
            for r in roles_of(m, c):
                mix[r] += n
        print(f"  {d:20s}" + "".join(f"{mix.get(r, 0) or '.':>13}" for r in roles))
    if lists_path:
        mine = json.load(open(lists_path))
        L = next((l for l in mine if l["label"] == label), mine[0])
        print(f"\n== map's 75 vs my list {L['name']} ==")
        for zone in ("main", "side"):
            a, b = m[zone], L[zone]
            d = [(c, a.get(c, 0), b.get(c, 0)) for c in sorted(set(a) | set(b)) if a.get(c, 0) != b.get(c, 0)]
            print(f"  {zone}: " + ("; ".join(f"{c} map {x} / mine {y}" for c, x, y in d) if d else "identical"))


def read_list(path):
    main, side, zone = {}, {}, "main"
    for line in open(path, encoding="utf-8"):
        line = line.strip()
        if not line:
            continue
        if line.upper().startswith("SIDEBOARD"):
            zone = "side"
            continue
        n, _, name = line.partition(" ")
        (main if zone == "main" else side)[name.strip()] = (main if zone == "main" else side).get(name.strip(), 0) + int(n)
    return main, side


def rebase(m, main, side, name):
    text = [f"list = {q(name)}", f"based_on = {q(m['list'])}", ""]
    text.append(emit_table("main", main))
    text.append(emit_table("side", side))
    text.append(emit_table("aliases", m["aliases"]))
    for k in ("codes", "meta"):
        if m.get(k):
            text.append(emit_table(k, m[k]))
    roles = ["[roles]"]
    for r, cs in m["roles"].items():
        roles.append(f"{q(r)} = [" + ", ".join(q(c) for c in cs) + "]")
    text.append("\n".join(roles) + "\n")
    for d, p in m["plans"].items():
        o = {c: min(n, main.get(c, 0)) for c, n in p.get("out", {}).items() if main.get(c, 0)}
        i = {c: min(n, side.get(c, 0)) for c, n in p.get("in", {}).items() if side.get(c, 0)}
        lost_o = {c: n - o.get(c, 0) for c, n in p.get("out", {}).items() if n - o.get(c, 0)}
        lost_i = {c: n - i.get(c, 0) for c, n in p.get("in", {}).items() if n - i.get(c, 0)}
        note = []
        if lost_o:
            note.append("outs gone: " + ", ".join(f"{n} {c}" for c, n in lost_o.items()))
        if lost_i:
            note.append("ins gone: " + ", ".join(f"{n} {c}" for c, n in lost_i.items()))
        bal = sum(o.values()) - sum(i.values())
        if bal:
            note.append(f"out {sum(o.values())} vs in {sum(i.values())}: {'find ' + str(bal) + ' more in or cut ' + str(bal) + ' fewer' if bal > 0 else 'cut ' + str(-bal) + ' more or bring ' + str(-bal) + ' fewer'}")
        head = ("# TODO " + "; ".join(note) + "\n") if note else ""
        text.append(head + emit_table(f"plans.{q(d)}.out", o))
        text.append(emit_table(f"plans.{q(d)}.in", i))
    return "\n".join(text)


def dump(m):
    text = [f"list = {q(m['list'])}"]
    for k in ("based_on", "source"):
        if k in m:
            text.append(f"{k} = {q(m[k])}")
    text.append("")
    text.append(emit_table("main", m["main"]))
    text.append(emit_table("side", m["side"]))
    text.append(emit_table("aliases", m["aliases"]))
    if m.get("codes"):
        text.append(emit_table("codes", m["codes"]))
    if m.get("meta"):
        text.append(emit_table("meta", m["meta"]))
    roles = ["[roles]"] + [f"{q(r)} = [" + ", ".join(q(c) for c in cs) + "]" for r, cs in m["roles"].items()]
    text.append("\n".join(roles) + "\n")
    for d, p in m["plans"].items():
        o, i = p.get("out", {}), p.get("in", {})
        bal = sum(o.values()) - sum(i.values())
        head = f"# TODO out {sum(o.values())} vs in {sum(i.values())}\n" if bal else ""
        text.append(head + emit_table(f"plans.{q(d)}.out", o))
        text.append(emit_table(f"plans.{q(d)}.in", i))
    return "\n".join(text)


def grid_rows(m, compact=False, codes=False):
    decks = list(m["plans"])
    rows = [["", "", *([m["codes"].get(d, d) for d in decks] if codes else decks)]]
    for zone, key in (("main", "out"), ("side", "in")):
        for c, n in m[zone].items():
            cells = [str(m["plans"][d].get(key, {}).get(c, "") or "") for d in decks]
            if compact and zone == "main" and not any(cells):
                continue
            rows.append([c, str(n), *cells])
        if zone == "main":
            rows.append(["Main Deck Total", str(sum(m["main"].values())), *[""] * len(decks)])
    outs = [sum(m["plans"][d].get("out", {}).values()) for d in decks]
    ins = [sum(m["plans"][d].get("in", {}).values()) for d in decks]
    rows.append(["Card Count", "OUT", *map(str, outs)])
    rows.append(["Card Count", "IN", *map(str, ins)])
    rows.append(["Sideboard Plan Check", str(sum(m["side"].values())), *[str(o - i) if o != i else "0" for o, i in zip(outs, ins)]])
    return rows


def grid(m, org=None, csv_path=None, full=False):
    rows = grid_rows(m, compact=not full, codes=not full)
    if csv_path:
        csv.writer(open(csv_path, "w", newline="", encoding="utf-8")).writerows(rows)
        return print("wrote", csv_path)
    if org:
        w = [max(len(r[i]) for r in rows) for i in range(len(rows[0]))]
        line = lambda r: "| " + " | ".join(c.ljust(w[i]) for i, c in enumerate(r)) + " |"
        sep = "|-" + "-+-".join("-" * x for x in w) + "-|"
        legend = ", ".join(f"{m['codes'].get(d, d)} = {d}" for d in m["plans"] if m["codes"].get(d, d) != d)
        out = [f"#+TITLE: {m['list']}", "# cells = copies swapped; main rows are OUT, sideboard rows IN. Edit, then: sbmap.py import FILE.org -o MAP.toml --from MAP.toml"]
        if not full:
            out.append("# main cards no plan touches are hidden; `grid --full` shows them")
        if legend:
            import textwrap
            out += ["# " + l for l in textwrap.wrap(legend, 100)]
        out += ["", line(rows[0]), sep]
        for r in rows[1:]:
            if r[0] == "Main Deck Total":
                out += [line(r), sep]
            elif r[0] == "Card Count" and r[1] == "OUT":
                out += [sep, line(r)]
            else:
                out.append(line(r))
        open(org, "w", encoding="utf-8").write("\n".join(out) + "\n")
        return print("wrote", org)
    decks = rows[0][2:]
    short = [d[:7] for d in decks]
    print(f"{'':28s}{'':>3s} " + " ".join(f"{x:>7s}" for x in short))
    if not full:
        print("  (" + ", ".join(f"{m['codes'].get(d, d)}={d}" for d in m["plans"] if m["codes"].get(d, d) != d) + ")")
    for r in rows[1:]:
        if r[0] in ("Main Deck Total",):
            print("-" * (32 + 8 * len(decks)))
            continue
        cells = [("!" + c if r[0] == "Sideboard Plan Check" and c not in ("", "0") else c) for c in r[2:]]
        print(f"{r[0][:28]:28s}{r[1]:>3s} " + " ".join(f"{(c or '.'):>7s}" for c in cells))
    print("\nSideboard Plan Check row: OUT minus IN per matchup, 0 when the plan balances.")


def read_meta(path):
    rows = list(csv.reader(open(path, newline="", encoding="utf-8")))
    out, on = [], False
    for r in rows:
        if r and r[0] == "Archetype":
            on = True
            continue
        if on:
            if not r[0]:
                break
            out.append({"deck": r[0], "share": float(r[1].rstrip("%")) / 100 if r[1].endswith("%") else None, "macro": r[2]})
    return out


def coverage(m, meta_path):
    meta = read_meta(meta_path)
    covered = set()
    print(f"== expected meta ({meta_path}) vs plans ==")
    for row in meta:
        plan = m["meta"].get(row["deck"])
        if row["deck"] == "Other":
            continue
        if plan and plan in m["plans"]:
            p = m["plans"][plan]
            state = f"plan {plan} ({sum(p.get('out', {}).values())} out)" if p.get("out") or p.get("in") else f"plan {plan} is EMPTY"
            covered.add(plan)
        elif row["deck"] in m["plans"]:
            state = f"plan {row['deck']}" + ("" if m["plans"][row["deck"]].get("out") else " is EMPTY")
            covered.add(row["deck"])
        else:
            state = "NO PLAN"
        print(f"  {row['deck']:16s} {row['share']:5.0%}  {state}")
    extra = [d for d in m["plans"] if d not in covered]
    print("\nplans for decks outside the expected meta: " + (", ".join(extra) or "none"))


def export(paths, out, meta_path=None):
    maps = []
    meta = read_meta(meta_path) if meta_path else []
    for path in paths:
        m = load(path)
        order = []
        for row in meta:
            plan = m["meta"].get(row["deck"]) or (row["deck"] if row["deck"] in m["plans"] else None)
            if plan in m["plans"]:
                order.append({"deck": plan, "meta": row["deck"], "share": row["share"]})
        maps.append({"name": m["list"], "source": m.get("source"), "main": m["main"], "side": m["side"], "aliases": m["aliases"], "codes": m["codes"], "roles": m["roles"],
                     "meta_order": order, "plans": {d: {"out": p.get("out", {}), "in": p.get("in", {})} for d, p in m["plans"].items()}, "issues": validate(m)})
    json.dump({"maps": maps}, open(out, "w", encoding="utf-8"), ensure_ascii=False, indent=0)
    print("wrote", out, "with", len(maps), "maps")


def set_plan(m, deck, outs, ins, swaps, new=False, drop=()):
    for d in drop:
        m["plans"].pop(d, None); m["codes"].pop(d, None); m["aliases"].pop(d, None)
    for old, new_ in swaps:
        for p in m["plans"].values():
            if old in p.get("in", {}):
                p["in"][new_] = p["in"].pop(old)
    if deck:
        if deck not in m["plans"]:
            if not new:
                sys.exit(f"no plan for {deck}; plans: {', '.join(m['plans'])} (use --new to add one)")
            m["plans"][deck] = {"out": {}, "in": {}}
        p = m["plans"][deck]
        for zone, changes in (("out", outs), ("in", ins)):
            p.setdefault(zone, {})
            for c, n in changes.items():
                if n:
                    p[zone][c] = n
                else:
                    p[zone].pop(c, None)


def parse_change(items):
    out = collections.Counter()
    for it in items or []:
        c, _, n = it.rpartition("=")
        out[c.strip()] += int(n) if n else 1
    return out


def whatif(m, remove, add):
    side = collections.Counter(m["side"])
    for c, n in remove.items():
        if side[c] < n:
            sys.exit(f"cannot remove {n} {c}: sideboard has {side[c]}")
        side[c] -= n
    side.update(add)
    side = +side
    print(f"== sideboard after change ({sum(side.values())} cards) ==")
    print("  " + ", ".join(f"{n} {c}" for c, n in side.items()))
    gone = {c for c in remove if side.get(c, 0) < m["side"][c]}
    print("\n== per matchup: slots that lose their card ==")
    for d, p in m["plans"].items():
        i = p.get("in", {})
        lost = {c: i[c] - min(i[c], side.get(c, 0)) for c in i if i[c] > side.get(c, 0)}
        if not lost:
            continue
        roles = {r for c in lost for r in roles_of(m, c)}
        cands = [(c, n) for c, n in side.items() if c not in i and set(roles_of(m, c)) & roles]
        new_unused = [(c, n - i.get(c, 0)) for c, n in side.items() if n - i.get(c, 0) > 0 and c not in lost]
        print(f"  {d:20s} loses " + ", ".join(f"{n} {c}" for c, n in lost.items()) + f" ({sum(lost.values())} slot{'s' if sum(lost.values()) != 1 else ''}; roles {', '.join(sorted(roles))})")
        print(f"  {'':20s} same-role cards not yet in: " + (", ".join(f"{n} {c}" for c, n in cands) or "none"))
        print(f"  {'':20s} any spare sideboard copies: " + (", ".join(f"{n} {c}" for c, n in new_unused) or "none"))
    print("\n== matchups unaffected ==")
    print("  " + ", ".join(d for d, p in m["plans"].items() if all(p.get("in", {}).get(c, 0) <= side.get(c, 0) for c in p.get("in", {}))))
    print("\n== added cards: where they could go (matchups whose plan brings in the same role) ==")
    for c in add:
        rs = set(roles_of(m, c)) - {"(no role)"}
        if not rs:
            print(f"  {c}: no role; add it to [roles] to get suggestions")
            continue
        hits = [d for d, p in m["plans"].items() if any(set(roles_of(m, x)) & rs for x in p.get("in", {}))]
        print(f"  {c} ({', '.join(sorted(rs))}): " + ", ".join(hits))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    a = sub.add_parser("import"); a.add_argument("grid"); a.add_argument("-o", required=True); a.add_argument("--from", dest="base"); a.add_argument("--list"); a.add_argument("--source")
    g = sub.add_parser("grid"); g.add_argument("map"); g.add_argument("--org"); g.add_argument("--csv"); g.add_argument("--full", action="store_true", help="all rows, full deck names")
    b = sub.add_parser("check"); b.add_argument("map"); b.add_argument("--lists"); b.add_argument("--label", default="UB Tempo DD")
    r = sub.add_parser("rebase"); r.add_argument("map"); r.add_argument("--list", required=True); r.add_argument("-o", required=True); r.add_argument("--name", default="rebased")
    e = sub.add_parser("set"); e.add_argument("map"); e.add_argument("--plan"); e.add_argument("--out", action="append", metavar="CARD=N"); e.add_argument("--in", dest="ins", action="append", metavar="CARD=N"); e.add_argument("--swap-in", action="append", metavar="OLD=NEW"); e.add_argument("--new", action="store_true", help="create the plan if missing"); e.add_argument("--drop", action="append", metavar="DECK", help="remove a plan")
    x = sub.add_parser("export"); x.add_argument("maps", nargs="+"); x.add_argument("-o", required=True); x.add_argument("--meta")
    cv = sub.add_parser("coverage"); cv.add_argument("map"); cv.add_argument("--meta", default="sbmaps/expected-meta.csv")
    w = sub.add_parser("whatif"); w.add_argument("map"); w.add_argument("--remove", action="append", metavar="CARD=N"); w.add_argument("--add", action="append", metavar="CARD=N")
    args = ap.parse_args()
    if args.cmd == "import":
        base = load(args.base) if args.base else None
        open(args.o, "w", encoding="utf-8").write(import_grid(args.grid, args.list or (base or {}).get("list"), args.source or (base or {}).get("source"), base))
        print("wrote", args.o)
    elif args.cmd == "grid":
        grid(load(args.map), args.org, args.csv, args.full)
    elif args.cmd == "rebase":
        main, side = read_list(args.list)
        open(args.o, "w", encoding="utf-8").write(rebase(load(args.map), main, side, args.name))
        print("wrote", args.o)
    elif args.cmd == "export":
        export(args.maps, args.o, args.meta)
    elif args.cmd == "coverage":
        coverage(load(args.map), args.meta)
    elif args.cmd == "set":
        m = load(args.map)
        swaps = [tuple(x.split("=", 1)) for x in args.swap_in or []]
        set_plan(m, args.plan, parse_change(args.out), parse_change(args.ins), swaps, args.new, args.drop or [])
        errs = validate(m)
        open(args.map, "w", encoding="utf-8").write(dump(m))
        print("\n".join(errs) if errs else "all plans balance")
    elif args.cmd == "check":
        check(load(args.map), args.lists, args.label)
    else:
        whatif(load(args.map), parse_change(args.remove), parse_change(args.add))


if __name__ == "__main__":
    main()
