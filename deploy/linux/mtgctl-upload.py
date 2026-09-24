#!/usr/bin/env python3
"""Uploads new MTGO logs to the mtgctl stats site and refreshes the results: the Linux twin of
deploy/windows/mtgctl-upload.ps1. Identify the player from the match logs, send what the server
lacks (match logs by name+size+mtime, session logs by hash as a small gzipped extract), then sync.
Standard library only."""
import argparse, gzip, hashlib, io, json, os, re, sys, time, urllib.request, uuid
from datetime import datetime, timezone

GAME = re.compile(r'^Match_GameLog_.*\.dat$')
SESSION = re.compile(r'^(mtgo\.log|mtgo-\d{8}-\d{6}\.log)$')
KEEP = ('Deck Used in Game ID:', 'Game Play Status Update for Game ID:', 'CreateAndFlipToDuelScene', 'Deck Used to Join Event ID:')
MAX_BATCH = 48 << 20

def log(msg, path, quiet):
    line = f"{datetime.now():%Y-%m-%d %H:%M:%S} {msg}"
    with open(path, 'a') as f: f.write(line + '\n')
    if not quiet: print(line, flush=True)

def multipart(parts):
    b = ('----mtgctl' + uuid.uuid4().hex).encode()
    out = io.BytesIO()
    for p in parts:
        out.write(b'--' + b + b'\r\n')
        if 'bytes' in p:
            out.write(f'Content-Disposition: form-data; name="{p["name"]}"; filename="{p["filename"]}"\r\nContent-Type: application/octet-stream\r\n\r\n'.encode()); out.write(p['bytes']); out.write(b'\r\n')
        else:
            out.write(f'Content-Disposition: form-data; name="{p["name"]}"\r\n\r\n{p["value"]}\r\n'.encode())
    out.write(b'--' + b + b'--\r\n')
    return out.getvalue(), b'multipart/form-data; boundary=' + b

def api(server, path, player=None, method='GET', body=None, ctype=None):
    req = urllib.request.Request(server + path, data=body, method=method)
    if player: req.add_header('X-Player', player)
    if ctype: req.add_header('Content-Type', ctype.decode() if isinstance(ctype, bytes) else ctype)
    with urllib.request.urlopen(req, timeout=900) as r: return json.loads(r.read().decode() or 'null')

def session_extract(path):
    """Same reduction the site applies: the lines naming decks, games and events, plus one line per hour."""
    out = []; last = None
    with open(path, 'r', encoding='utf-8', errors='replace') as f:
        for line in f:
            line = line.rstrip('\r\n')
            hour = line[:2] if len(line) >= 9 and line[2] == ':' and line[5] == ':' and line[8] == ' ' else None
            if any(k in line for k in KEEP) or line.startswith('Description=') or line.startswith('EventId=') or (hour and hour != last): out.append(line)
            if hour: last = hour
    return gzip.compress(('\n'.join(out) + '\n').encode())

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--server', default='https://bur.io/doomsday/stats/')
    ap.add_argument('--logs', action='append', default=[], help='MTGO client folder (Apps/2.0) or a folder of archived session logs; repeatable')
    ap.add_argument('--state', default=os.path.expanduser('~/.local/share/mtgctl'))
    ap.add_argument('--quiet', action='store_true')
    a = ap.parse_args()
    server = a.server.rstrip('/') + '/'
    os.makedirs(a.state, exist_ok=True)
    logfile = os.path.join(a.state, 'upload.log'); L = lambda m: log(m, logfile, a.quiet)
    statefile = os.path.join(a.state, 'state.json')
    state = json.load(open(statefile)) if os.path.exists(statefile) else {'player': None, 'last_change': None}

    games, sessions = {}, []
    for root in a.logs:
        for dp, _, fns in os.walk(root):
            for fn in fns:
                p = os.path.join(dp, fn); st = os.stat(p)
                if GAME.match(fn):
                    if fn not in games or st.st_mtime > games[fn][1].st_mtime: games[fn] = (p, st)
                elif SESSION.match(fn): sessions.append((p, st))
    if not games and not sessions: L(f"no MTGO logs under {', '.join(a.logs)}"); return 1
    latest = max(st.st_mtime for _, st in list(games.values()) + sessions)
    if state.get('player') and state.get('last_change') and latest <= state['last_change']:
        if not a.quiet: L('no log changed since the last run')
        return 0

    if not state.get('player'):
        if not games: L('only session logs found and no player known yet; point --logs at the MTGO client folder'); return 1
        newest = sorted(games.values(), key=lambda t: t[1].st_mtime)[-12:]
        body, ct = multipart([{'name': 'file', 'filename': os.path.basename(p), 'bytes': open(p, 'rb').read()} for p, _ in newest])
        state['player'] = api(server, 'api/detect', None, 'POST', body, ct)['name']
        L(f"you are {state['player']}")
    player = state['player']

    before = api(server, 'api/summary', player)
    manifest = api(server, 'api/manifest', player)
    have = {m['name']: m for m in manifest if m['kind'] == 'game'}
    have_hash = {m['sha256'] for m in manifest if m['kind'] == 'session'}
    todo = []
    for fn, (p, st) in games.items():
        h = have.get(fn); mt = int(st.st_mtime * 1000)
        if not h or st.st_size > h['size'] or mt > h['mtime'] + 1000: todo.append(('game', fn, mt, None, lambda p=p: open(p, 'rb').read()))
    for p, st in sessions:
        sha = hashlib.sha256(open(p, 'rb').read()).hexdigest()
        if sha in have_hash: continue
        have_hash.add(sha)
        gz = session_extract(p)
        todo.append(('session-extract', os.path.basename(p), int(st.st_mtime * 1000), sha, lambda gz=gz: gz))
    L(f"{len(games)} match logs and {len(sessions)} session logs on disk; {len(todo)} new to upload")

    i = 0
    while i < len(todo):
        parts, size, j = [], 0, i
        while j < len(todo):
            kind, name, mt, sha, data = todo[j]; b = data()
            if j > i and size + len(b) > MAX_BATCH: break
            parts += [{'name': 'kind', 'value': kind}, {'name': 'name', 'value': name}, {'name': 'mtime', 'value': str(mt)}, {'name': 'tz', 'value': str(-time.localtime(mt / 1000).tm_gmtoff // 60)}]
            if sha: parts.append({'name': 'sha256', 'value': sha})
            parts.append({'name': 'file', 'filename': name, 'bytes': b}); size += len(b); j += 1
        body, ct = multipart(parts)
        api(server, 'api/upload', player, 'POST', body, ct)
        L(f"uploaded {i + 1}-{j} of {len(todo)} ({size / 1048576:.1f} MB)"); i = j

    state['last_change'] = latest
    json.dump(state, open(statefile, 'w'))
    if not todo: L(f"nothing new; latest match on file {before.get('latest_match')}"); return 0
    rep = api(server, 'api/sync', player, 'POST', b'', 'application/json')
    after = api(server, 'api/summary', player)
    new = after['matches'] - before['matches']
    L(f"{new} new match(es); {after['matches']} on file, latest {after.get('latest_match')}" if new > 0 else f"no new matches; latest {after.get('latest_match')}")
    if rep.get('in_progress'): L(f"{rep['in_progress']} match(es) not finished yet")
    if after.get('latest_list'): L(f"latest list: {after['latest_list']}")
    return 0

if __name__ == '__main__':
    sys.exit(main())
