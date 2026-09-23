//! Era timeline: Legacy B&R changes and set releases (definitions/legacy_timeline.toml) →
//! `mtgo_eras`, and `mtgo_matches.era_id` = the latest era opened on/before the match date.

use super::db::{mtgo_eras, mtgo_game_cards, mtgo_matches, truncate};
use comfy_table::{presets::UTF8_FULL_CONDENSED, Table};
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Text};
use diesel::sqlite::SqliteConnection;
use serde::Deserialize;
use std::path::Path;

#[derive(Deserialize, Debug, Clone)]
#[serde(default)]
pub struct TimelineEvent {
    pub date: String,
    pub effective: String,
    pub kind: String,
    pub code: String,
    pub name: String,
    pub banned: Vec<String>,
    pub unbanned: Vec<String>,
    pub notes: String,
    pub source: String,
}

impl Default for TimelineEvent {
    fn default() -> Self {
        TimelineEvent { date: String::new(), effective: String::new(), kind: "manual".into(), code: String::new(), name: String::new(), banned: vec![], unbanned: vec![], notes: String::new(), source: String::new() }
    }
}

#[derive(Deserialize, Default)]
struct Timeline {
    #[serde(default)]
    event: Vec<TimelineEvent>,
}

#[derive(Insertable)]
#[diesel(table_name = mtgo_eras)]
struct NewEra {
    start_date: String,
    name: String,
    kind: String,
    banned: String,
    unbanned: String,
    notes: String,
    source: String,
}

pub fn load_timeline(path: &Path) -> Result<Vec<TimelineEvent>, String> {
    let s = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let t: Timeline = toml::from_str(&s).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut ev = t.event;
    for e in ev.iter_mut() {
        if e.effective.is_empty() {
            e.effective = e.date.clone();
        }
    }
    ev.sort_by(|a, b| a.effective.cmp(&b.effective));
    Ok(ev)
}

#[derive(QueryableByName)]
struct EraRow {
    #[diesel(sql_type = BigInt)]
    id: i64,
    #[diesel(sql_type = Text)]
    start_date: String,
    #[diesel(sql_type = Text)]
    name: String,
    #[diesel(sql_type = Text)]
    kind: String,
    #[diesel(sql_type = Text)]
    banned: String,
    #[diesel(sql_type = BigInt)]
    matches: i64,
}

/// A B&R announcement that changed nothing in Legacy does not open an era.
fn effective_event(e: &TimelineEvent) -> bool {
    e.kind != "banlist" || !e.banned.is_empty() || !e.unbanned.is_empty()
}

/// Era name as shown everywhere: set code + name, or the announcement's name.
pub fn era_name(e: &TimelineEvent) -> String {
    if e.code.is_empty() { e.name.clone() } else { format!("{} {}", e.code, e.name) }
}

/// (start date, name) of every era in the timeline file, oldest first.
pub fn eras_from_file(path: &Path) -> Result<Vec<(String, String)>, String> {
    Ok(opening_events(&load_timeline(path)?, false).iter().map(|e| (e.effective.clone(), era_name(e))).collect())
}

/// The era a date falls in: the latest era starting on or before it.
pub fn era_for<'a>(eras: &'a [(String, String)], date: &str) -> Option<&'a (String, String)> {
    eras.iter().rev().find(|(start, _)| start.as_str() <= date)
}

/// One era per effective date: events sharing a date (e.g. a set + its Commander release, or a
/// ban landing on a release day) are merged into a single era.
pub fn opening_events(events: &[TimelineEvent], only_bans: bool) -> Vec<TimelineEvent> {
    let mut opening: Vec<TimelineEvent> = Vec::new();
    for e in events.iter().filter(|e| effective_event(e) && (!only_bans || e.kind != "set")) {
        match opening.last_mut() {
            Some(last) if last.effective == e.effective => {
                let label = |x: &TimelineEvent| if x.code.is_empty() { x.name.clone() } else { x.code.clone() };
                let prev = if last.code.is_empty() { last.name.clone() } else { last.code.clone() };
                // keep the first name, append codes of the merged events
                last.name = if last.name.contains(" + ") || last.code.is_empty() { format!("{} + {}", last.name, label(e)) } else { format!("{} + {}", prev, label(e)) };
                last.code.clear();
                if e.kind == "banlist" { last.kind = "banlist".into(); }
                last.banned.extend(e.banned.iter().cloned());
                last.unbanned.extend(e.unbanned.iter().cloned());
                if !e.notes.is_empty() { if !last.notes.is_empty() { last.notes.push_str(" | "); } last.notes.push_str(&e.notes); }
                if !e.source.is_empty() { if !last.source.is_empty() { last.source.push_str(" ; "); } last.source.push_str(&e.source); }
            }
            _ => opening.push(e.clone()),
        }
    }
    opening
}

/// `--only-bans`: only banlist changes (and manual entries) open eras; set releases are kept in
/// the table as kind=set but do not get an era of their own.
pub fn run_eras(conn: &mut SqliteConnection, path: &Path, only_bans: bool, check: bool) -> QueryResult<()> {
    super::db::ensure_schema(conn)?;
    let events = match load_timeline(path) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("{e}");
            return Ok(());
        }
    };
    let opening = opening_events(&events, only_bans);
    conn.transaction::<_, diesel::result::Error, _>(|conn| {
        truncate(conn, "mtgo_eras")?;

        for e in &opening {
            diesel::insert_into(mtgo_eras::table)
                .values(NewEra {
                    start_date: e.effective.clone(),
                    name: era_name(e),
                    kind: e.kind.clone(),
                    banned: e.banned.join(", "),
                    unbanned: e.unbanned.join(", "),
                    notes: e.notes.clone(),
                    source: e.source.clone(),
                })
                .execute(conn)?;
        }
        // Assign era_id: latest era with start_date <= match date.
        diesel::sql_query(
            "UPDATE mtgo_matches SET era_id = (
                SELECT e.id FROM mtgo_eras e WHERE e.start_date <= substr(mtgo_matches.start_ts,1,10)
                ORDER BY e.start_date DESC, e.id DESC LIMIT 1)",
        )
        .execute(conn)?;
        Ok(())
    })?;

    let rows: Vec<EraRow> = diesel::sql_query(
        "SELECT e.id, e.start_date, e.name, e.kind, e.banned,
                (SELECT COUNT(*) FROM mtgo_matches m WHERE m.era_id = e.id AND m.replay = 0) AS matches
         FROM mtgo_eras e ORDER BY e.start_date, e.id",
    )
    .load(conn)?;
    let mut t = Table::new();
    t.load_preset(UTF8_FULL_CONDENSED);
    t.set_header(["era", "from", "kind", "name", "banned", "matches"]);
    for r in &rows {
        t.add_row(vec![r.id.to_string(), r.start_date.clone(), r.kind.clone(), r.name.clone(), r.banned.clone(), r.matches.to_string()]);
    }
    println!("{t}");
    let unassigned: i64 = mtgo_matches::table.filter(mtgo_matches::era_id.is_null()).filter(mtgo_matches::replay.eq(false)).count().get_result(conn)?;
    if unassigned > 0 {
        println!("{unassigned} matches predate the first era (era_id NULL)");
    }
    // Staleness guard: the timeline is a curated file; warn when play continues well past its last
    // event (sets land every ~6-7 weeks and B&R announcements are scheduled).
    if let Some(last_event) = events.iter().map(|e| e.effective.as_str()).max() {
        let newest: Option<String> = mtgo_matches::table
            .filter(mtgo_matches::replay.eq(false))
            .select(diesel::dsl::max(mtgo_matches::start_ts))
            .first(conn)?;
        if let Some(newest) = newest {
            let (a, b) = (chrono::NaiveDate::parse_from_str(last_event, "%Y-%m-%d"), chrono::NaiveDate::parse_from_str(&newest[..10.min(newest.len())], "%Y-%m-%d"));
            if let (Ok(a), Ok(b)) = (a, b) {
                let days = (b - a).num_days();
                if days > 45 {
                    println!("WARNING: newest match is {days} days past the last timeline event ({last_event}) — definitions/mtgo/legacy_timeline.toml is probably missing a set release or B&R announcement (next scheduled B&R: check magic.wizards.com).");
                }
            }
        }
    }

    if check {
        // Sanity: banned cards should not be seen after their effective date (MTGO applies bans promptly).
        println!("\nBan sanity check (games where a banned card appears on/after its effective date):");
        for e in events.iter().filter(|e| !e.banned.is_empty()) {
            for card in &e.banned {
                let n: i64 = mtgo_game_cards::table
                    .inner_join(mtgo_matches::table.on(mtgo_matches::id.eq(mtgo_game_cards::match_id)))
                    .filter(mtgo_game_cards::name.eq(card))
                    .filter(mtgo_matches::start_ts.ge(format!("{} 00:00:00", e.effective)))
                    .count()
                    .get_result(conn)?;
                let before: i64 = mtgo_game_cards::table
                    .inner_join(mtgo_matches::table.on(mtgo_matches::id.eq(mtgo_game_cards::match_id)))
                    .filter(mtgo_game_cards::name.eq(card))
                    .filter(mtgo_matches::start_ts.lt(format!("{} 00:00:00", e.effective)))
                    .count()
                    .get_result(conn)?;
                println!("  {} {:<32} before: {:5}  after: {:5} {}", e.effective, card, before, n, if n > 0 { "<-- check effective date" } else { "" });
            }
        }
    }
    Ok(())
}
