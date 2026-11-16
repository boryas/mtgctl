//! Which format a match was played in. `mtgo_matches.format` is the session log's play format
//! when the match links to one (decklist.rs); `mtgo_matches.legacy` is 1 unless that format is
//! another one, or, without a session log, a card banned in Legacy at the time was played by
//! either side. A Legacy session that played a banned card stays Legacy: that is a timeline
//! error, which `eras::ban_check` reports.

use super::eras::TimelineEvent;
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use std::collections::HashMap;

/// name → first date a card counts against Legacy: the day its timeline ban took effect, or ""
/// for a card banned before the timeline begins or never legal (Scryfall `banned` | `not_legal`).
/// `illegal` is `reference::load_legacy_illegal`. A card the timeline unbans after its last ban is
/// legal today and so absent from `illegal`.
pub fn markers(illegal: &HashMap<String, String>, timeline: &[TimelineEvent]) -> HashMap<String, String> {
    let mut since: HashMap<&str, &str> = HashMap::new();
    for e in timeline {
        for card in &e.banned {
            let d = since.entry(card.as_str()).or_insert(e.effective.as_str());
            if e.effective.as_str() > *d {
                *d = e.effective.as_str();
            }
        }
    }
    illegal.keys().map(|name| (name.clone(), since.get(name.as_str()).copied().unwrap_or("").to_string())).collect()
}

pub struct Stamped {
    /// matches whose format the session log named
    pub session: usize,
    /// matches marked not Legacy
    pub not_legacy: usize,
}

/// Set `mtgo_matches.legacy` for `me`'s matches (see the module doc).
pub fn stamp(conn: &mut SqliteConnection, me: &str, markers: &HashMap<String, String>) -> Result<Stamped, String> {
    let q = |s: &str| format!("'{}'", s.replace('\'', "''"));
    diesel::sql_query("CREATE TEMP TABLE IF NOT EXISTS legacy_markers(card TEXT PRIMARY KEY, since TEXT NOT NULL)").execute(conn).map_err(|e| e.to_string())?;
    diesel::sql_query("DELETE FROM legacy_markers").execute(conn).map_err(|e| e.to_string())?;
    for (card, since) in markers {
        diesel::sql_query(format!("INSERT INTO legacy_markers(card, since) VALUES ({}, {})", q(card), q(since))).execute(conn).map_err(|e| e.to_string())?;
    }
    // one pass over the player's card events, not one per match
    diesel::sql_query("CREATE TEMP TABLE IF NOT EXISTS legacy_marked(match_id INTEGER PRIMARY KEY)").execute(conn).map_err(|e| e.to_string())?;
    diesel::sql_query("DELETE FROM legacy_marked").execute(conn).map_err(|e| e.to_string())?;
    diesel::sql_query(format!(
        "INSERT OR IGNORE INTO legacy_marked(match_id)
         SELECT c.match_id FROM mtgo_matches m JOIN mtgo_card_events c ON c.match_id = m.id JOIN legacy_markers k ON k.card = c.card
         WHERE m.me = {} AND substr(m.start_ts, 1, 10) >= k.since",
        q(me)
    ))
    .execute(conn)
    .map_err(|e| e.to_string())?;
    diesel::sql_query(format!(
        "UPDATE mtgo_matches SET legacy = CASE WHEN format IS NOT NULL THEN format = 'Legacy' ELSE id NOT IN (SELECT match_id FROM legacy_marked) END WHERE me = {}",
        q(me)
    ))
    .execute(conn)
    .map_err(|e| e.to_string())?;
    #[derive(QueryableByName)]
    struct N {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        session: i64,
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        not_legacy: i64,
    }
    let n: N = diesel::sql_query(format!("SELECT COUNT(format) AS session, SUM(legacy = 0) AS not_legacy FROM mtgo_matches WHERE me = {} AND replay = 0", q(me))).get_result(conn).map_err(|e| e.to_string())?;
    Ok(Stamped { session: n.session as usize, not_legacy: n.not_legacy as usize })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timeline_ban_dates_the_marker_and_an_older_ban_has_no_date() {
        let illegal: HashMap<String, String> = [("Black Lotus", "banned"), ("Psychic Frog", "banned"), ("Arzakon", "not_legal")].iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        let timeline = vec![
            TimelineEvent { effective: "2024-12-16".into(), banned: vec!["Psychic Frog".into()], ..Default::default() },
            TimelineEvent { effective: "2025-01-01".into(), banned: vec!["Golgari Grave-Troll".into()], ..Default::default() },
        ];
        let m = markers(&illegal, &timeline);
        assert_eq!(m.get("Black Lotus").map(String::as_str), Some(""));
        assert_eq!(m.get("Psychic Frog").map(String::as_str), Some("2024-12-16"));
        assert_eq!(m.get("Arzakon").map(String::as_str), Some(""));
        assert!(!m.contains_key("Golgari Grave-Troll"), "legal today: no marker");
    }
}
