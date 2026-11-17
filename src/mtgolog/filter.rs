//! A filter as the stats page and the MCP state it, turned into a SQL condition over a dataset's
//! view. Callers send entries, never SQL: this is the only place the conditions are built.

use super::dataset::{Dataset, Kind};
use serde::Deserialize;
use serde_json::Value;

/// One filter entry. Column filters use `field`, `op`, `not`, `values`; card sentences
/// (`my_play`, `opp_play`) add `verb`, `other`, `when`, `scope`, `n`; decklist sentences
/// (`my_list_has`, `opp_list_has`) add `board`, `n`.
#[derive(Deserialize, Clone, Debug, Default)]
pub struct Entry {
    pub field: String,
    #[serde(default)]
    pub op: Option<String>,
    #[serde(default)]
    pub not: bool,
    #[serde(default)]
    pub values: Vec<Value>,
    #[serde(default)]
    pub verb: Option<String>,
    #[serde(default)]
    pub other: Vec<String>,
    #[serde(default)]
    pub when: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    /// at least n times (copies); the page's number box can hold a fraction
    #[serde(default)]
    pub n: Option<f64>,
    #[serde(default)]
    pub board: Option<String>,
}

/// (verb, takes an `other`, what it needs) for `mtgo_card_events.verb`; see the page's VERBS.
pub const VERBS: &[(&str, bool, Option<&str>)] = &[
    ("cast", true, None),
    ("land", false, None),
    ("cycled", false, None),
    ("activated", true, None),
    ("triggered", true, None),
    ("attacked", false, None),
    ("discarded", false, None),
    ("milled", false, None),
    ("revealed", false, None),
    ("exiled", true, None),
    ("returned", true, None),
    ("sacrificed", false, None),
    ("to_graveyard", false, None),
    ("opening_hand", false, Some("hand")),
    ("mulliganed", false, Some("hand")),
    ("bottomed", false, Some("hand")),
    ("drawn", false, Some("hand")),
    ("in_hand", false, Some("hand")),
    ("boarded_in", false, Some("hand")),
    ("boarded_out", false, Some("hand")),
    ("pile", false, Some("pile")),
];

/// (requirement, SQL) a filter or group implies, see the page's REQUIRE.
const REQUIRE: &[(&str, &str)] = &[
    ("my_list", "my_list_id IS NOT NULL"),
    ("opp_list", "opp_reference_id IS NOT NULL"),
    ("dd_deck", "my_archetype = 'Doomsday'"),
    ("hand", "my_hand_known = 1"),
    ("pile", "my_pile_known = 1"),
];

/// A filter or group on one of these columns means my Doomsday games (REQUIRE `dd_deck`).
const DD_COLS: &[&str] = &["my_dd_resolved", "my_dd_turn", "my_pile_known"];

fn col_needs(ds: &Dataset, col: &str) -> Option<&'static str> {
    (ds.features.cards && DD_COLS.contains(&col)).then_some("dd_deck")
}

pub fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn like(s: &str) -> String {
    let esc: String = s.chars().flat_map(|c| if matches!(c, '\\' | '%' | '_') { vec!['\\', c] } else { vec![c] }).collect();
    format!("{} ESCAPE '\\'", sq(&format!("%{esc}%")))
}

fn text(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

enum Field {
    Col(Kind),
    Play(&'static str),
    Deck(&'static str),
}

fn field_of(ds: &Dataset, name: &str) -> Result<Field, String> {
    if let Some(c) = ds.columns.iter().find(|c| c.name == name) {
        return Ok(Field::Col(c.kind));
    }
    if ds.features.cards {
        match name {
            "my_play" => return Ok(Field::Play("me")),
            "opp_play" => return Ok(Field::Play("opp")),
            "my_list_has" => return Ok(Field::Deck("me")),
            "opp_list_has" => return Ok(Field::Deck("opp")),
            _ => {}
        }
    }
    Err(format!("no filter field {name:?} in dataset {}", ds.key))
}

/// The SQL of one entry, and what it needs (REQUIRE keys); an entry with no values is inert.
fn entry_sql(ds: &Dataset, f: &Entry) -> Result<(Option<String>, Vec<&'static str>), String> {
    let vs: Vec<Option<String>> = f.values.iter().map(text).filter(|v| v.as_ref().map_or(true, |s| !s.trim().is_empty())).collect();
    let neg = |t: String| if f.not { format!("NOT {t}") } else { t };
    let c = f.field.as_str();
    let op = f.op.as_deref().unwrap_or("is");
    let n = f.n.unwrap_or(1.0).max(1.0).ceil() as i64;
    match field_of(ds, c)? {
        Field::Col(kind) => {
            if vs.is_empty() {
                return Ok((None, vec![]));
            }
            let needs: Vec<&'static str> = col_needs(ds, c).into_iter().collect();
            let lit = |v: &str| if matches!(kind, Kind::Num) && v.parse::<f64>().is_ok() { v.to_string() } else { sq(v) };
            let sql = match kind {
                Kind::Date => {
                    // values are [from, to] by position, either one empty
                    let at = |i: usize| f.values.get(i).and_then(text).filter(|d| !d.trim().is_empty());
                    let parts: Vec<String> = [(0, ">="), (1, "<=")].into_iter().filter_map(|(i, o)| at(i).map(|d| format!("date {o} {}", sq(&d)))).collect();
                    if parts.is_empty() {
                        return Ok((None, needs));
                    }
                    parts.join(" AND ")
                }
                Kind::Colors => {
                    let cs = vs[0].clone().unwrap_or_default();
                    match op {
                        "exact" => neg(format!("({c} = {})", sq(&cs))),
                        "none" => neg(format!("({})", cs.chars().map(|x| format!("NOT {c} LIKE '%{x}%'")).collect::<Vec<_>>().join(" AND "))),
                        _ => neg(format!("({})", cs.chars().map(|x| format!("{c} LIKE '%{x}%'")).collect::<Vec<_>>().join(" AND "))),
                    }
                }
                Kind::Text | Kind::Num => match op {
                    "contains" => {
                        let t = format!("({})", vs.iter().flatten().map(|v| format!("{c} LIKE {}", like(v))).collect::<Vec<_>>().join(" OR "));
                        if f.not { format!("({c} IS NULL OR NOT {t})") } else { t }
                    }
                    ">=" | "≥" | "<=" | "≤" | ">" | "<" => {
                        let o = match op { "≥" => ">=", "≤" => "<=", o => o };
                        let Some(Some(v)) = vs.first() else { return Ok((None, needs)) };
                        neg(format!("({c} {o} {})", lit(v)))
                    }
                    "is" => {
                        let nn: Vec<String> = vs.iter().flatten().map(|v| lit(v)).collect();
                        let has_null = vs.iter().any(|v| v.is_none());
                        let in_list = if nn.len() == 1 { format!("{c} = {}", nn[0]) } else { format!("{c} IN ({})", nn.join(", ")) };
                        if f.not {
                            // NOT IN alone drops the NULL rows, so ∅ is kept or excluded explicitly
                            let mut parts = vec![];
                            if !nn.is_empty() {
                                parts.push(format!("({c} NOT IN ({}){})", nn.join(", "), if has_null { String::new() } else { format!(" OR {c} IS NULL") }));
                            }
                            if has_null {
                                parts.push(format!("{c} IS NOT NULL"));
                            }
                            parts.join(" AND ")
                        } else if !nn.is_empty() && has_null {
                            format!("({in_list} OR {c} IS NULL)")
                        } else if !nn.is_empty() {
                            in_list
                        } else {
                            format!("{c} IS NULL")
                        }
                    }
                    o => return Err(format!("unknown op {o:?} for {c}")),
                },
            };
            Ok((Some(sql), needs))
        }
        Field::Play(side) => {
            let cards: Vec<String> = vs.iter().flatten().map(|v| sq(v)).collect();
            if cards.is_empty() {
                return Ok((None, vec![]));
            }
            let verb = f.verb.as_deref().unwrap_or("cast");
            let Some((_, takes_other, needs)) = VERBS.iter().find(|(v, _, _)| *v == verb) else { return Err(format!("unknown verb {verb:?}")) };
            let unit = if f.scope.as_deref() == Some("match") { "match_id" } else { "game_id" };
            let mut conds = vec![format!("c.side = '{side}'"), format!("c.verb = {}", sq(verb)), format!("c.card IN ({})", cards.join(", "))];
            if *takes_other && !f.other.is_empty() {
                conds.push(format!("c.other IN ({})", f.other.iter().map(|o| sq(o)).collect::<Vec<_>>().join(", ")));
            }
            let mut req: Vec<&'static str> = needs.iter().copied().collect();
            match f.when.as_deref() {
                Some("before") => {
                    conds.push("(g.my_dd_seq IS NULL OR c.seq < g.my_dd_seq)".into());
                    req.push("dd_deck");
                }
                Some("after") => {
                    conds.push("c.seq > g.my_dd_seq".into());
                    req.push("dd_deck");
                }
                _ => {}
            }
            let having = if n > 1 { format!(" GROUP BY c.{unit} HAVING COUNT(*) >= {n}") } else { String::new() };
            let sql = format!(
                "{unit} {} (SELECT c.{unit} FROM mtgo_card_events c JOIN mtgo_games g ON g.id = c.game_id WHERE {}{having})",
                if f.not { "NOT IN" } else { "IN" },
                conds.join(" AND ")
            );
            Ok((Some(sql), req))
        }
        Field::Deck(side) => {
            let cards: Vec<&String> = vs.iter().flatten().collect();
            if cards.is_empty() {
                return Ok((None, vec![]));
            }
            let (table, key, req) = if side == "me" { ("mtgo_list_cards", "list_id = my_list_id", "my_list") } else { ("shared.reference_cards", "reference_id = opp_reference_id", "opp_list") };
            let board = match f.board.as_deref() {
                Some("main") => " AND lc.sideboard = 0",
                Some("side") => " AND lc.sideboard = 1",
                _ => "",
            };
            let one = |v: &str| format!("(SELECT COALESCE(SUM(lc.qty), 0) FROM {table} lc WHERE lc.{key} AND lc.name = {}{board}) >= {n}", sq(v));
            let t = format!("({})", cards.iter().map(|v| one(v)).collect::<Vec<_>>().join(" OR "));
            Ok((Some(neg(t)), vec![req]))
        }
    }
}

/// The WHERE (ANDed) of `filters` but the one at `except`, and the yes/no split condition of each
/// card sentence in `groups` (column groups are the caller's `by`). Every filter (`except` too),
/// group and `by` column adds what it needs (REQUIRE) to the WHERE, so both sides of a split hold
/// only games where the sentence can be known.
pub fn query_sql(ds: &Dataset, filters: &[Entry], except: Option<usize>, groups: &[Entry], by: &[String]) -> Result<(Option<String>, Vec<String>), String> {
    let mut parts = vec![];
    let mut needs: Vec<&'static str> = vec![];
    for (i, f) in filters.iter().enumerate() {
        let (sql, req) = entry_sql(ds, f)?;
        if except != Some(i) {
            parts.extend(sql);
        }
        needs.extend(req);
    }
    let mut splits = vec![];
    for g in groups {
        if let Field::Col(_) = field_of(ds, &g.field)? {
            needs.extend(col_needs(ds, &g.field));
        } else {
            let (sql, req) = entry_sql(ds, &Entry { not: false, ..g.clone() })?;
            splits.push(sql.unwrap_or_else(|| "1".into()));
            needs.extend(req);
        }
    }
    needs.extend(by.iter().filter_map(|b| col_needs(ds, b)));
    needs.sort();
    needs.dedup();
    parts.extend(needs.iter().map(|k| REQUIRE.iter().find(|(r, _)| r == k).map(|(_, s)| s.to_string()).unwrap()));
    Ok((if parts.is_empty() { None } else { Some(parts.join(" AND ")) }, splits))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mtgolog::dataset::MTGO;

    fn e(json: &str) -> Entry {
        serde_json::from_str(json).unwrap()
    }

    fn where_sql(filters: &[Entry], by: &[String]) -> Result<Option<String>, String> {
        query_sql(&MTGO, filters, None, &[], by).map(|(w, _)| w)
    }

    #[test]
    fn column_filters() {
        let w = |j: &str| where_sql(&[e(j)], &[]).unwrap().unwrap();
        assert_eq!(w(r#"{"field":"opp_deck","values":["Lands"]}"#), "opp_deck = 'Lands'");
        assert_eq!(w(r#"{"field":"opp_deck","values":["Lands","Tron"]}"#), "opp_deck IN ('Lands', 'Tron')");
        assert_eq!(w(r#"{"field":"opp_archetype","not":true,"values":[null]}"#), "opp_archetype IS NOT NULL");
        assert_eq!(w(r#"{"field":"opp_deck","not":true,"values":["Lands"]}"#), "(opp_deck NOT IN ('Lands') OR opp_deck IS NULL)");
        assert_eq!(w(r#"{"field":"turns","op":">=","values":[5]}"#), "(turns >= 5)");
        assert_eq!(w(r#"{"field":"opp_colors","op":"all","values":["UB"]}"#), "(opp_colors LIKE '%U%' AND opp_colors LIKE '%B%')");
        assert_eq!(w(r#"{"field":"date","values":["2026-09-01",""]}"#), "date >= '2026-09-01'");
        assert_eq!(w(r#"{"field":"date","values":["","2026-10-01"]}"#), "date <= '2026-10-01'");
        assert_eq!(w(r#"{"field":"event_name","op":"contains","values":["Challenge"]}"#), "(event_name LIKE '%Challenge%' ESCAPE '\\')");
    }

    #[test]
    fn card_filters_and_requirements() {
        let w = where_sql(&[e(r#"{"field":"my_play","verb":"cast","values":["Doomsday"],"when":"before","scope":"match"}"#)], &[]).unwrap().unwrap();
        assert!(w.starts_with("match_id IN (SELECT c.match_id FROM mtgo_card_events c"));
        assert!(w.ends_with(" AND my_archetype = 'Doomsday'"));
        let w = where_sql(&[e(r#"{"field":"opp_list_has","values":["Wasteland"],"board":"main","n":4}"#)], &[]).unwrap().unwrap();
        assert_eq!(w, "((SELECT COALESCE(SUM(lc.qty), 0) FROM shared.reference_cards lc WHERE lc.reference_id = opp_reference_id AND lc.name = 'Wasteland' AND lc.sideboard = 0) >= 4) AND opp_reference_id IS NOT NULL");
        let w = where_sql(&[e(r#"{"field":"my_play","verb":"drawn","values":["Force of Will"],"n":2}"#)], &[]).unwrap().unwrap();
        assert!(w.contains("c.verb = 'drawn'") && w.contains("HAVING COUNT(*) >= 2") && w.ends_with(" AND my_hand_known = 1"));
        assert!(where_sql(&[e(r#"{"field":"my_play","verb":"pile","values":["Thassa's Oracle"]}"#)], &[]).unwrap().unwrap().ends_with(" AND my_pile_known = 1"));
        assert!(where_sql(&[e(r#"{"field":"nope","values":["x"]}"#)], &[]).is_err());
        assert_eq!(where_sql(&[], &["my_dd_turn".into()]).unwrap().unwrap(), "my_archetype = 'Doomsday'");
    }

    #[test]
    fn except_and_groups_keep_their_needs() {
        let fs = [e(r#"{"field":"my_dd_turn","op":">=","values":[3]}"#), e(r#"{"field":"opp_deck","values":["Lands"]}"#)];
        assert_eq!(query_sql(&MTGO, &fs, Some(0), &[], &[]).unwrap().0.unwrap(), "opp_deck = 'Lands' AND my_archetype = 'Doomsday'");
        let gs = [e(r#"{"field":"my_deck"}"#), e(r#"{"field":"my_play","verb":"drawn","values":["Force of Will"],"not":true}"#)];
        let (w, splits) = query_sql(&MTGO, &[], None, &gs, &["my_deck".into()]).unwrap();
        assert_eq!(w.unwrap(), "my_hand_known = 1");
        assert_eq!(splits.len(), 1);
        assert!(splits[0].starts_with("game_id IN (SELECT") && !splits[0].contains("my_hand_known"));
    }
}
