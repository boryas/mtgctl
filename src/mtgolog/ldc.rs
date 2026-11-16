//! The Legacy Data Collection (LDC): per-event sheets of MTGO Legacy events (Challenges,
//! Showcases, qualifiers) naming each player's deck and every round's opponent and game score.
//! Kept in its own `<data>/ldc.db`, never mixed with uploaded logs, and sliced by the same stats
//! engine through `DATASET`.
//!
//! A match is stored once per side, so every row reads from its `player`'s side: a mirror shows
//! 50%, and a deck's matchup row is that deck's view of it. Sheets give game scores only, so the
//! per-game rows of `ldc_game_view` carry a result but no order, play/draw or turns.
//!
//! The sheets name decks at the LDC's own grain ("Doomsday"). For the top 32 of each Challenge
//! mtgo.com publishes the list, and `ldc_lists` places it on the reference decks the way a
//! registered list is placed (`reference::assign_one`), which splits the LDC's name further.

use super::dataset::{Column, Dataset, Features, Kind::*, Level::*};
use super::reference::{self, References};
use diesel::prelude::*;
use diesel::sql_types::Text as SqlText;
use diesel::sqlite::SqliteConnection;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// The LDC's link-shared "Public Facing Sheets" folders, one per year; no credentials needed.
const YEAR_FOLDERS: &[(&str, &str)] = &[("2026", "1hcNxR3csKUXx4367-A-CltHlWWQkgO8G")];

pub static DATASET: Dataset = Dataset {
    key: "ldc",
    title: "LDC (all players)",
    about: "Every player's matches in MTGO Legacy Challenges and other events, from the Legacy Data Collection. Each match is counted once from each player's side (A), against the other (B); scores only, so no play/draw, turns or cards. A Challenge top-32 list (published by mtggoldfish) is placed on the nearest reference deck, as a registered list is; the (reference) columns are empty for everyone else.",
    view: "ldc_game_view",
    columns: &[
        Column { name: "my_deck", label: "deck A", level: Match, kind: Text, values: &[] },
        Column { name: "my_archetype", label: "deck A's LDC archetype", level: Match, kind: Text, values: &[] },
        Column { name: "my_notes", label: "deck A note (LDC)", level: Match, kind: Text, values: &[] },
        Column { name: "my_ref_deck", label: "deck A (reference)", level: Match, kind: Text, values: &[] },
        Column { name: "my_ref_archetype", label: "deck A's archetype (reference)", level: Match, kind: Text, values: &[] },
        Column { name: "my_colors", label: "deck A's colours", level: Match, kind: Colors, values: &[] },
        Column { name: "my_list_published", label: "deck A's list published", level: Match, kind: Text, values: &[("1", "yes"), ("0", "no")] },
        Column { name: "opp_deck", label: "deck B", level: Match, kind: Text, values: &[] },
        Column { name: "opp_archetype", label: "deck B's LDC archetype", level: Match, kind: Text, values: &[] },
        Column { name: "opp_notes", label: "deck B note (LDC)", level: Match, kind: Text, values: &[] },
        Column { name: "opp_ref_deck", label: "deck B (reference)", level: Match, kind: Text, values: &[] },
        Column { name: "opp_ref_archetype", label: "deck B's archetype (reference)", level: Match, kind: Text, values: &[] },
        Column { name: "opp_colors", label: "deck B's colours", level: Match, kind: Colors, values: &[] },
        Column { name: "opp_list_published", label: "deck B's list published", level: Match, kind: Text, values: &[("1", "yes"), ("0", "no")] },
        Column { name: "player", label: "player A", level: Match, kind: Text, values: &[] },
        Column { name: "opponent", label: "player B", level: Match, kind: Text, values: &[] },
        Column { name: "match_result", label: "A's match result", level: Match, kind: Text, values: &[] },
        Column { name: "round", label: "round", level: Match, kind: Num, values: &[] },
        Column { name: "event_type", label: "event type", level: Match, kind: Text, values: &[] },
        Column { name: "event_name", label: "event", level: Match, kind: Text, values: &[] },
        Column { name: "week", label: "week", level: Match, kind: Text, values: &[] },
        Column { name: "month", label: "month", level: Match, kind: Text, values: &[] },
        Column { name: "date", label: "date", level: Match, kind: Date, values: &[] },
        Column { name: "era", label: "era", level: Match, kind: Text, values: &[] },
    ],
    game_cols: &["game_id", "won"],
    features: Features { cards: false, lists: false, rows: false, players: false, game_details: false },
    default_filters: "[]",
    start_filters: "[]",
    default_group: "opp_deck",
    connect,
    ensure: ensure_view,
};

pub fn db_path(data: &Path) -> PathBuf {
    data.join("ldc.db")
}
fn sheets_dir(data: &Path) -> PathBuf {
    data.join("ldc").join("sheets")
}
fn pages_dir(data: &Path) -> PathBuf {
    data.join("ldc").join("pages")
}
pub(super) fn lists_dir(data: &Path) -> PathBuf {
    data.join("ldc").join("lists")
}

fn connect(data: &Path) -> Result<SqliteConnection, String> {
    std::fs::create_dir_all(data).map_err(|e| e.to_string())?;
    let mut conn = crate::db::establish_connection_at(&db_path(data))?;
    diesel::sql_query("PRAGMA journal_mode = WAL").execute(&mut conn).map_err(|e| e.to_string())?;
    Ok(conn)
}

const DDL: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS ldc_events (
        event TEXT PRIMARY KEY,          -- the sheet's name, e.g. 'Legacy Challenge 32 9/6/2026 - 2'
        sheet_id TEXT NOT NULL,
        date TEXT NOT NULL,              -- YYYY-MM-DD
        event_type TEXT NOT NULL,        -- as mtgo_game_view.event_type names it
        era TEXT                         -- from the Legacy timeline, by date
    )",
    "CREATE TABLE IF NOT EXISTS ldc_players (
        event TEXT NOT NULL,
        player TEXT NOT NULL,
        player_key TEXT NOT NULL,        -- lower-cased: the sheets' pairing names differ in case
        deck TEXT,
        archetype TEXT,                  -- the LDC's macro-archetype (Tempo, Spell Combo, ...)
        notes TEXT,
        wins INTEGER,
        losses INTEGER,
        PRIMARY KEY (event, player_key)
    )",
    "CREATE TABLE IF NOT EXISTS ldc_matches (
        id INTEGER PRIMARY KEY,
        event TEXT NOT NULL,
        round INTEGER NOT NULL,
        player TEXT NOT NULL,
        opponent TEXT NOT NULL,
        game_wins INTEGER NOT NULL,
        game_losses INTEGER NOT NULL
    )",
    // one row per game a score implies: game_wins rows won, game_losses rows lost
    "CREATE TABLE IF NOT EXISTS ldc_games (
        id INTEGER PRIMARY KEY,
        match_id INTEGER NOT NULL,
        won INTEGER NOT NULL
    )",
    "CREATE INDEX IF NOT EXISTS idx_ldc_games_match ON ldc_games(match_id)",
    "CREATE TABLE IF NOT EXISTS ldc_lists (
        event TEXT NOT NULL,
        player_key TEXT NOT NULL,
        goldfish_id TEXT NOT NULL,       -- list id (see Page::rows), the list is <data>/ldc/lists/<id>.txt
        goldfish_deck TEXT NOT NULL,     -- the name mtggoldfish gave the list; '' for a list from mtgo.com
        n_main INTEGER NOT NULL,
        n_side INTEGER NOT NULL,
        deck TEXT,                       -- see reference::assign_one; NULL when no reference is near enough
        archetype TEXT,
        reference_id INTEGER,
        dist_main INTEGER NOT NULL,
        dist_side INTEGER NOT NULL,
        runner_up_deck TEXT,
        runner_up_dist INTEGER,
        colors TEXT NOT NULL,
        PRIMARY KEY (event, player_key)
    )",
];

/// One row per game per side, under the column names `mtgo_game_view` uses for the same facts.
/// The NULL columns are ones the engine's statistics read that sheets cannot supply.
pub const VIEW_SQL: &str = "CREATE VIEW ldc_game_view AS
SELECT
  g.id                AS game_id,
  m.id                AS match_id,
  e.date              AS date,
  substr(e.date,1,7)  AS month,
  strftime('%Y-W%W', e.date) AS week,
  e.era               AS era,
  e.event             AS event_name,
  e.event_type        AS event_type,
  m.round             AS round,
  m.player            AS player,
  m.opponent          AS opponent,
  p.deck              AS my_deck,
  p.archetype         AS my_archetype,
  p.notes             AS my_notes,
  o.deck              AS opp_deck,
  o.archetype         AS opp_archetype,
  o.notes             AS opp_notes,
  pl.deck             AS my_ref_deck,
  pl.archetype        AS my_ref_archetype,
  pl.colors           AS my_colors,
  pl.goldfish_id IS NOT NULL AS my_list_published,
  ol.deck             AS opp_ref_deck,
  ol.archetype        AS opp_ref_archetype,
  ol.colors           AS opp_colors,
  ol.goldfish_id IS NOT NULL AS opp_list_published,
  CASE WHEN m.game_wins > m.game_losses THEN 'W' WHEN m.game_wins < m.game_losses THEN 'L' ELSE 'D' END AS match_result,
  m.game_wins         AS my_game_wins,
  m.game_losses       AS opp_game_wins,
  m.game_wins + m.game_losses AS match_games,
  g.won               AS won,
  NULL                AS turns,
  NULL                AS duration_secs,
  NULL                AS my_mulligans,
  NULL                AS opp_mulligans
FROM ldc_games g
JOIN ldc_matches m ON m.id = g.match_id
JOIN ldc_events e ON e.event = m.event
LEFT JOIN ldc_players p ON p.event = m.event AND p.player_key = lower(m.player)
LEFT JOIN ldc_players o ON o.event = m.event AND o.player_key = lower(m.opponent)
LEFT JOIN ldc_lists pl ON pl.event = m.event AND pl.player_key = lower(m.player)
LEFT JOIN ldc_lists ol ON ol.event = m.event AND ol.player_key = lower(m.opponent)";

/// Creates the tables, and recreates the view only when its stored SQL differs from VIEW_SQL.
fn ensure_view(conn: &mut SqliteConnection) -> Result<(), String> {
    #[derive(QueryableByName)]
    struct S {
        #[diesel(sql_type = SqlText)]
        sql: String,
    }
    let cur: Option<S> = diesel::sql_query("SELECT sql FROM sqlite_master WHERE type = 'view' AND name = 'ldc_game_view'").get_result(conn).optional().map_err(|e| e.to_string())?;
    if cur.is_some_and(|c| c.sql == VIEW_SQL) {
        return Ok(());
    }
    for d in DDL {
        diesel::sql_query(*d).execute(conn).map_err(|e| e.to_string())?;
    }
    diesel::sql_query("DROP VIEW IF EXISTS ldc_game_view").execute(conn).map_err(|e| e.to_string())?;
    diesel::sql_query(VIEW_SQL).execute(conn).map_err(|e| e.to_string())?;
    Ok(())
}

pub(super) fn curl(url: &str) -> Result<Vec<u8>, String> {
    curl_with(url, None)
}

/// `jar`: a cookie file read and written by the request (mtgo.com's session cookie pins the
/// backend that answered, see curl_page).
fn curl_with(url: &str, jar: Option<&Path>) -> Result<Vec<u8>, String> {
    let mut err = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
        let mut args = vec!["-sSL", "--max-time", "60", "-A", "Mozilla/5.0"];
        let jar = jar.map(|j| j.to_string_lossy().to_string());
        if let Some(j) = &jar {
            args.extend(["-b", j, "-c", j]);
        }
        args.push(url);
        let out = std::process::Command::new("curl").args(&args).output().map_err(|e| format!("curl: {e}"))?;
        if out.status.success() {
            return Ok(out.stdout);
        }
        err = format!("{url}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Err(err)
}

/// (name, sheet id) of every sheet under a Drive folder, read from its embedded folder view.
fn list_folder(id: &str, out: &mut Vec<(String, String)>) -> Result<(), String> {
    let html = String::from_utf8_lossy(&curl(&format!("https://drive.google.com/embeddedfolderview?id={id}#list"))?).to_string();
    let entry = regex::Regex::new(r#"(?s)<a href="([^"]+)"[^>]*>.*?<div class="flip-entry-title">(.*?)</div>"#).unwrap();
    let folder = regex::Regex::new(r"/folders/([A-Za-z0-9_-]+)").unwrap();
    let sheet = regex::Regex::new(r"/(?:file/d|spreadsheets/d)/([A-Za-z0-9_-]+)").unwrap();
    for c in entry.captures_iter(&html) {
        let (href, name) = (&c[1], unescape(c[2].trim()));
        if let Some(f) = folder.captures(href) {
            list_folder(&f[1], out)?;
        } else if let Some(s) = sheet.captures(href) {
            out.push((name, s[1].to_string()));
        }
    }
    Ok(())
}

pub(super) fn unescape(s: &str) -> String {
    s.replace("&amp;", "&").replace("&#39;", "'").replace("&quot;", "\"").replace("&lt;", "<").replace("&gt;", ">")
}

/// Downloads every published sheet not extracted yet (`all`: every sheet again, for corrections
/// made after publishing) and keeps only its extract, `<data>/ldc/sheets/<id>.json`: an exported
/// sheet is ~5 MB, its players and pairings a few KB. `sheets.json` maps id → name.
pub fn fetch(data: &Path, all: bool) -> Result<usize, String> {
    let dir = sheets_dir(data);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let manifest_path = dir.join("sheets.json");
    let mut names: BTreeMap<String, String> = std::fs::read_to_string(&manifest_path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let mut listed = vec![];
    for (_, folder) in YEAR_FOLDERS {
        list_folder(folder, &mut listed)?;
    }
    let mut n = 0;
    for (name, id) in listed {
        let (xlsx, json) = (dir.join(format!("{id}.xlsx")), dir.join(format!("{id}.json")));
        names.insert(id.clone(), name.clone());
        if json.exists() && !all {
            continue;
        }
        // a spreadsheet left by an interrupted run is extracted without downloading it again
        if !xlsx.exists() || all {
            // one failed download leaves that sheet to the next sync
            let bytes = match curl(&format!("https://docs.google.com/spreadsheets/d/{id}/export?format=xlsx")) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("ldc: {name}: {e}");
                    continue;
                }
            };
            if !bytes.starts_with(b"PK") {
                eprintln!("ldc: {name}: not a spreadsheet (unpublished or moved?)");
                continue;
            }
            std::fs::write(&xlsx, bytes).map_err(|e| e.to_string())?;
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        match read_sheet(&xlsx) {
            Ok(sheet) => std::fs::write(&json, serde_json::to_string(&sheet).unwrap()).map_err(|e| e.to_string())?,
            Err(e) => eprintln!("ldc: {name}: {e}"),
        }
        std::fs::remove_file(&xlsx).map_err(|e| e.to_string())?;
        n += 1;
    }
    std::fs::write(&manifest_path, serde_json::to_string_pretty(&names).unwrap()).map_err(|e| e.to_string())?;
    Ok(n)
}

/// YYYY-MM-DD from a sheet name's M/D/YYYY (or M_D_YYYY).
fn event_date(name: &str) -> Option<String> {
    let re = regex::Regex::new(r"(\d{1,2})[/_](\d{1,2})[/_](\d{4})").unwrap();
    let d = re.captures(name)?;
    Some(format!("{}-{:02}-{:02}", &d[3], d[1].parse::<u32>().ok()?, d[2].parse::<u32>().ok()?))
}

/// One event's published lists: `<data>/ldc/pages/<id>.json`. From mtgo.com since 2026-11 (`id` =
/// the MTGO event id, rows in final-rank order, no deck name); before that from mtggoldfish, which
/// answers list downloads with a browser challenge since 2026-11-07 (`id` = its tournament id,
/// rows in its order with its deck name).
#[derive(serde::Serialize, serde::Deserialize)]
struct Page {
    id: String,
    name: String,
    /// the event's day; goldfish dated a page by the event's end in UTC, so its Friday-evening
    /// Challenges are dated Saturday
    date: String,
    /// (list id, deck name, player); the list is `<data>/ldc/lists/<list id>.txt`
    rows: Vec<(String, String, String)>,
}

/// (slug, date, event id) of every Legacy event but the leagues on mtgo.com's decklist index for
/// the month; the slug ends in the date followed by the event id, `legacy-challenge-32-2026-11-0312857422`.
fn month_events(year: i32, month: u32, jar: &Path) -> Result<Vec<(String, String, String)>, String> {
    Ok(parse_month_index(&curl_page(&format!("https://www.mtgo.com/decklists/{year}/{month:02}"), jar, |h| !parse_month_index(h).is_empty())?))
}

/// `curl` through the cookie `jar`, repeated while `complete` rejects the body. About half of
/// mtgo.com's backends answer with the page shell and none of the data, or an event without its
/// lists; its session cookie pins the backend that answered, so a complete answer's cookie is
/// kept and an incomplete one's thrown away.
fn curl_page(url: &str, jar: &Path, complete: impl Fn(&str) -> bool) -> Result<String, String> {
    let mut html = String::new();
    for attempt in 0..6 {
        if attempt > 0 {
            let _ = std::fs::remove_file(jar);
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        html = String::from_utf8_lossy(&curl_with(url, Some(jar))?).to_string();
        if complete(&html) {
            break;
        }
    }
    Ok(html)
}

fn parse_month_index(html: &str) -> Vec<(String, String, String)> {
    let re = regex::Regex::new(r"decklist/(legacy-[a-z0-9-]*?(\d{4}-\d{2}-\d{2})(\d+))").unwrap();
    let mut out: Vec<(String, String, String)> = re.captures_iter(html).filter(|c| !c[1].contains("-league-")).map(|c| (c[1].to_string(), c[2].to_string(), c[3].to_string())).collect();
    out.sort();
    out.dedup();
    out
}

/// The page's embedded `window.MTGO.decklists.data`: the Page, rows in final-rank order, and each
/// list's text in the `parse_decklist` format by list id. Only Legacy events (`format` CLEGACY).
fn parse_event_page(html: &str) -> Result<(Page, Vec<(String, String)>), String> {
    let at = html.find("window.MTGO.decklists.data").ok_or("no decklist data on the page")?;
    let rest = &html[at..];
    let json = &rest[rest.find('{').ok_or("no decklist data on the page")?..];
    let mut de = serde_json::Deserializer::from_str(json);
    let v: serde_json::Value = serde::Deserialize::deserialize(&mut de).map_err(|e| format!("decklist data: {e}"))?;
    // ids are numbers in `decklists` and strings in the rank tables
    let s = |v: &serde_json::Value| match v {
        serde_json::Value::String(x) => x.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => String::new(),
    };
    if s(&v["format"]) != "CLEGACY" {
        return Err(format!("format {:?}", s(&v["format"])));
    }
    let mut rank: HashMap<String, i64> = HashMap::new();
    for key in ["standings", "final_rank"] {
        for r in v[key].as_array().into_iter().flatten() {
            if let Ok(n) = s(&r["rank"]).parse::<i64>() {
                rank.insert(s(&r["loginid"]), n);
            }
        }
    }
    let mut rows = vec![];
    let mut lists = vec![];
    for d in v["decklists"].as_array().ok_or("no decklists")? {
        let id = s(&d["decktournamentid"]);
        let mut text = String::new();
        for (key, side) in [("main_deck", false), ("sideboard_deck", true)] {
            let cards: Vec<_> = d[key].as_array().into_iter().flatten().collect();
            if side && !cards.is_empty() {
                text.push('\n');
            }
            for c in cards {
                text.push_str(&format!("{} {}\n", s(&c["qty"]), s(&c["card_attributes"]["card_name"])));
            }
        }
        rows.push((rank.get(&s(&d["loginid"])).copied().unwrap_or(i64::MAX), id.clone(), s(&d["player"])));
        lists.push((id, text));
    }
    rows.sort();
    let date: String = s(&v["starttime"]).chars().take(10).collect();
    let page = Page { id: s(&v["event_id"]), name: format!("{} {date}", s(&v["description"])), date, rows: rows.into_iter().map(|(_, id, p)| (id, String::new(), p)).collect() };
    Ok((page, lists))
}

fn load_pages(data: &Path) -> Result<Vec<Page>, String> {
    let mut pages = vec![];
    if let Ok(rd) = std::fs::read_dir(pages_dir(data)) {
        for e in rd.flatten() {
            let path = e.path();
            if path.extension().is_some_and(|x| x == "json") {
                pages.push(serde_json::from_str(&std::fs::read_to_string(&path).map_err(|e| e.to_string())?).map_err(|e| format!("{}: {e}", path.display()))?);
            }
        }
    }
    Ok(pages)
}

/// Downloads the published lists of the events the sheets cover: every Legacy Challenge,
/// qualifier or last-chance event on mtgo.com's monthly decklist index from a week before the
/// newest stored page (or the first sheet) to today, the page to `<data>/ldc/pages/<id>.json`
/// and its lists to `<data>/ldc/lists/<id>.txt`. Which sheet a page belongs to is decided at
/// import, see `pair_pages`. Returns (pages, lists) downloaded.
pub fn fetch_lists(data: &Path) -> Result<(usize, usize), String> {
    use chrono::Datelike;
    let (pages, lists) = (pages_dir(data), lists_dir(data));
    std::fs::create_dir_all(&pages).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&lists).map_err(|e| e.to_string())?;
    let names: BTreeMap<String, String> = std::fs::read_to_string(sheets_dir(data).join("sheets.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    // a page none of whose lists downloaded (goldfish's, after 2026-11-07) does not count as covered
    let mut stored = load_pages(data)?;
    stored.retain(|p| p.rows.iter().any(|r| lists.join(format!("{}.txt", r.0)).exists()));
    let today = chrono::Local::now().date_naive();
    let mut from = match stored.iter().map(|p| p.date.as_str()).max() {
        Some(d) => chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").map_err(|e| e.to_string())? - chrono::Days::new(7),
        None => match names.values().filter_map(|n| event_date(n)).min() {
            Some(d) => chrono::NaiveDate::parse_from_str(&d, "%Y-%m-%d").map_err(|e| e.to_string())?,
            None => return Ok((0, 0)),
        },
    };
    // a recent sheet no covered page pairs with (its page came back without lists) is fetched again
    let mut sheets: Vec<(String, String, String, Sheet)> = vec![];
    for (id, name) in &names {
        if let Some(date) = event_date(name) {
            if let Ok(d) = chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d") {
                if d + chrono::Days::new(45) >= today {
                    if let Ok(sheet) = std::fs::read_to_string(sheets_dir(data).join(format!("{id}.json"))).map_err(|e| e.to_string()).and_then(|t| serde_json::from_str::<Sheet>(&t).map_err(|e| e.to_string())) {
                        sheets.push((date, id.clone(), name.clone(), sheet));
                    }
                }
            }
        }
    }
    let paired = pair_pages(&sheets, &stored);
    for (date, _, name, _) in &sheets {
        if !paired.contains_key(name) {
            if let Ok(d) = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") {
                from = from.min(d);
            }
        }
    }
    let (mut np, mut nl) = (0, 0);
    let jar = data.join("ldc").join("mtgo-cookies.txt");
    let _ = std::fs::remove_file(&jar);
    let mut month = from.with_day(1).unwrap();
    while month <= today {
        for (slug, date, id) in month_events(month.year(), month.month(), &jar)? {
            let path = pages.join(format!("{id}.json"));
            if date < from.to_string() || path.exists() || !["challenge", "qualifier", "last-chance"].iter().any(|k| slug.contains(k)) {
                continue;
            }
            // a page that fails to download or read is left to the next sync
            let html = match curl_page(&format!("https://www.mtgo.com/decklist/{slug}"), &jar, |h| parse_event_page(h).is_ok_and(|(p, _)| !p.rows.is_empty())) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("ldc: {e}");
                    continue;
                }
            };
            std::thread::sleep(std::time::Duration::from_millis(500));
            let (page, texts) = match parse_event_page(&html) {
                Ok(x) => x,
                Err(e) => {
                    eprintln!("ldc: {slug}: {e}");
                    continue;
                }
            };
            for (lid, text) in &texts {
                let p = lists.join(format!("{lid}.txt"));
                if !p.exists() {
                    std::fs::write(&p, text).map_err(|e| e.to_string())?;
                    nl += 1;
                }
            }
            std::fs::write(&path, serde_json::to_string(&page).unwrap()).map_err(|e| e.to_string())?;
            np += 1;
        }
        month = chrono::NaiveDate::from_ymd_opt(month.year() + (month.month() == 12) as i32, month.month() % 12 + 1, 1).unwrap();
    }
    let _ = std::fs::remove_file(&jar);
    Ok((np, nl))
}

/// Each sheet's goldfish page: the page dated the event's day or the day after that shares the
/// most players with it, at least half of the page's and eight. Goldfish lists some events twice;
/// a sheet takes one page.
fn pair_pages<'a>(sheets: &[(String, String, String, Sheet)], pages: &'a [Page]) -> HashMap<String, &'a Page> {
    let players: Vec<std::collections::HashSet<String>> = sheets.iter().map(|(_, _, _, s)| s.players.iter().map(|p| p.0.to_lowercase()).collect()).collect();
    let mut out: HashMap<String, &Page> = HashMap::new();
    let mut scored: Vec<(usize, usize, &Page)> = vec![];
    for page in pages {
        let Ok(pd) = chrono::NaiveDate::parse_from_str(&page.date, "%Y-%m-%d") else { continue };
        let names: std::collections::HashSet<String> = page.rows.iter().map(|r| r.2.to_lowercase()).collect();
        for (i, (date, _, _, _)) in sheets.iter().enumerate() {
            let Ok(sd) = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") else { continue };
            if pd != sd && pd != sd + chrono::Days::new(1) {
                continue;
            }
            let shared = names.intersection(&players[i]).count();
            if shared >= 8 && shared * 2 >= names.len() {
                scored.push((shared, i, page));
            }
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.2.id.cmp(&b.2.id)));
    let mut used = std::collections::HashSet::new();
    for (_, i, page) in scored {
        let event = &sheets[i].2;
        if !out.contains_key(event) && used.insert(page.rows.iter().map(|r| r.2.to_lowercase()).collect::<std::collections::BTreeSet<_>>()) {
            out.insert(event.clone(), page);
        }
    }
    out
}

/// The same buckets `mtgo_game_view.event_type` uses, tested in the same order.
fn event_type(name: &str) -> &'static str {
    let has = |s: &str| name.contains(s);
    if has("Challenge") {
        "Challenge"
    } else if has("Preliminary") || has("Prelim") {
        "Preliminary"
    } else if has("Qualifier") {
        "Qualifier"
    } else if has("Showcase") {
        "Showcase"
    } else if has("Last Chance") {
        "Last Chance"
    } else {
        "Other"
    }
}

fn cell(r: &[calamine::Data], i: Option<usize>) -> Option<String> {
    use calamine::Data::*;
    match r.get(i?)? {
        String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Int(v) => Some(v.to_string()),
        Float(v) => Some(if v.fract() == 0.0 { format!("{}", *v as i64) } else { v.to_string() }),
        _ => None,
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Sheet {
    players: Vec<(String, Option<String>, Option<String>, Option<String>, Option<i64>, Option<i64>)>,
    /// (round, player, opponent, game wins, game losses)
    matches: Vec<(i64, String, String, i64, i64)>,
}

/// The "Input" tab (one row per player: Name, Wins, Losses, Archetype, Deck, Notes) and the
/// "Match Up Input" tab (name, deck, then an opponent and an "a-b" game score per round).
fn read_sheet(path: &Path) -> Result<Sheet, String> {
    use calamine::Reader;
    let mut wb = calamine::open_workbook_auto(path).map_err(|e| e.to_string())?;
    let input = wb.worksheet_range("Input").map_err(|e| format!("Input: {e}"))?;
    let mut rows = input.rows();
    let hdr: HashMap<String, usize> = rows.next().map(|r| r.iter().enumerate().filter_map(|(i, c)| cell(std::slice::from_ref(c), Some(0)).map(|s| (s, i))).collect()).unwrap_or_default();
    let ix = |k: &str| hdr.get(k).copied();
    let num = |r: &[calamine::Data], k: &str| cell(r, ix(k)).and_then(|s| s.parse::<f64>().ok()).map(|v| v as i64);
    let mut players = vec![];
    for r in rows {
        let Some(name) = cell(r, ix("Name")) else { continue };
        players.push((name, cell(r, ix("Deck")), cell(r, ix("Archetype")), cell(r, ix("Notes")), num(r, "Wins"), num(r, "Losses")));
    }
    let score = regex::Regex::new(r"^(\d)\s*-\s*(\d)$").unwrap();
    let mut matches = vec![];
    let mu = wb.worksheet_range("Match Up Input").map_err(|e| format!("Match Up Input: {e}"))?;
    for r in mu.rows().skip(1) {
        let Some(name) = cell(r, Some(0)) else { continue };
        let mut round = 0;
        let mut i = 2;
        while i + 1 < r.len() {
            round += 1;
            if let (Some(opp), Some(s)) = (cell(r, Some(i)), cell(r, Some(i + 1))) {
                if let Some(m) = score.captures(&s) {
                    matches.push((round, name.clone(), opp, m[1].parse().unwrap(), m[2].parse().unwrap()));
                }
            }
            i += 2;
        }
    }
    Ok(Sheet { players, matches })
}

pub struct Imported {
    pub events: usize,
    pub matches: usize,
    /// published lists stored, and those placed on a reference deck
    pub lists: usize,
    pub lists_named: usize,
    pub skipped: Vec<String>,
    pub first: String,
    pub last: String,
}

/// Rebuilds `ldc.db` from every stored sheet and page, in one transaction so readers never see it
/// half done. `refs` are the reference decklists the published lists are placed on.
pub fn import(data: &Path, timeline: &Path, refs: &References) -> Result<Imported, String> {
    let dir = sheets_dir(data);
    let names: BTreeMap<String, String> = std::fs::read_to_string(dir.join("sheets.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let eras = super::eras::eras_from_file(timeline)?;
    let conn = &mut connect(data)?;
    ensure_view(conn)?;
    let mut out = Imported { events: 0, matches: 0, lists: 0, lists_named: 0, skipped: vec![], first: String::new(), last: String::new() };
    let mut sheets: Vec<(String, String, String)> = vec![];
    for (id, name) in &names {
        let Some(date) = event_date(name) else {
            out.skipped.push(format!("{name}: no date in the name"));
            continue;
        };
        sheets.push((date, id.clone(), name.clone()));
    }
    sheets.sort();
    diesel::sql_query("BEGIN IMMEDIATE").execute(conn).map_err(|e| e.to_string())?;
    match write_all(conn, data, &sheets, &eras, refs, &mut out) {
        Ok(()) => diesel::sql_query("COMMIT").execute(conn).map_err(|e| e.to_string())?,
        Err(e) => {
            let _ = diesel::sql_query("ROLLBACK").execute(conn);
            return Err(e);
        }
    };
    Ok(out)
}

fn write_all(conn: &mut SqliteConnection, data: &Path, sheets: &[(String, String, String)], eras: &[(String, String)], refs: &References, out: &mut Imported) -> Result<(), String> {
    let dir = sheets_dir(data);
    let sq = |s: &str| format!("'{}'", s.replace('\'', "''"));
    let o = |s: &Option<String>| s.as_deref().map(sq).unwrap_or_else(|| "NULL".into());
    for t in ["ldc_games", "ldc_matches", "ldc_players", "ldc_events", "ldc_lists"] {
        diesel::sql_query(format!("DELETE FROM {t}")).execute(conn).map_err(|e| e.to_string())?;
    }
    let mut seen_events = std::collections::HashSet::new();
    let mut loaded: Vec<(String, String, String, Sheet)> = vec![];
    for (date, id, name) in sheets {
        let sheet: Sheet = match std::fs::read_to_string(dir.join(format!("{id}.json"))).map_err(|e| e.to_string()).and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string())) {
            Ok(s) => s,
            Err(e) => {
                out.skipped.push(format!("{name}: {e}"));
                continue;
            }
        };
        // the same event published twice keeps its first sheet
        if !seen_events.insert(name.clone()) || sheet.matches.is_empty() {
            out.skipped.push(format!("{name}: {}", if sheet.matches.is_empty() { "no pairings" } else { "duplicate name" }));
            continue;
        }
        loaded.push((date.clone(), id.clone(), name.clone(), sheet));
    }
    let mut pages = load_pages(data)?;
    pages.retain(|p| p.rows.iter().any(|r| lists_dir(data).join(format!("{}.txt", r.0)).exists()));
    let paired = pair_pages(&loaded, &pages);
    for (date, id, name, sheet) in &loaded {
        let era = super::eras::era_for(eras, date);
        diesel::sql_query(format!("INSERT INTO ldc_events(event, sheet_id, date, event_type, era) VALUES ({}, {}, {}, {}, {})", sq(name), sq(id), sq(date), sq(event_type(name)), o(&era.map(|(_, n)| n.clone())))).execute(conn).map_err(|e| e.to_string())?;
        if let Some(page) = paired.get(name) {
            write_lists(conn, data, name, page, era.map_or("", |(s, _)| s), refs, out)?;
        }
        for (p, deck, arch, notes, w, l) in &sheet.players {
            let n = |v: &Option<i64>| v.map(|x| x.to_string()).unwrap_or_else(|| "NULL".into());
            diesel::sql_query(format!(
                "INSERT OR IGNORE INTO ldc_players(event, player, player_key, deck, archetype, notes, wins, losses) VALUES ({}, {}, {}, {}, {}, {}, {}, {})",
                sq(name), sq(p), sq(&p.to_lowercase()), o(deck), o(arch), o(notes), n(w), n(l)
            ))
            .execute(conn)
            .map_err(|e| e.to_string())?;
        }
        for (round, p, opp, w, l) in &sheet.matches {
            diesel::sql_query(format!("INSERT INTO ldc_matches(event, round, player, opponent, game_wins, game_losses) VALUES ({}, {round}, {}, {}, {w}, {l})", sq(name), sq(p), sq(opp))).execute(conn).map_err(|e| e.to_string())?;
            if w + l > 0 {
                diesel::sql_query(format!(
                    "INSERT INTO ldc_games(match_id, won) SELECT (SELECT MAX(id) FROM ldc_matches), value < {w} FROM json_each('[{}]')",
                    (0..w + l).map(|i| i.to_string()).collect::<Vec<_>>().join(",")
                ))
                .execute(conn)
                .map_err(|e| e.to_string())?;
            }
            out.matches += 1;
        }
        if out.first.is_empty() {
            out.first = date.clone();
        }
        out.last = date.clone();
        out.events += 1;
    }
    Ok(())
}


/// The event's published lists, each placed on the reference decks of its era.
fn write_lists(conn: &mut SqliteConnection, data: &Path, event: &str, page: &Page, era_start: &str, refs: &References, out: &mut Imported) -> Result<(), String> {
    let sq = |s: &str| format!("'{}'", s.replace('\'', "''"));
    let o = |s: &Option<String>| s.as_deref().map(sq).unwrap_or_else(|| "NULL".into());
    let n = |v: Option<i32>| v.map_or("NULL".to_string(), |x| x.to_string());
    for (id, deck_name, player) in &page.rows {
        let Ok(list) = std::fs::read_to_string(lists_dir(data).join(format!("{id}.txt"))) else { continue };
        let (main, side) = match reference::parse_decklist(&list) {
            Ok(x) => x,
            Err(e) => {
                out.skipped.push(format!("{event}: list {id}: {e}"));
                continue;
            }
        };
        let a = reference::assign_one(0, &main, &side, era_start, refs);
        diesel::sql_query(format!(
            "INSERT OR IGNORE INTO ldc_lists(event, player_key, goldfish_id, goldfish_deck, n_main, n_side, deck, archetype, reference_id, dist_main, dist_side, runner_up_deck, runner_up_dist, colors) VALUES ({}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {})",
            sq(event), sq(&player.to_lowercase()), sq(id), sq(deck_name), main.values().sum::<i32>(), side.values().sum::<i32>(), o(&a.deck), o(&a.archetype), n(a.reference_id), a.dist_main, a.dist_side, o(&a.runner_up_deck), n(a.runner_up_dist), sq(&a.colors)
        ))
        .execute(conn)
        .map_err(|e| e.to_string())?;
        out.lists += 1;
        out.lists_named += a.deck.is_some() as usize;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn month_index_lists_events_but_not_leagues() {
        let html = r#"<a href="/decklist/legacy-league-2026-11-0111169">x</a> <a href="/decklist/legacy-challenge-32-2026-11-0312857422">y</a><a href="/decklist/legacy-rc-super-qualifier-2026-11-0812857690">z</a><a href="/decklist/legacy-challenge-32-2026-11-0312857422">y again</a>"#;
        assert_eq!(
            parse_month_index(html),
            vec![("legacy-challenge-32-2026-11-0312857422".into(), "2026-11-03".into(), "12857422".into()), ("legacy-rc-super-qualifier-2026-11-0812857690".into(), "2026-11-08".into(), "12857690".into())]
        );
    }

    #[test]
    fn event_page_rows_in_rank_order_with_list_text() {
        let html = r#"<script>window.MTGO.decklists.data = {"event_id":12857422,"description":"Legacy Challenge 32","starttime":"2026-11-03 14:00:00.0","format":"CLEGACY","decklists":[{"loginid":1,"decktournamentid":501,"player":"b","main_deck":[{"qty":"4","card_attributes":{"card_name":"Brainstorm"}},{"qty":"1","card_attributes":{"card_name":"Rough/Tumble"}}],"sideboard_deck":[{"qty":"2","card_attributes":{"card_name":"Surgical Extraction"}}]},{"loginid":2,"decktournamentid":502,"player":"a","main_deck":[{"qty":"4","card_attributes":{"card_name":"Ponder"}}],"sideboard_deck":[]}],"final_rank":[{"loginid":"2","rank":"1"},{"loginid":"1","rank":"2"}],"standings":[]};</script>"#;
        let (page, lists) = parse_event_page(html).unwrap();
        assert_eq!((page.id.as_str(), page.name.as_str(), page.date.as_str()), ("12857422", "Legacy Challenge 32 2026-11-03", "2026-11-03"));
        assert_eq!(page.rows, vec![("502".into(), String::new(), "a".into()), ("501".into(), String::new(), "b".into())]);
        let t = &lists.iter().find(|(id, _)| id == "501").unwrap().1;
        assert_eq!(t, "4 Brainstorm\n1 Rough/Tumble\n\n2 Surgical Extraction\n");
        let (main, side) = reference::parse_decklist(t).unwrap();
        assert_eq!((main["Brainstorm"], side["Surgical Extraction"]), (4, 2));
        assert_eq!(reference::parse_decklist(&lists[1].1).unwrap().1.len(), 0);
        assert!(parse_event_page(&html.replace("CLEGACY", "CMODERN")).is_err());
    }

    /// Needs the network: mtgo.com's month index and an event page still carry their data.
    #[test]
    #[ignore]
    fn mtgo_com_still_serves_decklists() {
        let jar = std::env::temp_dir().join("mtgctl-test-mtgo-cookies.txt");
        let ev = month_events(2026, 11, &jar).unwrap();
        eprintln!("{} events: {ev:?}", ev.len());
        let (slug, _, _) = ev.iter().find(|(s, _, _)| s.contains("challenge")).expect("a Challenge on the index");
        let html = curl_page(&format!("https://www.mtgo.com/decklist/{slug}"), &jar, |h| parse_event_page(h).is_ok_and(|(p, _)| !p.rows.is_empty())).unwrap();
        let _ = std::fs::remove_file(&jar);
        let (page, lists) = parse_event_page(&html).unwrap();
        eprintln!("{}: {} rows, {} lists", page.name, page.rows.len(), lists.len());
        assert!(page.rows.len() >= 8 && lists.len() == page.rows.len());
    }
}
