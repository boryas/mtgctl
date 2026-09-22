//! Legacy Data Collection (LDC) per-event sheets: who played what in each MTGO Legacy event.
//! Imported from the JSON written by ~/.local/share/mtgo/ldc/parse_ldc.py. Rows naming an opponent
//! I faced in that same event are ground truth for the labeller (see knn.rs).

use diesel::prelude::*;
use diesel::sql_types::{Integer, Nullable, Text};
use diesel::sqlite::SqliteConnection;
use std::path::Path;

pub const DDL: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS ldc_players (
        event TEXT NOT NULL,             -- sheet name, e.g. 'Legacy Challenge 32 9_6_2026 - 2'
        date TEXT NOT NULL,              -- YYYY-MM-DD
        kind TEXT,                       -- Challenge | Showcase | Last Chance
        player TEXT NOT NULL,
        deck TEXT,                       -- LDC deck name (definitions/mtgo/ldc_decks.toml maps it)
        archetype TEXT,                  -- LDC macro-archetype (Tempo, Spell Combo, ...)
        notes TEXT,
        wins INTEGER,
        losses INTEGER,
        PRIMARY KEY (event, player)
    )",
    "CREATE INDEX IF NOT EXISTS idx_ldc_players_date ON ldc_players(date)",
];

pub fn run_import(conn: &mut SqliteConnection, path: &Path) -> Result<(), String> {
    super::db::ensure_schema(conn).map_err(|e| e.to_string())?;
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let players = v.get("players").and_then(|p| p.as_array()).ok_or("no 'players' array")?;
    let s = |p: &serde_json::Value, k: &str| p.get(k).and_then(|x| x.as_str()).map(str::to_string);
    let n = |p: &serde_json::Value, k: &str| p.get(k).and_then(|x| x.as_f64()).map(|x| x as i32);
    let mut rows = 0;
    conn.transaction::<_, diesel::result::Error, _>(|conn| {
        diesel::sql_query("DELETE FROM ldc_players").execute(conn)?;
        for p in players {
            let (Some(event), Some(date), Some(player)) = (s(p, "event"), s(p, "date"), s(p, "name")) else { continue };
            diesel::sql_query("INSERT OR REPLACE INTO ldc_players(event, date, kind, player, deck, archetype, notes, wins, losses) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)")
                .bind::<Text, _>(event)
                .bind::<Text, _>(date)
                .bind::<Nullable<Text>, _>(s(p, "kind"))
                .bind::<Text, _>(player)
                .bind::<Nullable<Text>, _>(s(p, "deck"))
                .bind::<Nullable<Text>, _>(s(p, "archetype"))
                .bind::<Nullable<Text>, _>(s(p, "notes"))
                .bind::<Nullable<Integer>, _>(n(p, "wins"))
                .bind::<Nullable<Integer>, _>(n(p, "losses"))
                .execute(conn)?;
            rows += 1;
        }
        Ok(())
    })
    .map_err(|e| e.to_string())?;
    #[derive(QueryableByName)]
    struct C {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        n: i64,
        #[diesel(sql_type = Nullable<Text>)]
        lo: Option<String>,
        #[diesel(sql_type = Nullable<Text>)]
        hi: Option<String>,
    }
    let c: C = diesel::sql_query("SELECT COUNT(DISTINCT event) AS n, MIN(date) AS lo, MAX(date) AS hi FROM ldc_players").get_result(conn).map_err(|e| e.to_string())?;
    println!("ldc_players: {rows} player-events over {} events, {} .. {}", c.n, c.lo.unwrap_or_default(), c.hi.unwrap_or_default());
    Ok(())
}
