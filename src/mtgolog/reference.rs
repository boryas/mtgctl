//! Reference decklists: concrete, dated lists that stand for a deck (a plan within an archetype).
//! They live in the shared database, not a player's, because every player's lists and opponents
//! are assigned against the same set. Curated by hand; each row keeps where it came from.

use diesel::prelude::*;
use diesel::sql_types::{Integer, Nullable, Text};
use diesel::sqlite::SqliteConnection;
use std::collections::BTreeMap;

pub const DDL: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS archetypes (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        name TEXT NOT NULL UNIQUE              -- Doomsday, Lands, Breakfast
    )",
    "CREATE TABLE IF NOT EXISTS decks (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        archetype_id INTEGER NOT NULL REFERENCES archetypes(id),
        name TEXT NOT NULL,                    -- UB Tempo DD, Turbo DD, Mono-G Sphere Lands
        UNIQUE(archetype_id, name)
    )",
    "CREATE TABLE IF NOT EXISTS reference_decklists (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        deck_id INTEGER NOT NULL REFERENCES decks(id),
        name TEXT NOT NULL,                    -- e.g. 'Legacy Challenge 32 2026-09-06 5th, boryas'
        list_date TEXT,                        -- YYYY-MM-DD the list was played
        era TEXT NOT NULL,                     -- name of the timeline era (bans/sets) the list belongs to
        era_start TEXT,                        -- that era's start date
        source TEXT,                           -- URL or note saying where it came from
        added_by TEXT,
        added_at TEXT NOT NULL,
        n_main INTEGER NOT NULL,
        n_side INTEGER NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS reference_cards (
        reference_id INTEGER NOT NULL REFERENCES reference_decklists(id) ON DELETE CASCADE,
        name TEXT NOT NULL,
        qty INTEGER NOT NULL,
        sideboard BOOLEAN NOT NULL,
        PRIMARY KEY (reference_id, name, sideboard)
    )",
];

pub fn ensure_schema(conn: &mut SqliteConnection) -> QueryResult<()> {
    for stmt in DDL {
        diesel::sql_query(*stmt).execute(conn)?;
    }
    Ok(())
}

/// A pasted decklist: `4 Brainstorm` lines, main first; a line starting with `sideboard` (any
/// case) or `SB:` or an empty line after the main deck starts the sideboard. Quantities may be
/// `4x`. Returns (main, side) as name → qty, merging repeated names.
pub fn parse_decklist(text: &str) -> Result<(BTreeMap<String, i32>, BTreeMap<String, i32>), String> {
    let (mut main, mut side) = (BTreeMap::new(), BTreeMap::new());
    let mut in_side = false;
    let mut seen_main = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            if seen_main {
                in_side = true;
            }
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("sideboard") || lower.starts_with("sb:") {
            in_side = true;
            continue;
        }
        if lower.starts_with("main") || lower.starts_with("deck") || lower.starts_with("//") {
            continue;
        }
        let (qty, name) = match line.split_once(' ') {
            Some((q, n)) if q.trim_end_matches('x').parse::<i32>().is_ok() => (q.trim_end_matches('x').parse::<i32>().unwrap(), n.trim()),
            _ => return Err(format!("cannot read a quantity from line {line:?}")),
        };
        if name.is_empty() || qty <= 0 {
            return Err(format!("bad line {line:?}"));
        }
        seen_main = true;
        *(if in_side { &mut side } else { &mut main }).entry(name.to_string()).or_insert(0) += qty;
    }
    if main.is_empty() {
        return Err("no cards".into());
    }
    Ok((main, side))
}

fn q(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

#[derive(QueryableByName)]
struct IdRow {
    #[diesel(sql_type = Integer)]
    id: i32,
}

fn archetype_id(conn: &mut SqliteConnection, name: &str) -> QueryResult<i32> {
    diesel::sql_query(format!("INSERT OR IGNORE INTO archetypes(name) VALUES ({})", q(name))).execute(conn)?;
    Ok(diesel::sql_query(format!("SELECT id FROM archetypes WHERE name = {}", q(name))).get_result::<IdRow>(conn)?.id)
}

fn deck_id(conn: &mut SqliteConnection, archetype: &str, deck: &str) -> QueryResult<i32> {
    let a = archetype_id(conn, archetype)?;
    diesel::sql_query(format!("INSERT OR IGNORE INTO decks(archetype_id, name) VALUES ({a}, {})", q(deck))).execute(conn)?;
    Ok(diesel::sql_query(format!("SELECT id FROM decks WHERE archetype_id = {a} AND name = {}", q(deck))).get_result::<IdRow>(conn)?.id)
}

pub struct NewReference<'a> {
    pub archetype: &'a str,
    pub deck: &'a str,
    pub name: &'a str,
    /// YYYY-MM-DD the list was played; the era is resolved from the timeline
    pub date: &'a str,
    pub source: Option<&'a str>,
    pub added_by: Option<&'a str>,
    pub text: &'a str,
}

/// Add one reference decklist (creating its archetype and deck rows as needed). Returns its id.
pub fn add(conn: &mut SqliteConnection, r: &NewReference, eras: &[(String, String)]) -> Result<i32, String> {
    ensure_schema(conn).map_err(|e| e.to_string())?;
    for (col, ty) in [("list_date", "TEXT"), ("era_start", "TEXT")] {
        if diesel::sql_query(format!("SELECT {col} FROM reference_decklists LIMIT 0")).execute(conn).is_err() {
            diesel::sql_query(format!("ALTER TABLE reference_decklists ADD COLUMN {col} {ty}")).execute(conn).map_err(|e| e.to_string())?;
        }
    }
    if !regex::Regex::new(r"^\d{4}-\d{2}-\d{2}$").unwrap().is_match(r.date) {
        return Err("date must be YYYY-MM-DD".into());
    }
    let (era_start, era) = super::eras::era_for(eras, r.date).cloned().ok_or_else(|| format!("no era in the timeline covers {}", r.date))?;
    let (main, side) = parse_decklist(r.text)?;
    let d = deck_id(conn, r.archetype, r.deck).map_err(|e| e.to_string())?;
    let (n_main, n_side): (i32, i32) = (main.values().sum(), side.values().sum());
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    conn.transaction::<_, diesel::result::Error, _>(|conn| {
        diesel::sql_query(format!(
            "INSERT INTO reference_decklists(deck_id, name, list_date, era, era_start, source, added_by, added_at, n_main, n_side) VALUES ({d}, {}, {}, {}, {}, {}, {}, '{now}', {n_main}, {n_side})",
            q(r.name), q(r.date), q(&era), q(&era_start), r.source.map(q).unwrap_or("NULL".into()), r.added_by.map(q).unwrap_or("NULL".into())
        ))
        .execute(conn)?;
        let id = diesel::sql_query("SELECT last_insert_rowid() AS id").get_result::<IdRow>(conn)?.id;
        for (cards, sb) in [(&main, 0), (&side, 1)] {
            for (name, qty) in cards {
                diesel::sql_query(format!("INSERT INTO reference_cards(reference_id, name, qty, sideboard) VALUES ({id}, {}, {qty}, {sb})", q(name))).execute(conn)?;
            }
        }
        Ok(id)
    })
    .map_err(|e| e.to_string())
}

#[derive(QueryableByName, serde::Serialize, Debug)]
pub struct ReferenceRow {
    #[diesel(sql_type = Integer)]
    pub id: i32,
    #[diesel(sql_type = Text)]
    pub archetype: String,
    #[diesel(sql_type = Text)]
    pub deck: String,
    #[diesel(sql_type = Text)]
    pub name: String,
    #[diesel(sql_type = Text)]
    pub era: String,
    #[diesel(sql_type = Nullable<Text>)]
    pub era_start: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub list_date: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub source: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub added_by: Option<String>,
    #[diesel(sql_type = Text)]
    pub added_at: String,
    #[diesel(sql_type = Integer)]
    pub n_main: i32,
    #[diesel(sql_type = Integer)]
    pub n_side: i32,
}

pub fn list(conn: &mut SqliteConnection) -> QueryResult<Vec<ReferenceRow>> {
    ensure_schema(conn)?;
    for (col, ty) in [("list_date", "TEXT"), ("era_start", "TEXT")] {
        if diesel::sql_query(format!("SELECT {col} FROM reference_decklists LIMIT 0")).execute(conn).is_err() {
            diesel::sql_query(format!("ALTER TABLE reference_decklists ADD COLUMN {col} {ty}")).execute(conn)?;
        }
    }
    diesel::sql_query(
        "SELECT r.id, a.name AS archetype, d.name AS deck, r.name, r.era, r.era_start, r.list_date, r.source, r.added_by, r.added_at, r.n_main, r.n_side
         FROM reference_decklists r JOIN decks d ON d.id = r.deck_id JOIN archetypes a ON a.id = d.archetype_id
         ORDER BY a.name, d.name, r.era_start, r.id",
    )
    .load(conn)
}

#[derive(QueryableByName, serde::Serialize)]
pub struct ReferenceCard {
    #[diesel(sql_type = Text)]
    pub name: String,
    #[diesel(sql_type = Integer)]
    pub qty: i32,
    #[diesel(sql_type = diesel::sql_types::Bool)]
    pub sideboard: bool,
}

pub fn cards(conn: &mut SqliteConnection, id: i32) -> QueryResult<Vec<ReferenceCard>> {
    diesel::sql_query(format!("SELECT name, qty, sideboard FROM reference_cards WHERE reference_id = {id} ORDER BY sideboard, name")).load(conn)
}

/// Move a deck under another archetype (created if new); an archetype left with no decks is dropped.
pub fn move_deck(conn: &mut SqliteConnection, deck: &str, archetype: &str) -> Result<(), String> {
    ensure_schema(conn).map_err(|e| e.to_string())?;
    let a = archetype_id(conn, archetype).map_err(|e| e.to_string())?;
    let n = diesel::sql_query(format!("UPDATE decks SET archetype_id = {a} WHERE name = {}", q(deck))).execute(conn).map_err(|e| e.to_string())?;
    if n == 0 {
        return Err(format!("no deck named {deck:?}"));
    }
    diesel::sql_query("DELETE FROM archetypes WHERE id NOT IN (SELECT archetype_id FROM decks)").execute(conn).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn remove(conn: &mut SqliteConnection, id: i32) -> QueryResult<usize> {
    diesel::sql_query(format!("DELETE FROM reference_cards WHERE reference_id = {id}")).execute(conn)?;
    diesel::sql_query(format!("DELETE FROM reference_decklists WHERE id = {id}")).execute(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mtgo_style_lists() {
        let (m, s) = parse_decklist("4 Brainstorm\n4x Ponder\n1 Doomsday\n\nSideboard\n2 Flusterstorm\n").unwrap();
        assert_eq!(m["Brainstorm"], 4);
        assert_eq!(m["Ponder"], 4);
        assert_eq!(s["Flusterstorm"], 2);
        assert!(!m.contains_key("Flusterstorm"));
        assert!(parse_decklist("Brainstorm\n").is_err());
    }
}

// ---- assignment of a player's registered lists to decks via the nearest reference decklist ----

/// Main-deck changes beyond which the nearest reference is not close enough to name the list.
pub const MAX_ASSIGN_DIST: i32 = 15;
/// The nearest reference must beat the best reference of any other deck by this many main-deck
/// changes; closer than that the list is ambiguous and stays unassigned (a missing reference for
/// its real deck is the usual cause).
pub const MIN_MARGIN: i32 = 4;

pub const ASSIGN_DDL: &str = "CREATE TABLE IF NOT EXISTS list_assignments (
        list_id INTEGER PRIMARY KEY,
        reference_id INTEGER,                  -- winning reference (shared db), NULL = unassigned
        archetype TEXT,                        -- denormalised from the shared db at assignment time
        deck TEXT,
        dist_main INTEGER NOT NULL,            -- score to the nearest reference: main-deck distance + colour penalty
        dist_side INTEGER NOT NULL,
        colors TEXT NOT NULL DEFAULT '',       -- the list's mana colours (WUBRG letters)
        runner_up_id INTEGER,
        runner_up_deck TEXT,
        runner_up_dist INTEGER,
        assigned_at TEXT NOT NULL
    )";

#[derive(Clone)]
pub struct RefList {
    pub id: i32,
    pub archetype: String,
    pub deck: String,
    pub era: String,
    pub main: BTreeMap<String, i32>,
    pub side: BTreeMap<String, i32>,
    pub colors: String,
}

pub struct References {
    pub refs: Vec<RefList>,
    pub produces: Produces,
    /// (start, name) of every timeline era, for placing lists and references in the same era
    pub eras: Vec<(String, String)>,
}

pub fn load_references(conn: &mut SqliteConnection, eras: &[(String, String)]) -> QueryResult<References> {
    let produces = load_produces(conn)?;
    let mut refs = Vec::new();
    for r in list(conn)? {
        let (mut main, mut side) = (BTreeMap::new(), BTreeMap::new());
        for c in cards(conn, r.id)? {
            (if c.sideboard { &mut side } else { &mut main }).insert(c.name, c.qty);
        }
        let colors = mana_colors(&main, &side, &produces);
        refs.push(RefList { id: r.id, archetype: r.archetype, deck: r.deck, era: r.era_start.unwrap_or_default(), main, side, colors });
    }
    Ok(References { refs, produces, eras: eras.to_vec() })
}

/// Re-resolve every reference's era from its list date (or the date in its name) against the timeline.
pub fn reassign_eras(conn: &mut SqliteConnection, eras: &[(String, String)]) -> QueryResult<usize> {
    let rows = list(conn)?;
    let re = regex::Regex::new(r"\d{4}-\d{2}-\d{2}").unwrap();
    let mut n = 0;
    for r in rows {
        let date = r.list_date.clone().or_else(|| re.find(&r.name).map(|m| m.as_str().to_string()));
        let Some(date) = date else { continue };
        if let Some((start, name)) = super::eras::era_for(eras, &date) {
            diesel::sql_query(format!("UPDATE reference_decklists SET list_date = {}, era = {}, era_start = {} WHERE id = {}", q(&date), q(name), q(start), r.id)).execute(conn)?;
            n += 1;
        }
    }
    Ok(n)
}

/// Edit distance between two card multisets: cards to swap to turn one into the other.
pub fn distance(a: &BTreeMap<String, i32>, b: &BTreeMap<String, i32>) -> i32 {
    let keys: std::collections::BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    keys.into_iter().map(|k| (a.get(k).copied().unwrap_or(0) - b.get(k).copied().unwrap_or(0)).abs()).sum::<i32>() / 2
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Assignment {
    pub list_id: i32,
    pub reference_id: Option<i32>,
    pub archetype: Option<String>,
    pub deck: Option<String>,
    pub dist_main: i32,
    pub dist_side: i32,
    pub runner_up_id: Option<i32>,
    pub runner_up_deck: Option<String>,
    pub runner_up_dist: Option<i32>,
    /// the list's own mana colours, for the report
    pub colors: String,
}

/// Decide one list. `era` is the start date of the list's own era (from its first sighting):
/// among equally near references it prefers one from that era, then the smaller sideboard distance.
pub fn assign_one(list_id: i32, main: &BTreeMap<String, i32>, side: &BTreeMap<String, i32>, era: &str, r: &References) -> Assignment {
    let colors = mana_colors(main, side, &r.produces);
    // score = main-deck distance + a penalty per colour of mana the two disagree on
    let mut scored: Vec<(i32, i32, &RefList)> = r.refs.iter().map(|x| (distance(main, &x.main) + COLOR_PENALTY * color_diff(&colors, &x.colors), distance(side, &x.side), x)).collect();
    scored.sort_by_key(|(dm, ds, x)| (*dm, if x.era == era { 0 } else { 1 }, *ds));
    let Some(&(dm, ds, best)) = scored.first() else {
        return Assignment { list_id, reference_id: None, archetype: None, deck: None, dist_main: 0, dist_side: 0, runner_up_id: None, runner_up_deck: None, runner_up_dist: None, colors };
    };
    let runner = scored.iter().find(|(_, _, r)| r.deck != best.deck);
    let clear = runner.map_or(true, |(rd, _, _)| rd - dm >= MIN_MARGIN);
    let assigned = dm <= MAX_ASSIGN_DIST && clear;
    Assignment {
        list_id,
        reference_id: assigned.then_some(best.id),
        archetype: assigned.then(|| best.archetype.clone()),
        deck: assigned.then(|| best.deck.clone()),
        dist_main: dm,
        dist_side: ds,
        runner_up_id: runner.map(|(_, _, r)| r.id),
        runner_up_deck: runner.map(|(_, _, r)| r.deck.clone()),
        runner_up_dist: runner.map(|(rd, _, _)| *rd),
        colors,
    }
}

#[derive(QueryableByName)]
struct ListMeta {
    #[diesel(sql_type = Integer)]
    id: i32,
    #[diesel(sql_type = Text)]
    first_seen: String,
}

#[derive(QueryableByName)]
struct LC {
    #[diesel(sql_type = Integer)]
    list_id: i32,
    #[diesel(sql_type = Text)]
    name: String,
    #[diesel(sql_type = Integer)]
    qty: i32,
    #[diesel(sql_type = diesel::sql_types::Bool)]
    sideboard: bool,
}

/// Assign every registered list in a player's database against `refs`; rewrites list_assignments.
pub fn assign_player(conn: &mut SqliteConnection, refs: &References) -> QueryResult<Vec<Assignment>> {
    // fully derived: rebuilt from scratch every run (also picks up column changes)
    diesel::sql_query("DROP TABLE IF EXISTS list_assignments").execute(conn)?;
    diesel::sql_query(ASSIGN_DDL).execute(conn)?;
    let lists: Vec<ListMeta> = diesel::sql_query("SELECT id, first_seen FROM mtgo_lists ORDER BY id").load(conn)?;
    let cards: Vec<LC> = diesel::sql_query("SELECT list_id, name, qty, sideboard FROM mtgo_list_cards").load(conn)?;
    let mut by_list: std::collections::HashMap<i32, (BTreeMap<String, i32>, BTreeMap<String, i32>)> = std::collections::HashMap::new();
    for c in cards {
        let e = by_list.entry(c.list_id).or_default();
        (if c.sideboard { &mut e.1 } else { &mut e.0 }).insert(c.name, c.qty);
    }
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let mut out = Vec::new();
    for l in lists {
        let (main, side) = by_list.get(&l.id).cloned().unwrap_or_default();
        let list_era = super::eras::era_for(&refs.eras, &l.first_seen[..10.min(l.first_seen.len())]).map(|(s, _)| s.as_str()).unwrap_or("");
        let a = assign_one(l.id, &main, &side, list_era, refs);
        let opt_s = |v: &Option<String>| v.as_ref().map(|s| q(s)).unwrap_or("NULL".into());
        let opt_i = |v: Option<i32>| v.map(|x| x.to_string()).unwrap_or("NULL".into());
        diesel::sql_query(format!(
            "INSERT INTO list_assignments(list_id, reference_id, archetype, deck, dist_main, dist_side, colors, runner_up_id, runner_up_deck, runner_up_dist, assigned_at) VALUES ({}, {}, {}, {}, {}, {}, {}, {}, {}, {}, '{now}')",
            a.list_id, opt_i(a.reference_id), opt_s(&a.archetype), opt_s(&a.deck), a.dist_main, a.dist_side, q(&a.colors), opt_i(a.runner_up_id), opt_s(&a.runner_up_deck), opt_i(a.runner_up_dist)
        ))
        .execute(conn)?;
        out.push(a);
    }
    Ok(out)
}

// ---- card facts (from Scryfall): colours of spells, colours produced by lands ----

pub const FACTS_DDL: &str = "CREATE TABLE IF NOT EXISTS card_facts (
        name TEXT PRIMARY KEY,
        colors TEXT NOT NULL,      -- WUBRG letters of the card itself ('' for lands/colourless)
        produces TEXT NOT NULL,    -- WUBRG letters a land makes ('' for fetches, Cavern, non-lands)
        land BOOLEAN NOT NULL
    )";

/// Load `card_colors.json` (name → {c, p, land}, written by fetch_scryfall.py) into the shared db.
pub fn import_card_facts(conn: &mut SqliteConnection, path: &std::path::Path) -> Result<usize, String> {
    diesel::sql_query(FACTS_DDL).execute(conn).map_err(|e| e.to_string())?;
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let obj = v.as_object().ok_or("expected an object of name → {c, p, land}")?;
    let mut n = 0;
    conn.transaction::<_, diesel::result::Error, _>(|conn| {
        for (name, f) in obj {
            let c = f.get("c").and_then(|x| x.as_str()).unwrap_or("");
            let p = f.get("p").and_then(|x| x.as_str()).unwrap_or("");
            let land = f.get("land").and_then(|x| x.as_bool()).unwrap_or(false);
            diesel::sql_query(format!("INSERT OR REPLACE INTO card_facts(name, colors, produces, land) VALUES ({}, {}, {}, {})", q(name), q(c), q(p), land as i32)).execute(conn)?;
            n += 1;
        }
        Ok(())
    })
    .map_err(|e| e.to_string())?;
    Ok(n)
}

/// name → colours its lands produce (WUBRG letters), for every land known.
pub type Produces = std::collections::HashMap<String, String>;

pub fn load_produces(conn: &mut SqliteConnection) -> QueryResult<Produces> {
    diesel::sql_query(FACTS_DDL).execute(conn)?;
    #[derive(QueryableByName)]
    struct R {
        #[diesel(sql_type = Text)]
        name: String,
        #[diesel(sql_type = Text)]
        produces: String,
    }
    let rows: Vec<R> = diesel::sql_query("SELECT name, produces FROM card_facts WHERE land = 1 AND produces <> ''").load(conn)?;
    Ok(rows.into_iter().map(|r| (r.name, r.produces)).collect())
}

/// The colours a list can make: union over its lands, main and sideboard (a sideboard Plains
/// says the deck splashes white), as a sorted WUBRG string.
pub fn mana_colors(main: &BTreeMap<String, i32>, side: &BTreeMap<String, i32>, produces: &Produces) -> String {
    let mut set = std::collections::BTreeSet::new();
    for name in main.keys().chain(side.keys()) {
        let key = name.split(" // ").next().unwrap_or(name);
        if let Some(p) = produces.get(key) {
            set.extend(p.chars());
        }
    }
    "WUBRG".chars().filter(|c| set.contains(c)).collect()
}

/// Cards of distance charged per colour of mana the list and a reference disagree on.
pub const COLOR_PENALTY: i32 = 6;

fn color_diff(a: &str, b: &str) -> i32 {
    let (sa, sb): (std::collections::BTreeSet<char>, std::collections::BTreeSet<char>) = (a.chars().collect(), b.chars().collect());
    sa.symmetric_difference(&sb).count() as i32
}

// ---- inference from the cards actually seen (a match's rollout), for lists that were never
// ---- registered and for opponents: containment of what was seen inside each reference ----

/// Below this weighted coverage the rollout does not fit any reference well enough to name
/// (0.70: on 53 LDC-labelled opponents, 0 wrong of 24 named; errors begin at 0.60).
pub const INFER_MIN_COVERAGE: f64 = 0.70;
/// The best reference must beat the best reference of any other deck by this much coverage,
/// and by at least INFER_MIN_MARGIN_CARDS cards' worth of evidence (one card seen out of twelve
/// is 8% and proves nothing); closer than that the decks it could be are reported instead of one.
pub const INFER_MIN_MARGIN: f64 = 0.08;
pub const INFER_MIN_MARGIN_CARDS: f64 = 2.0;
/// Coverage lost per colour of mana the seen lands make that the reference cannot.
pub const INFER_COLOR_PENALTY: f64 = 0.15;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Inference {
    /// the single deck, when the evidence is clear
    pub deck: Option<String>,
    pub archetype: Option<String>,
    pub reference_id: Option<i32>,
    pub coverage: f64,
    /// decks within the margin of the best, including it, when the answer is not single
    pub candidates: Vec<String>,
    /// archetypes of the candidates; one entry = the archetype is known even if the deck is not
    pub archetypes: Vec<String>,
    pub runner_up_deck: Option<String>,
    pub runner_up_coverage: f64,
    pub colors: String,
    pub n_seen: i32,
}

/// Card weights: a card in few references says more than one in all of them; a card in none
/// still counts against every reference (it is evidence of some other deck).
fn card_weights(refs: &[RefList]) -> std::collections::HashMap<String, f64> {
    let mut df: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for r in refs {
        for name in r.main.keys().chain(r.side.keys()) {
            *df.entry(name.as_str()).or_default() += 1;
        }
    }
    let n = refs.len().max(1) as f64;
    df.into_iter().map(|(k, d)| (k.to_string(), 1.0 + (n / d as f64).ln())).collect()
}

/// `seen` is name → copies seen (main and sideboard cards alike; a rollout does not know which).
pub fn infer(seen: &BTreeMap<String, i32>, r: &References) -> Inference {
    let weights = card_weights(&r.refs);
    let unknown_w = 1.0 + ((r.refs.len().max(1) + 1) as f64).ln();
    let w = |name: &str| weights.get(name).copied().unwrap_or(unknown_w);
    let total: f64 = seen.iter().map(|(n, q)| w(n) * *q as f64).sum();
    let colors = mana_colors(seen, &BTreeMap::new(), &r.produces);
    let n_seen: i32 = seen.values().sum();
    if total == 0.0 || r.refs.is_empty() {
        return Inference { deck: None, archetype: None, reference_id: None, coverage: 0.0, candidates: vec![], archetypes: vec![], runner_up_deck: None, runner_up_coverage: 0.0, colors, n_seen };
    }
    let mut scored: Vec<(f64, &RefList)> = r
        .refs
        .iter()
        .map(|x| {
            let covered: f64 = seen.iter().map(|(n, q)| w(n) * (*q).min(x.main.get(n).copied().unwrap_or(0) + x.side.get(n).copied().unwrap_or(0)) as f64).sum();
            let missing_colors = colors.chars().filter(|c| !x.colors.contains(*c)).count() as f64;
            (covered / total - INFER_COLOR_PENALTY * missing_colors, x)
        })
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    let (best_cov, best) = scored[0];
    let runner = scored.iter().find(|(_, x)| x.deck != best.deck);
    let (runner_cov, runner_deck) = runner.map(|(c, x)| (*c, Some(x.deck.clone()))).unwrap_or((0.0, None));
    // margin: the larger of the fixed fraction and two average-weight cards of evidence
    let avg_w = total / n_seen.max(1) as f64;
    let margin = INFER_MIN_MARGIN.max(INFER_MIN_MARGIN_CARDS * avg_w / total);
    let fits = best_cov >= INFER_MIN_COVERAGE;
    let clear = runner.map_or(true, |(c, _)| best_cov - c >= margin);
    let mut candidates: Vec<String> = Vec::new();
    let mut archetypes: Vec<String> = Vec::new();
    if fits {
        for (c, x) in &scored {
            if best_cov - c < margin {
                if !candidates.contains(&x.deck) {
                    candidates.push(x.deck.clone());
                }
                if !archetypes.contains(&x.archetype) {
                    archetypes.push(x.archetype.clone());
                }
            }
        }
    }
    // the deck needs a clear winner; the archetype only needs every candidate to agree
    let archetype = if fits && archetypes.len() == 1 { Some(archetypes[0].clone()) } else { None };
    Inference {
        deck: (fits && clear).then(|| best.deck.clone()),
        archetype,
        reference_id: (fits && clear).then_some(best.id),
        coverage: best_cov,
        candidates,
        archetypes,
        runner_up_deck: runner_deck,
        runner_up_coverage: runner_cov,
        colors,
        n_seen,
    }
}

// ---- per-match deck labels for both sides, with provenance ----

pub const MATCH_DECKS_DDL: &str = "CREATE TABLE IF NOT EXISTS match_decks (
        match_id INTEGER NOT NULL,
        side TEXT NOT NULL,                    -- me | opp
        source TEXT NOT NULL,                  -- list (registered list named by a reference) | cards (inferred from the rollout)
        deck TEXT,                             -- NULL when ambiguous or unknown
        archetype TEXT,                        -- set whenever every candidate deck shares it
        candidates TEXT NOT NULL,              -- '|'-joined decks the rollout could be ('' = none fits)
        coverage REAL NOT NULL,
        runner_up_deck TEXT,                   -- best reference of another deck, and its coverage
        runner_up_coverage REAL NOT NULL DEFAULT 0,
        n_seen INTEGER NOT NULL,
        PRIMARY KEY (match_id, side)
    )";

#[derive(QueryableByName)]
struct SeenRow {
    #[diesel(sql_type = Integer)]
    match_id: i32,
    #[diesel(sql_type = Text)]
    name: String,
    #[diesel(sql_type = Integer)]
    qty: i32,
}

#[derive(QueryableByName)]
struct MatchRow {
    #[diesel(sql_type = Integer)]
    id: i32,
    #[diesel(sql_type = Text)]
    date: String,
    #[diesel(sql_type = Nullable<Text>)]
    list_deck: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    list_archetype: Option<String>,
}

/// Cards seen per match for one side: name → most copies in any one game.
fn seen_by_match(conn: &mut SqliteConnection, side: &str) -> QueryResult<std::collections::HashMap<i32, BTreeMap<String, i32>>> {
    let rows: Vec<SeenRow> = diesel::sql_query(format!("SELECT match_id, name, COALESCE(MAX(instances), 1) AS qty FROM mtgo_game_cards WHERE side = '{side}' AND played = 1 GROUP BY match_id, name")).load(conn)?;
    let mut out: std::collections::HashMap<i32, BTreeMap<String, i32>> = std::collections::HashMap::new();
    for r in rows {
        out.entry(r.match_id).or_default().insert(r.name, r.qty);
    }
    Ok(out)
}

/// My side plays one list for a run of matches: within a day, in time order, pool the cards of
/// consecutive matches while the pooled evidence still fits some reference; a switch breaks the run.
fn pool_runs(matches: &[MatchRow], seen: &std::collections::HashMap<i32, BTreeMap<String, i32>>, refs: &References) -> std::collections::HashMap<i32, BTreeMap<String, i32>> {
    let mut days: std::collections::BTreeMap<&str, Vec<i32>> = Default::default();
    for m in matches {
        if seen.contains_key(&m.id) {
            days.entry(m.date.as_str()).or_default().push(m.id);
        }
    }
    let mut pooled = std::collections::HashMap::new();
    for ids in days.values_mut() {
        ids.sort();
        let (mut run, mut acc): (Vec<i32>, BTreeMap<String, i32>) = (vec![], BTreeMap::new());
        for id in ids.iter() {
            let s = &seen[id];
            let mut trial = acc.clone();
            for (n, q) in s {
                let e = trial.entry(n.clone()).or_insert(0);
                *e = (*e).max(*q);
            }
            if !run.is_empty() && infer(&trial, refs).coverage < INFER_MIN_COVERAGE {
                for r in run.drain(..) {
                    pooled.insert(r, acc.clone());
                }
                acc = s.clone();
            } else {
                acc = trial;
            }
            run.push(*id);
        }
        for r in run {
            pooled.insert(r, acc.clone());
        }
    }
    pooled
}

pub struct MatchDeckCounts {
    pub me_list: usize,
    pub me_cards: usize,
    pub opp_named: usize,
    pub opp_archetype_only: usize,
}

/// Rewrite match_decks for a player: my side from the registered list when it is named, else from
/// the pooled rollout; the opponent from their rollout per match.
pub fn label_matches(conn: &mut SqliteConnection, refs: &References) -> QueryResult<MatchDeckCounts> {
    diesel::sql_query("DROP TABLE IF EXISTS match_decks").execute(conn)?;
    diesel::sql_query(MATCH_DECKS_DDL).execute(conn)?;
    let matches: Vec<MatchRow> = diesel::sql_query(
        "SELECT m.id, COALESCE(substr(m.start_ts,1,10), '') AS date, a.deck AS list_deck, a.archetype AS list_archetype
         FROM mtgo_matches m LEFT JOIN list_assignments a ON a.list_id = m.my_list_id WHERE m.replay = 0 ORDER BY m.start_ts",
    )
    .load(conn)?;
    let me = seen_by_match(conn, "me")?;
    let opp = seen_by_match(conn, "opp")?;
    let pooled = pool_runs(&matches, &me, refs);
    let mut counts = MatchDeckCounts { me_list: 0, me_cards: 0, opp_named: 0, opp_archetype_only: 0 };
    let ins = |conn: &mut SqliteConnection, id: i32, side: &str, source: &str, deck: &Option<String>, arch: &Option<String>, cands: &[String], cov: f64, ru: &Option<String>, ruc: f64, n: i32| -> QueryResult<usize> {
        let o = |v: &Option<String>| v.as_ref().map(|s| q(s)).unwrap_or("NULL".into());
        diesel::sql_query(format!("INSERT INTO match_decks(match_id, side, source, deck, archetype, candidates, coverage, runner_up_deck, runner_up_coverage, n_seen) VALUES ({id}, '{side}', '{source}', {}, {}, {}, {cov:.3}, {}, {ruc:.3}, {n})", o(deck), o(arch), q(&cands.join("|")), o(ru))).execute(conn)
    };
    conn.transaction::<_, diesel::result::Error, _>(|conn| {
        for m in &matches {
            if m.list_deck.is_some() {
                ins(conn, m.id, "me", "list", &m.list_deck, &m.list_archetype, &[m.list_deck.clone().unwrap()], 1.0, &None, 0.0, 0)?;
                counts.me_list += 1;
            } else if let Some(s) = pooled.get(&m.id) {
                let i = infer(s, refs);
                ins(conn, m.id, "me", "cards", &i.deck, &i.archetype, &i.candidates, i.coverage, &i.runner_up_deck, i.runner_up_coverage, i.n_seen)?;
                if i.deck.is_some() || i.archetype.is_some() {
                    counts.me_cards += 1;
                }
            }
            if let Some(s) = opp.get(&m.id) {
                let i = infer(s, refs);
                ins(conn, m.id, "opp", "cards", &i.deck, &i.archetype, &i.candidates, i.coverage, &i.runner_up_deck, i.runner_up_coverage, i.n_seen)?;
                if i.deck.is_some() {
                    counts.opp_named += 1;
                } else if i.archetype.is_some() {
                    counts.opp_archetype_only += 1;
                }
            }
        }
        Ok(())
    })?;
    Ok(counts)
}
