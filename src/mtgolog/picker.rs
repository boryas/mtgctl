//! Interactive front-end for `mtgo stats`: fuzzy-pick filter values and group-by columns from
//! what actually exists in `mtgo_game_view`, then hand a `StatsQuery` to the same SQL engine
//! the flag-driven path uses. Prints the equivalent command line so the flags are learnable.

use super::dataset::{Dataset, MTGO};
use super::stats::{Source, StatsQuery};
use super::MtgoConfig;
use crate::game::fuzzy_multi_select;
use dialoguer::{Input, MultiSelect, Select};
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Nullable, Text};
use diesel::sqlite::SqliteConnection;

/// Pseudo-filter offered alongside the view's columns.
const LAST_N: &str = "last N matches";
const NULL_LABEL: &str = "∅ (none)";
const MORE: &str = "more columns…";
/// Row orders offered by the picker (flag value, label); same vocabulary as `--sort`.
const SORTS: &[(&str, &str)] = &[
    ("games", "size (matches; games in game mode; decks, eras, months by time)"),
    ("wr", "win rate"),
    ("key", "key (the group value)"),
    ("time", "time (first appearance)"),
];

/// Curated first screen; everything else hides behind "more columns…". Overridable via
/// [mtgo] picker_filter_columns / picker_groupby_columns in config.toml.
const DEFAULT_FILTER_COLS: &[&str] = &["my_deck", "my_archetype", "my_list", "my_deck_grade", "my_experimental", "opp_archetype", "opp_deck", "opp_colors", "era", "month", "opponent", "event_kind", "game_number", "on_play", "my_mulligans"];
const DEFAULT_GROUPBY_COLS: &[&str] = &["opp_archetype", "opp_deck", "opp_colors", "my_deck", "my_list", "era", "month", "game_number", "on_play", "my_mulligans", "win_by"];

/// Two-tier multi-select: a short curated list (defaults pre-checked) + "more columns…" which opens
/// a fuzzy finder over the remaining columns. Returns the chosen column names (None = aborted).
fn pick_columns(prompt: &str, all: &[String], primary: &[String], defaults: &[String], extra_last: Option<&str>) -> Result<Option<Vec<String>>, String> {
    let mut items: Vec<String> = primary.iter().filter(|c| all.contains(c)).cloned().collect();
    // defaults that aren't in the curated list still show up front
    for d in defaults {
        if all.contains(d) && !items.contains(d) {
            items.push(d.clone());
        }
    }
    if let Some(x) = extra_last {
        items.push(x.to_string());
    }
    items.push(MORE.to_string());
    let checks: Vec<bool> = items.iter().map(|c| defaults.contains(c)).collect();
    let Some(chosen) = MultiSelect::new().with_prompt(prompt).items(&items).defaults(&checks).interact_opt().map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let mut out: Vec<String> = Vec::new();
    let mut want_more = false;
    for i in chosen {
        if items[i] == MORE {
            want_more = true;
        } else {
            out.push(items[i].clone());
        }
    }
    if want_more {
        let rest: Vec<String> = all.iter().filter(|c| !items.contains(c)).cloned().collect();
        for c in fuzzy_multi_select(&format!("{prompt} — more columns"), &rest) {
            if !out.contains(&c) {
                out.push(c);
            }
        }
    }
    Ok(Some(out))
}

#[derive(QueryableByName)]
struct ColName {
    #[diesel(sql_type = Text)]
    name: String,
}

#[derive(QueryableByName)]
struct ValRow {
    #[diesel(sql_type = Nullable<Text>)]
    val: Option<String>,
    #[diesel(sql_type = BigInt)]
    n: i64,
    #[diesel(sql_type = BigInt)]
    games: i64,
}

/// `conn` holds `ds`'s view (see `Dataset::open`).
pub fn view_columns(conn: &mut SqliteConnection, ds: &Dataset) -> QueryResult<Vec<String>> {
    let cols: Vec<ColName> = diesel::sql_query(format!("SELECT name FROM pragma_table_info('{}')", ds.view)).load(conn)?;
    Ok(cols.into_iter().map(|c| c.name).filter(|c| c != "game_id" && c != "match_id").collect())
}

/// Distinct values of a column with match (and game) counts, most matches first.
pub fn column_values(conn: &mut SqliteConnection, ds: &Dataset, col: &str, where_sql: &[String]) -> QueryResult<Vec<(Option<String>, i64, i64)>> {
    let wher = if where_sql.is_empty() { String::new() } else { format!("WHERE {}", where_sql.join(" AND ")) };
    let sql = format!("SELECT CAST({col} AS TEXT) AS val, COUNT(DISTINCT match_id) AS n, COUNT(*) AS games FROM {} {wher} GROUP BY {col} ORDER BY n DESC, val LIMIT 500", ds.view);
    let rows: Vec<ValRow> = diesel::sql_query(sql).load(conn)?;
    Ok(rows.into_iter().map(|r| (r.val, r.n, r.games)).collect())
}

fn sql_quote(v: &str) -> String {
    format!("'{}'", v.replace('\'', "''"))
}

/// Turn picked values into one WHERE term. Numeric-looking columns still get quoted literals:
/// SQLite applies the column's affinity to the right-hand side of IN, so `on_play IN ('1')` works.
pub fn where_term(col: &str, picked: &[Option<String>]) -> Option<String> {
    if picked.is_empty() {
        return None;
    }
    let has_null = picked.iter().any(|v| v.is_none());
    let vals: Vec<String> = picked.iter().flatten().map(|v| sql_quote(v)).collect();
    let mut parts = Vec::new();
    if !vals.is_empty() {
        parts.push(if vals.len() == 1 { format!("{col} = {}", vals[0]) } else { format!("{col} IN ({})", vals.join(", ")) });
    }
    if has_null {
        parts.push(format!("{col} IS NULL"));
    }
    Some(if parts.len() == 1 { parts.remove(0) } else { format!("({})", parts.join(" OR ")) })
}

/// Parse a picker label back into the value ("Doomsday + Dark Ritual  ⟨1415⟩" → Some("Doomsday + Dark Ritual")).
fn label_value(label: &str) -> Option<String> {
    let v = label.rsplit_once("  ⟨").map(|(v, _)| v).unwrap_or(label);
    if v == NULL_LABEL {
        None
    } else {
        Some(v.to_string())
    }
}

/// `player` narrows the "last N matches" pseudo-filter to that player's matches.
pub fn pick(src: &mut Source, cfg: &MtgoConfig, base: StatsQuery, player: Option<&str>) -> Result<Option<StatsQuery>, String> {
    let cols = src.columns()?;

    // Step 1: which columns to filter on (curated first screen + "more columns…").
    let primary_f: Vec<String> = if cfg.picker_filter_columns.is_empty() { DEFAULT_FILTER_COLS.iter().map(|s| s.to_string()).collect() } else { cfg.picker_filter_columns.clone() };
    let Some(chosen) = pick_columns("Filter on (space to toggle, enter to continue)", &cols, &primary_f, &cfg.stats_filters, Some(LAST_N))? else { return Ok(None) };

    // Step 1b: values for each chosen column (narrowing progressively so later pickers only show
    // values that still exist under the earlier filters).
    let mut terms: Vec<String> = base.where_sql.iter().cloned().collect();
    for col in &chosen {
        if col == LAST_N {
            let n: usize = Input::new().with_prompt("Last N matches").default(50).interact_text().map_err(|e| e.to_string())?;
            let mine = player.map(|p| format!(" AND me = {}", sql_quote(p))).unwrap_or_default();
            terms.push(format!("match_id IN (SELECT id FROM mtgo_matches WHERE replay = 0{mine} ORDER BY start_ts DESC LIMIT {n})"));
            continue;
        }
        let values = src.values(col, &terms)?;
        let game_col = MTGO.is_game_col(col);
        let labels: Vec<String> = values
            .iter()
            .map(|(v, n, g)| {
                let count = if game_col { format!("{n} matches · {g} games") } else { format!("{n} matches") };
                format!("{}  ⟨{count}⟩", v.as_deref().unwrap_or(NULL_LABEL))
            })
            .collect();
        let picked: Vec<Option<String>> = fuzzy_multi_select(&format!("{col} ="), &labels).iter().map(|l| label_value(l)).collect();
        if let Some(t) = where_term(col, &picked) {
            terms.push(t);
        }
    }

    // Step 2: group-bys (same two-tier screen).
    let primary_g: Vec<String> = if cfg.picker_groupby_columns.is_empty() { DEFAULT_GROUPBY_COLS.iter().map(|s| s.to_string()).collect() } else { cfg.picker_groupby_columns.clone() };
    let Some(gb) = pick_columns("Group by (space to toggle, enter to run)", &cols, &primary_g, &cfg.stats_groupbys, None)? else { return Ok(None) };

    // Step 3: row order; an explicit --sort wins over [mtgo] stats_sort.
    let current = if base.sort == "games" { cfg.stats_sort.clone().unwrap_or_else(|| "games".into()) } else { base.sort.clone() };
    let labels: Vec<&str> = SORTS.iter().map(|(_, l)| *l).collect();
    let default = SORTS.iter().position(|(k, _)| *k == current).unwrap_or(0);
    let Some(si) = Select::new().with_prompt("Sort rows by").items(&labels).default(default).interact_opt().map_err(|e| e.to_string())? else { return Ok(None) };

    let mut q = base;
    q.by = gb;
    q.sort = SORTS[si].0.to_string();
    q.where_sql = if terms.is_empty() { None } else { Some(terms.join(" AND ")) };
    if q.min_games <= 1 {
        q.min_games = cfg.stats_min_games.unwrap_or(1).max(1);
    }
    if q.hist.is_empty() {
        q.hist = cfg.stats_hist.clone();
    }
    if q.by.len() >= 1 && q.sort == "games" && q.by.iter().any(|c| matches!(c.as_str(), "month" | "date" | "era_id" | "game_number" | "my_mulligans" | "opp_mulligans" | "turns")) {
        q.sort = "key".into();
    }
    Ok(Some(q))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn where_terms() {
        assert_eq!(where_term("on_play", &[Some("1".into())]), Some("on_play = '1'".into()));
        assert_eq!(where_term("opp_deck", &[Some("A".into()), Some("B'C".into())]), Some("opp_deck IN ('A', 'B''C')".into()));
        assert_eq!(where_term("win_by", &[None, Some("normal".into())]), Some("(win_by = 'normal' OR win_by IS NULL)".into()));
        assert_eq!(where_term("x", &[]), None);
        assert_eq!(label_value("Doomsday + Dark Ritual  ⟨565 matches⟩"), Some("Doomsday + Dark Ritual".into()));
        assert_eq!(label_value("∅ (none)  ⟨4 matches · 9 games⟩"), None);
    }
}
