//! Generic slicing over imported MTGO games: group by any column(s) of `mtgo_game_view`,
//! with an optional SQL WHERE clause. One flat view = one uniform set of axes.

use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, CellAlignment, Table};
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Double, Nullable, Text};
use diesel::sqlite::SqliteConnection;
use std::collections::HashMap;

/// One row per game, joined with its match and both sides' deck labels.
pub const VIEW_SQL: &str = "CREATE VIEW mtgo_game_view AS
SELECT
  g.id                AS game_id,
  m.id                AS match_id,
  substr(m.start_ts,1,10) AS date,
  m.start_ts          AS match_start,
  substr(m.start_ts,1,7)  AS month,
  strftime('%Y-W%W', substr(m.start_ts,1,10)) AS week,
  m.era_id            AS era_id,
  e.name              AS era,
  m.me                AS player,
  m.opponent          AS opponent,
  m.result            AS match_result,
  m.my_game_wins      AS my_game_wins,
  m.opp_game_wins     AS opp_game_wins,
  m.games             AS match_games,
  m.event_name        AS event_name,
  CASE
    WHEN m.event_name = 'League' THEN 'League'
    WHEN m.event_name LIKE '%Challenge%' THEN 'Challenge'
    WHEN m.event_name LIKE '%Preliminary%' OR m.event_name LIKE '%Prelim%' THEN 'Preliminary'
    WHEN m.event_name LIKE '%Qualifier%' THEN 'Qualifier'
    WHEN m.event_name LIKE '%Showcase%' THEN 'Showcase'
    WHEN m.event_name LIKE '%Last Chance%' THEN 'Last Chance'
    WHEN m.event_name IS NULL THEN NULL
    ELSE 'Other'
  END                 AS event_type,
  m.format            AS format,
  md.colors           AS my_colors,
  od.colors           AS opp_colors,
  md.reference_id     AS my_reference_id,
  od.reference_id     AS opp_reference_id,
  md.seen             AS my_seen,
  COALESCE(m.my_list_id, md.list_id) AS my_list_id,
  CASE WHEN m.my_list_id IS NOT NULL THEN 1 ELSE 0 END AS has_decklist,
  COALESCE(li.name, li.auto_name, CASE WHEN li.id IS NOT NULL THEN 'list#' || li.id END) AS my_list,
  md.deck             AS my_deck,
  md.archetype        AS my_archetype,
  md.source           AS my_deck_source,
  CASE WHEN md.source = 'list' AND md.deck IS NOT NULL THEN 'hard' WHEN md.deck IS NOT NULL OR md.archetype IS NOT NULL THEN 'inferred' ELSE 'none' END AS my_deck_grade,
  CASE WHEN la.deck IS NOT NULL THEN 1 ELSE 0 END AS my_deck_known,
  od.deck             AS opp_deck,
  od.archetype        AS opp_archetype,
  od.candidates       AS opp_deck_candidates,
  od.candidate_posteriors AS opp_candidate_posteriors,
  od.posterior        AS opp_posterior,
  od.surprise         AS opp_surprise,
  od.archetype_surprise AS opp_archetype_surprise,
  od.misfit           AS opp_misfit,
  od.seen             AS opp_seen,
  COALESCE(li.experimental, 0) AS my_experimental,
  g.list_id           AS game_list_id,
  dl.event_kind       AS event_kind,
  g.game_number       AS game_number,
  g.start_ts          AS game_start,
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
LEFT JOIN mtgo_eras e ON e.id = m.era_id
LEFT JOIN match_decks md ON md.match_id = m.id AND md.side = 'me'
LEFT JOIN match_decks od ON od.match_id = m.id AND od.side = 'opp'
LEFT JOIN mtgo_lists li ON li.id = COALESCE(m.my_list_id, md.list_id)
LEFT JOIN list_assignments la ON la.list_id = COALESCE(m.my_list_id, md.list_id)
LEFT JOIN mtgo_decklists dl ON dl.game_db_id = g.id
WHERE m.replay = 0";

/// Recreates the view only when its stored SQL differs from VIEW_SQL, so a reader takes no write lock.
pub fn ensure_view(conn: &mut SqliteConnection) -> QueryResult<()> {
    #[derive(QueryableByName)]
    struct S {
        #[diesel(sql_type = Text)]
        sql: String,
    }
    let cur: Option<S> = diesel::sql_query("SELECT sql FROM sqlite_master WHERE type = 'view' AND name = 'mtgo_game_view'").get_result(conn).optional()?;
    if cur.is_some_and(|c| c.sql == VIEW_SQL) {
        return Ok(());
    }
    diesel::sql_query("DROP VIEW IF EXISTS mtgo_game_view").execute(conn)?;
    diesel::sql_query(VIEW_SQL).execute(conn)?;
    Ok(())
}

#[derive(QueryableByName, Debug, serde::Serialize, serde::Deserialize)]
pub struct Row {
    #[diesel(sql_type = Nullable<Text>)]
    pub key: Option<String>,
    #[diesel(sql_type = BigInt)]
    pub games: i64,
    #[diesel(sql_type = BigInt)]
    pub decided: i64,
    #[diesel(sql_type = BigInt)]
    pub wins: i64,
    #[diesel(sql_type = BigInt)]
    pub matches: i64,
    #[diesel(sql_type = BigInt)]
    pub match_wins: i64,
    #[diesel(sql_type = BigInt)]
    pub match_decided: i64,
    /// first match date in the group — chronological order for decks, eras, months
    #[diesel(sql_type = Nullable<Text>)]
    pub first_date: Option<String>,
    #[diesel(sql_type = Nullable<Double>)]
    pub avg_turns: Option<f64>,
    #[diesel(sql_type = Nullable<Double>)]
    pub avg_minutes: Option<f64>,
    #[diesel(sql_type = Nullable<Double>)]
    pub avg_my_mulls: Option<f64>,
    #[diesel(sql_type = Nullable<Double>)]
    pub avg_my_mulls_win: Option<f64>,
    #[diesel(sql_type = Nullable<Double>)]
    pub avg_my_mulls_loss: Option<f64>,
    #[diesel(sql_type = Nullable<Double>)]
    pub avg_opp_mulls: Option<f64>,
    #[diesel(sql_type = Nullable<Double>)]
    pub avg_turns_win: Option<f64>,
    #[diesel(sql_type = Nullable<Double>)]
    pub avg_turns_loss: Option<f64>,
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
    /// one table per value of this column (a facet), each grouped by `by`
    pub per: Option<String>,
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
        if let Some(p) = &self.per {
            s.push_str(&format!(" --per {p}"));
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

/// Where `mtgo stats` reads from: a database, or the stats site, whose API runs this same `compute`.
pub enum Source {
    Db(SqliteConnection),
    /// the site's base URL, e.g. https://bur.io/doomsday/stats
    Site(String),
}

impl Source {
    /// `data`: a directory holding the pooled database `mtgctl-web --data <dir> rebuild` made; None: the site.
    pub fn open(data: Option<&std::path::Path>, site: String) -> Result<Source, String> {
        Ok(match data {
            Some(d) => Source::Db(super::pipeline::connect(d)?),
            None => Source::Site(site),
        })
    }
    pub fn stats(&mut self, q: &StatsQuery) -> Result<StatsResult, String> {
        match self {
            Source::Db(conn) => compute(conn, q),
            Source::Site(base) => site_get(
                base,
                "api/stats",
                &[("by", q.by.join(",")), ("where", q.where_sql.clone().unwrap_or_default()), ("min", q.min_games.to_string()), ("limit", q.limit.to_string()), ("sort", q.sort.clone()), ("full", q.full.to_string()), ("hist", q.hist.join(","))],
            ),
        }
    }
    pub fn columns(&mut self) -> Result<Vec<String>, String> {
        match self {
            Source::Db(conn) => super::picker::view_columns(conn).map_err(|e| e.to_string()),
            Source::Site(base) => site_get(base, "api/columns", &[]),
        }
    }
    /// Distinct values of `col` under the ANDed `terms`: (value, matches, games), most matches first.
    pub fn values(&mut self, col: &str, terms: &[String]) -> Result<Vec<(Option<String>, i64, i64)>, String> {
        match self {
            Source::Db(conn) => super::picker::column_values(conn, col, terms).map_err(|e| e.to_string()),
            Source::Site(base) => {
                #[derive(serde::Deserialize)]
                struct V {
                    value: Option<String>,
                    matches: i64,
                    games: i64,
                }
                let vs: Vec<V> = site_get(base, "api/values", &[("col", col.to_string()), ("where", terms.join(" AND "))])?;
                Ok(vs.into_iter().map(|v| (v.value, v.matches, v.games)).collect())
            }
        }
    }
}

/// GET `<base>/<path>?params` with curl (as the Scryfall fetch does); a non-200 reply's `error` is the Err.
fn site_get<T: serde::de::DeserializeOwned>(base: &str, path: &str, params: &[(&str, String)]) -> Result<T, String> {
    let mut cmd = std::process::Command::new("curl");
    cmd.args(["-sS", "-G", "--max-time", "60", "-w", "\n%{http_code}"]).arg(format!("{}/{path}", base.trim_end_matches('/')));
    for (k, v) in params {
        cmd.arg("--data-urlencode").arg(format!("{k}={v}"));
    }
    let out = cmd.output().map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("{path}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, code) = text.rsplit_once('\n').unwrap_or((&text, ""));
    if code != "200" {
        let msg = serde_json::from_str::<serde_json::Value>(body).ok().and_then(|v| v["error"].as_str().map(String::from)).unwrap_or_else(|| body.to_string());
        return Err(format!("{path}: {code} {msg}"));
    }
    serde_json::from_str(body).map_err(|e| format!("{path}: {e}"))
}

pub fn run_stats(src: &mut Source, q: &StatsQuery) -> Result<(), String> {
    if let Some(per) = &q.per {
        return run_per(src, q, per);
    }
    print_result(q, src.stats(q)?);
    Ok(())
}

/// `--per col`: the same table once per value of `col`, values in the order they first occurred.
fn run_per(src: &mut Source, q: &StatsQuery, per: &str) -> Result<(), String> {
    if !ident_ok(per) {
        return Err(format!("bad column name: {per}"));
    }
    // the facet values are the groups of `--by per`, keyed as compute keys them (NULL → ∅)
    let facets = src.stats(&StatsQuery { by: vec![per.to_string()], per: None, where_sql: q.where_sql.clone(), min_games: 1, sort: "time".into(), limit: usize::MAX, full: false, hist: vec![] })?;
    for f in facets.rows {
        let v = f.key.unwrap_or_default();
        let cond = if v == "∅" { format!("{per} IS NULL") } else { format!("CAST({per} AS TEXT) = '{}'", v.replace('\'', "''")) };
        let sub = StatsQuery { per: None, where_sql: Some(match &q.where_sql { Some(w) => format!("({w}) AND {cond}"), None => cond }), ..q.clone() };
        println!("\n══ {per} = {v} ({} matches) ══", f.matches);
        print_result(&sub, src.stats(&sub)?);
    }
    Ok(())
}

/// The computed table behind `mtgo stats` (and the web API): groups after --min/--limit,
/// the rolled-up remainder, the grand total, and per-group histograms keyed by group key.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct StatsResult {
    pub by: Vec<String>,
    pub rows: Vec<Row>,
    pub rollup: Option<Row>,
    pub total: Option<Row>,
    /// one map per `q.hist` column: group key → [(value, n)] most common first
    pub hists: Vec<HashMap<String, Vec<(String, i64)>>>,
    pub all_matches: i64,
    pub all_games: i64,
    pub all_groups: usize,
    pub hidden_groups: usize,
    pub game_mode: bool,
}

pub fn compute(conn: &mut SqliteConnection, q: &StatsQuery) -> Result<StatsResult, String> {
    super::db::ensure_schema(conn).map_err(|e| e.to_string())?;
    ensure_view(conn).map_err(|e| e.to_string())?;
    let by = &q.by;
    let (where_sql, min_games, sort, limit) = (q.where_sql.as_deref(), q.min_games, q.sort.as_str(), q.limit);
    for c in by.iter().chain(q.hist.iter()) {
        if !ident_ok(c) {
            return Err(format!("bad column name: {c}"));
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
            MIN(date) AS first_date,
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
    let rows: Vec<Row> = diesel::sql_query(&sql).load(conn).map_err(|e| format!("query failed: {e}\n{sql}"))?;
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
        let hrows: Vec<HistRow> = diesel::sql_query(&hsql).load(conn).map_err(|e| e.to_string())?;
        let mut m: HashMap<String, Vec<(String, i64)>> = HashMap::new();
        for r in hrows {
            m.entry(r.key.unwrap_or_default()).or_default().push((r.val.unwrap_or_default(), r.n));
        }
        hists.push(m);
    }
    // Decks, versions, eras and dates read best in the order they happened; sizes are for the rest.
    const CHRONOLOGICAL: &[&str] = &["my_list", "my_list_id", "game_list_id", "era", "era_id", "month", "date"];
    let sort = if sort == "games" && by.first().map(|c| CHRONOLOGICAL.contains(&c.as_str())).unwrap_or(false) { "time" } else { sort };
    match sort {
        "time" => rows.sort_by(|a, b| a.first_date.cmp(&b.first_date).then_with(|| natural_cmp(a.key.as_deref().unwrap_or(""), b.key.as_deref().unwrap_or("")))),
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
    Ok(StatsResult { by: by.clone(), rows, rollup, total, hists, all_matches, all_games, all_groups, hidden_groups: hidden.len(), game_mode: is_game_mode(q) })
}

fn print_result(q: &StatsQuery, r: StatsResult) {
    let StatsResult { by, rows, rollup, total, hists, all_matches, all_games, all_groups, hidden_groups, .. } = r;
    let (min_games, limit) = (q.min_games, q.limit);
    let total_games = all_games;

    let mut t = Table::new();
    t.load_preset(UTF8_FULL_CONDENSED);
    // the group columns are what a row *is*; a double bar separates them from what was measured
    let mut header: Vec<String> = if by.is_empty() { vec!["all".into()] } else { by.clone() };
    header.push("║".into());
    let game_mode = is_game_mode(q) || q.full;
    if game_mode {
        header.extend(["matches", "W–L", "match WR", "95% CI", "games", "W–L", "game WR", "95% CI", "avg turns", "avg min"].map(String::from));
    } else {
        header.extend(["matches", "W–L", "match WR", "95% CI", "games W–L", "avg min"].map(String::from));
    }
    if q.full {
        header.extend(["share of games", "avg my mulls (W / L)", "avg opp mulls", "avg turns (W / L)"].map(String::from));
    }
    for h in &q.hist {
        header.push(format!("{h} (top 5)"));
    }
    t.set_header(header);
    let right = |s: String| Cell::new(s).set_alignment(CellAlignment::Right);
    let pct = |w: i64, n: i64| -> String { if n == 0 { String::new() } else { format!("{:.1}%", 100.0 * w as f64 / n as f64) } };
    let ci = |w: i64, n: i64| -> String {
        if n == 0 {
            return String::new();
        }
        let (lo, hi) = wilson(w, n);
        format!("{:.0}–{:.0}", lo * 100.0, hi * 100.0)
    };
    let wl = |w: i64, n: i64| format!("{}–{}", w, n - w);
    let f2 = |x: Option<f64>| x.map(|v| format!("{v:.2}")).unwrap_or_default();
    let f1 = |x: Option<f64>| x.map(|v| format!("{v:.1}")).unwrap_or_default();
    for r in rows.iter().chain(rollup.iter()).chain(total.iter()) {
        let key = r.key.clone().unwrap_or_default();
        let mut row: Vec<Cell> = if by.len() > 1 && !key.starts_with('(') {
            let mut parts: Vec<String> = key.split(" │ ").map(String::from).collect();
            parts.resize(by.len(), String::new());
            parts.into_iter().map(Cell::new).collect()
        } else {
            let mut cells = vec![Cell::new(key)];
            cells.resize_with(by.len().max(1), || Cell::new(""));
            cells
        };
        row.push(Cell::new("║"));
        row.extend([
            right(r.matches.to_string()),
            right(wl(r.match_wins, r.match_decided)),
            right(pct(r.match_wins, r.match_decided)),
            right(ci(r.match_wins, r.match_decided)),
        ]);
        if game_mode {
            row.push(right(r.games.to_string()));
            row.push(right(wl(r.wins, r.decided)));
            row.push(right(pct(r.wins, r.decided)));
            row.push(right(ci(r.wins, r.decided)));
            row.push(right(f1(r.avg_turns)));
            row.push(right(r.avg_minutes.map(|x| format!("{x:.0}")).unwrap_or_default()));
        } else {
            row.push(right(wl(r.wins, r.decided)));
            // minutes per match ≈ avg game minutes × games per match
            let per_match = r.avg_minutes.map(|m| m * r.games as f64 / r.matches.max(1) as f64);
            row.push(right(per_match.map(|x| format!("{x:.0}")).unwrap_or_default()));
        }
        if q.full {
            row.push(right(format!("{:.1}%", 100.0 * r.games as f64 / total_games.max(1) as f64)));
            row.push(right(format!("{} ({} / {})", f2(r.avg_my_mulls), f2(r.avg_my_mulls_win), f2(r.avg_my_mulls_loss))));
            row.push(right(f2(r.avg_opp_mulls)));
            row.push(right(format!("{} ({} / {})", f1(r.avg_turns), f1(r.avg_turns_win), f1(r.avg_turns_loss))));
        }
        for h in &hists {
            let k = r.key.clone().unwrap_or_default();
            let top: Vec<String> = h.get(&k).map(|v| v.iter().take(5).map(|(val, n)| format!("{val} {n}")).collect()).unwrap_or_default();
            row.push(Cell::new(top.join(", ")));
        }
        t.add_row(row);
    }
    if let Some(c) = t.column_mut(by.len().max(1)) {
        c.set_padding((0, 0));
    }
    println!("{t}");
    let shown_matches: i64 = rows.iter().map(|r| r.matches).sum();
    let shown_games: i64 = rows.iter().map(|r| r.games).sum();
    let covering = if is_game_mode(q) { format!("{shown_games} games") } else { format!("{shown_matches} matches / {shown_games} games") };
    println!(
        "filter: {all_matches} matches / {all_games} games in {all_groups} groups; shown: {} groups covering {covering}{}",
        rows.len(),
        if hidden_groups == 0 { String::new() } else { format!("; {hidden_groups} groups rolled up into the last row (--min {min_games}, --limit {limit})") }
    );
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
        first_date: hidden.iter().filter_map(|r| r.first_date.clone()).min(),
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

/// Printed with `mtgo columns`.
pub const COLUMNS_NOTE: &str = "note: my_deck/my_archetype and opp_deck/opp_archetype = the reference deck a registered list is nearest to, or the side's revealed cards fit; my_deck_grade = hard (a registered list, from the client's 'Deck Used' session logs, 2026-08-18 on, named by a reference) | inferred (from the cards played) | none; opp_deck_candidates = the decks not ruled out; my_colors/opp_colors = WUBRG letters the revealed lands (or the list's lands) can make; my_list = the exact registered list; my_experimental = 1 for a throwaway brew.";

pub fn print_columns(src: &mut Source) -> Result<(), String> {
    let cols = src.columns()?;
    println!("mtgo_game_view columns (use with --by / --where):");
    println!("  {}", cols.join(", "));
    println!("{COLUMNS_NOTE}");
    println!("Tables: mtgo_matches, mtgo_games, mtgo_game_cards (game_id, side, name, instances, plays, first_turn, played), mtgo_events (game_id, seq, ts, turn, side, kind, card, target, text), mtgo_lists, match_decks");
    Ok(())
}
