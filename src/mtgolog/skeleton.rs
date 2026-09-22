//! Archetype skeletons: MTGGoldfish's per-archetype card breakdown ("4.0 in 52% of decks") and
//! metagame share, imported from the skeletons.json written by ~/.local/share/mtgo/ldc/fetch_goldfish.py.
//! A skeleton is what a deck *contains*, not what it looks like across two games; the labeller
//! consults skeletons only when no observed anchor is near (see knn.rs).

use diesel::prelude::*;
use diesel::sql_types::{Bool, Double, Nullable, Text};
use diesel::sqlite::SqliteConnection;
use std::collections::BTreeMap;
use std::path::Path;

pub const DDL: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS skeletons (
        name TEXT PRIMARY KEY,           -- Goldfish archetype name
        share REAL,                      -- META% (online)
        fetched TEXT NOT NULL,
        example TEXT                     -- an example decklist path on mtggoldfish.com
    )",
    "CREATE TABLE IF NOT EXISTS skeleton_cards (
        name TEXT NOT NULL,
        card TEXT NOT NULL,
        copies REAL NOT NULL,            -- average copies when played
        pct REAL NOT NULL,               -- share of the archetype's lists that play it (0..1)
        sideboard BOOLEAN NOT NULL,
        PRIMARY KEY (name, card, sideboard)
    )",
];

pub fn run_import(conn: &mut SqliteConnection, path: &Path) -> Result<(), String> {
    super::db::ensure_schema(conn).map_err(|e| e.to_string())?;
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let fetched = v.get("fetched").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let archs = v.get("archetypes").and_then(|a| a.as_array()).ok_or("no 'archetypes' array")?;
    let (mut n, mut cards) = (0, 0);
    conn.transaction::<_, diesel::result::Error, _>(|conn| {
        diesel::sql_query("DELETE FROM skeletons").execute(conn)?;
        diesel::sql_query("DELETE FROM skeleton_cards").execute(conn)?;
        for a in archs {
            let Some(name) = a.get("name").and_then(|x| x.as_str()) else { continue };
            diesel::sql_query("INSERT INTO skeletons(name, share, fetched, example) VALUES (?, ?, ?, ?)")
                .bind::<Text, _>(name)
                .bind::<Nullable<Double>, _>(a.get("share").and_then(|x| x.as_f64()))
                .bind::<Text, _>(&fetched)
                .bind::<Nullable<Text>, _>(a.get("example").and_then(|x| x.as_str()).map(str::to_string))
                .execute(conn)?;
            n += 1;
            for c in a.get("cards").and_then(|c| c.as_array()).into_iter().flatten() {
                let (Some(card), Some(copies), Some(pct)) = (c.get("name").and_then(|x| x.as_str()), c.get("copies").and_then(|x| x.as_f64()), c.get("pct").and_then(|x| x.as_f64())) else { continue };
                diesel::sql_query("INSERT OR REPLACE INTO skeleton_cards(name, card, copies, pct, sideboard) VALUES (?, ?, ?, ?, ?)")
                    .bind::<Text, _>(name)
                    .bind::<Text, _>(card)
                    .bind::<Double, _>(copies)
                    .bind::<Double, _>(pct)
                    .bind::<Bool, _>(c.get("sideboard").and_then(|x| x.as_bool()).unwrap_or(false))
                    .execute(conn)?;
                cards += 1;
            }
        }
        Ok(())
    })
    .map_err(|e| e.to_string())?;
    println!("skeletons: {n} archetypes, {cards} card rows (fetched {fetched})");
    Ok(())
}

pub struct Skeleton {
    pub name: String,
    pub share: f64,
    /// card → weight: share of lists playing it; sideboard cards at half weight (seen post-board only)
    pub cards: BTreeMap<String, f32>,
}

pub fn load(conn: &mut SqliteConnection) -> QueryResult<Vec<Skeleton>> {
    #[derive(QueryableByName)]
    struct R {
        #[diesel(sql_type = Text)]
        name: String,
        #[diesel(sql_type = Nullable<Double>)]
        share: Option<f64>,
        #[diesel(sql_type = Text)]
        card: String,
        #[diesel(sql_type = Double)]
        pct: f64,
        #[diesel(sql_type = Bool)]
        sideboard: bool,
    }
    let rows: Vec<R> = diesel::sql_query("SELECT s.name, s.share, c.card, c.pct, c.sideboard FROM skeletons s JOIN skeleton_cards c ON c.name = s.name ORDER BY s.name").load(conn)?;
    let mut out: Vec<Skeleton> = Vec::new();
    for r in rows {
        if out.last().map(|s| s.name != r.name).unwrap_or(true) {
            out.push(Skeleton { name: r.name.clone(), share: r.share.unwrap_or(0.0), cards: BTreeMap::new() });
        }
        let w = (r.pct as f32) * if r.sideboard { 0.5 } else { 1.0 };
        let e = out.last_mut().unwrap().cards.entry(r.card).or_insert(0.0);
        *e = e.max(w);
    }
    Ok(out)
}
