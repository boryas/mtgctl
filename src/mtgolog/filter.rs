//! A filter as the stats page states it, turned into a SQL condition over a dataset's view.
//! The page builds the same SQL in the browser (index.html `filterSql`); this is the server's
//! copy for callers that must not send SQL, such as the MCP tools.

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
    #[serde(default)]
    pub n: Option<i64>,
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
];

/// (requirement, SQL) a filter or group implies, see the page's REQUIRE.
const REQUIRE: &[(&str, &str)] = &[
    ("my_list", "my_list_id IS NOT NULL"),
    ("opp_list", "opp_reference_id IS NOT NULL"),
    ("dd_deck", "my_archetype = 'Doomsday'"),
    ("hand", "my_hand_known = 1"),
    ("pile", "my_pile_known = 1"),
];

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
    let n = f.n.unwrap_or(1).max(1);
    match field_of(ds, c)? {
        Field::Col(kind) => {
            if vs.is_empty() {
                return Ok((None, vec![]));
            }
            let mut needs = vec![];
            if ds.features.cards && ["my_dd_resolved", "my_dd_turn", "my_pile_known"].contains(&c) {
                needs.push("dd_deck");
            }
            let lit = |v: &str| if matches!(kind, Kind::Num) && v.parse::<f64>().is_ok() { v.to_string() } else { sq(v) };
            let sql = match kind {
                Kind::Date => {
                    let from = vs.first().cloned().flatten().map(|d| format!("date >= {}", sq(&d)));
                    let to = vs.get(1).cloned().flatten().map(|d| format!("date <= {}", sq(&d)));
                    let parts: Vec<String> = from.into_iter().chain(to).collect();
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

/// The WHERE of a filter list (ANDed) with what the filters and the grouping columns require.
/// `None` when nothing filters.
pub fn where_sql(ds: &Dataset, filters: &[Entry], by: &[String]) -> Result<Option<String>, String> {
    let mut parts = vec![];
    let mut needs: Vec<&'static str> = vec![];
    for f in filters {
        let (sql, req) = entry_sql(ds, f)?;
        parts.extend(sql);
        needs.extend(req);
    }
    if ds.features.cards {
        for b in by {
            if ["my_dd_resolved", "my_dd_turn", "my_pile_known"].contains(&b.as_str()) {
                needs.push("dd_deck");
            }
        }
    }
    needs.sort();
    needs.dedup();
    for k in needs {
        parts.push(REQUIRE.iter().find(|(r, _)| *r == k).map(|(_, s)| s.to_string()).unwrap());
    }
    Ok(if parts.is_empty() { None } else { Some(parts.join(" AND ")) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mtgolog::dataset::MTGO;

    fn e(json: &str) -> Entry {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn column_filters() {
        let w = |j: &str| where_sql(&MTGO, &[e(j)], &[]).unwrap().unwrap();
        assert_eq!(w(r#"{"field":"opp_deck","values":["Lands"]}"#), "opp_deck = 'Lands'");
        assert_eq!(w(r#"{"field":"opp_deck","values":["Lands","Tron"]}"#), "opp_deck IN ('Lands', 'Tron')");
        assert_eq!(w(r#"{"field":"opp_archetype","not":true,"values":[null]}"#), "opp_archetype IS NOT NULL");
        assert_eq!(w(r#"{"field":"opp_deck","not":true,"values":["Lands"]}"#), "(opp_deck NOT IN ('Lands') OR opp_deck IS NULL)");
        assert_eq!(w(r#"{"field":"turns","op":">=","values":[5]}"#), "(turns >= 5)");
        assert_eq!(w(r#"{"field":"opp_colors","op":"all","values":["UB"]}"#), "(opp_colors LIKE '%U%' AND opp_colors LIKE '%B%')");
        assert_eq!(w(r#"{"field":"date","values":["2026-09-01",""]}"#), "date >= '2026-09-01'");
        assert_eq!(w(r#"{"field":"event_name","op":"contains","values":["Challenge"]}"#), "(event_name LIKE '%Challenge%' ESCAPE '\\')");
    }

    #[test]
    fn card_filters_and_requirements() {
        let w = where_sql(&MTGO, &[e(r#"{"field":"my_play","verb":"cast","values":["Doomsday"],"when":"before","scope":"match"}"#)], &[]).unwrap().unwrap();
        assert!(w.starts_with("match_id IN (SELECT c.match_id FROM mtgo_card_events c"));
        assert!(w.ends_with(" AND my_archetype = 'Doomsday'"));
        let w = where_sql(&MTGO, &[e(r#"{"field":"opp_list_has","values":["Wasteland"],"board":"main","n":4}"#)], &[]).unwrap().unwrap();
        assert_eq!(w, "((SELECT COALESCE(SUM(lc.qty), 0) FROM shared.reference_cards lc WHERE lc.reference_id = opp_reference_id AND lc.name = 'Wasteland' AND lc.sideboard = 0) >= 4) AND opp_reference_id IS NOT NULL");
        assert!(where_sql(&MTGO, &[e(r#"{"field":"nope","values":["x"]}"#)], &[]).is_err());
        assert_eq!(where_sql(&MTGO, &[], &["my_dd_turn".into()]).unwrap().unwrap(), "my_archetype = 'Doomsday'");
    }
}
