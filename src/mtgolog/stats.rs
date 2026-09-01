//! Generic slicing over imported MTGO games: group by any column(s) of `mtgo_game_view`,
//! with an optional SQL WHERE clause. One flat view = one uniform set of axes.

use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, CellAlignment, Table};
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Double, Nullable, Text};
use diesel::sqlite::SqliteConnection;
use std::collections::HashMap;

/// One row per game, joined with its match and cluster names. Rebuilt on every run (cheap).
pub const VIEW_SQL: &str = "CREATE VIEW mtgo_game_view AS
SELECT
  g.id                AS game_id,
  m.id                AS match_id,
  substr(m.start_ts,1,10) AS date,
  substr(m.start_ts,1,7)  AS month,
  m.era_id            AS era_id,
  e.name              AS era,
  m.opponent          AS opponent,
  m.result            AS match_result,
  m.my_game_wins      AS my_game_wins,
  m.opp_game_wins     AS opp_game_wins,
  m.games             AS match_games,
  m.event_name        AS event_name,
  m.format            AS format,
  m.my_cluster        AS my_cards_id,
  cm.name             AS my_cards,
  m.opp_cluster       AS opp_cards_id,
  co.name             AS opp_cards,
  m.my_variant        AS my_cards_variant_id,
  vm.name             AS my_cards_variant,
  m.opp_variant       AS opp_cards_variant_id,
  vo.name             AS opp_cards_variant,
  lg.archetype        AS my_guess_archetype,
  lg.subtype          AS my_guess_subtype,
  lg.category         AS my_guess_category,
  lg.archetype_marker AS my_guess_marker,
  lg.subtype_marker   AS my_guess_subtype_marker,
  lo.archetype        AS opp_guess_archetype,
  lo.subtype          AS opp_guess_subtype,
  lo.category         AS opp_guess_category,
  lo.archetype_marker AS opp_guess_marker,
  lo.subtype_marker   AS opp_guess_subtype_marker,
  m.my_list_id        AS my_list_id,
  CASE WHEN m.my_list_id IS NOT NULL THEN 1 ELSE 0 END AS has_decklist,
  COALESCE(li.name, li.auto_name, CASE WHEN li.id IS NOT NULL THEN 'list#' || li.id END) AS my_deck,
  ll.archetype        AS my_list_archetype,
  ll.subtype          AS my_list_subtype,
  ll.category         AS my_list_category,
  g.list_id           AS game_list_id,
  dl.event_kind       AS event_kind,
  g.game_number       AS game_number,
  g.on_play           AS on_play,
  g.turns             AS turns,
  g.duration_secs     AS duration_secs,
  g.duration_secs/60  AS minutes,
  g.winner            AS winner,
  CASE g.winner WHEN 'me' THEN 1 WHEN 'opp' THEN 0 END AS won,
  g.win_by            AS win_by,
  g.my_mulligans      AS my_mulligans,
  g.opp_mulligans     AS opp_mulligans,
  g.my_hand           AS my_hand,
  g.opp_hand          AS opp_hand,
  g.my_lands          AS my_lands,
  g.opp_lands         AS opp_lands,
  g.my_spells         AS my_spells,
  g.opp_spells        AS opp_spells
FROM mtgo_games g
JOIN mtgo_matches m ON m.id = g.match_id
LEFT JOIN mtgo_clusters cm ON cm.id = m.my_cluster
LEFT JOIN mtgo_clusters co ON co.id = m.opp_cluster
LEFT JOIN mtgo_variants vm ON vm.id = m.my_variant
LEFT JOIN mtgo_variants vo ON vo.id = m.opp_variant
LEFT JOIN mtgo_eras e ON e.id = m.era_id
LEFT JOIN mtgo_match_labels lg ON lg.match_id = m.id AND lg.side = 'me' AND lg.source = 'guess'
LEFT JOIN mtgo_match_labels lo ON lo.match_id = m.id AND lo.side = 'opp' AND lo.source = 'guess'
LEFT JOIN mtgo_lists li ON li.id = m.my_list_id
LEFT JOIN mtgo_match_labels ll ON ll.match_id = m.id AND ll.side = 'me' AND ll.source = 'list'
LEFT JOIN mtgo_decklists dl ON dl.game_db_id = g.id
WHERE m.replay = 0";

pub fn ensure_view(conn: &mut SqliteConnection) -> QueryResult<()> {
    diesel::sql_query("DROP VIEW IF EXISTS mtgo_game_view").execute(conn)?;
    diesel::sql_query(VIEW_SQL).execute(conn)?;
    Ok(())
}

#[derive(QueryableByName, Debug)]
struct Row {
    #[diesel(sql_type = Nullable<Text>)]
    key: Option<String>,
    #[diesel(sql_type = BigInt)]
    games: i64,
    #[diesel(sql_type = BigInt)]
    decided: i64,
    #[diesel(sql_type = BigInt)]
    wins: i64,
    #[diesel(sql_type = BigInt)]
    matches: i64,
    #[diesel(sql_type = BigInt)]
    match_wins: i64,
    #[diesel(sql_type = BigInt)]
    match_decided: i64,
    #[diesel(sql_type = Nullable<Double>)]
    avg_turns: Option<f64>,
    #[diesel(sql_type = Nullable<Double>)]
    avg_minutes: Option<f64>,
    #[diesel(sql_type = Nullable<Double>)]
    avg_my_mulls: Option<f64>,
    #[diesel(sql_type = Nullable<Double>)]
    avg_my_mulls_win: Option<f64>,
    #[diesel(sql_type = Nullable<Double>)]
    avg_my_mulls_loss: Option<f64>,
    #[diesel(sql_type = Nullable<Double>)]
    avg_opp_mulls: Option<f64>,
    #[diesel(sql_type = Nullable<Double>)]
    avg_turns_win: Option<f64>,
    #[diesel(sql_type = Nullable<Double>)]
    avg_turns_loss: Option<f64>,
}

#[derive(QueryableByName)]
struct HistRow {
    #[diesel(sql_type = Nullable<Text>)]
    key: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    val: Option<String>,
    #[diesel(sql_type = BigInt)]
    n: i64,
}

/// Everything needed to run one stats query; built by flags or by the interactive picker.
#[derive(Debug, Clone, Default)]
pub struct StatsQuery {
    pub by: Vec<String>,
    pub where_sql: Option<String>,
    pub min_games: i64,
    pub sort: String,
    pub limit: usize,
    pub full: bool,
    pub hist: Vec<String>,
}

impl StatsQuery {
    /// The equivalent command line (so interactive users learn the flags).
    pub fn command_line(&self) -> String {
        let mut s = String::from("mtgctl mtgo stats");
        if !self.by.is_empty() {
            s.push_str(&format!(" --by {}", self.by.join(",")));
        }
        if let Some(w) = &self.where_sql {
            s.push_str(&format!(" --where \"{}\"", w.replace('"', "\\\"")));
        }
        if self.min_games > 1 {
            s.push_str(&format!(" --min {}", self.min_games));
        }
        if self.sort != "games" {
            s.push_str(&format!(" --sort {}", self.sort));
        }
        if self.full {
            s.push_str(" --full");
        }
        for h in &self.hist {
            s.push_str(&format!(" --hist {h}"));
        }
        s
    }
}

/// Natural ordering: digit runs compare numerically ("2" < "10"), everything else lexically.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let chunks = |s: &str| -> Vec<(bool, String)> {
        let mut out: Vec<(bool, String)> = Vec::new();
        for c in s.chars() {
            let d = c.is_ascii_digit();
            match out.last_mut() {
                Some((ld, buf)) if *ld == d => buf.push(c),
                _ => out.push((d, c.to_string())),
            }
        }
        out
    };
    let (ca, cb) = (chunks(a), chunks(b));
    for (x, y) in ca.iter().zip(cb.iter()) {
        let o = if x.0 && y.0 {
            x.1.parse::<u64>().unwrap_or(0).cmp(&y.1.parse::<u64>().unwrap_or(0)).then_with(|| x.1.cmp(&y.1))
        } else {
            x.1.cmp(&y.1)
        };
        if o != Ordering::Equal {
            return o;
        }
    }
    ca.len().cmp(&cb.len())
}

fn ident_ok(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Wilson score interval (95%) for a binomial proportion.
pub fn wilson(wins: i64, n: i64) -> (f64, f64) {
    if n == 0 {
        return (0.0, 0.0);
    }
    let z = 1.96f64;
    let p = wins as f64 / n as f64;
    let nf = n as f64;
    let denom = 1.0 + z * z / nf;
    let centre = p + z * z / (2.0 * nf);
    let half = z * ((p * (1.0 - p) + z * z / (4.0 * nf)) / nf).sqrt();
    ((centre - half) / denom, (centre + half) / denom)
}

pub fn run_stats(conn: &mut SqliteConnection, q: &StatsQuery) -> QueryResult<()> {
    super::db::ensure_schema(conn)?;
    ensure_view(conn)?;
    let by = &q.by;
    let (where_sql, min_games, sort, limit) = (q.where_sql.as_deref(), q.min_games, q.sort.as_str(), q.limit);
    for c in by.iter().chain(q.hist.iter()) {
        if !ident_ok(c) {
            eprintln!("bad column name: {c}");
            return Ok(());
        }
    }
    // Group key as one text column so the result shape is static.
    let key_expr = if by.is_empty() {
        "'all'".to_string()
    } else {
        by.iter().map(|c| format!("COALESCE(CAST({c} AS TEXT),'∅')")).collect::<Vec<_>>().join(" || ' │ ' || ")
    };
    let group = if by.is_empty() { String::new() } else { format!("GROUP BY {}", by.join(", ")) };
    let wher = where_sql.map(|w| format!("WHERE {w}")).unwrap_or_default();
    let select_body = format!(
        "COUNT(*) AS games,
            SUM(won IS NOT NULL) AS decided,
            SUM(COALESCE(won,0)) AS wins,
            COUNT(DISTINCT match_id) AS matches,
            COUNT(DISTINCT CASE WHEN match_result='W' THEN match_id END) AS match_wins,
            COUNT(DISTINCT CASE WHEN match_result IN ('W','L') THEN match_id END) AS match_decided,
            AVG(turns) AS avg_turns,
            AVG(duration_secs)/60.0 AS avg_minutes,
            AVG(my_mulligans) AS avg_my_mulls,
            AVG(CASE WHEN won=1 THEN my_mulligans END) AS avg_my_mulls_win,
            AVG(CASE WHEN won=0 THEN my_mulligans END) AS avg_my_mulls_loss,
            AVG(opp_mulligans) AS avg_opp_mulls,
            AVG(CASE WHEN won=1 THEN turns END) AS avg_turns_win,
            AVG(CASE WHEN won=0 THEN turns END) AS avg_turns_loss
         FROM mtgo_game_view {wher}"
    );
    let sql = format!("SELECT {key_expr} AS key, {select_body} {group}");
    let total_sql = format!("SELECT '(total)' AS key, {select_body}");
    let total: Option<Row> = diesel::sql_query(&total_sql).get_result::<Row>(conn).ok().filter(|r| r.games > 0);
    let rows: Vec<Row> = match diesel::sql_query(&sql).load(conn) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("query failed: {e}\n{sql}");
            return Ok(());
        }
    };
    let total_games: i64 = rows.iter().map(|r| r.games).sum();
    let all_groups = rows.len();
    let all_games = total_games;
    let all_matches: i64 = total.as_ref().map_or(0, |t| t.matches);
    let (kept, mut hidden): (Vec<Row>, Vec<Row>) = rows.into_iter().partition(|r| if is_game_mode(q) || q.full { r.games >= min_games } else { r.matches >= min_games });
    let mut rows = kept;

    // Per-group histograms of arbitrary columns (e.g. --hist win_by, --hist opp_deck).
    let mut hists: Vec<HashMap<String, Vec<(String, i64)>>> = Vec::new();
    for hcol in &q.hist {
        let hsql = format!(
            "SELECT {key_expr} AS key, COALESCE(CAST({hcol} AS TEXT),'∅') AS val, COUNT(*) AS n
             FROM mtgo_game_view {wher} GROUP BY key, val ORDER BY key, n DESC"
        );
        let hrows: Vec<HistRow> = diesel::sql_query(&hsql).load(conn)?;
        let mut m: HashMap<String, Vec<(String, i64)>> = HashMap::new();
        for r in hrows {
            m.entry(r.key.unwrap_or_default()).or_default().push((r.val.unwrap_or_default(), r.n));
        }
        hists.push(m);
    }
    match sort {
        "wr" => rows.sort_by(|a, b| {
            let wa = if a.match_decided > 0 { a.match_wins as f64 / a.match_decided as f64 } else { -1.0 };
            let wb = if b.match_decided > 0 { b.match_wins as f64 / b.match_decided as f64 } else { -1.0 };
            wb.partial_cmp(&wa).unwrap()
        }),
        "key" => rows.sort_by(|a, b| natural_cmp(a.key.as_deref().unwrap_or(""), b.key.as_deref().unwrap_or(""))),
        _ => {
            if is_game_mode(q) || q.full {
                rows.sort_by(|a, b| b.games.cmp(&a.games))
            } else {
                rows.sort_by(|a, b| b.matches.cmp(&a.matches).then(b.games.cmp(&a.games)))
            }
        }
    }
    // Everything beyond --limit joins the hidden pile; it is rolled up into one trailing row.
    if rows.len() > limit {
        hidden.extend(rows.drain(limit..));
    }
    let rollup = if hidden.is_empty() { None } else { Some(rollup_rows(&hidden)) };

    let mut t = Table::new();
    t.load_preset(UTF8_FULL_CONDENSED);
    let mut header = vec![by.join(" │ ")];
    if by.is_empty() {
        header[0] = "all".into();
    }
    let game_mode = is_game_mode(q) || q.full;
    if game_mode {
        header.extend(["matches", "match WR (95% CI)", "games", "game WR (95% CI)", "avg turns", "avg min"].map(String::from));
    } else {
        header.extend(["matches", "match WR (95% CI)", "games W–L", "avg min"].map(String::from));
    }
    if q.full {
        header.extend(["share of games", "avg my mulls (W / L)", "avg opp mulls", "avg turns (W / L)"].map(String::from));
    }
    for h in &q.hist {
        header.push(format!("{h} (top 5)"));
    }
    t.set_header(header);
    let wr_ci = |w: i64, n: i64| -> String {
        if n == 0 {
            return String::new();
        }
        let (lo, hi) = wilson(w, n);
        format!("{:.1}% ({:.0}–{:.0})", 100.0 * w as f64 / n as f64, lo * 100.0, hi * 100.0)
    };
    let f2 = |x: Option<f64>| x.map(|v| format!("{v:.2}")).unwrap_or_default();
    let f1 = |x: Option<f64>| x.map(|v| format!("{v:.1}")).unwrap_or_default();
    for r in rows.iter().chain(rollup.iter()).chain(total.iter()) {
        let mut row = vec![
            Cell::new(r.key.clone().unwrap_or_default()),
            Cell::new(r.matches).set_alignment(CellAlignment::Right),
            Cell::new(wr_ci(r.match_wins, r.match_decided)).set_alignment(CellAlignment::Right),
        ];
        if game_mode {
            row.push(Cell::new(r.games).set_alignment(CellAlignment::Right));
            row.push(Cell::new(wr_ci(r.wins, r.decided)).set_alignment(CellAlignment::Right));
            row.push(Cell::new(f1(r.avg_turns)));
            row.push(Cell::new(r.avg_minutes.map(|x| format!("{x:.0}")).unwrap_or_default()));
        } else {
            row.push(Cell::new(format!("{}–{}", r.wins, r.decided - r.wins)).set_alignment(CellAlignment::Right));
            // minutes per match ≈ avg game minutes × games per match
            let per_match = r.avg_minutes.map(|m| m * r.games as f64 / r.matches.max(1) as f64);
            row.push(Cell::new(per_match.map(|x| format!("{x:.0}")).unwrap_or_default()));
        }
        if q.full {
            row.push(Cell::new(format!("{:.1}%", 100.0 * r.games as f64 / total_games.max(1) as f64)).set_alignment(CellAlignment::Right));
            row.push(Cell::new(format!("{} ({} / {})", f2(r.avg_my_mulls), f2(r.avg_my_mulls_win), f2(r.avg_my_mulls_loss))));
            row.push(Cell::new(f2(r.avg_opp_mulls)));
            row.push(Cell::new(format!("{} ({} / {})", f1(r.avg_turns), f1(r.avg_turns_win), f1(r.avg_turns_loss))));
        }
        for h in &hists {
            let k = r.key.clone().unwrap_or_default();
            let top: Vec<String> = h.get(&k).map(|v| v.iter().take(5).map(|(val, n)| format!("{val} {n}")).collect()).unwrap_or_default();
            row.push(Cell::new(top.join(", ")));
        }
        t.add_row(row);
    }
    println!("{t}");
    let shown_matches: i64 = rows.iter().map(|r| r.matches).sum();
    let shown_games: i64 = rows.iter().map(|r| r.games).sum();
    let covering = if is_game_mode(q) { format!("{shown_games} games") } else { format!("{shown_matches} matches / {shown_games} games") };
    println!(
        "filter: {all_matches} matches / {all_games} games in {all_groups} groups; shown: {} groups covering {covering}{}",
        rows.len(),
        if hidden.is_empty() { String::new() } else { format!("; {} groups rolled up into the last row (--min {min_games}, --limit {limit})", hidden.len()) }
    );
    let uses_signature = by.iter().chain(q.hist.iter()).any(|c| SIGNATURE_COLS.contains(&c.as_str()))
        || where_sql.is_some_and(|w| SIGNATURE_COLS.iter().any(|c| w.contains(c)));
    if uses_signature {
        println!("{SIGNATURE_NOTE}");
    }
    Ok(())
}

/// Sum hidden groups into one "(other)" row; averages are game-weighted.
fn rollup_rows(hidden: &[Row]) -> Row {
    let games: i64 = hidden.iter().map(|r| r.games).sum();
    let wavg = |f: &dyn Fn(&Row) -> Option<f64>, wf: &dyn Fn(&Row) -> i64| -> Option<f64> {
        let (mut num, mut den) = (0.0, 0i64);
        for r in hidden {
            if let Some(v) = f(r) {
                let w = wf(r);
                num += v * w as f64;
                den += w;
            }
        }
        if den > 0 { Some(num / den as f64) } else { None }
    };
    Row {
        key: Some(format!("(other: {} groups)", hidden.len())),
        games,
        decided: hidden.iter().map(|r| r.decided).sum(),
        wins: hidden.iter().map(|r| r.wins).sum(),
        // matches may overlap across groups only for game-level group-bys; summing is the best we have here
        matches: hidden.iter().map(|r| r.matches).sum(),
        match_wins: hidden.iter().map(|r| r.match_wins).sum(),
        match_decided: hidden.iter().map(|r| r.match_decided).sum(),
        avg_turns: wavg(&|r| r.avg_turns, &|r| r.games),
        avg_minutes: wavg(&|r| r.avg_minutes, &|r| r.games),
        avg_my_mulls: wavg(&|r| r.avg_my_mulls, &|r| r.games),
        avg_my_mulls_win: wavg(&|r| r.avg_my_mulls_win, &|r| r.wins),
        avg_my_mulls_loss: wavg(&|r| r.avg_my_mulls_loss, &|r| r.decided - r.wins),
        avg_opp_mulls: wavg(&|r| r.avg_opp_mulls, &|r| r.games),
        avg_turns_win: wavg(&|r| r.avg_turns_win, &|r| r.wins),
        avg_turns_loss: wavg(&|r| r.avg_turns_loss, &|r| r.decided - r.wins),
    }
}

/// Game-level columns of mtgo_game_view. If none is used in --by/--where/--hist the table is
/// presented match-first (match WR, games W–L) — game WR etc. only in game mode or with --full.
pub const GAME_COLS: &[&str] = &[
    "game_id", "game_number", "on_play", "turns", "duration_secs", "minutes", "winner", "won", "win_by",
    "my_mulligans", "opp_mulligans", "my_hand", "opp_hand", "my_lands", "opp_lands", "my_spells", "opp_spells",
];

fn mentions_col(sql: &str, col: &str) -> bool {
    // word-boundary match so "turns" doesn't trigger on "returns"
    let bytes = sql.as_bytes();
    let mut start = 0;
    while let Some(pos) = sql[start..].find(col) {
        let i = start + pos;
        let j = i + col.len();
        let before_ok = i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        let after_ok = j >= bytes.len() || !(bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_');
        if before_ok && after_ok {
            return true;
        }
        start = j;
    }
    false
}

pub fn is_game_mode(q: &StatsQuery) -> bool {
    q.by.iter().chain(q.hist.iter()).any(|c| GAME_COLS.contains(&c.as_str()))
        || q.where_sql.as_deref().is_some_and(|w| GAME_COLS.iter().any(|c| mentions_col(w, c)))
}

/// Columns derived from clustering observed cards (not registered decklists).
pub const SIGNATURE_COLS: &[&str] = &["my_cards", "opp_cards", "my_cards_variant", "opp_cards_variant", "my_cards_id", "opp_cards_id", "my_cards_variant_id", "opp_cards_variant_id"];
pub const SIGNATURE_NOTE: &str = "note: opp_cards = cluster of the cards the opponent actually showed me (what I faced). my_cards = cluster of the cards I drew/played — NOT my decklist (my_cards_variant merges my matches per day+cluster); my_guess_*/opp_guess_* = advisory labels from marker cards (definitions/mtgo/markers.toml). my_deck/my_list_* = exact registered list (has_decklist=1), from the client's 'Deck Used' session logs — available from 2026-08-18 on.";

pub fn print_columns(conn: &mut SqliteConnection) -> QueryResult<()> {
    super::db::ensure_schema(conn)?;
    ensure_view(conn)?;
    #[derive(QueryableByName)]
    struct Col {
        #[diesel(sql_type = Text)]
        name: String,
    }
    let cols: Vec<Col> = diesel::sql_query("SELECT name FROM pragma_table_info('mtgo_game_view')").load(conn)?;
    println!("mtgo_game_view columns (use with --by / --where):");
    println!("  {}", cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", "));
    println!("{SIGNATURE_NOTE}");
    println!("Tables: mtgo_matches, mtgo_games, mtgo_game_cards (game_id, side, name, instances, plays, first_turn, played), mtgo_events (game_id, seq, ts, turn, side, kind, card, target, text), mtgo_clusters, mtgo_eras");
    Ok(())
}
