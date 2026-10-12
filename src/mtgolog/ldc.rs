//! The Legacy Data Collection (LDC): per-event sheets of MTGO Legacy events (Challenges,
//! Showcases, qualifiers) naming each player's deck and every round's opponent and game score.
//! Kept in its own `<data>/ldc.db`, never mixed with uploaded logs, and sliced by the same stats
//! engine through `DATASET`.
//!
//! A match is stored once per side, so every row reads from its `player`'s side: a mirror shows
//! 50%, and a deck's matchup row is that deck's view of it. Sheets give game scores only, so the
//! per-game rows of `ldc_game_view` carry a result but no order, play/draw or turns.

use super::dataset::{Column, Dataset, Features, Kind::*, Level::*};
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
    about: "Every player's matches in MTGO Legacy Challenges and other events, from the Legacy Data Collection. Each match is counted once from each player's side; scores only, so no play/draw, turns or cards.",
    view: "ldc_game_view",
    columns: &[
        Column { name: "my_deck", label: "deck", level: Match, kind: Text, values: &[] },
        Column { name: "my_archetype", label: "deck's LDC archetype", level: Match, kind: Text, values: &[] },
        Column { name: "my_notes", label: "deck note (LDC)", level: Match, kind: Text, values: &[] },
        Column { name: "opp_deck", label: "opponent deck", level: Match, kind: Text, values: &[] },
        Column { name: "opp_archetype", label: "opponent's LDC archetype", level: Match, kind: Text, values: &[] },
        Column { name: "opp_notes", label: "opponent deck note (LDC)", level: Match, kind: Text, values: &[] },
        Column { name: "player", label: "player", level: Match, kind: Text, values: &[] },
        Column { name: "opponent", label: "opponent", level: Match, kind: Text, values: &[] },
        Column { name: "match_result", label: "match result", level: Match, kind: Text, values: &[] },
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
LEFT JOIN ldc_players o ON o.event = m.event AND o.player_key = lower(m.opponent)";

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
    let out = std::process::Command::new("curl").args(["-sSL", "--max-time", "60", "-A", "Mozilla/5.0", url]).output().map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("{url}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(out.stdout)
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
    pub skipped: Vec<String>,
    pub first: String,
    pub last: String,
}

/// Rebuilds `ldc.db` from every stored sheet, in one transaction so readers never see it half done.
pub fn import(data: &Path, timeline: &Path) -> Result<Imported, String> {
    let dir = sheets_dir(data);
    let names: BTreeMap<String, String> = std::fs::read_to_string(dir.join("sheets.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let eras = super::eras::eras_from_file(timeline)?;
    let date_re = regex::Regex::new(r"(\d{1,2})[/_](\d{1,2})[/_](\d{4})").unwrap();
    let conn = &mut connect(data)?;
    ensure_view(conn)?;
    let mut out = Imported { events: 0, matches: 0, skipped: vec![], first: String::new(), last: String::new() };
    let mut sheets: Vec<(String, String, String)> = vec![];
    for (id, name) in &names {
        let Some(d) = date_re.captures(name) else {
            out.skipped.push(format!("{name}: no date in the name"));
            continue;
        };
        sheets.push((format!("{}-{:02}-{:02}", &d[3], d[1].parse::<u32>().unwrap(), d[2].parse::<u32>().unwrap()), id.clone(), name.clone()));
    }
    sheets.sort();
    diesel::sql_query("BEGIN IMMEDIATE").execute(conn).map_err(|e| e.to_string())?;
    match write_all(conn, &dir, &sheets, &eras, &mut out) {
        Ok(()) => diesel::sql_query("COMMIT").execute(conn).map_err(|e| e.to_string())?,
        Err(e) => {
            let _ = diesel::sql_query("ROLLBACK").execute(conn);
            return Err(e);
        }
    };
    Ok(out)
}

fn write_all(conn: &mut SqliteConnection, dir: &Path, sheets: &[(String, String, String)], eras: &[(String, String)], out: &mut Imported) -> Result<(), String> {
    let sq = |s: &str| format!("'{}'", s.replace('\'', "''"));
    let o = |s: &Option<String>| s.as_deref().map(sq).unwrap_or_else(|| "NULL".into());
    for t in ["ldc_games", "ldc_matches", "ldc_players", "ldc_events"] {
        diesel::sql_query(format!("DELETE FROM {t}")).execute(conn).map_err(|e| e.to_string())?;
    }
    let mut seen_events = std::collections::HashSet::new();
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
        let era = super::eras::era_for(eras, date).map(|(_, n)| n.clone());
        diesel::sql_query(format!("INSERT INTO ldc_events(event, sheet_id, date, event_type, era) VALUES ({}, {}, {}, {}, {})", sq(name), sq(id), sq(date), sq(event_type(name)), o(&era))).execute(conn).map_err(|e| e.to_string())?;
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

