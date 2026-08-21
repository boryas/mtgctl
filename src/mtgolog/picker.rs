//! Interactive front-end for `mtgo stats`: fuzzy-pick filter values and group-by columns from
//! what actually exists in `mtgo_game_view`, then hand a `StatsQuery` to the same SQL engine
//! the flag-driven path uses. Prints the equivalent command line so the flags are learnable.

use super::stats::{ensure_view, StatsQuery};
use super::MtgoConfig;
use crate::game::fuzzy_multi_select;
use dialoguer::{Input, MultiSelect};
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Nullable, Text};
use diesel::sqlite::SqliteConnection;

/// Pseudo-filter offered alongside the view's columns.
const LAST_N: &str = "last N matches";
const NULL_LABEL: &str = "∅ (none)";

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

pub fn view_columns(conn: &mut SqliteConnection) -> QueryResult<Vec<String>> {
    ensure_view(conn)?;
    let cols: Vec<ColName> = diesel::sql_query("SELECT name FROM pragma_table_info('mtgo_game_view')").load(conn)?;
    Ok(cols.into_iter().map(|c| c.name).filter(|c| c != "game_id" && c != "match_id").collect())
}

/// Distinct values of a column with match (and game) counts, most matches first.
fn column_values(conn: &mut SqliteConnection, col: &str, where_sql: &[String]) -> QueryResult<Vec<(Option<String>, i64, i64)>> {
    let wher = if where_sql.is_empty() { String::new() } else { format!("WHERE {}", where_sql.join(" AND ")) };
    let sql = format!("SELECT CAST({col} AS TEXT) AS val, COUNT(DISTINCT match_id) AS n, COUNT(*) AS games FROM mtgo_game_view {wher} GROUP BY {col} ORDER BY n DESC, val LIMIT 500");
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

pub fn pick(conn: &mut SqliteConnection, cfg: &MtgoConfig, base: StatsQuery) -> Result<Option<StatsQuery>, String> {
    let cols = view_columns(conn).map_err(|e| e.to_string())?;

    // Step 1: which columns to filter on.
    let mut filter_options: Vec<String> = cols.clone();
    filter_options.push(LAST_N.to_string());
    let filter_defaults: Vec<bool> = filter_options.iter().map(|c| cfg.stats_filters.contains(c)).collect();
    let chosen = MultiSelect::new()
        .with_prompt("Filter on (space to toggle, enter to continue)")
        .items(&filter_options)
        .defaults(&filter_defaults)
        .interact_opt()
        .map_err(|e| e.to_string())?;
    let Some(chosen) = chosen else { return Ok(None) };

    // Step 1b: values for each chosen column (narrowing progressively so later pickers only show
    // values that still exist under the earlier filters).
    let mut terms: Vec<String> = base.where_sql.iter().cloned().collect();
    for idx in chosen {
        let col = &filter_options[idx];
        if col == LAST_N {
            let n: usize = Input::new().with_prompt("Last N matches").default(50).interact_text().map_err(|e| e.to_string())?;
            terms.push(format!("match_id IN (SELECT id FROM mtgo_matches WHERE replay = 0 ORDER BY start_ts DESC LIMIT {n})"));
            continue;
        }
        let values = column_values(conn, col, &terms).map_err(|e| e.to_string())?;
        let game_col = super::stats::GAME_COLS.contains(&col.as_str());
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

    // Step 2: group-bys.
    let gb_defaults: Vec<bool> = cols.iter().map(|c| cfg.stats_groupbys.contains(c)).collect();
    let gb = MultiSelect::new()
        .with_prompt("Group by (space to toggle, enter to run)")
        .items(&cols)
        .defaults(&gb_defaults)
        .interact_opt()
        .map_err(|e| e.to_string())?;
    let Some(gb) = gb else { return Ok(None) };

    let mut q = base;
    q.by = gb.into_iter().map(|i| cols[i].clone()).collect();
    q.where_sql = if terms.is_empty() { None } else { Some(terms.join(" AND ")) };
    if q.min_games <= 1 {
        q.min_games = cfg.stats_min_games.unwrap_or(1);
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
