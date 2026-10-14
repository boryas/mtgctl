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
//! mtggoldfish publishes the list, and `ldc_lists` places it on the reference decks the way a
//! registered list is placed (`reference::assign_one`), which splits the LDC's name further.

use super::dataset::{Column, Dataset, Features, Kind::*, Level::*};
use super::reference::{self, References};
use diesel::prelude::*;
use diesel::sql_types::Text as SqlText;
use diesel::sqlite::SqliteConnection;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// Concurrent goldfish list downloads; one at a time took over two seconds a list.
const FETCH_WORKERS: usize = 6;

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
fn lists_dir(data: &Path) -> PathBuf {
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
        goldfish_id TEXT NOT NULL,       -- mtggoldfish deck id, the list is <data>/ldc/lists/<id>.txt
        goldfish_deck TEXT NOT NULL,     -- the name mtggoldfish gives the list
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

fn curl(url: &str) -> Result<Vec<u8>, String> {
    let mut err = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
        let out = std::process::Command::new("curl").args(["-sSL", "--max-time", "60", "-A", "Mozilla/5.0", url]).output().map_err(|e| format!("curl: {e}"))?;
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

fn unescape(s: &str) -> String {
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

/// One mtggoldfish tournament page: `<data>/ldc/pages/<id>.json`.
#[derive(serde::Serialize, serde::Deserialize)]
struct Page {
    id: String,
    name: String,
    /// goldfish's date, the event's end in UTC: a Friday-evening Challenge is dated Saturday
    date: String,
    /// (goldfish deck id, deck name, player)
    rows: Vec<(String, String, String)>,
}

/// (tournament id, name, date) of every Legacy event goldfish's tournament search lists from
/// `from` to today, a month per search, 20 rows per result page.
fn search_tournaments(from: &str) -> Result<Vec<(String, String, String)>, String> {
    use chrono::Datelike;
    let today = chrono::Local::now().date_naive();
    let mut d = chrono::NaiveDate::parse_from_str(from, "%Y-%m-%d").map_err(|e| format!("{from}: {e}"))?;
    let row = regex::Regex::new(r#"(?s)<td>(\d{4}-\d{2}-\d{2})</td>\s*<td>\s*<a href="/tournament/(\d+)">([^<]*)</a>"#).unwrap();
    let mut out = vec![];
    while d <= today {
        let end = chrono::NaiveDate::from_ymd_opt(d.year() + (d.month() == 12) as i32, d.month() % 12 + 1, 1).unwrap().pred_opt().unwrap().min(today);
        for page in 1.. {
            let url = format!(
                "https://www.mtggoldfish.com/tournament_searches/create?commit=Search&tournament_search%5Bdate_range%5D={}+-+{}&tournament_search%5Bformat%5D=legacy&tournament_search%5Bname%5D=&page={page}",
                d.format("%m%%2F%d%%2F%Y"),
                end.format("%m%%2F%d%%2F%Y")
            );
            let html = String::from_utf8_lossy(&curl(&url)?).to_string();
            std::thread::sleep(std::time::Duration::from_millis(500));
            let n = out.len();
            for c in row.captures_iter(&html) {
                out.push((c[2].to_string(), unescape(c[3].trim()), c[1].to_string()));
            }
            if out.len() == n {
                break;
            }
        }
        d = end.succ_opt().unwrap();
    }
    Ok(out)
}

/// (goldfish deck id, deck name, player) per row of a tournament page.
fn parse_page(html: &str) -> Vec<(String, String, String)> {
    let row = regex::Regex::new(r#"(?s)<a href="/deck/(\d+)">([^<]*)</a>.*?<a href="/player/[^"]*">([^<]*)</a>"#).unwrap();
    row.captures_iter(html).map(|c| (c[1].to_string(), unescape(c[2].trim()), unescape(c[3].trim()))).collect()
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

/// Downloads the published lists of the events the sheets cover: the page of every Legacy
/// Challenge, qualifier or last-chance event goldfish lists from the first sheet (or a week
/// before the newest page) to today, to `<data>/ldc/pages/<id>.json`, and each list to
/// `<data>/ldc/lists/<id>.txt`. Which sheet a page belongs to is decided at import, see `pair_pages`.
/// Returns (pages, lists) downloaded.
pub fn fetch_lists(data: &Path) -> Result<(usize, usize), String> {
    let (pages, lists) = (pages_dir(data), lists_dir(data));
    std::fs::create_dir_all(&pages).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&lists).map_err(|e| e.to_string())?;
    let names: BTreeMap<String, String> = std::fs::read_to_string(sheets_dir(data).join("sheets.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let stored = load_pages(data)?;
    let from = match stored.iter().map(|p| p.date.as_str()).max() {
        Some(d) => (chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").map_err(|e| e.to_string())? - chrono::Days::new(7)).to_string(),
        None => match names.values().filter_map(|n| event_date(n)).min() {
            Some(d) => d,
            None => return Ok((0, 0)),
        },
    };
    let mut np = 0;
    let mut found = search_tournaments(&from)?;
    found.retain(|(_, name, _)| ["Challenge", "Qualifier", "Last Chance"].iter().any(|k| name.contains(k)));
    for (id, name, date) in found {
        let path = pages.join(format!("{id}.json"));
        if path.exists() {
            continue;
        }
        // a page that fails to download is left to the next sync
        let html = match curl(&format!("https://www.mtggoldfish.com/tournament/{id}")) {
            Ok(b) => String::from_utf8_lossy(&b).to_string(),
            Err(e) => {
                eprintln!("ldc: {e}");
                continue;
            }
        };
        std::thread::sleep(std::time::Duration::from_millis(500));
        let rows = parse_page(&html);
        if rows.is_empty() {
            eprintln!("ldc: {name} ({id}): no lists on the page");
            continue;
        }
        std::fs::write(&path, serde_json::to_string(&Page { id, name, date, rows }).unwrap()).map_err(|e| e.to_string())?;
        np += 1;
    }
    // a list is immutable once published; `FETCH_WORKERS` downloads at once, each paced
    let mut todo: Vec<String> = load_pages(data)?.into_iter().flat_map(|p| p.rows.into_iter().map(|r| r.0)).filter(|id| !lists.join(format!("{id}.txt")).exists()).collect();
    todo.sort();
    todo.dedup();
    let queue = std::sync::Mutex::new(todo);
    let done = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|sc| {
        for _ in 0..FETCH_WORKERS {
            sc.spawn(|| loop {
                let Some(id) = queue.lock().unwrap().pop() else { break };
                let path = lists.join(format!("{id}.txt"));
                let text = match curl(&format!("https://www.mtggoldfish.com/deck/download/{id}")) {
                    Ok(b) => String::from_utf8_lossy(&b).to_string(),
                    Err(e) => {
                        eprintln!("ldc: list {id}: {e}");
                        continue;
                    }
                };
                std::thread::sleep(std::time::Duration::from_millis(500));
                if reference::parse_decklist(&text).is_err() {
                    eprintln!("ldc: list {id}: not a decklist");
                    continue;
                }
                if std::fs::write(&path, text).is_ok() {
                    done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            });
        }
    });
    let nl = done.into_inner();
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
    let pages = load_pages(data)?;
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
