//! Per-card contingency analysis: for each card a side played in a game, compare my win rate in
//! games where it appeared vs games where it did not (within the same filter set).
//! First-order only (no confounder control) — a logistic regression over card presence is the
//! planned follow-up; this is the "which cards correlate with winning/losing" census.

use super::stats::{ensure_view, wilson};
use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, CellAlignment, Table};
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Text};
use diesel::sqlite::SqliteConnection;

#[derive(QueryableByName)]
struct Tot {
    #[diesel(sql_type = BigInt)]
    n: i64,
    #[diesel(sql_type = BigInt)]
    w: i64,
}

#[derive(QueryableByName)]
struct CardRow {
    #[diesel(sql_type = Text)]
    name: String,
    #[diesel(sql_type = BigInt)]
    n: i64,
    #[diesel(sql_type = BigInt)]
    w: i64,
}

pub fn run_cards(conn: &mut SqliteConnection, side: &str, where_sql: Option<&str>, by_turn: Option<i32>, seen: bool, min: i64, sort: &str, limit: usize) -> QueryResult<()> {
    super::db::ensure_schema(conn)?;
    ensure_view(conn)?;
    let side = match side {
        "me" | "opp" => side,
        _ => {
            eprintln!("--side must be me or opp");
            return Ok(());
        }
    };
    let wher = where_sql.map(|w| format!("AND ({w})")).unwrap_or_default();
    let games = format!("SELECT game_id, won FROM mtgo_game_view WHERE won IS NOT NULL {wher}");
    let mut card_filter = format!("side = '{side}'");
    if !seen {
        card_filter.push_str(" AND played = 1");
    }
    if let Some(t) = by_turn {
        card_filter.push_str(&format!(" AND first_turn IS NOT NULL AND first_turn <= {t}"));
    }
    let tot: Tot = diesel::sql_query(format!("SELECT COUNT(*) AS n, SUM(won) AS w FROM ({games})")).get_result(conn)?;
    if tot.n == 0 {
        println!("no decided games match the filter");
        return Ok(());
    }
    let sql = format!(
        "WITH g AS ({games}),
              c AS (SELECT DISTINCT game_id, name FROM mtgo_game_cards WHERE {card_filter})
         SELECT c.name AS name, COUNT(*) AS n, SUM(g.won) AS w
         FROM g JOIN c ON c.game_id = g.game_id
         GROUP BY c.name HAVING COUNT(*) >= {min}"
    );
    let mut rows: Vec<CardRow> = match diesel::sql_query(&sql).load(conn) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("query failed: {e}\n{sql}");
            return Ok(());
        }
    };
    let base = tot.w as f64 / tot.n as f64;
    let lift = |r: &CardRow| {
        let with = r.w as f64 / r.n as f64;
        let (nw, ww) = (tot.n - r.n, tot.w - r.w);
        let without = if nw > 0 { ww as f64 / nw as f64 } else { f64::NAN };
        (with, without, with - without)
    };
    match sort {
        "n" => rows.sort_by(|a, b| b.n.cmp(&a.n)),
        "name" => rows.sort_by(|a, b| a.name.cmp(&b.name)),
        "worst" => rows.sort_by(|a, b| lift(a).2.partial_cmp(&lift(b).2).unwrap()),
        _ => rows.sort_by(|a, b| lift(b).2.partial_cmp(&lift(a).2).unwrap()),
    }
    println!(
        "{} decided games in filter, baseline game WR {:.1}%; card side = {side}{}{}",
        tot.n,
        base * 100.0,
        if seen { " (played or seen)" } else { " (played)" },
        by_turn.map(|t| format!(", first played by turn {t}")).unwrap_or_default()
    );
    let mut t = Table::new();
    t.load_preset(UTF8_FULL_CONDENSED);
    t.set_header(["card", "games with", "WR with", "95% CI", "WR without", "lift"]);
    for r in rows.iter().take(limit) {
        let (with, without, d) = lift(r);
        let (lo, hi) = wilson(r.w, r.n);
        t.add_row(vec![
            Cell::new(&r.name),
            Cell::new(r.n).set_alignment(CellAlignment::Right),
            Cell::new(format!("{:.1}%", with * 100.0)).set_alignment(CellAlignment::Right),
            Cell::new(format!("{:.0}–{:.0}", lo * 100.0, hi * 100.0)),
            Cell::new(if without.is_nan() { String::new() } else { format!("{:.1}%", without * 100.0) }).set_alignment(CellAlignment::Right),
            Cell::new(format!("{:+.1}", d * 100.0)).set_alignment(CellAlignment::Right),
        ]);
    }
    println!("{t}");
    if rows.len() > limit {
        println!("({} more cards; raise --limit)", rows.len() - limit);
    }
    Ok(())
}
