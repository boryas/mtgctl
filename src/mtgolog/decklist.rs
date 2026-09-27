//! Exact registered decklists from the MTGO client's session log (`mtgo.log`):
//! `HH:MM:SS [INF] (Twitch Info|Username: X Deck Used in Game ID: N) [{"CatalogId":..,"Quantity":..,"InSideboard":..},…]`
//! The session log is recreated per client start, so the launcher archives it; this module reads the
//! archives plus the live file, resolves catalog ids to names (names already seen in the game logs,
//! else Scryfall's /cards/mtgo/<id>, cached), dedupes identical lists, and links each to the imported
//! game whose start time matches (validated by containment of the cards actually played).

use super::MtgoConfig;
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, CellAlignment, Table};
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Bool, Integer, Nullable, Text};
use diesel::sqlite::SqliteConnection;
use regex::Regex;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

diesel::table! {
    mtgo_lists (id) {
        id -> Integer,
        hash -> Text,
        name -> Nullable<Text>,
        auto_name -> Nullable<Text>,
        n_main -> Integer,
        n_side -> Integer,
        first_seen -> Text,
        last_seen -> Text,
    }
}

diesel::table! {
    mtgo_list_cards (id) {
        id -> Integer,
        list_id -> Integer,
        catalog_id -> Integer,
        name -> Text,
        qty -> Integer,
        sideboard -> Bool,
    }
}

diesel::table! {
    mtgo_decklists (id) {
        id -> Integer,
        mtgo_game_id -> BigInt,
        mtgo_match_id -> Nullable<BigInt>,
        seen_at -> Text,
        source_file -> Text,
        list_id -> Integer,
        game_db_id -> Nullable<Integer>,
        match_db_id -> Nullable<Integer>,
        event_kind -> Nullable<Text>,
    }
}

diesel::table! {
    mtgo_cards (catalog_id) {
        catalog_id -> Integer,
        name -> Text,
        source -> Text,
    }
}

pub const DDL: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS mtgo_lists (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        me TEXT,                            -- the player whose list it is; lineage and marks are theirs
        hash TEXT NOT NULL,                 -- canonical (name, qty, sideboard) multiset, unique per me
        name TEXT,                          -- user-given name
        auto_name TEXT,                     -- Archetype/Subtype v2 (08-21): lineage + version + first-seen
        fork TEXT,                          -- concept declared at this list (mtgo fork); NULL = inherit from parent
        fork_experimental BOOLEAN NOT NULL DEFAULT 0, -- the declared concept is a throwaway brew
        parent_set INTEGER,                 -- mtgo reparent: NULL = auto, 0 = root, else a list id
        parent_id INTEGER,                  -- derived, see resolve_lineage
        concept TEXT,                       -- derived
        experimental BOOLEAN NOT NULL DEFAULT 0, -- derived
        n_main INTEGER NOT NULL,
        n_side INTEGER NOT NULL,
        first_seen TEXT NOT NULL,
        last_seen TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS mtgo_list_cards (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        list_id INTEGER NOT NULL REFERENCES mtgo_lists(id),
        catalog_id INTEGER NOT NULL,
        name TEXT NOT NULL,
        qty INTEGER NOT NULL,
        sideboard BOOLEAN NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS mtgo_decklists (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        me TEXT,                            -- whose session log the line is from
        mtgo_game_id INTEGER NOT NULL,      -- MTGO's numeric game id from the log line, unique per me
        mtgo_match_id INTEGER,              -- MTGO's numeric match id (== mtgo_game_history Id)
        seen_at TEXT NOT NULL,              -- timestamp of the Deck Used line
        source_file TEXT NOT NULL,
        list_id INTEGER NOT NULL REFERENCES mtgo_lists(id),
        game_db_id INTEGER,                 -- linked mtgo_games.id
        match_db_id INTEGER,                -- linked mtgo_matches.id
        event_kind TEXT,                    -- e.g. League / Tournament / Constructed (from the duel-scene line)
        event_name TEXT                     -- 'League' or the tournament name ('Legacy Challenge 32')
    )",
    "CREATE TABLE IF NOT EXISTS mtgo_cards (
        catalog_id INTEGER PRIMARY KEY,
        name TEXT NOT NULL,
        source TEXT NOT NULL                -- seen | scryfall | unknown
    )",
    "CREATE INDEX IF NOT EXISTS idx_mtgo_list_cards_list ON mtgo_list_cards(list_id)",
];

/// Created after `db::ensure_schema` has added `me` to databases from before it.
pub const INDEXES: &[&str] = &[
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_mtgo_lists_me_hash ON mtgo_lists(me, hash)",
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_mtgo_decklists_me_game ON mtgo_decklists(me, mtgo_game_id)",
];

fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Delete one player's lists and the Deck Used lines naming them.
pub fn delete_lists(conn: &mut SqliteConnection, me: &str) -> QueryResult<()> {
    let me = sq(me);
    diesel::sql_query(format!("DELETE FROM mtgo_list_cards WHERE list_id IN (SELECT id FROM mtgo_lists WHERE me = {me})")).execute(conn)?;
    diesel::sql_query(format!("DELETE FROM list_assignments WHERE list_id IN (SELECT id FROM mtgo_lists WHERE me = {me})")).execute(conn)?;
    diesel::sql_query(format!("DELETE FROM mtgo_decklists WHERE me = {me}")).execute(conn)?;
    diesel::sql_query(format!("DELETE FROM mtgo_lists WHERE me = {me}")).execute(conn)?;
    Ok(())
}

#[derive(Deserialize)]
struct DeckEntry {
    #[serde(rename = "CatalogId")]
    catalog_id: i64,
    #[serde(rename = "Quantity")]
    quantity: i64,
    #[serde(rename = "InSideboard")]
    in_sideboard: bool,
}

struct DeckUsed {
    at: NaiveDateTime,
    game_id: i64,
    entries: Vec<DeckEntry>,
    source: String,
    match_id: Option<i64>,
    event_kind: Option<String>,
    /// "League", or the tournament's name ("Legacy Challenge 32"), from the session log
    event_name: Option<String>,
}

static TS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d\d):(\d\d):(\d\d) ").unwrap());
static DECK_USED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"Deck Used in Game ID: (\d+)\) (\[.*\])\s*$").unwrap());
static STATUS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"Game Play Status Update for Game ID: (\d+), Match ID: (\d+)").unwrap());
static SCENE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"CreateAndFlipToDuelScene\) Details: (\w+)").unwrap());
static FNAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"mtgo-(\d{4})(\d\d)(\d\d)-").unwrap());

/// All session logs: archived copies plus the live file(s) inside the MTGO profile.
pub fn session_logs(cfg: &MtgoConfig) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let archive = cfg.session_logs_dir();
    if let Ok(rd) = std::fs::read_dir(&archive) {
        for e in rd.flatten() {
            let p = e.path();
            let n = p.file_name().and_then(|x| x.to_str()).unwrap_or("");
            if n.ends_with(".log") || n.ends_with(".log.gz") {
                out.push(p);
            }
        }
    }
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > 8 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, depth + 1, out);
            } else if p.file_name().and_then(|n| n.to_str()) == Some("mtgo.log") {
                out.push(p);
            }
        }
    }
    walk(&cfg.log_dir(), 0, &mut out);
    out.sort();
    out
}

/// Parse one session log. Lines carry only HH:MM:SS; the date comes from the file's mtime (archived
/// copies are named after it) minus the number of midnight rollovers observed in the file.
/// The lines a session log is kept for: registered lists, game/match status (life totals and
/// zone counts live in its JSON), the duel-scene kind, the event descriptors (`EventId=` /
/// `Description=` name an event; a match block's `Description=Tournament:<id> …` or league text
/// says which event a game belongs to), plus one line per hour so the midnight rollover count in
/// parse_session_log still sees a dense clock. ~1% of the raw file.
pub fn session_extract(text: &str) -> String {
    let mut out = String::new();
    let mut last_hour: Option<&str> = None;
    for line in text.lines() {
        let hour = TS.captures(line).map(|_| &line[..2]);
        // substring tests, not the parsing regexes: the collector page applies the same tests, so
        // both sides produce byte-identical extracts for the same raw log
        let keep = line.contains("Deck Used in Game ID:") || line.contains("Game Play Status Update for Game ID:") || line.contains("CreateAndFlipToDuelScene")
            || line.starts_with("Description=") || line.starts_with("EventId=") || line.contains("Deck Used to Join Event ID:")
            || (hour.is_some() && hour != last_hour);
        if keep {
            out.push_str(line);
            out.push('\n');
        }
        if hour.is_some() {
            last_hour = hour;
        }
    }
    out
}

/// Read a session log, gunzipping `.gz` extracts.
pub(crate) fn read_session_log(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    if path.extension().and_then(|x| x.to_str()) == Some("gz") {
        use std::io::Read;
        let mut s = String::new();
        flate2::read::GzDecoder::new(&bytes[..]).read_to_string(&mut s).ok()?;
        Some(s)
    } else {
        String::from_utf8(bytes).ok()
    }
}

/// Dates a session log's `HH:MM:SS` lines: the file name (archived copies are named after the
/// session's end) or else its mtime gives the last day, and each midnight rollover observed in the
/// file moves the first day back by one. Feed it every line in order.
pub(crate) struct SessionClock {
    date: NaiveDate,
    last: Option<NaiveTime>,
}

impl SessionClock {
    pub(crate) fn new(path: &Path, text: &str) -> Self {
        let end_date = session_end_date(path);
        let mut last: Option<NaiveTime> = None;
        let mut rollovers = 0;
        for line in text.lines() {
            if let Some(t) = line_time(line) {
                if last.is_some_and(|l| t < l && (l - t).num_hours() > 12) {
                    rollovers += 1;
                }
                last = Some(t);
            }
        }
        Self { date: end_date - chrono::Days::new(rollovers), last: None }
    }

    /// The line's timestamp, None for an untimed line.
    pub(crate) fn stamp(&mut self, line: &str) -> Option<NaiveDateTime> {
        let t = line_time(line)?;
        if self.last.is_some_and(|l| t < l && (l - t).num_hours() > 12) {
            self.date = self.date + chrono::Days::new(1);
        }
        self.last = Some(t);
        Some(NaiveDateTime::new(self.date, t))
    }
}

fn line_time(line: &str) -> Option<NaiveTime> {
    let c = TS.captures(line)?;
    NaiveTime::from_hms_opt(c[1].parse().ok()?, c[2].parse().ok()?, c[3].parse().ok()?)
}

/// The day a session log ended: from an archived name, else the file's mtime.
pub(crate) fn session_end_date(path: &Path) -> NaiveDate {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    FNAME
        .captures(name)
        .and_then(|c| NaiveDate::from_ymd_opt(c[1].parse().ok()?, c[2].parse().ok()?, c[3].parse().ok()?))
        .or_else(|| std::fs::metadata(path).and_then(|m| m.modified()).ok().map(|t| chrono::DateTime::<chrono::Local>::from(t).date_naive()))
        .unwrap_or_else(|| chrono::Local::now().date_naive())
}

fn parse_session_log(path: &Path) -> Vec<DeckUsed> {
    let Some(text) = read_session_log(path) else { return Vec::new() };
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let mut clock = SessionClock::new(path, &text);
    let mut out: Vec<DeckUsed> = Vec::new();
    let mut game_match: HashMap<i64, i64> = HashMap::new();
    let mut pending_scene: Option<usize> = None; // index in out awaiting its "Details:" line
    // event id → name, from an `EventId=N` line followed by its `Description=`; the match block
    // before a Deck Used line says `Description=Tournament:N Round:R` or the league blurb.
    let mut event_names: HashMap<i64, String> = HashMap::new();
    let mut pending_event_id: Option<i64> = None;
    let mut current_event: Option<String> = None;
    for line in text.lines() {
        if let Some(id) = line.strip_prefix("EventId=").and_then(|s| s.trim().parse::<i64>().ok()) {
            pending_event_id = Some(id);
            continue;
        }
        if let Some(desc) = line.strip_prefix("Description=") {
            let desc = desc.trim();
            if let Some(id) = pending_event_id.take() {
                if !desc.is_empty() && !desc.starts_with("Tournament:") {
                    event_names.insert(id, desc.to_string());
                }
            }
            current_event = if desc.starts_with("Play up to") {
                Some("League".to_string())
            } else if let Some(rest) = desc.strip_prefix("Tournament:") {
                let id: Option<i64> = rest.split_whitespace().next().and_then(|s| s.parse().ok());
                Some(id.and_then(|i| event_names.get(&i).cloned()).unwrap_or_else(|| format!("Tournament {}", id.unwrap_or(0))))
            } else if desc.is_empty() {
                current_event.clone()
            } else {
                Some(desc.to_string())
            };
            continue;
        }
        let Some(at) = clock.stamp(line) else {
            continue;
        };
        if let Some(m) = STATUS.captures(line) {
            game_match.insert(m[1].parse().unwrap(), m[2].parse().unwrap());
            continue;
        }
        if let Some(idx) = pending_scene {
            if let Some(m) = SCENE.captures(line) {
                out[idx].event_kind = Some(m[1].to_string());
                pending_scene = None;
                continue;
            }
        }
        if let Some(m) = DECK_USED.captures(line) {
            let Ok(entries) = serde_json::from_str::<Vec<DeckEntry>>(&m[2]) else { continue };
            let game_id: i64 = m[1].parse().unwrap();
            // MTGO logs the same deck twice when sideboarding is confirmed; keep the last one per game
            if let Some(prev) = out.iter().position(|d| d.game_id == game_id) {
                out.remove(prev);
            }
            out.push(DeckUsed { at, game_id, entries, source: name.to_string(), match_id: None, event_kind: None, event_name: current_event.clone() });
            pending_scene = Some(out.len() - 1);
        }
    }
    for d in out.iter_mut() {
        d.match_id = game_match.get(&d.game_id).copied();
    }
    out
}

#[derive(QueryableByName)]
struct NameRow {
    #[diesel(sql_type = Integer)]
    catalog_id: i32,
    #[diesel(sql_type = Text)]
    name: String,
}

fn front_face(name: &str) -> String {
    name.split(" // ").next().unwrap_or(name).trim().to_string()
}

/// MTGO's own card database: `CardDataSource/CARDNAME_STRING.xml` (string table) and
/// `client_<SET>.xml` (DigitalObject → CARDNAME_STRING id, or CLONE_ID for foil/alt printings).
fn client_card_names(cfg: &MtgoConfig, wanted: &BTreeSet<i64>) -> HashMap<i64, String> {
    let mut out = HashMap::new();
    // newest CardDataSource under the profile
    let mut dirs: Vec<PathBuf> = Vec::new();
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > 6 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().and_then(|n| n.to_str()) == Some("CardDataSource") {
                    out.push(p);
                } else {
                    walk(&p, depth + 1, out);
                }
            }
        }
    }
    walk(&cfg.log_dir(), 0, &mut dirs);
    dirs.sort_by_key(|d| std::fs::metadata(d.join("CARDNAME_STRING.xml")).and_then(|m| m.modified()).ok());
    let Some(dir) = dirs.last() else { return out };
    let Ok(names_xml) = std::fs::read_to_string(dir.join("CARDNAME_STRING.xml")) else { return out };
    static NAME_ITEM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"<CARDNAME_STRING_ITEM id="([^"]+)">([^<]*)</CARDNAME_STRING_ITEM>"#).unwrap());
    let strings: HashMap<&str, &str> = NAME_ITEM.captures_iter(&names_xml).map(|c| (c.get(1).unwrap().as_str(), c.get(2).unwrap().as_str())).collect();
    static OBJ: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"<DigitalObject DigitalObjectCatalogID="DOC_(\d+)">(.*?)</DigitalObject>"#).unwrap());
    static NAME_REF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"<CARDNAME_STRING id="([^"]+)"/>"#).unwrap());
    static CLONE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"<CLONE_ID value="(\d+)"/>"#).unwrap());
    let mut name_of: HashMap<i64, String> = HashMap::new();
    let mut clone_of: HashMap<i64, i64> = HashMap::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            let fname = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !fname.starts_with("client_") || !fname.ends_with(".xml") {
                continue;
            }
            let Ok(xml) = std::fs::read_to_string(&p) else { continue };
            let xml = xml.replace('\n', " ");
            for c in OBJ.captures_iter(&xml) {
                let id: i64 = c[1].parse().unwrap_or(0);
                let body = &c[2];
                if let Some(n) = NAME_REF.captures(body) {
                    if let Some(name) = strings.get(&n[1]) {
                        name_of.insert(id, html_unescape(name));
                    }
                }
                if let Some(cl) = CLONE.captures(body) {
                    clone_of.insert(id, cl[1].parse().unwrap_or(0));
                }
            }
        }
    }
    for id in wanted {
        let mut cur = *id;
        for _ in 0..5 {
            if let Some(n) = name_of.get(&cur) {
                out.insert(*id, front_face(n));
                break;
            }
            match clone_of.get(&cur) {
                Some(c) => cur = *c,
                None => break,
            }
        }
    }
    out
}

fn html_unescape(s: &str) -> String {
    s.replace("&amp;", "&").replace("&quot;", "\"").replace("&apos;", "'").replace("&lt;", "<").replace("&gt;", ">")
}

/// catalog id → name from the cache table and the names seen in imported games, writing nothing
/// (safe while a sync holds the database).
pub fn known_names(conn: &mut SqliteConnection) -> QueryResult<HashMap<i64, String>> {
    let mut map: HashMap<i64, String> = HashMap::new();
    for r in diesel::sql_query("SELECT DISTINCT catalog_id, name FROM mtgo_game_cards").load::<NameRow>(conn)? {
        map.insert(r.catalog_id as i64, r.name);
    }
    for r in diesel::sql_query("SELECT catalog_id, name FROM mtgo_cards WHERE source <> 'unknown'").load::<NameRow>(conn)? {
        map.insert(r.catalog_id as i64, front_face(&r.name));
    }
    Ok(map)
}

/// catalog id → name: cache table, then names seen in imported games, then MTGO's CardDataSource,
/// then Scryfall (curl). DFC names are normalised to the front face.
pub fn resolve_names(conn: &mut SqliteConnection, cfg: &MtgoConfig, ids: &BTreeSet<i64>, fetch: bool) -> QueryResult<HashMap<i64, String>> {
    let mut map: HashMap<i64, String> = HashMap::new();
    let cached: Vec<NameRow> = diesel::sql_query("SELECT catalog_id, name FROM mtgo_cards WHERE source <> 'unknown'").load(conn)?;
    for r in cached {
        map.insert(r.catalog_id as i64, front_face(&r.name));
    }
    let seen: Vec<NameRow> = diesel::sql_query("SELECT DISTINCT catalog_id, name FROM mtgo_game_cards").load(conn)?;
    for r in seen {
        if !map.contains_key(&(r.catalog_id as i64)) {
            diesel::sql_query(format!("INSERT OR IGNORE INTO mtgo_cards(catalog_id, name, source) VALUES ({}, '{}', 'seen')", r.catalog_id, r.name.replace('\'', "''"))).execute(conn)?;
            map.insert(r.catalog_id as i64, r.name);
        }
    }
    let missing: BTreeSet<i64> = ids.iter().copied().filter(|id| !map.contains_key(id)).collect();
    if !missing.is_empty() {
        let client = client_card_names(cfg, &missing);
        for (id, name) in client {
            diesel::sql_query(format!("INSERT OR REPLACE INTO mtgo_cards(catalog_id, name, source) VALUES ({id}, '{}', 'client')", name.replace('\'', "''"))).execute(conn)?;
            map.insert(id, name);
        }
    }
    let missing: Vec<i64> = ids.iter().copied().filter(|id| !map.contains_key(id)).collect();
    if !missing.is_empty() && fetch {
        eprintln!("resolving {} catalog ids via Scryfall…", missing.len());
        for id in &missing {
            let out = std::process::Command::new("curl").args(["-s", "--max-time", "10", &format!("https://api.scryfall.com/cards/mtgo/{id}")]).output();
            let name = out.ok().and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok()).and_then(|v| v.get("name").and_then(|n| n.as_str()).map(front_face));
            match name {
                Some(n) => {
                    diesel::sql_query(format!("INSERT OR REPLACE INTO mtgo_cards(catalog_id, name, source) VALUES ({id}, '{}', 'scryfall')", n.replace('\'', "''"))).execute(conn)?;
                    map.insert(*id, n);
                }
                None => {
                    diesel::sql_query(format!("INSERT OR IGNORE INTO mtgo_cards(catalog_id, name, source) VALUES ({id}, 'mtgo#{id}', 'unknown')")).execute(conn)?;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(120));
        }
    }
    Ok(map)
}

#[derive(QueryableByName, Clone)]
struct GameStart {
    #[diesel(sql_type = Integer)]
    id: i32,
    #[diesel(sql_type = Integer)]
    match_id: i32,
    #[diesel(sql_type = Text)]
    start_ts: String,
}

#[derive(QueryableByName)]
struct PlayedRow {
    #[diesel(sql_type = Text)]
    name: String,
}

#[derive(QueryableByName)]
struct IdRow {
    #[diesel(sql_type = Integer)]
    id: i32,
}

pub struct DecklistStats {
    pub lines: usize,
    pub lists: usize,
    pub linked: usize,
    pub unlinked: usize,
    pub mismatched: usize,
    pub unknown_names: usize,
}

/// Rebuild `me`'s lists from the session logs `cfg` names and link them to `me`'s games.
pub fn run_decklists(conn: &mut SqliteConnection, cfg: &MtgoConfig, me: &str, fetch: bool, verbose: bool) -> QueryResult<DecklistStats> {
    super::db::ensure_schema(conn)?;
    for stmt in DDL {
        diesel::sql_query(*stmt).execute(conn)?;
    }
    if diesel::sql_query("SELECT auto_name FROM mtgo_lists LIMIT 0").execute(conn).is_err() {
        diesel::sql_query("ALTER TABLE mtgo_lists ADD COLUMN auto_name TEXT").execute(conn)?;
    }
    let mut all: Vec<DeckUsed> = Vec::new();
    let timing = std::env::var_os("MTGCTL_TIMING").is_some();
    let t0 = std::time::Instant::now();
    for p in session_logs(cfg) {
        all.extend(parse_session_log(&p));
    }
    // de-dupe across archived copies of the same session (same game id) — keep the latest seen
    let mut by_game: BTreeMap<i64, DeckUsed> = BTreeMap::new();
    for d in all {
        by_game.insert(d.game_id, d);
    }
    let ids: BTreeSet<i64> = by_game.values().flat_map(|d| d.entries.iter().map(|e| e.catalog_id)).collect();
    if timing { eprintln!("decklists: parse sessions {:.1}s", t0.elapsed().as_secs_f64()); }
    let t1 = std::time::Instant::now();
    let names = resolve_names(conn, cfg, &ids, fetch)?;
    if timing { eprintln!("decklists: resolve names {:.1}s", t1.elapsed().as_secs_f64()); }
    let t2 = std::time::Instant::now();
    // Lists are kept by (me, hash), so their ids and the marks on them (name, fork, experimental)
    // survive; the Deck Used lines and links are rebuilt, and lists no line names any more dropped.
    // one transaction: hundreds of inserts and lookups, each its own fsync otherwise
    let st = conn.transaction::<_, diesel::result::Error, _>(|conn| {
        let mine = format!("SELECT id FROM mtgo_matches WHERE me = {}", sq(me));
        diesel::sql_query(format!("DELETE FROM mtgo_decklists WHERE me = {}", sq(me))).execute(conn)?;
        let mut seen_lists: HashSet<i32> = HashSet::new();
        diesel::sql_query(format!("UPDATE mtgo_games SET list_id = NULL WHERE match_id IN ({mine})")).execute(conn)?;
        diesel::sql_query(format!("UPDATE mtgo_matches SET my_list_id = NULL WHERE id IN ({mine})")).execute(conn)?;
        let unknown_names = ids.iter().filter(|id| !names.contains_key(id)).count();

        let games: Vec<GameStart> = diesel::sql_query(format!("SELECT g.id, g.match_id, g.start_ts FROM mtgo_games g JOIN mtgo_matches m ON m.id = g.match_id WHERE m.replay = 0 AND m.me = {}", sq(me))).load(conn)?;
        let mut game_times: Vec<(NaiveDateTime, GameStart)> = games.into_iter().filter_map(|g| NaiveDateTime::parse_from_str(&g.start_ts, "%Y-%m-%d %H:%M:%S").ok().map(|t| (t, g))).collect();
        game_times.sort_by_key(|(t, _)| *t);

        let mut st = DecklistStats { lines: by_game.len(), lists: 0, linked: 0, unlinked: 0, mismatched: 0, unknown_names };
        let mut touched_matches: HashSet<i32> = HashSet::new();
        for d in by_game.values() {
            // canonical list hash
            // merge printings of the same card: (name, sideboard) → (qty, lowest catalog id)
            let mut merged: BTreeMap<(String, bool), (i64, i64)> = BTreeMap::new();
            for e in &d.entries {
                let name = names.get(&e.catalog_id).cloned().unwrap_or_else(|| format!("mtgo#{}", e.catalog_id));
                let ent = merged.entry((name, e.in_sideboard)).or_insert((0, e.catalog_id));
                ent.0 += e.quantity;
                ent.1 = ent.1.min(e.catalog_id);
            }
            let cards: Vec<(String, i64, bool, i64)> = merged.into_iter().map(|((n, sb), (q, cid))| (n, q, sb, cid)).collect();
            let key = cards.iter().map(|(n, q, sb, _)| format!("{n}|{q}|{}", *sb as u8)).collect::<Vec<_>>().join(";");
            let hash = format!("{:016x}", fxhash(&key));
            let n_main: i64 = cards.iter().filter(|c| !c.2).map(|c| c.1).sum();
            let n_side: i64 = cards.iter().filter(|c| c.2).map(|c| c.1).sum();
            let seen_at = d.at.format("%Y-%m-%d %H:%M:%S").to_string();
            let existing: Option<IdRow> = diesel::sql_query(format!("SELECT id FROM mtgo_lists WHERE hash = '{hash}' AND me = {}", sq(me))).get_result(conn).optional()?;
            let list_id = match existing {
                Some(r) => {
                    // the first line naming a kept list this run resets its dates
                    let (first, last) = if seen_lists.insert(r.id) { (format!("'{seen_at}'"), format!("'{seen_at}'")) } else { (format!("MIN(first_seen, '{seen_at}')"), format!("MAX(last_seen, '{seen_at}')")) };
                    diesel::sql_query(format!("UPDATE mtgo_lists SET first_seen = {first}, last_seen = {last} WHERE id = {}", r.id)).execute(conn)?;
                    r.id
                }
                None => {
                    diesel::sql_query(format!("INSERT INTO mtgo_lists(me, hash, n_main, n_side, first_seen, last_seen) VALUES ({}, '{hash}', {n_main}, {n_side}, '{seen_at}', '{seen_at}')", sq(me))).execute(conn)?;
                    let r: IdRow = diesel::sql_query("SELECT last_insert_rowid() AS id").get_result(conn)?;
                    for (name, qty, sb, cid) in &cards {
                        diesel::sql_query(format!("INSERT INTO mtgo_list_cards(list_id, catalog_id, name, qty, sideboard) VALUES ({}, {cid}, '{}', {qty}, {})", r.id, name.replace('\'', "''"), *sb as i32)).execute(conn)?;
                    }
                    seen_lists.insert(r.id);
                    r.id
                }
            };
            // link to the imported game that starts within ±90 s
            let mut best: Option<(i64, &GameStart)> = None;
            for (t, g) in &game_times {
                let dt = (*t - d.at).num_seconds().abs();
                if dt <= 90 && best.map_or(true, |(b, _)| dt < b) {
                    best = Some((dt, g));
                }
            }
            let (mut game_db_id, mut match_db_id) = (None, None);
            if let Some((_, g)) = best {
                // validate: cards I played in that game must (mostly) be in the list
                let played: Vec<PlayedRow> = diesel::sql_query(format!("SELECT DISTINCT name FROM mtgo_game_cards WHERE game_id = {} AND side = 'me' AND played = 1", g.id)).load(conn)?;
                let list_names: HashSet<&str> = cards.iter().map(|c| c.0.as_str()).collect();
                let hits = played.iter().filter(|p| list_names.contains(p.name.as_str())).count();
                let ok = played.is_empty() || hits as f64 / played.len() as f64 >= 0.8;
                if ok {
                    game_db_id = Some(g.id);
                    match_db_id = Some(g.match_id);
                    touched_matches.insert(g.match_id);
                    st.linked += 1;
                } else {
                    st.mismatched += 1;
                    if verbose {
                        eprintln!("  game {} at {}: only {hits}/{} played cards in list — not linked", d.game_id, seen_at, played.len());
                    }
                }
            } else {
                st.unlinked += 1;
            }
            let ek = d.event_kind.as_deref().map(|e| format!("'{e}'")).unwrap_or("NULL".into());
            let en = d.event_name.as_deref().map(|e| format!("'{}'", e.replace('\'', "''"))).unwrap_or("NULL".into());
            let mid = d.match_id.map(|m| m.to_string()).unwrap_or("NULL".into());
            diesel::sql_query(format!(
                "INSERT INTO mtgo_decklists(me, mtgo_game_id, mtgo_match_id, seen_at, source_file, list_id, game_db_id, match_db_id, event_kind, event_name)
                 VALUES ({}, {}, {mid}, '{seen_at}', '{}', {list_id}, {}, {}, {ek}, {en})
                 ON CONFLICT(me, mtgo_game_id) DO UPDATE SET list_id = excluded.list_id, game_db_id = excluded.game_db_id, match_db_id = excluded.match_db_id, event_kind = COALESCE(excluded.event_kind, mtgo_decklists.event_kind), event_name = COALESCE(excluded.event_name, mtgo_decklists.event_name), mtgo_match_id = COALESCE(excluded.mtgo_match_id, mtgo_decklists.mtgo_match_id)",
                sq(me), d.game_id, d.source.replace('\'', "''"),
                game_db_id.map(|x| x.to_string()).unwrap_or("NULL".into()), match_db_id.map(|x| x.to_string()).unwrap_or("NULL".into())
            ))
            .execute(conn)?;
            if let Some(gid) = game_db_id {
                diesel::sql_query(format!("UPDATE mtgo_games SET list_id = {list_id} WHERE id = {gid}")).execute(conn)?;
            }
            if let (Some(mid), Some(_)) = (match_db_id, d.event_name.as_deref()) {
                diesel::sql_query(format!("UPDATE mtgo_matches SET event_name = {en} WHERE id = {mid}")).execute(conn)?;
            }
        }
        // match-level list = the list of its earliest linked game (the registered list)
        for mid in touched_matches {
            let first: Option<IdRow> = diesel::sql_query(format!("SELECT g.list_id AS id FROM mtgo_games g WHERE g.match_id = {mid} AND g.list_id IS NOT NULL ORDER BY g.game_number LIMIT 1")).get_result(conn).optional()?;
            if let Some(r) = first {
                diesel::sql_query(format!("UPDATE mtgo_matches SET my_list_id = {} WHERE id = {mid}", r.id)).execute(conn)?;
            }
        }
        let gone = format!("SELECT id FROM mtgo_lists WHERE me = {} AND id NOT IN ({})", sq(me), seen_lists.iter().map(|i| i.to_string()).chain(["0".to_string()]).collect::<Vec<_>>().join(", "));
        for t in ["mtgo_list_cards", "list_assignments"] {
            diesel::sql_query(format!("DELETE FROM {t} WHERE list_id IN ({gone})")).execute(conn)?;
        }
        diesel::sql_query(format!("DELETE FROM mtgo_lists WHERE id IN ({gone})")).execute(conn)?;
        st.lists = seen_lists.len();
        Ok(st)
    })?;
    if timing { eprintln!("decklists: rebuild + link {:.1}s", t2.elapsed().as_secs_f64()); }
    Ok(st)
}

/// Tag a registered list as an experimental brew (my_experimental = 1) or not.
pub fn set_experimental(conn: &mut SqliteConnection, me: &str, id: i32, on: bool) -> Result<(), String> {
    match diesel::sql_query(format!("UPDATE mtgo_lists SET experimental = {} WHERE id = {id} AND me = {}", on as i32, sq(me))).execute(conn) {
        Ok(1) => Ok(()),
        Ok(_) => Err(format!("no list {id}")),
        Err(e) => Err(e.to_string()),
    }
}

/// The `n` most-changed cards between two lists as compact tokens: "+Jace −Edge".
fn diff_tokens(conn: &mut SqliteConnection, a: i32, b: i32, n: usize) -> QueryResult<Vec<String>> {
    let load = |conn: &mut SqliteConnection, id: i32| -> QueryResult<BTreeMap<String, i32>> {
        let rows: Vec<CardRow> = diesel::sql_query(format!("SELECT name, qty, sideboard FROM mtgo_list_cards WHERE list_id = {id}")).load(conn)?;
        let mut m = BTreeMap::new();
        for r in rows {
            *m.entry(r.name).or_insert(0) += r.qty;
        }
        Ok(m)
    };
    let (ma, mb) = (load(conn, a)?, load(conn, b)?);
    let keys: BTreeSet<&String> = ma.keys().chain(mb.keys()).collect();
    let mut diffs: Vec<(i32, String)> = keys
        .into_iter()
        .filter_map(|k| {
            let d = mb.get(k).copied().unwrap_or(0) - ma.get(k).copied().unwrap_or(0);
            if d == 0 {
                return None;
            }
            let short = k.split(',').next().unwrap_or(k).trim();
            Some((d, format!("{}{}", if d > 0 { "+" } else { "−" }, short)))
        })
        .collect();
    diffs.sort_by_key(|(d, _)| std::cmp::Reverse(d.abs()));
    Ok(diffs.into_iter().take(n).map(|(_, t)| t).collect())
}

fn fxhash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[derive(QueryableByName)]
struct ListRow {
    #[diesel(sql_type = Integer)]
    id: i32,
    #[diesel(sql_type = Nullable<Text>)]
    name: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    deck: Option<String>,
    #[diesel(sql_type = Bool)]
    experimental: bool,
    #[diesel(sql_type = Integer)]
    n_main: i32,
    #[diesel(sql_type = Integer)]
    n_side: i32,
    #[diesel(sql_type = Text)]
    first_seen: String,
    #[diesel(sql_type = Text)]
    last_seen: String,
    #[diesel(sql_type = BigInt)]
    matches: i64,
    #[diesel(sql_type = BigInt)]
    games: i64,
}

pub fn print_lists(conn: &mut SqliteConnection) -> QueryResult<()> {
    for stmt in DDL {
        diesel::sql_query(*stmt).execute(conn)?;
    }
    let rows: Vec<ListRow> = diesel::sql_query(
        "SELECT l.id, COALESCE(l.name, l.auto_name) AS name, a.deck, l.experimental, l.n_main, l.n_side, l.first_seen, l.last_seen,
                (SELECT COUNT(*) FROM mtgo_matches m WHERE m.my_list_id = l.id) AS matches,
                (SELECT COUNT(*) FROM mtgo_games g WHERE g.list_id = l.id) AS games
         FROM mtgo_lists l LEFT JOIN list_assignments a ON a.list_id = l.id ORDER BY l.first_seen, l.id",
    )
    .load(conn)?;
    let mut t = Table::new();
    t.load_preset(UTF8_FULL_CONDENSED);
    t.set_header(["id", "name (my_list)", "deck (my_deck)", "exp", "main/side", "first seen", "last seen", "matches", "games"]);
    for r in &rows {
        let disp = r.name.clone().unwrap_or_else(|| format!("list#{}", r.id));
        t.add_row(vec![
            Cell::new(r.id),
            Cell::new(disp),
            Cell::new(r.deck.clone().unwrap_or_default()),
            Cell::new(if r.experimental { "✓" } else { "" }),
            Cell::new(format!("{}/{}", r.n_main, r.n_side)),
            Cell::new(&r.first_seen[..10]),
            Cell::new(&r.last_seen[..10]),
            Cell::new(r.matches).set_alignment(CellAlignment::Right),
            Cell::new(r.games).set_alignment(CellAlignment::Right),
        ]);
    }
    println!("{t}");
    println!("{} distinct lists (a list = exact main+side multiset; post-board configurations are separate lists). `mtgctl mtgo list <id>` prints one.", rows.len());
    Ok(())
}

/// Compact "+2 Wasteland −2 Bilbo, SB +1 Duress…" summary between two lists; main first, then side.
fn diff_summary(conn: &mut SqliteConnection, a: i32, b: i32) -> QueryResult<String> {
    let load = |conn: &mut SqliteConnection, id: i32| -> QueryResult<BTreeMap<(bool, String), i32>> {
        let rows: Vec<CardRow> = diesel::sql_query(format!("SELECT name, qty, sideboard FROM mtgo_list_cards WHERE list_id = {id}")).load(conn)?;
        Ok(rows.into_iter().map(|r| ((r.sideboard, r.name), r.qty)).collect())
    };
    let (ma, mb) = (load(conn, a)?, load(conn, b)?);
    let keys: BTreeSet<&(bool, String)> = ma.keys().chain(mb.keys()).collect();
    let mut parts = Vec::new();
    for k in keys {
        let d = mb.get(k).copied().unwrap_or(0) - ma.get(k).copied().unwrap_or(0);
        if d != 0 {
            parts.push(format!("{}{}{} {}", if k.0 { "SB " } else { "" }, if d > 0 { "+" } else { "−" }, d.abs(), k.1));
        }
    }
    let mut out = parts.join(", ");
    if out.chars().count() > 70 {
        out = out.chars().take(67).collect();
        out.push('…');
    }
    Ok(out)
}

#[derive(QueryableByName)]
struct CardRow {
    #[diesel(sql_type = Text)]
    name: String,
    #[diesel(sql_type = Integer)]
    qty: i32,
    #[diesel(sql_type = Bool)]
    sideboard: bool,
}

pub fn print_list(conn: &mut SqliteConnection, id: i32, diff: Option<i32>) -> QueryResult<()> {
    let load = |conn: &mut SqliteConnection, id: i32| -> QueryResult<BTreeMap<(bool, String), i32>> {
        let rows: Vec<CardRow> = diesel::sql_query(format!("SELECT name, qty, sideboard FROM mtgo_list_cards WHERE list_id = {id} ORDER BY sideboard, name")).load(conn)?;
        Ok(rows.into_iter().map(|r| ((r.sideboard, r.name), r.qty)).collect())
    };
    let a = load(conn, id)?;
    if a.is_empty() {
        println!("no list {id}");
        return Ok(());
    }
    match diff {
        None => {
            for sb in [false, true] {
                println!("{}", if sb { "\nSideboard:" } else { "Main:" });
                for ((s, n), q) in &a {
                    if *s == sb {
                        println!("  {q} {n}");
                    }
                }
            }
        }
        Some(other) => {
            let b = load(conn, other)?;
            println!("list {id} vs list {other} (− only in {id}, + only in {other}):");
            let keys: BTreeSet<&(bool, String)> = a.keys().chain(b.keys()).collect();
            for k in keys {
                let (qa, qb) = (a.get(k).copied().unwrap_or(0), b.get(k).copied().unwrap_or(0));
                if qa != qb {
                    println!("  {} {:+} {}{}", if k.0 { "SB" } else { "  " }, qb - qa, k.1, if qa > 0 && qb > 0 { format!(" ({qa}→{qb})") } else { String::new() });
                }
            }
        }
    }
    Ok(())
}
