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
// Card facts and images: names are resolved in one Scryfall batch call (75 per request, the per-card
// `cards/named` route throttles a grid's burst) and cached in localStorage. A card is
// { img, cost, mv, type, pow, kw, pm, text }: `type` names every face ("Instant // Land"), the
// rest describe the front face; `pm` is the colours of mana any face makes.
const CARD_KEY = 'mtgctl.cards';
let cardCache = null;
function cardCacheGet() {
  if (!cardCache) { try { cardCache = JSON.parse(localStorage.getItem(CARD_KEY) || '{}'); localStorage.removeItem('mtgctl.img'); } catch (e) { cardCache = {}; } }
  return cardCache;
}
async function resolveCards(names) {
  const cache = cardCacheGet();
  const missing = [...new Set(names.filter(n => !cache[n]))];
  for (let i = 0; i < missing.length; i += 75) {
    const chunk = missing.slice(i, i + 75);
    try {
      const r = await fetch('https://api.scryfall.com/cards/collection', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ identifiers: chunk.map(n => ({ name: n.split(' // ')[0] })) }) });
      const d = await r.json();
      for (const c of d.data || []) {
        const face = (c.card_faces || [])[0] || {};
        const key = chunk.find(n => n.split(' // ')[0].toLowerCase() === c.name.split(' // ')[0].toLowerCase()) || c.name;
        cache[key] = {
          img: (c.image_uris || face.image_uris || {}).small, cost: c.mana_cost || face.mana_cost || '', mv: c.cmc || 0, type: c.type_line || '',
          pow: c.power !== undefined ? c.power : face.power, kw: c.keywords || [], pm: (c.produced_mana || []).join(''),
          text: (c.oracle_text !== undefined ? c.oracle_text : face.oracle_text || '').slice(0, 700),
        };
      }
    } catch (e) { report({ error: 'scryfall: ' + e.message }); }
  }
  try { localStorage.setItem(CARD_KEY, JSON.stringify(cache)); } catch (e) {}
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
  const cache = await resolveCards(imgs.map(i => i.dataset.name));
  for (const i of imgs) { const u = (cache[i.dataset.name] || {}).img; if (u) i.src = u; else i.alt = i.dataset.name + ' (no image)'; delete i.dataset.name; }
}
// Compact text form of a list: "4 Brainstorm, 4 Ponder …" main, then sideboard.
function listText(cards) {
  const part = sb => cards.filter(c => c.sideboard === sb).map(c => `${c.qty} ${c.name}`).join(', ');
  return { main: part(false), side: part(true) };
}
function diffText(diff) { return diff.map(([n, sb, a, b]) => `${sb ? 'SB ' : ''}${b > a ? '+' : '−'}${Math.abs(b - a)} ${n}`).join(', '); }
function esc(s) { return String(s).replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c])); }
function statsLink(col, val) { return `index.html#${encodeURIComponent(col)}=${encodeURIComponent(val)}`; }

// Every <pre class="copy"> gets a copy button; the block's text goes to the clipboard as is.
document.addEventListener('DOMContentLoaded', () => {
  document.querySelectorAll('pre.copy').forEach(pre => {
    const wrap = document.createElement('div'); wrap.className = 'copywrap'; pre.parentNode.insertBefore(wrap, pre); wrap.appendChild(pre);
    const b = document.createElement('button'); b.type = 'button'; b.textContent = 'copy'; wrap.appendChild(b);
    b.onclick = async () => { try { await navigator.clipboard.writeText(pre.textContent.trim() + '\n'); b.textContent = 'copied'; } catch (e) { b.textContent = 'select it'; } setTimeout(() => b.textContent = 'copy', 1500); };
  });
});

// The hand-mark editor (marks.rs): a popover under `anchor` for one field of `row` (a match or
// game row with match_id, the *_source columns, note and excluded); `onSaved` re-renders.
const FORMATS = ['Legacy', 'Vintage', 'Modern', 'Pauper', 'Premodern', 'Standard', 'Pioneer', 'Commander', 'Limited', 'Freeform'];
let DECKS = null;
function closeMarkEditor() { const e = document.getElementById('markEd'); if (e) e.remove(); }
async function markEditor(anchor, row, onSaved) {
  closeMarkEditor();
  const field = anchor.dataset.mk, id = row.match_id;
  const sel = v => (v ? ' selected' : '');
  let inner;
  if (field === 'opp_deck' || field === 'my_deck') {
    if (!DECKS) DECKS = await api('api/decks');
    const groups = {}; DECKS.forEach(d => (groups[d.archetype] = groups[d.archetype] || []).push(d.deck));
    const cur = row[field + '_source'] === 'mark' ? row[field] : '';
    inner = `<label>${field === 'opp_deck' ? 'opponent deck' : 'my deck'} <select id="mkVal"><option value="">(no mark: the inferred deck)</option>${Object.keys(groups).sort().map(a => `<optgroup label="${esc(a)}">${groups[a].sort().map(d => `<option${sel(d === cur)}>${esc(d)}</option>`).join('')}</optgroup>`).join('')}</select></label>`;
  } else if (field === 'format') {
    const cur = row.format_source === 'mark' ? row.format : '';
    inner = `<label>format <select id="mkVal"><option value="">(no mark: the session log's, else the card test)</option>${FORMATS.map(f => `<option${sel(f === cur)}>${f}</option>`).join('')}</select></label>`;
  } else {
    inner = `<label>note <input id="mkVal" value="${esc(row.note || '')}" size="40"></label>`;
  }
  inner += `<label><input type="checkbox" id="mkExcl"${row.excluded ? ' checked' : ''}> exclude this match from the stats</label><div><button id="mkSave">save</button> <button id="mkCancel">cancel</button></div>`;
  const ed = document.createElement('div'); ed.id = 'markEd'; ed.className = 'box'; ed.innerHTML = inner;
  document.body.appendChild(ed);
  const r = anchor.getBoundingClientRect();
  ed.style.left = `${Math.min(window.scrollX + r.left, window.scrollX + window.innerWidth - ed.offsetWidth - 16)}px`;
  ed.style.top = `${window.scrollY + r.bottom + 4}px`;
  ed.querySelector('#mkCancel').onclick = closeMarkEditor;
  const post = (field, value) => api('api/mark', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ match_id: id, field, value }) });
  ed.querySelector('#mkSave').onclick = async () => {
    const v = ed.querySelector('#mkVal').value.trim();
    const excl = ed.querySelector('#mkExcl').checked;
    const marked = field === 'note' ? !!row.note : row[field + '_source'] === 'mark';
    try {
      if (v || marked) await post(field, v || null);
      if (excl !== !!row.excluded) await post('exclude', excl ? '1' : null);
      closeMarkEditor(); onSaved();
    } catch (e) { alert(e.message); }
  };
}
