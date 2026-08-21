//! Pivot analysis for staged (combo) plans: split each game at the first *resolved* cast of a
//! pivot card (e.g. Doomsday), then ask what happened before it (reach), after it (convert) and
//! in games where it was never cast (the pivot/tempo plan). Populations come from `--where` over
//! mtgo_game_view, so builds/eras can be isolated. Tables are conditional win rates (first order,
//! actions = state signals, not causal card impact); the regressions live in analysis/*.py.

use super::stats::{ensure_view, wilson};
use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, CellAlignment, Table};
use diesel::prelude::*;
use diesel::sql_types::{Integer, Nullable, Text};
use diesel::sqlite::SqliteConnection;
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(QueryableByName, Clone)]
struct GameRow {
    #[diesel(sql_type = Integer)]
    game_id: i32,
    #[diesel(sql_type = Integer)]
    won: i32,
    #[diesel(sql_type = Integer)]
    turns: i32,
}

#[derive(QueryableByName)]
struct EvRow {
    #[diesel(sql_type = Integer)]
    game_id: i32,
    #[diesel(sql_type = Integer)]
    seq: i32,
    #[diesel(sql_type = Integer)]
    turn: i32,
    #[diesel(sql_type = Nullable<Text>)]
    side: Option<String>,
    #[diesel(sql_type = Text)]
    kind: String,
    #[diesel(sql_type = Nullable<Text>)]
    card: Option<String>,
}

struct GameInfo {
    won: bool,
    turns: i32,
    cast: bool,
    resolved: bool,
    res_turn: Option<i32>,
    converted: bool,
    /// feature keys ("me:Card", "opp:act Card") seen before the first cast (whole game if never cast)
    pre: BTreeSet<String>,
    /// feature keys seen after resolution
    post: BTreeSet<String>,
}

fn pct_ci(w: i64, n: i64) -> String {
    if n == 0 {
        return String::new();
    }
    let (lo, hi) = wilson(w, n);
    format!("{:.1}% ({:.0}–{:.0})", 100.0 * w as f64 / n as f64, lo * 100.0, hi * 100.0)
}

pub fn run_pivot(conn: &mut SqliteConnection, card: &str, convert: Option<&str>, where_sql: Option<&str>, min_n: i64, top: usize) -> QueryResult<()> {
    super::db::ensure_schema(conn)?;
    ensure_view(conn)?;
    let wher = where_sql.map(|w| format!("AND ({w})")).unwrap_or_default();
    let games: Vec<GameRow> = diesel::sql_query(format!("SELECT game_id, won, turns FROM mtgo_game_view WHERE won IS NOT NULL {wher}")).load(conn)?;
    if games.is_empty() {
        println!("no decided games match the filter");
        return Ok(());
    }
    let evs: Vec<EvRow> = diesel::sql_query(format!(
        "SELECT game_id, seq, turn, side, kind, card FROM mtgo_events
         WHERE kind IN ('Casts','Activates','Counters') AND card IS NOT NULL
           AND game_id IN (SELECT game_id FROM mtgo_game_view WHERE won IS NOT NULL {wher})
         ORDER BY game_id, seq"
    ))
    .load(conn)?;
    let mut by_game: HashMap<i32, Vec<EvRow>> = HashMap::new();
    for e in evs {
        by_game.entry(e.game_id).or_default().push(e);
    }
    let convert_card = convert.map(str::to_string).or_else(|| if card == "Doomsday" { Some("Thassa's Oracle".to_string()) } else { None });

    let mut infos: Vec<GameInfo> = Vec::with_capacity(games.len());
    for g in &games {
        let ev = by_game.get(&g.game_id).map(Vec::as_slice).unwrap_or(&[]);
        // first resolved cast of the pivot card by me
        let mut cast_seq: Option<i32> = None;
        let mut res: Option<(i32, i32)> = None;
        for (i, e) in ev.iter().enumerate() {
            if e.side.as_deref() == Some("me") && e.kind == "Casts" && e.card.as_deref() == Some(card) {
                cast_seq.get_or_insert(e.seq);
                let countered = ev[i + 1..].iter().take_while(|x| x.seq <= e.seq + 15).any(|x| x.kind == "Counters" && x.card.as_deref() == Some(card));
                if !countered {
                    res = Some((e.seq, e.turn));
                    break;
                }
            }
        }
        let key = |e: &EvRow| -> Option<String> {
            let side = e.side.as_deref()?;
            let c = e.card.as_deref()?;
            Some(match e.kind.as_str() {
                "Casts" => format!("{side}:{c}"),
                "Activates" => format!("{side}:act {c}"),
                _ => return None,
            })
        };
        let mut pre = BTreeSet::new();
        let mut post = BTreeSet::new();
        for e in ev {
            let Some(k) = key(e) else { continue };
            match (cast_seq, res) {
                (None, _) => {
                    pre.insert(k);
                }
                (Some(cs), _) if e.seq < cs => {
                    pre.insert(k);
                }
                (_, Some((rs, _))) if e.seq > rs => {
                    post.insert(k);
                }
                _ => {}
            }
        }
        let converted = convert_card.as_deref().is_some_and(|cc| res.is_some() && post.contains(&format!("me:{cc}")));
        infos.push(GameInfo { won: g.won == 1, turns: g.turns, cast: cast_seq.is_some(), resolved: res.is_some(), res_turn: res.map(|r| r.1), converted, pre, post });
    }

    // ---- populations ----
    let n = infos.len();
    let wr_all = infos.iter().filter(|i| i.won).count() as i64;
    println!("Pivot card: {card}{}; {} decided games in filter, WR {}", convert_card.as_deref().map(|c| format!(" (convert: {c})")).unwrap_or_default(), n, pct_ci(wr_all, n as i64));
    let mut t = Table::new();
    t.load_preset(UTF8_FULL_CONDENSED);
    t.set_header(["population", "games", "share", "WR (95% CI)", "avg turns"]);
    let pop = |label: &str, f: &dyn Fn(&GameInfo) -> bool, t: &mut Table| {
        let sel: Vec<&GameInfo> = infos.iter().filter(|i| f(i)).collect();
        let w = sel.iter().filter(|i| i.won).count() as i64;
        let turns = if sel.is_empty() { 0.0 } else { sel.iter().map(|i| i.turns as f64).sum::<f64>() / sel.len() as f64 };
        t.add_row(vec![
            Cell::new(label),
            Cell::new(sel.len()).set_alignment(CellAlignment::Right),
            Cell::new(format!("{:.0}%", 100.0 * sel.len() as f64 / n.max(1) as f64)).set_alignment(CellAlignment::Right),
            Cell::new(pct_ci(w, sel.len() as i64)).set_alignment(CellAlignment::Right),
            Cell::new(format!("{turns:.1}")),
        ]);
    };
    pop(&format!("never cast {card}"), &|i| !i.cast, &mut t);
    pop("cast but never resolved (countered)", &|i| i.cast && !i.resolved, &mut t);
    pop("resolved", &|i| i.resolved, &mut t);
    if let Some(cc) = &convert_card {
        pop(&format!("  resolved and {cc} cast"), &|i| i.resolved && i.converted, &mut t);
        pop(&format!("  resolved, no {cc}"), &|i| i.resolved && !i.converted, &mut t);
    }
    println!("{t}");

    // resolution turn
    let mut by_turn: BTreeMap<i32, (i64, i64)> = BTreeMap::new();
    for i in infos.iter().filter(|i| i.resolved) {
        let e = by_turn.entry(i.res_turn.unwrap_or(0)).or_default();
        e.0 += 1;
        e.1 += i.won as i64;
    }
    let rt: Vec<String> = by_turn.iter().map(|(t, (c, w))| format!("T{t}: {c} games, {:.0}%", 100.0 * *w as f64 / *c as f64)).collect();
    println!("resolution turn → WR: {}", rt.join(" · "));

    // ---- conditional tables ----
    let cond = |title: &str, sel: &dyn Fn(&GameInfo) -> bool, feats: &dyn Fn(&GameInfo) -> &BTreeSet<String>, outcome: &dyn Fn(&GameInfo) -> bool, outcome_name: &str| {
        let pop: Vec<&GameInfo> = infos.iter().filter(|i| sel(i)).collect();
        if pop.is_empty() {
            return;
        }
        let mut counts: HashMap<&str, (i64, i64)> = HashMap::new();
        for i in &pop {
            for k in feats(i) {
                let e = counts.entry(k.as_str()).or_default();
                e.0 += 1;
                e.1 += outcome(i) as i64;
            }
        }
        let total = pop.len() as i64;
        let total_out = pop.iter().filter(|i| outcome(i)).count() as i64;
        let mut rows: Vec<(String, i64, i64, f64)> = counts
            .into_iter()
            .filter(|(_, (c, _))| *c >= min_n)
            .map(|(k, (c, w))| {
                let with = w as f64 / c as f64;
                let without = if total > c { (total_out - w) as f64 / (total - c) as f64 } else { f64::NAN };
                (k.to_string(), c, w, if without.is_nan() { 0.0 } else { with - without })
            })
            .collect();
        rows.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap());
        let mut t = Table::new();
        t.load_preset(UTF8_FULL_CONDENSED);
        t.set_header(["event (side:card; 'act' = activated)", "games with", &format!("{outcome_name} with (95% CI)"), &format!("{outcome_name} without"), "Δ pts"]);
        let show: Vec<&(String, i64, i64, f64)> = if rows.len() > 2 * top { rows.iter().take(top).chain(rows.iter().rev().take(top).collect::<Vec<_>>().into_iter().rev()).collect() } else { rows.iter().collect() };
        let mut last_pos = true;
        for (k, c, w, d) in show {
            if *d < 0.0 && last_pos && rows.len() > 2 * top {
                t.add_row(vec!["…", "", "", "", ""]);
                last_pos = false;
            }
            let (without_s, delta_s) = if total > *c {
                let without = (total_out - w) as f64 / (total - c) as f64;
                (format!("{:.1}%", without * 100.0), format!("{:+.1}", d * 100.0))
            } else {
                ("(all games)".to_string(), String::new())
            };
            t.add_row(vec![
                Cell::new(k),
                Cell::new(c).set_alignment(CellAlignment::Right),
                Cell::new(pct_ci(*w, *c)).set_alignment(CellAlignment::Right),
                Cell::new(without_s).set_alignment(CellAlignment::Right),
                Cell::new(delta_s).set_alignment(CellAlignment::Right),
            ]);
        }
        println!("\n== {title} ({} games, base {outcome_name} {:.1}%; events with ≥{min_n} games)", pop.len(), 100.0 * total_out as f64 / total as f64);
        println!("{t}");
    };
    cond(&format!("BEFORE {card} is cast → does {card} resolve?"), &|_| true, &|i| &i.pre, &|i| i.resolved, "resolve");
    cond(&format!("BEFORE {card} is cast → win?"), &|_| true, &|i| &i.pre, &|i| i.won, "WR");
    cond(&format!("AFTER {card} resolved → win? (conversion)"), &|i| i.resolved, &|i| &i.post, &|i| i.won, "WR");
    cond(&format!("games where {card} was NEVER cast → win? (pivot plan)"), &|i| !i.cast, &|i| &i.pre, &|i| i.won, "WR");
    println!("\nnote: events are actions taken (state signals, selection applies); 'pre' of never-cast games = the whole game. Regressions with permutation nulls: analysis/dd_pivot.py.");
    Ok(())
}
