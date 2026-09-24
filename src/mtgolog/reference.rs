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
/// Fold one deck into another: its references become the other deck's versions; the emptied deck
/// and an archetype left without decks go.
pub fn merge_deck(conn: &mut SqliteConnection, deck: &str, into: &str) -> Result<usize, String> {
    ensure_schema(conn).map_err(|e| e.to_string())?;
    let id = |conn: &mut SqliteConnection, name: &str| -> Result<i32, String> {
        diesel::sql_query(format!("SELECT id FROM decks WHERE name = {}", q(name))).get_result::<IdRow>(conn).map(|r| r.id).map_err(|_| format!("no deck named {name:?}"))
    };
    let (from, to) = (id(conn, deck)?, id(conn, into)?);
    if from == to {
        return Err("same deck".into());
    }
    let n = diesel::sql_query(format!("UPDATE reference_decklists SET deck_id = {to} WHERE deck_id = {from}")).execute(conn).map_err(|e| e.to_string())?;
    diesel::sql_query(format!("DELETE FROM decks WHERE id = {from}")).execute(conn).map_err(|e| e.to_string())?;
    diesel::sql_query("DELETE FROM archetypes WHERE id NOT IN (SELECT archetype_id FROM decks)").execute(conn).map_err(|e| e.to_string())?;
    Ok(n)
}

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

/// Remove a reference; a deck left without references and an archetype left without decks go with it.
pub fn remove(conn: &mut SqliteConnection, id: i32) -> QueryResult<usize> {
    diesel::sql_query(format!("DELETE FROM reference_cards WHERE reference_id = {id}")).execute(conn)?;
    let n = diesel::sql_query(format!("DELETE FROM reference_decklists WHERE id = {id}")).execute(conn)?;
    diesel::sql_query("DELETE FROM decks WHERE id NOT IN (SELECT deck_id FROM reference_decklists)").execute(conn)?;
    diesel::sql_query("DELETE FROM archetypes WHERE id NOT IN (SELECT archetype_id FROM decks)").execute(conn)?;
    Ok(n)
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
    /// main and side already passed through `canon`
    pub refs: Vec<RefList>,
    pub produces: Produces,
    pub fetches: Fetches,
    /// every land known to the card facts (reveal rate 1: a drawn land is played)
    pub lands: Fetches,
    /// (start, name) of every timeline era, for placing lists and references in the same era
    pub eras: Vec<(String, String)>,
    /// scoring tables derived from `refs`, see `prepare`
    pub bg: Background,
    /// per reference, main + side together (the pool after game one)
    pub all_cards: Vec<BTreeMap<String, i32>>,
    /// per archetype, the most copies of each card any of its references runs, main and side apart
    pub pools: std::collections::HashMap<String, (BTreeMap<String, i32>, BTreeMap<String, i32>)>,
    /// the most copies of a card any reference runs, main and side together, when above four
    pub max_copies: std::collections::HashMap<String, i32>,
}

impl References {
    /// Derive the scoring tables from `refs`; call after any change to `refs`.
    pub fn prepare(&mut self) {
        self.bg = Background::new(&self.refs);
        self.max_copies.clear();
        for x in &self.refs {
            let mut all: BTreeMap<&str, i32> = BTreeMap::new();
            for (n, q) in x.main.iter().chain(x.side.iter()) {
                *all.entry(n.as_str()).or_default() += q;
            }
            for (n, q) in all {
                if q > 4 {
                    let e = self.max_copies.entry(n.to_string()).or_insert(0);
                    *e = (*e).max(q);
                }
            }
        }
        self.all_cards = self
            .refs
            .iter()
            .map(|x| {
                let mut all = x.main.clone();
                for (n, q) in &x.side {
                    *all.entry(n.clone()).or_default() += q;
                }
                all
            })
            .collect();
        self.pools.clear();
        for x in &self.refs {
            let (pm, ps) = self.pools.entry(x.archetype.clone()).or_default();
            for (n, q) in &x.main {
                let e = pm.entry(n.clone()).or_insert(0);
                *e = (*e).max(*q);
            }
            for (n, q) in &x.side {
                let e = ps.entry(n.clone()).or_insert(0);
                *e = (*e).max(*q);
            }
        }
    }
    /// Copies of a card a list can hold: four, or the most any reference runs (basics). A revealed
    /// count above that is the same object recast or reshuffled (Throes of Chaos, Emrakul), not evidence.
    /// Below it an extra copy stays evidence: on my registered-list games only 1.4% of card-in-game
    /// pairs show more instances than the list holds (Consider flashback, replayed lands), so an
    /// excess copy is worth about the deviation it is charged; capping at the reference's own count
    /// cost 11 of 79 correct list placements.
    pub fn cap(&self, name: &str, k: i32) -> i32 {
        k.min(self.max_copies.get(name).copied().unwrap_or(4))
    }
    /// The references plus the player's registered lists as extra hypotheses (see `player_lists`).
    pub fn with_lists(&self, lists: &[PlayerList]) -> References {
        let mut r = References { refs: self.refs.clone(), produces: self.produces.clone(), fetches: self.fetches.clone(), lands: self.lands.clone(), eras: vec![], bg: Background::default(), all_cards: vec![], pools: Default::default(), max_copies: Default::default() };
        r.refs.extend(lists.iter().map(|l| l.list.clone()));
        r.prepare();
        r
    }
}

pub fn load_references(conn: &mut SqliteConnection, eras: &[(String, String)]) -> QueryResult<References> {
    let produces = load_produces(conn)?;
    let fetches = load_fetches(conn)?;
    let lands = load_lands(conn)?;
    let mut refs = Vec::new();
    for r in list(conn)? {
        let (mut main, mut side) = (BTreeMap::new(), BTreeMap::new());
        for c in cards(conn, r.id)? {
            (if c.sideboard { &mut side } else { &mut main }).insert(c.name, c.qty);
        }
        let colors = mana_colors(&main, &side, &produces);
        let (main, side) = (canon(&main, &fetches), canon(&side, &fetches));
        refs.push(RefList { id: r.id, archetype: r.archetype, deck: r.deck, era: r.era_start.unwrap_or_default(), main, side, colors });
    }
    let mut r = References { refs, produces, fetches, lands, eras: eras.to_vec(), bg: Background::default(), all_cards: vec![], pools: Default::default(), max_copies: Default::default() };
    r.prepare();
    Ok(r)
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
    let (main, side) = (&canon(main, &r.fetches), &canon(side, &r.fetches));
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
    name_lists(conn)?;
    Ok(out)
}

/// Auto-names from the assignment: "<deck> v<n> (<MM-DD>)", n counting the player's lists of that
/// deck in the order first seen; "unassigned #<id> (<MM-DD>)" otherwise. A user-given name wins in the view.
fn name_lists(conn: &mut SqliteConnection) -> QueryResult<()> {
    #[derive(QueryableByName)]
    struct L {
        #[diesel(sql_type = Integer)]
        id: i32,
        #[diesel(sql_type = Text)]
        first_seen: String,
        #[diesel(sql_type = Nullable<Text>)]
        deck: Option<String>,
    }
    let lists: Vec<L> = diesel::sql_query("SELECT l.id, l.first_seen, a.deck FROM mtgo_lists l LEFT JOIN list_assignments a ON a.list_id = l.id ORDER BY l.first_seen, l.id").load(conn)?;
    let mut n: std::collections::HashMap<String, i32> = Default::default();
    for l in lists {
        let day = l.first_seen.get(5..10).unwrap_or("");
        let name = match l.deck {
            Some(d) => {
                let c = n.entry(d.clone()).or_insert(0);
                *c += 1;
                format!("{d} v{c} ({day})")
            }
            None => format!("unassigned #{} ({day})", l.id),
        };
        diesel::sql_query(format!("UPDATE mtgo_lists SET auto_name = {} WHERE id = {}", q(&name), l.id)).execute(conn)?;
    }
    Ok(())
}

// ---- card facts (from Scryfall): colours of spells, colours produced by lands ----

pub const FACTS_DDL: &str = "CREATE TABLE IF NOT EXISTS card_facts (
        name TEXT PRIMARY KEY,
        colors TEXT NOT NULL,      -- WUBRG letters of the card itself ('' for lands/colourless)
        produces TEXT NOT NULL,    -- WUBRG letters a land makes ('' for fetches, Cavern, non-lands)
        land BOOLEAN NOT NULL,
        fetch BOOLEAN NOT NULL DEFAULT 0  -- sacrificed to find a land: interchangeable with every other fetch, see FETCH
    )";
/// Fetch lands are interchangeable (which basics a list fetches is a colour question, not a card
/// question), so every fetch land is scored under this one name in references and rollouts alike.
pub const FETCH: &str = "fetch land";
pub type Fetches = std::collections::HashSet<String>;

/// Rewrite a card multiset with every fetch land merged into FETCH.
pub fn canon(cards: &BTreeMap<String, i32>, fetches: &Fetches) -> BTreeMap<String, i32> {
    let mut out = BTreeMap::new();
    for (n, q) in cards {
        *out.entry(if fetches.contains(n) { FETCH.to_string() } else { n.clone() }).or_default() += q;
    }
    out
}

fn ensure_facts(conn: &mut SqliteConnection) -> QueryResult<()> {
    diesel::sql_query(FACTS_DDL).execute(conn)?;
    let _ = diesel::sql_query("ALTER TABLE card_facts ADD COLUMN fetch BOOLEAN NOT NULL DEFAULT 0").execute(conn);
    Ok(())
}

pub fn load_lands(conn: &mut SqliteConnection) -> QueryResult<Fetches> {
    ensure_facts(conn)?;
    #[derive(QueryableByName)]
    struct R {
        #[diesel(sql_type = Text)]
        name: String,
    }
    let rows: Vec<R> = diesel::sql_query("SELECT name FROM card_facts WHERE land = 1").load(conn)?;
    Ok(rows.into_iter().map(|r| r.name).collect())
}

pub fn load_fetches(conn: &mut SqliteConnection) -> QueryResult<Fetches> {
    ensure_facts(conn)?;
    #[derive(QueryableByName)]
    struct R {
        #[diesel(sql_type = Text)]
        name: String,
    }
    let rows: Vec<R> = diesel::sql_query("SELECT name FROM card_facts WHERE fetch = 1").load(conn)?;
    Ok(rows.into_iter().map(|r| r.name).collect())
}

/// Load `card_colors.json` (name → {c, p, land}, written by fetch_scryfall.py) into the shared db.
pub fn import_card_facts(conn: &mut SqliteConnection, path: &std::path::Path) -> Result<usize, String> {
    ensure_facts(conn).map_err(|e| e.to_string())?;
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let obj = v.as_object().ok_or("expected an object of name → {c, p, land}")?;
    let mut n = 0;
    conn.transaction::<_, diesel::result::Error, _>(|conn| {
        for (name, f) in obj {
            let c = f.get("c").and_then(|x| x.as_str()).unwrap_or("");
            let p = f.get("p").and_then(|x| x.as_str()).unwrap_or("");
            let land = f.get("land").and_then(|x| x.as_bool()).unwrap_or(false);
            let fetch = f.get("f").and_then(|x| x.as_bool()).unwrap_or(false);
            diesel::sql_query(format!("INSERT OR REPLACE INTO card_facts(name, colors, produces, land, fetch) VALUES ({}, {}, {}, {}, {})", q(name), q(c), q(p), land as i32, fetch as i32)).execute(conn)?;
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
    ensure_facts(conn)?;
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

// ---- inference from the cards revealed in a match: the likelihood of what was revealed, game by
// ---- game, under each reference decklist; a deck is the mixture of its references ----

/// Cards really drawn per nominal draw (7 - mulligans + turns - 1 on the play): lands are played
/// whenever drawn, so the land reveal rate on my registered-list games measures the cantrip surplus.
pub const INFER_DRAW_MULT: f64 = 1.22;
/// Chance a drawn spell is revealed, my registered-list games after the draw multiplier; lands 1.
pub const INFER_REVEAL_SPELL: f64 = 0.78;
/// Rate at which a slot of a real list differs from its reference (my lists against their nearest
/// reference, and pairs of references of one deck). Results are the same from 0.09 to 0.25.
pub const INFER_VARIATION: f64 = 0.094;
/// Mismatch, nats per revealed card from the revealed cards' own terms under the best reference
/// (the draw bookkeeping is left out: it is the same for every deck and grows with the cards a
/// pilot drew but never played), above which no reference explains the rollout. The 98th
/// percentile of my registered-list matches scored under their own deck; at the 95th, Doomsday and
/// Cradle lists a few cards off their anchors were refused, at the 99th decks without a reference
/// are named as their nearest neighbour.
pub const INFER_UNFIT: f64 = 0.27;
/// Posterior mass a deck, or the decks of one archetype together, needs to be named.
pub const INFER_NAME: f64 = 0.9;
/// Decks with at least this posterior are listed as candidates.
pub const INFER_CANDIDATE: f64 = 0.05;

/// One game's evidence for one side.
#[derive(Debug, Clone)]
pub struct GameSeen {
    pub game_number: i32,
    /// nominal cards drawn: 7 - mulligans + turns - 1 on the play
    pub draws: i32,
    /// name → copies revealed (played), fetch lands already merged into FETCH
    pub revealed: BTreeMap<String, i32>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Inference {
    /// the single deck, when its posterior clears INFER_NAME and the rollout fits
    pub deck: Option<String>,
    /// the archetype, when its decks' posteriors together clear INFER_NAME and the rollout fits
    pub archetype: Option<String>,
    /// the best-fitting reference of the best deck
    pub reference_id: Option<i32>,
    /// posterior of the best deck (flat prior over decks)
    pub posterior: f64,
    /// mismatch of the revealed cards under the best reference, see INFER_UNFIT; above it no deck is named
    pub surprise: f64,
    /// mismatch under the best archetype's pooled lists (most copies of each card any of its references
    /// runs, main and side apart): a hybrid of two lists of one archetype is inside the pool
    pub archetype_surprise: f64,
    /// decks with posterior ≥ INFER_CANDIDATE, best first, empty when the rollout fits nothing
    pub candidates: Vec<(String, f64)>,
    pub runner_up_deck: Option<String>,
    pub runner_up_posterior: f64,
    pub n_seen: i32,
    /// cards revealed that the best reference lacks or has fewer of, largest excess first, at most six
    pub misfit: Vec<String>,
    /// every card revealed, rarest across references first ("name" or "name ×n")
    pub seen: Vec<String>,
    /// WUBRG letters the lands revealed can make
    pub colors: String,
}

/// Slot frequency of every card across all references; a card in none gets one slot in all of them.
#[derive(Default)]
pub struct Background {
    freq: std::collections::HashMap<String, f64>,
    floor: f64,
}
impl Background {
    fn new(refs: &[RefList]) -> Background {
        let mut copies: std::collections::HashMap<String, f64> = Default::default();
        let mut slots = 0.0;
        for r in refs {
            for (n, q) in r.main.iter().chain(r.side.iter()) {
                *copies.entry(n.clone()).or_default() += *q as f64;
                slots += *q as f64;
            }
        }
        let slots = slots.max(1.0);
        Background { freq: copies.into_iter().map(|(n, c)| (n, c / slots)).collect(), floor: 1.0 / slots }
    }
    fn of(&self, name: &str) -> f64 {
        self.freq.get(name).copied().unwrap_or(0.0).max(self.floor)
    }
}

/// log |Γ(x)|, Lanczos approximation (g = 7, n = 9); exact to ~1e-13 for x > 0, and by reflection
/// for x < 0.5 (a fully revealed 12-of takes q - k + 1 below zero, where Γ is negative).
fn lgamma(x: f64) -> f64 {
    const C: [f64; 9] = [0.999_999_999_999_809_9, 676.520_368_121_885_1, -1_259.139_216_722_402_8, 771.323_428_777_653_1, -176.615_029_162_140_6, 12.507_343_278_686_905, -0.138_571_095_265_720_12, 9.984_369_578_019_572e-6, 1.505_632_735_149_311_6e-7];
    if x < 0.5 {
        return (std::f64::consts::PI / (std::f64::consts::PI * x).sin().abs()).ln() - lgamma(1.0 - x);
    }
    let x = x - 1.0;
    let mut a = C[0];
    let t = x + 7.5;
    for (i, c) in C.iter().enumerate().skip(1) {
        a += c / (x + i as f64);
    }
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + a.ln()
}
/// log of the binomial coefficient for a possibly fractional q ≥ k.
fn lchoose(q: f64, k: f64) -> f64 {
    lgamma(q + 1.0) - lgamma(k + 1.0) - lgamma(q - k + 1.0)
}

/// Log-likelihood of one game's revealed cards under a pool (main deck, or main + side after game
/// one). The drawn cards are the revealed ones plus hidden ones; a slot of the list is the reference's
/// card at rate 1 - INFER_VARIATION, else a background card, so a revealed card the reference lacks
/// costs the variation rate rather than everything. Lands never hide; a drawn spell hides at
/// 1 - INFER_REVEAL_SPELL, so a four-of never revealed costs exactly the chance it was not drawn or was held.
fn loglik_game(pool: &BTreeMap<String, i32>, g: &GameSeen, bg: &Background, lands: &Fetches) -> (f64, f64) {
    let n_pool: i32 = pool.values().sum();
    let n_pool_f = n_pool as f64;
    let is_land = |c: &str| c == FETCH || lands.contains(c);
    let m: i32 = g.revealed.values().sum();
    let n = ((g.draws as f64 * INFER_DRAW_MULT).round() as i32).max(m).min(n_pool);
    let mut ll = -lchoose(n_pool_f, n as f64);
    let mut cards = 0.0;
    for (c, k) in &g.revealed {
        let q = pool.get(c).copied().unwrap_or(0);
        let qe = (1.0 - INFER_VARIATION) * q as f64 + INFER_VARIATION * n_pool_f * bg.of(c);
        let r = if is_land(c) { 1.0 } else { INFER_REVEAL_SPELL };
        let t = if *k <= q {
            lchoose(qe, *k as f64)
        } else {
            (if q > 0 { lchoose(qe, q as f64) } else { 0.0 }) + (*k - q) as f64 * (INFER_VARIATION * n_pool_f * bg.of(c)).min(0.5).ln()
        } + *k as f64 * r.ln();
        ll += t;
        cards += t;
    }
    let spells_left: i32 = pool.iter().filter(|(c, _)| !is_land(c)).map(|(c, q)| (q - g.revealed.get(c).copied().unwrap_or(0)).max(0)).sum();
    let h = (n - m).max(0).min(spells_left);
    (ll + lchoose(spells_left as f64, h as f64) + h as f64 * (1.0 - INFER_REVEAL_SPELL).ln(), cards)
}

/// (log-likelihood, the revealed cards' own terms) of a match's games under one reference.
fn loglik_ref(main: &BTreeMap<String, i32>, all: &BTreeMap<String, i32>, games: &[GameSeen], bg: &Background, lands: &Fetches) -> (f64, f64) {
    games.iter().map(|g| loglik_game(if g.game_number == 1 { main } else { all }, g, bg, lands)).fold((0.0, 0.0), |a, b| (a.0 + b.0, a.1 + b.1))
}

/// Per reference, (log-likelihood, the revealed cards' own terms) of a match's games. Games are
/// independent given the list, so the scores of pooled matches are the sums of their scores.
pub fn scores(games: &[GameSeen], r: &References) -> Vec<(f64, f64)> {
    r.refs.iter().zip(&r.all_cards).map(|(x, all)| loglik_ref(&x.main, all, games, &r.bg, &r.lands)).collect()
}

/// name → most copies revealed in any one game, over a match's games.
pub fn revealed_all(games: &[GameSeen]) -> BTreeMap<String, i32> {
    let mut all: BTreeMap<String, i32> = BTreeMap::new();
    for g in games {
        for (n, q) in &g.revealed {
            let e = all.entry(n.clone()).or_insert(0);
            *e = (*e).max(*q);
        }
    }
    all
}

/// Score a match's games for one side against every reference. Deck = mixture of its references
/// (equal weights: a list that placed twice is two references), archetype = its decks together.
pub fn infer(games: &[GameSeen], r: &References) -> Inference {
    let n_seen: i32 = games.iter().map(|g| g.revealed.values().sum::<i32>()).sum();
    decide(&revealed_all(games), n_seen, &scores(games, r), games, r)
}

/// The decision from per-reference scores: `all` and `n_seen` describe the revealed cards, `games`
/// is only needed for the archetype pool's own score.
pub fn decide(all: &BTreeMap<String, i32>, n_seen: i32, scored: &[(f64, f64)], games: &[GameSeen], r: &References) -> Inference {
    let empty = |seen: Vec<String>, colors: String| Inference { deck: None, archetype: None, reference_id: None, posterior: 0.0, surprise: 0.0, archetype_surprise: 0.0, candidates: vec![], runner_up_deck: None, runner_up_posterior: 0.0, n_seen, misfit: vec![], seen, colors };
    let bg = &r.bg;
    let mut seen: Vec<(f64, String)> = all.iter().map(|(n, q)| (bg.of(n), if *q > 1 { format!("{n} ×{q}") } else { n.clone() })).collect();
    seen.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap().then(a.1.cmp(&b.1)));
    let seen: Vec<String> = seen.into_iter().map(|(_, s)| s).collect();
    let colors = mana_colors(all, &BTreeMap::new(), &r.produces);
    if n_seen == 0 || r.refs.is_empty() || scored.iter().all(|s| s.0 == f64::NEG_INFINITY) {
        return empty(seen, colors);
    }
    let lls: Vec<f64> = scored.iter().map(|s| s.0).collect();
    let mut by_deck: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, x) in r.refs.iter().enumerate() {
        by_deck.entry(x.deck.as_str()).or_default().push(i);
    }
    let mut deck_ll: Vec<(f64, &str, usize)> = by_deck
        .iter()
        .map(|(d, idx)| {
            let m = idx.iter().map(|i| lls[*i]).fold(f64::NEG_INFINITY, f64::max);
            let best = *idx.iter().max_by(|a, b| lls[**a].total_cmp(&lls[**b])).unwrap();
            let ll = if m == f64::NEG_INFINITY { m } else { m + (idx.iter().map(|i| (lls[*i] - m).exp()).sum::<f64>() / idx.len() as f64).ln() };
            (ll, *d, best)
        })
        .collect();
    deck_ll.sort_by(|a, b| b.0.total_cmp(&a.0));
    let top = deck_ll[0].0;
    let z: f64 = deck_ll.iter().map(|(l, _, _)| (l - top).exp()).sum();
    let post: Vec<(String, f64, usize)> = deck_ll.iter().map(|(l, d, b)| (d.to_string(), (l - top).exp() / z, *b)).collect();
    let (best_deck, best_p, best_ref) = (&post[0].0, post[0].1, post[0].2);
    let surprise = -scored[best_ref].1 / n_seen as f64;
    let fits = surprise <= INFER_UNFIT;
    let mut by_arch: BTreeMap<&str, f64> = BTreeMap::new();
    for (_, p, b) in &post {
        *by_arch.entry(r.refs[*b].archetype.as_str()).or_default() += p;
    }
    let (arch, arch_p) = by_arch.iter().max_by(|a, b| a.1.partial_cmp(b.1).unwrap()).map(|(a, p)| (a.to_string(), *p)).unwrap();
    let archetype_surprise = match r.pools.get(&arch) {
        Some((pm, ps)) => {
            let mut pall = pm.clone();
            for (n, q) in ps {
                *pall.entry(n.clone()).or_default() += q;
            }
            -loglik_ref(pm, &pall, games, bg, &r.lands).1 / n_seen as f64
        }
        None => f64::INFINITY,
    };
    let arch_fits = fits || archetype_surprise <= INFER_UNFIT;
    let x = &r.refs[best_ref];
    let mut misfit: Vec<(i32, String)> = all
        .iter()
        .map(|(n, q)| (q - x.main.get(n).copied().unwrap_or(0) - x.side.get(n).copied().unwrap_or(0), n.clone()))
        .filter(|(e, _)| *e > 0)
        .collect();
    misfit.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    Inference {
        deck: (fits && best_p >= INFER_NAME).then(|| best_deck.clone()),
        archetype: (arch_fits && arch_p >= INFER_NAME).then_some(arch),
        reference_id: fits.then_some(x.id),
        posterior: best_p,
        surprise,
        archetype_surprise,
        candidates: if fits { post.iter().filter(|(_, p, _)| *p >= INFER_CANDIDATE).map(|(d, p, _)| (d.clone(), *p)).collect() } else { vec![] },
        runner_up_deck: post.get(1).map(|(d, _, _)| d.clone()),
        runner_up_posterior: post.get(1).map_or(0.0, |(_, p, _)| *p),
        n_seen,
        misfit: misfit.into_iter().take(6).map(|(_, n)| n).collect(),
        seen,
        colors,
    }
}

// ---- per-match deck labels for both sides, with provenance ----

pub const MATCH_DECKS_DDL: &str = "CREATE TABLE IF NOT EXISTS match_decks (
        match_id INTEGER NOT NULL,
        side TEXT NOT NULL,                    -- me | opp
        source TEXT NOT NULL,                  -- list (registered list named by a reference) | cards (inferred from the games)
        deck TEXT,                             -- NULL when ambiguous or unfit
        archetype TEXT,                        -- NULL when ambiguous across archetypes or unfit
        candidates TEXT NOT NULL,              -- '|'-joined decks with posterior >= INFER_CANDIDATE, best first ('' = unfit)
        candidate_posteriors TEXT NOT NULL DEFAULT '',  -- '|'-joined posteriors matching candidates
        posterior REAL NOT NULL,               -- of the best deck
        surprise REAL NOT NULL,                -- mismatch under the best reference, see INFER_UNFIT
        archetype_surprise REAL NOT NULL DEFAULT 0,  -- mismatch under the best archetype's pooled lists
        runner_up_deck TEXT,
        runner_up_posterior REAL NOT NULL DEFAULT 0,
        n_seen INTEGER NOT NULL,
        list_id INTEGER,                       -- me: the registered list inferred for an unlinked match (see infer_list)
        reference_id INTEGER,                  -- the best-fitting reference of the deck (a list's assigned reference for source = list)
        colors TEXT NOT NULL DEFAULT '',       -- WUBRG letters the lands revealed can make (a list's lands for source = list)
        misfit TEXT NOT NULL DEFAULT '',       -- '|'-joined cards revealed that the best reference lacks, largest excess first
        seen TEXT NOT NULL DEFAULT '',         -- '|'-joined cards revealed, rarest first
        PRIMARY KEY (match_id, side)
    )";

#[derive(QueryableByName)]
struct GameRow {
    #[diesel(sql_type = Integer)]
    id: i32,
    #[diesel(sql_type = Integer)]
    match_id: i32,
    #[diesel(sql_type = Integer)]
    game_number: i32,
    #[diesel(sql_type = Nullable<diesel::sql_types::Bool>)]
    on_play: Option<bool>,
    #[diesel(sql_type = Integer)]
    turns: i32,
    #[diesel(sql_type = Integer)]
    mulligans: i32,
}

#[derive(QueryableByName)]
struct RevealedRow {
    #[diesel(sql_type = Integer)]
    game_id: i32,
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

/// Every game of every match for one side, with what that side revealed; games with nothing
/// revealed still count (their draws are evidence of absence).
pub fn games_by_match(conn: &mut SqliteConnection, side: &str, fetches: &Fetches) -> QueryResult<std::collections::HashMap<i32, Vec<GameSeen>>> {
    let mull = if side == "me" { "my_mulligans" } else { "opp_mulligans" };
    let games: Vec<GameRow> = diesel::sql_query(format!("SELECT id, match_id, game_number, on_play, COALESCE(turns, 0) AS turns, COALESCE({mull}, 0) AS mulligans FROM mtgo_games ORDER BY match_id, game_number")).load(conn)?;
    let revealed: Vec<RevealedRow> = diesel::sql_query(format!("SELECT game_id, name, COALESCE(MAX(instances), 1) AS qty FROM mtgo_game_cards WHERE side = '{side}' AND played = 1 GROUP BY game_id, name")).load(conn)?;
    let mut by_game: std::collections::HashMap<i32, BTreeMap<String, i32>> = Default::default();
    for r in revealed {
        *by_game.entry(r.game_id).or_default().entry(if fetches.contains(&r.name) { FETCH.to_string() } else { r.name }).or_default() += r.qty;
    }
    let mut out: std::collections::HashMap<i32, Vec<GameSeen>> = Default::default();
    for g in games {
        let me_on_play = g.on_play.unwrap_or(false);
        let on_play = if side == "me" { me_on_play } else { !me_on_play };
        let draws = (7 - g.mulligans + g.turns - on_play as i32).max(1);
        out.entry(g.match_id).or_default().push(GameSeen { game_number: g.game_number, draws, revealed: by_game.remove(&g.id).unwrap_or_default() });
    }
    out.retain(|_, gs| gs.iter().any(|g| !g.revealed.is_empty()));
    Ok(out)
}

/// Revealed counts capped at what a list can hold, see `References::cap`.
pub fn cap_revealed(games: &mut [GameSeen], r: &References) {
    for g in games {
        for (n, k) in g.revealed.iter_mut() {
            *k = r.cap(n, *k);
        }
    }
}

/// My side plays one list for a run of matches: within a day, in time order, pool the games of
/// consecutive matches while the pooled games still fit some deck; a switch breaks the run.
pub fn pool_runs(matches: &[(i32, String)], games: &std::collections::HashMap<i32, Vec<GameSeen>>, refs: &References) -> std::collections::HashMap<i32, (Vec<GameSeen>, Vec<(f64, f64)>)> {
    let mut days: std::collections::BTreeMap<&str, Vec<i32>> = Default::default();
    for (id, date) in matches {
        if games.contains_key(id) {
            days.entry(date.as_str()).or_default().push(*id);
        }
    }
    let mut pooled = std::collections::HashMap::new();
    let sc: std::collections::HashMap<i32, Vec<(f64, f64)>> = games.iter().map(|(id, gs)| (*id, scores(gs, refs))).collect();
    let add = |a: &[(f64, f64)], b: &[(f64, f64)]| -> Vec<(f64, f64)> { a.iter().zip(b).map(|(x, y)| (x.0 + y.0, x.1 + y.1)).collect() };
    for ids in days.values_mut() {
        ids.sort();
        let (mut run, mut acc, mut acc_sc): (Vec<i32>, Vec<GameSeen>, Vec<(f64, f64)>) = (vec![], vec![], vec![]);
        for id in ids.iter() {
            let gs = &games[id];
            let mut trial = acc.clone();
            trial.extend(gs.iter().cloned());
            let trial_sc = if run.is_empty() { sc[id].clone() } else { add(&acc_sc, &sc[id]) };
            let n: i32 = trial.iter().map(|g| g.revealed.values().sum::<i32>()).sum();
            if !run.is_empty() && decide(&revealed_all(&trial), n, &trial_sc, &trial, refs).surprise > INFER_UNFIT {
                for r in run.drain(..) {
                    pooled.insert(r, (acc.clone(), acc_sc.clone()));
                }
                acc = gs.clone();
                acc_sc = sc[id].clone();
            } else {
                acc = trial;
                acc_sc = trial_sc;
            }
            run.push(*id);
        }
        for r in run {
            pooled.insert(r, (acc.clone(), acc_sc.clone()));
        }
    }
    pooled
}

#[derive(QueryableByName)]
struct ListCardRow {
    #[diesel(sql_type = Integer)]
    list_id: i32,
    #[diesel(sql_type = Text)]
    name: String,
    #[diesel(sql_type = Integer)]
    qty: i32,
    #[diesel(sql_type = diesel::sql_types::Bool)]
    sideboard: bool,
}
#[derive(QueryableByName)]
struct ListSeenRow {
    #[diesel(sql_type = Integer)]
    id: i32,
    #[diesel(sql_type = Text)]
    first_seen: String,
}
#[derive(QueryableByName)]
struct ListDeckRow {
    #[diesel(sql_type = Integer)]
    list_id: i32,
    #[diesel(sql_type = Nullable<Text>)]
    deck: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    archetype: Option<String>,
    #[diesel(sql_type = Nullable<Integer>)]
    reference_id: Option<i32>,
    #[diesel(sql_type = Nullable<Text>)]
    colors: Option<String>,
}

/// A registered list as a hypothesis: its own "deck" named `list#<id>` (id negated as the reference
/// id) under its assigned archetype, available to matches from the day the client first reported it.
pub struct PlayerList {
    pub list: RefList,
    /// YYYY-MM-DD
    pub first_seen: String,
}

/// The player's registered lists as hypotheses next to the references, so an unlinked match can be
/// placed on the exact list it was played with.
pub fn player_lists(conn: &mut SqliteConnection, fetches: &Fetches) -> QueryResult<Vec<PlayerList>> {
    let cards: Vec<ListCardRow> = diesel::sql_query("SELECT list_id, name, qty, sideboard FROM mtgo_list_cards").load(conn)?;
    let seen: Vec<ListSeenRow> = diesel::sql_query("SELECT id, COALESCE(substr(first_seen, 1, 10), '') AS first_seen FROM mtgo_lists").load(conn)?;
    let first_seen: std::collections::HashMap<i32, String> = seen.into_iter().map(|r| (r.id, r.first_seen)).collect();
    let decks: Vec<ListDeckRow> = diesel::sql_query("SELECT list_id, deck, archetype, reference_id, colors FROM list_assignments").load(conn)?;
    let assigned: std::collections::HashMap<i32, (Option<String>, Option<String>)> = decks.into_iter().map(|d| (d.list_id, (d.deck, d.archetype))).collect();
    let mut lists: BTreeMap<i32, (BTreeMap<String, i32>, BTreeMap<String, i32>)> = BTreeMap::new();
    for c in cards {
        let e = lists.entry(c.list_id).or_default();
        *(if c.sideboard { &mut e.1 } else { &mut e.0 }).entry(c.name).or_default() += c.qty;
    }
    Ok(lists
        .into_iter()
        .map(|(id, (main, side))| {
            let (_, arch) = assigned.get(&id).cloned().unwrap_or((None, None));
            PlayerList { list: RefList { id: -id, archetype: arch.unwrap_or_else(|| "unassigned list".into()), deck: format!("list#{id}"), era: first_seen.get(&id).cloned().unwrap_or_default(), main: canon(&main, fetches), side: canon(&side, fetches), colors: String::new() }, first_seen: first_seen.get(&id).cloned().unwrap_or_default() }
        })
        .collect())
}

/// The registered list a match on `date` was played with, when one of the player's lists known by
/// then beats every reference and every other list: (list id, posterior). `all` is
/// `refs.with_lists(lists)`; a list first seen after `date` (its `era`) is not a hypothesis.
pub fn infer_list(games: &[GameSeen], date: &str, all: &References) -> Option<(i32, f64)> {
    if !all.refs.iter().any(|x| x.id < 0 && !x.era.is_empty() && x.era.as_str() <= date) {
        return None;
    }
    let mut sc = scores(games, all);
    for (s, x) in sc.iter_mut().zip(&all.refs) {
        if x.id < 0 && (x.era.is_empty() || x.era.as_str() > date) {
            *s = (f64::NEG_INFINITY, 0.0);
        }
    }
    let n_seen: i32 = games.iter().map(|g| g.revealed.values().sum::<i32>()).sum();
    let i = decide(&revealed_all(games), n_seen, &sc, games, all);
    let deck = i.deck?;
    let id: i32 = deck.strip_prefix("list#")?.parse().ok()?;
    Some((id, i.posterior))
}

pub struct MatchDeckCounts {
    pub me_list: usize,
    pub me_cards: usize,
    /// unlinked matches placed on one of the player's registered lists
    pub me_list_inferred: usize,
    pub opp_named: usize,
    pub opp_archetype_only: usize,
}

/// Rewrite match_decks for a player: my side from the registered list when it is named, else from
/// the pooled games; the opponent from their games per match.
pub fn label_matches(conn: &mut SqliteConnection, refs: &References) -> QueryResult<MatchDeckCounts> {
    diesel::sql_query("DROP TABLE IF EXISTS match_decks").execute(conn)?;
    diesel::sql_query(MATCH_DECKS_DDL).execute(conn)?;
    let matches: Vec<MatchRow> = diesel::sql_query(
        "SELECT m.id, COALESCE(substr(m.start_ts,1,10), '') AS date, a.deck AS list_deck, a.archetype AS list_archetype
         FROM mtgo_matches m LEFT JOIN list_assignments a ON a.list_id = m.my_list_id WHERE m.replay = 0 ORDER BY m.start_ts",
    )
    .load(conn)?;
    let timing = std::env::var_os("MTGCTL_TIMING").is_some();
    let t = std::time::Instant::now();
    let mut me = games_by_match(conn, "me", &refs.fetches)?;
    let mut opp = games_by_match(conn, "opp", &refs.fetches)?;
    for gs in me.values_mut().chain(opp.values_mut()) {
        cap_revealed(gs, refs);
    }
    if timing { eprintln!("label: load games {:.1}s", t.elapsed().as_secs_f64()); }
    let t = std::time::Instant::now();
    let pooled = pool_runs(&matches.iter().map(|m| (m.id, m.date.clone())).collect::<Vec<_>>(), &me, refs);
    if timing { eprintln!("label: pool runs {:.1}s", t.elapsed().as_secs_f64()); }
    let t = std::time::Instant::now();
    let mut counts = MatchDeckCounts { me_list: 0, me_cards: 0, me_list_inferred: 0, opp_named: 0, opp_archetype_only: 0 };
    let lists = player_lists(conn, &refs.fetches)?;
    let with_lists = refs.with_lists(&lists);
    let list_deck: std::collections::HashMap<i32, ListDeckRow> = {
        let rows: Vec<ListDeckRow> = diesel::sql_query("SELECT list_id, deck, archetype, reference_id, colors FROM list_assignments").load(conn)?;
        rows.into_iter().map(|d| (d.list_id, d)).collect()
    };
    let list_of_match: std::collections::HashMap<i32, i32> = {
        #[derive(QueryableByName)]
        struct R {
            #[diesel(sql_type = Integer)]
            id: i32,
            #[diesel(sql_type = Integer)]
            list_id: i32,
        }
        let rows: Vec<R> = diesel::sql_query("SELECT id, my_list_id AS list_id FROM mtgo_matches WHERE my_list_id IS NOT NULL").load(conn)?;
        rows.into_iter().map(|r| (r.id, r.list_id)).collect()
    };
    let ins = |conn: &mut SqliteConnection, id: i32, side: &str, source: &str, i: &Inference, list_id: Option<i32>| -> QueryResult<usize> {
        let o = |v: &Option<String>| v.as_ref().map(|s| q(s)).unwrap_or("NULL".into());
        let cands: Vec<String> = i.candidates.iter().map(|(d, _)| d.clone()).collect();
        let posts: Vec<String> = i.candidates.iter().map(|(_, p)| format!("{p:.2}")).collect();
        diesel::sql_query(format!(
            "INSERT INTO match_decks(match_id, side, source, deck, archetype, candidates, candidate_posteriors, posterior, surprise, archetype_surprise, runner_up_deck, runner_up_posterior, n_seen, list_id, reference_id, colors, misfit, seen) VALUES ({id}, '{side}', '{source}', {}, {}, {}, {}, {:.3}, {:.3}, {:.3}, {}, {:.3}, {}, {}, {}, {}, {}, {})",
            o(&i.deck), o(&i.archetype), q(&cands.join("|")), q(&posts.join("|")), i.posterior, i.surprise, i.archetype_surprise, o(&i.runner_up_deck), i.runner_up_posterior, i.n_seen, list_id.map_or("NULL".to_string(), |l| l.to_string()), i.reference_id.map_or("NULL".to_string(), |l| l.to_string()), q(&i.colors), q(&i.misfit.join("|")), q(&i.seen.join("|"))
        ))
        .execute(conn)
    };
    if timing { eprintln!("label: player lists {:.1}s", t.elapsed().as_secs_f64()); }
    let t = std::time::Instant::now();
    conn.transaction::<_, diesel::result::Error, _>(|conn| {
        for m in &matches {
            if m.list_deck.is_some() {
                let la = list_of_match.get(&m.id).and_then(|l| list_deck.get(l));
                let listed = Inference { deck: m.list_deck.clone(), archetype: m.list_archetype.clone(), reference_id: la.and_then(|a| a.reference_id), posterior: 1.0, surprise: 0.0, archetype_surprise: 0.0, candidates: vec![(m.list_deck.clone().unwrap(), 1.0)], runner_up_deck: None, runner_up_posterior: 0.0, n_seen: 0, misfit: vec![], seen: vec![], colors: la.and_then(|a| a.colors.clone()).unwrap_or_default() };
                ins(conn, m.id, "me", "list", &listed, None)?;
                counts.me_list += 1;
            } else if let Some((gs, gsc)) = pooled.get(&m.id) {
                // the list from this match alone: a run pools across version switches a card apart, which the fit test cannot see
                if let Some((lid, p)) = infer_list(&me[&m.id], &m.date, &with_lists) {
                    // the games were played with a registered list: its assigned deck, if it has one
                    let la = list_deck.get(&lid);
                    let deck = la.and_then(|a| a.deck.clone());
                    let i = Inference { deck: deck.clone(), archetype: la.and_then(|a| a.archetype.clone()), reference_id: la.and_then(|a| a.reference_id), posterior: p, surprise: 0.0, archetype_surprise: 0.0, candidates: deck.iter().map(|d| (d.clone(), p)).collect(), runner_up_deck: None, runner_up_posterior: 0.0, n_seen: gs.iter().map(|g| g.revealed.values().sum::<i32>()).sum(), misfit: vec![], seen: vec![], colors: la.and_then(|a| a.colors.clone()).unwrap_or_default() };
                    ins(conn, m.id, "me", "cards", &i, Some(lid))?;
                    counts.me_list_inferred += 1;
                    if i.deck.is_some() || i.archetype.is_some() {
                        counts.me_cards += 1;
                    }
                } else {
                    let n: i32 = gs.iter().map(|g| g.revealed.values().sum::<i32>()).sum();
                    let i = decide(&revealed_all(gs), n, gsc, gs, refs);
                    ins(conn, m.id, "me", "cards", &i, None)?;
                    if i.deck.is_some() || i.archetype.is_some() {
                        counts.me_cards += 1;
                    }
                }
            }
            if let Some(gs) = opp.get(&m.id) {
                let i = infer(gs, refs);
                ins(conn, m.id, "opp", "cards", &i, None)?;
                if i.deck.is_some() {
                    counts.opp_named += 1;
                } else if i.archetype.is_some() {
                    counts.opp_archetype_only += 1;
                }
            }
        }
        Ok(())
    })?;
    if timing { eprintln!("label: score + insert {:.1}s", t.elapsed().as_secs_f64()); }
    Ok(counts)
}
