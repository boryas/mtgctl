//! Minimal observed colours: what a side's play *proves* about its deck's colours. A land it played
//! proves the colours that land produces; a spell it cast by paying the mana cost proves the
//! single-colour symbols in that cost. Nothing else counts: fetchlands, Wasteland and rainbow lands
//! produce no specific colour; cards cast for an alternative cost (pitch, life, evoke), cycled,
//! revealed or discarded were never paid for — Edge of Autumn is cycled in a deck with no green
//! source. Card facts come from Scryfall via ~/.local/share/mtgo/ldc/fetch_scryfall.py →
//! `mtgo cards-import`. Stored per (match, side) as a WUBRG-ordered string, so "RW" means an
//! Energy deck that has shown no black — an observation, not a vote.

use diesel::prelude::*;
use diesel::sql_types::{Bool, Text};
use diesel::sqlite::SqliteConnection;
use std::collections::HashMap;
use std::path::Path;

pub const DDL: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS card_colors (
        name TEXT PRIMARY KEY,
        colors TEXT NOT NULL,            -- mana-cost colours, WUBRG order ('' = colourless)
        produces TEXT NOT NULL,          -- colours a land produces ('' = none / any)
        land BOOLEAN NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS mtgo_match_colors (
        match_id INTEGER NOT NULL,
        side TEXT NOT NULL,
        colors TEXT NOT NULL,            -- minimal observed colours, WUBRG order
        PRIMARY KEY (match_id, side)
    )",
];

pub fn run_import(conn: &mut SqliteConnection, path: &Path) -> Result<(), String> {
    super::db::ensure_schema(conn).map_err(|e| e.to_string())?;
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let obj = v.as_object().ok_or("expected a JSON object of card name → {c, p, land}")?;
    let mut n = 0;
    conn.transaction::<_, diesel::result::Error, _>(|conn| {
        diesel::sql_query("DELETE FROM card_colors").execute(conn)?;
        for (name, e) in obj {
            let g = |k: &str| e.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
            diesel::sql_query("INSERT OR REPLACE INTO card_colors(name, colors, produces, land) VALUES (?, ?, ?, ?)")
                .bind::<Text, _>(name)
                .bind::<Text, _>(g("c"))
                .bind::<Text, _>(g("p"))
                .bind::<Bool, _>(e.get("land").and_then(|x| x.as_bool()).unwrap_or(false))
                .execute(conn)?;
            n += 1;
        }
        Ok(())
    })
    .map_err(|e| e.to_string())?;
    println!("card_colors: {n} names");
    Ok(())
}

const ORDER: &str = "WUBRG";

/// Recompute mtgo_match_colors for every (match, side) from the cards each side showed.
pub fn compute(conn: &mut SqliteConnection) -> QueryResult<usize> {
    #[derive(QueryableByName)]
    struct Card {
        #[diesel(sql_type = Text)]
        name: String,
        #[diesel(sql_type = Text)]
        colors: String,
        #[diesel(sql_type = Text)]
        produces: String,
        #[diesel(sql_type = Bool)]
        land: bool,
    }
    #[derive(QueryableByName)]
    struct Seen {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        match_id: i32,
        #[diesel(sql_type = Text)]
        side: String,
        #[diesel(sql_type = Text)]
        name: String,
    }
    let facts: HashMap<String, (String, String, bool)> = diesel::sql_query("SELECT name, colors, produces, land FROM card_colors").load::<Card>(conn)?.into_iter().map(|c| (c.name, (c.colors, c.produces, c.land))).collect();
    if facts.is_empty() {
        return Ok(0);
    }
    let dfc = super::cluster::dfc_map(conn)?;
    // paid casts and land drops only; " by …" / "for its … cost" mark alternative costs
    let seen: Vec<Seen> = diesel::sql_query(
        "SELECT DISTINCT e.match_id, e.side, e.card AS name FROM mtgo_events e JOIN mtgo_matches m ON m.id = e.match_id AND m.replay = 0
         WHERE e.card IS NOT NULL AND e.side IS NOT NULL AND (e.kind = 'PlaysLand' OR (e.kind = 'Casts' AND e.text NOT LIKE '% by %' AND e.text NOT LIKE '% for its % cost%'))",
    )
    .load(conn)?;
    let mut proof: HashMap<(i32, String), [bool; 5]> = HashMap::new();
    for s in seen {
        let name = dfc.get(&s.name).cloned().unwrap_or(s.name);
        let Some((colors, produces, land)) = facts.get(&name) else { continue };
        let cols = if *land { produces } else { colors };
        let e = proof.entry((s.match_id, s.side)).or_insert([false; 5]);
        for (i, ch) in ORDER.chars().enumerate() {
            if cols.contains(ch) {
                e[i] = true;
            }
        }
    }
    let n = proof.len();
    conn.transaction::<_, diesel::result::Error, _>(|conn| {
        diesel::sql_query("DELETE FROM mtgo_match_colors").execute(conn)?;
        for ((mid, side), p) in proof {
            let s: String = ORDER.chars().zip(p).filter(|(_, b)| *b).map(|(c, _)| c).collect();
            diesel::sql_query("INSERT INTO mtgo_match_colors(match_id, side, colors) VALUES (?, ?, ?)")
                .bind::<diesel::sql_types::Integer, _>(mid)
                .bind::<Text, _>(side)
                .bind::<Text, _>(s)
                .execute(conn)?;
        }
        Ok(())
    })?;
    Ok(n)
}
