// Shared by the pages: which player we are (the MTGO username the logs name as "me") and JSON calls.
const PLAYER_KEY = 'mtgctl.player';
function getPlayer() { try { return localStorage.getItem(PLAYER_KEY) || ''; } catch (e) { return ''; } }
function setPlayer(p) { try { localStorage.setItem(PLAYER_KEY, p); } catch (e) {} }
// Failures are reported to the server (journal) so "works for me" bugs can be read from the droplet.
function report(what) {
  try { navigator.sendBeacon('api/client-log', JSON.stringify(Object.assign({ page: location.href, ua: navigator.userAgent, player: getPlayer() }, what))); } catch (e) {}
}
window.addEventListener('error', e => report({ error: e.message, at: `${e.filename}:${e.lineno}` }));
window.addEventListener('unhandledrejection', e => report({ error: String(e.reason && e.reason.message || e.reason) }));
async function api(path, opts = {}) {
  const headers = Object.assign({}, getPlayer() ? { 'X-Player': getPlayer() } : {}, opts.headers || {});
  const r = await fetch(path, Object.assign({}, opts, { headers }));
  const text = await r.text();
  let body; try { body = JSON.parse(text); } catch (e) { body = { error: text }; }
  if (!r.ok) { report({ request: path, resolved: new URL(path, location.href).href, status: r.status, body: text.slice(0, 200) }); throw new Error(`${r.status} ${body.error || r.statusText} (${path})`); }
  return body;
}
function wilson(w, n) {
  if (!n) return null;
  const z = 1.959964, p = w / n, d = 1 + z * z / n;
  const c = (p + z * z / (2 * n)) / d, h = z * Math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d;
  return [100 * (c - h), 100 * (c + h)];
}
// Player bar: a dropdown of everyone with data here, plus the one detected from your own uploads.
async function playerBar(el, onChange) {
  let known = [];
  try { known = await api('api/players'); } catch (e) {}
  let cur = getPlayer();
  if (cur && !known.includes(cur)) known.push(cur);
  // nothing chosen yet (no upload from this browser): show the first player with data rather than a dead page
  if (!cur && known.length) { cur = known[0]; setPlayer(cur); }
  el.innerHTML = `<label>player <select id="playerSel">${known.map(p => `<option ${p === cur ? 'selected' : ''}>${p}</option>`).join('')}</select></label> <span id="who" class="muted">${cur ? '' : 'nobody has uploaded logs yet'}</span>`;
  el.querySelector('#playerSel').onchange = e => { setPlayer(e.target.value); if (onChange) onChange(e.target.value); else location.reload(); };
  return cur;
}
// Card images: names are resolved in one Scryfall batch call (75 per request, the per-card
// `cards/named` route throttles a grid's burst) to CDN image URLs, cached in localStorage.
const IMG_KEY = 'mtgctl.img';
let imgCache = null;
function imgCacheGet() { if (!imgCache) { try { imgCache = JSON.parse(localStorage.getItem(IMG_KEY) || '{}'); } catch (e) { imgCache = {}; } } return imgCache; }
async function resolveImages(names) {
  const cache = imgCacheGet();
  const missing = [...new Set(names.filter(n => !cache[n]))];
  for (let i = 0; i < missing.length; i += 75) {
    const chunk = missing.slice(i, i + 75);
    try {
      const r = await fetch('https://api.scryfall.com/cards/collection', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ identifiers: chunk.map(n => ({ name: n.split(' // ')[0] })) }) });
      const d = await r.json();
      for (const c of d.data || []) {
        const uri = (c.image_uris || (c.card_faces && c.card_faces[0].image_uris) || {}).small;
        const key = chunk.find(n => n.split(' // ')[0].toLowerCase() === c.name.split(' // ')[0].toLowerCase()) || c.name;
        if (uri) cache[key] = uri;
      }
    } catch (e) { report({ error: 'scryfall: ' + e.message }); }
  }
  try { localStorage.setItem(IMG_KEY, JSON.stringify(cache)); } catch (e) {}
  return cache;
}
function cardEl(name, qty, cls = '') {
  const d = document.createElement('div'); d.className = 'card ' + cls; d.title = name;
  const img = document.createElement('img'); img.alt = name; img.dataset.name = name;
  d.appendChild(img);
  if (qty !== undefined && qty !== 1) { const b = document.createElement('span'); b.className = 'card-badge'; b.textContent = qty; d.appendChild(b); }
  return d;
}
async function loadImages(root) {
  const imgs = [...root.querySelectorAll('img[data-name]')];
  const cache = await resolveImages(imgs.map(i => i.dataset.name));
  for (const i of imgs) { const u = cache[i.dataset.name]; if (u) i.src = u; else i.alt = i.dataset.name + ' (no image)'; delete i.dataset.name; }
}
// Compact text form of a list: "4 Brainstorm, 4 Ponder …" main, then sideboard.
function listText(cards) {
  const part = sb => cards.filter(c => c.sideboard === sb).map(c => `${c.qty} ${c.name}`).join(', ');
  return { main: part(false), side: part(true) };
}
function diffText(diff) { return diff.map(([n, sb, a, b]) => `${sb ? 'SB ' : ''}${b > a ? '+' : '−'}${Math.abs(b - a)} ${n}`).join(', '); }
function statsLink(col, val) { return `index.html#${encodeURIComponent(col)}=${encodeURIComponent(val)}`; }
