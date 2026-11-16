#!/usr/bin/env python3
"""Pull sheet as a one-page landscape-Letter PDF (and the same sheet as a standalone HTML page),
from a working map JSON (the page's db document, or a map from sbmaps.json): all 75 in, then pull
the cards listed per matchup.  usage: pull_sheet_pdf.py MAP.json OUT.pdf [OUT.html]"""
import json, math, sys, datetime

W, H, M, GAP, COLS = 792, 612, 24, 8, 5
# Helvetica advance widths (AFM, per 1000 em) for the characters card and deck names use.
_W = dict(zip("abcdefghijklmnopqrstuvwxyz", [556,556,500,556,556,278,556,556,222,222,500,222,833,556,556,556,556,333,500,278,556,500,722,500,500,500]))
_W.update(zip("ABCDEFGHIJKLMNOPQRSTUVWXYZ", [667,667,722,722,667,611,778,722,278,500,667,556,833,722,778,667,778,722,667,611,722,667,944,667,667,611]))
_W.update({c: 556 for c in "0123456789"}); _W.update({" ": 278, ",": 278, ".": 278, "'": 191, "-": 333, "&": 667, "(": 333, ")": 333, ":": 278, "%": 889, "~": 584, "/": 278})
def tw(s, size): return sum(_W.get(c, 600) for c in s) * size / 1000
def esc(s): return s.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")
def wrap(s, size, width):
    words, lines, cur = s.split(" "), [], ""
    for w in words:
        t = (cur + " " + w).strip()
        if tw(t, size) <= width or not cur: cur = t
        else: lines.append(cur); cur = w
    return lines + [cur]

def boxes_of(m):
    side = sorted(m["side"].items())
    out = [{"title": "Sideboard", "share": None, "items": side, "total": sum(m["side"].values()), "kind": "side"}]
    decks = sorted(m["decks"], key=lambda x: -(x["share"] or 0))
    for x in decks:
        p = m["plans"].get(x["deck"]) or {"out": {}, "in": {}}
        if not p["out"] and not p["in"]: continue
        n = dict(p["out"])  # a card in both main and side (Murktide) is one line
        for c, q in m["side"].items():
            if q - p["in"].get(c, 0) > 0: n[c] = n.get(c, 0) + q - p["in"].get(c, 0)
        items = sorted(n.items()); total = sum(n.values())
        out.append({"title": x["deck"], "share": x["share"], "items": items, "total": total,
                    "kind": "off" if total != 15 else "rough" if p.get("rough") else ""})
    return out

def layout(m, fs):
    bw = (W - 2 * M - (COLS - 1) * GAP) / COLS; cw = (bw - 12) / 2; lh = fs * 1.25
    rows, row = [], []
    for b in boxes_of(m):
        half = math.ceil(len(b["items"]) / 2)
        cols = [b["items"][:half], b["items"][half:]]
        lines = [[(n, wrap(c, fs, cw - 9)) for c, n in col] for col in cols]
        nl = max(sum(len(w) for _, w in col) for col in lines) if b["items"] else 1
        b["lines"], b["h"] = lines, 6 + (fs + 2) * 1.3 + nl * lh + 5
        row.append(b)
        if len(row) == COLS: rows.append(row); row = []
    if row: rows.append(row)
    total_h = 26 + sum(max(b["h"] for b in r) for r in rows) + GAP * (len(rows) - 1) + 14
    return rows, bw, cw, lh, total_h

def render(m, out_path):
    fs = 8.0
    while True:
        rows, bw, cw, lh, total_h = layout(m, fs)
        if total_h <= H - 2 * M or fs <= 5.5: break
        fs -= 0.25
    c = []
    c.append(f"BT /F2 11 Tf {M} {H - M - 10} Td ({esc(m['name'])}) Tj ET")
    sub = f"All 75 go in, then pull the cards listed. Every box totals 15. Dashed = depends on their build or play/draw. {datetime.date.today().isoformat()}"
    c.append(f"0.4 g BT /F1 7.5 Tf {W - M - tw(sub, 7.5):.1f} {H - M - 10} Td ({esc(sub)}) Tj ET 0 g")
    y = H - M - 26
    for r in rows:
        rh = max(b["h"] for b in r)
        for i, b in enumerate(r):
            x = M + i * (bw + GAP)
            stroke = {"side": "0.12 0.37 0.48", "off": "0.7 0.15 0.12", "rough": "0.54 0.38 0"}.get(b["kind"], "0.7 0.7 0.7")
            dash = "[3 2] 0 d" if b["kind"] == "rough" else "[] 0 d"
            lw = 1.4 if b["kind"] == "side" else 0.7
            c.append(f"q {stroke} RG {lw} w {dash} {x:.1f} {y - rh:.1f} {bw:.1f} {rh:.1f} re S Q")
            ty = y - 6 - (fs + 2)
            title = ("~ " if b["kind"] == "rough" else "") + b["title"]
            c.append(f"BT /F2 {fs + 1.5} Tf {x + 6:.1f} {ty:.1f} Td ({esc(title)}) Tj ET")
            right = str(b["total"]); rw = tw(right, fs)
            c.append(f"0.4 g BT /F1 {fs} Tf {x + bw - 6 - rw:.1f} {ty:.1f} Td ({right}) Tj ET 0 g")
            if b["share"] is not None:
                sh = f"{round(b['share'] * 100)}%"
                c.append(f"0.12 0.37 0.48 rg BT /F1 {fs - 1} Tf {x + bw - 6 - rw - 5 - tw(sh, fs - 1):.1f} {ty:.1f} Td ({sh}) Tj ET 0 g")
            for ci, col in enumerate(b["lines"]):
                cx = x + 6 + ci * cw; ly = ty - (fs + 2) * 1.3
                for n, words in col:
                    c.append(f"0.4 g BT /F1 {fs} Tf {cx + 6 - tw(str(n), fs):.1f} {ly:.1f} Td ({n}) Tj ET 0 g")
                    for w in words:
                        c.append(f"BT /F1 {fs} Tf {cx + 9:.1f} {ly:.1f} Td ({esc(w)}) Tj ET"); ly -= lh
        y -= rh + GAP
    foot = "Decks ordered by expected meta share; decks without a share are outside the expected meta."
    c.append(f"0.4 g BT /F1 7 Tf {M} {M - 6} Td ({esc(foot)}) Tj ET")
    stream = "\n".join(c).encode("latin-1")
    objs = [b"<< /Type /Catalog /Pages 2 0 R >>",
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {W} {H}] /Resources << /Font << /F1 4 0 R /F2 5 0 R >> >> /Contents 6 0 R >>".encode(),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>",
            b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n" + stream + b"\nendstream"]
    pdf, offs = bytearray(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n"), []
    for i, o in enumerate(objs, 1):
        offs.append(len(pdf)); pdf += f"{i} 0 obj\n".encode() + o + b"\nendobj\n"
    xref = len(pdf)
    pdf += f"xref\n0 {len(objs) + 1}\n0000000000 65535 f \n".encode() + b"".join(f"{o:010d} 00000 n \n".encode() for o in offs)
    pdf += f"trailer\n<< /Size {len(objs) + 1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode()
    open(out_path, "wb").write(pdf)
    print(f"{out_path}: {len(rows)} rows, font {fs}pt, height {total_h:.0f} of {H - 2 * M} pt")

HTML_CSS = """:root{--ink:#111;--muted:#666;--line:#bbb;--accent:#1f5f7a;--bad:#b3261e;--warn:#8a6100}*{box-sizing:border-box}
body{margin:0;padding:10mm;background:#fff;color:var(--ink);font:9.5pt/1.3 "IBM Plex Sans","Helvetica Neue",Arial,sans-serif}
.head{display:flex;justify-content:space-between;align-items:baseline;margin-bottom:6px}.head b{font-size:12pt}.head span{color:var(--muted);font-size:8.5pt}
.grid{display:grid;grid-template-columns:repeat(4,minmax(0,1fr));gap:5px}
.pull{border:1px solid var(--line);border-radius:5px;padding:4px 7px;break-inside:avoid}.pull.side{border-color:var(--accent);border-width:2px}.pull.off{border-color:var(--bad)}.pull.rough{border-color:var(--warn);border-style:dashed}
h3{display:flex;gap:6px;align-items:baseline;margin:0 0 3px;font-size:9.5pt}h3 .n{margin-left:auto;color:var(--muted);font-weight:400;font-family:ui-monospace,monospace}h3 .share{color:var(--accent);font-size:8pt;font-weight:500}
.list{display:grid;grid-template-columns:1fr 1fr;gap:0 8px;font-size:8.3pt}.pl{display:flex;gap:5px;line-height:1.3}.pl .num{color:var(--muted);width:1.1em;flex:none;text-align:right;font-family:ui-monospace,monospace}
.foot{margin-top:6px;color:var(--muted);font-size:8pt}.bar{margin-bottom:10px}.bar button{font:inherit;padding:6px 12px}
@media print{@page{size:landscape;margin:8mm}body{padding:0}.bar{display:none}.grid{grid-template-columns:repeat(5,minmax(0,1fr));gap:4px}}"""

def render_html(m, out_path):
    import html
    e = html.escape
    parts = []
    for b in boxes_of(m):
        share = f'<span class="share">{round(b["share"] * 100)}%</span>' if b["share"] is not None else ""
        lst = "".join(f'<div class="pl"><span class="num">{n}</span>{e(c)}</div>' for c, n in b["items"])
        parts.append(f'<section class="pull {b["kind"]}"><h3>{"~ " if b["kind"] == "rough" else ""}{e(b["title"])}{share}<span class="n">{b["total"]}</span></h3><div class="list">{lst}</div></section>')
    page = f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Pull sheet</title><style>{HTML_CSS}</style></head><body>
<div class="bar"><button onclick="window.print()">Print</button> <span class="muted">Landscape. Ctrl+P / Cmd+P also works.</span></div>
<div class="head"><b>{e(m["name"])}</b><span>All 75 go in, then pull the cards listed. Every box totals 15. Dashed = depends on their build or play/draw. {datetime.date.today().isoformat()}</span></div>
<div class="grid">{"".join(parts)}</div>
<div class="foot">Decks ordered by expected meta share; decks without a share are outside the expected meta.</div>
</body></html>"""
    open(out_path, "w", encoding="utf-8").write(page)
    print(out_path)

if __name__ == "__main__":
    m = json.load(open(sys.argv[1]))
    if "maps" in m: m = m["maps"][0]; m["decks"] = [{"deck": o["deck"], "share": o["share"]} for o in m["meta_order"]] + [{"deck": d, "share": None} for d in m["plans"] if d not in {o["deck"] for o in m["meta_order"]}]
    render(m, sys.argv[2] if len(sys.argv) > 2 else "pull-sheet.pdf")
    if len(sys.argv) > 3: render_html(m, sys.argv[3])
