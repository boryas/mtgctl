//! Era timeline: Legacy B&R changes and set releases (definitions/mtgo/legacy_timeline.toml) →
//! eras, each opened by the changes that took effect on one date; `stamp` writes them to
//! `mtgo_eras` and `mtgo_matches.era_id`.

use super::db::mtgo_eras;
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


/// What `stamp` did: eras in the timeline, the date from which matches were re-placed (None: only
/// matches without an era were), and how many matches were placed.
pub struct Stamped {
    pub eras: usize,
    pub from: Option<String>,
    pub placed: usize,
}

/// Brings `mtgo_eras` and `mtgo_matches.era_id` (every player's) in line with the timeline, touching
/// only what changed: when the stored eras equal the file's, only matches without an era are placed;
/// otherwise the table is rewritten (ids follow start date, so the unchanged leading eras keep
/// theirs) and matches from the first changed era's start on are re-placed. `all` re-places every match.
pub fn stamp(conn: &mut SqliteConnection, path: &Path, all: bool) -> Result<Stamped, String> {
    #[derive(QueryableByName, PartialEq)]
    struct E {
        #[diesel(sql_type = Text)]
        start_date: String,
        #[diesel(sql_type = Text)]
        name: String,
        #[diesel(sql_type = Text)]
        kind: String,
        #[diesel(sql_type = Text)]
        banned: String,
        #[diesel(sql_type = Text)]
        unbanned: String,
    }
    let opening = opening_events(&load_timeline(path)?, false);
    let want: Vec<E> = opening.iter().map(|e| E { start_date: e.effective.clone(), name: era_name(e), kind: e.kind.clone(), banned: e.banned.join(", "), unbanned: e.unbanned.join(", ") }).collect();
    let have: Vec<E> = diesel::sql_query("SELECT start_date, name, kind, banned, unbanned FROM mtgo_eras ORDER BY id").load(conn).map_err(|e| e.to_string())?;
    // the first era that differs; its start (the earlier of old and new) is where era ids can shift
    let diverge = (0..want.len().max(have.len())).find(|&i| want.get(i) != have.get(i));
    let from = if all { Some(String::new()) } else { diverge.map(|i| [want.get(i), have.get(i)].into_iter().flatten().map(|e| e.start_date.clone()).min().unwrap_or_default()) };
    let placed = conn
        .transaction::<_, diesel::result::Error, _>(|conn| {
            if diverge.is_some() {
                super::db::truncate(conn, "mtgo_eras")?;
                for e in &opening {
                    diesel::insert_into(mtgo_eras::table)
                        .values((
                            mtgo_eras::start_date.eq(&e.effective),
                            mtgo_eras::name.eq(era_name(e)),
                            mtgo_eras::kind.eq(&e.kind),
                            mtgo_eras::banned.eq(e.banned.join(", ")),
                            mtgo_eras::unbanned.eq(e.unbanned.join(", ")),
                            mtgo_eras::notes.eq(&e.notes),
                            mtgo_eras::source.eq(&e.source),
                        ))
                        .execute(conn)?;
                }
            }
            // matches before the first era have none, and are left alone rather than re-placed as none every time
            let first = want.first().map(|e| e.start_date.replace('\'', "''")).unwrap_or_default();
            let which = match &from {
                Some(d) => format!("(era_id IS NULL AND substr(start_ts, 1, 10) >= '{first}') OR substr(start_ts, 1, 10) >= '{}'", d.replace('\'', "''")),
                None => format!("era_id IS NULL AND substr(start_ts, 1, 10) >= '{first}'"),
            };
            diesel::sql_query(format!(
                "UPDATE mtgo_matches SET era_id = (
                    SELECT e.id FROM mtgo_eras e WHERE e.start_date <= substr(mtgo_matches.start_ts, 1, 10)
                    ORDER BY e.start_date DESC, e.id DESC LIMIT 1)
                 WHERE {which}"
            ))
            .execute(conn)
        })
        .map_err(|e| e.to_string())?;
    Ok(Stamped { eras: opening.len(), from, placed })
}

/// Sets land every 6–7 weeks and B&R announcements are scheduled, so a newest match more than this
/// many days past the timeline's last event means the file is missing one.
pub const STALE_DAYS: i64 = 45;

/// (days, last event date) when `me`'s newest match is more than STALE_DAYS past the timeline's last event.
pub fn staleness(conn: &mut SqliteConnection, path: &Path, me: &str) -> Result<Option<(i64, String)>, String> {
    let Some(last) = load_timeline(path)?.into_iter().map(|e| e.effective).max() else { return Ok(None) };
    let newest: Option<String> = super::db::mtgo_matches::table
        .filter(super::db::mtgo_matches::me.eq(me))
        .filter(super::db::mtgo_matches::replay.eq(false))
        .select(diesel::dsl::max(super::db::mtgo_matches::start_ts))
        .first(conn)
        .map_err(|e| e.to_string())?;
    let parse = |s: &str| chrono::NaiveDate::parse_from_str(&s[..10.min(s.len())], "%Y-%m-%d").ok();
    Ok(match (newest.as_deref().and_then(parse), parse(&last)) {
        (Some(n), Some(l)) if (n - l).num_days() > STALE_DAYS => Some(((n - l).num_days(), last)),
        _ => None,
    })
}

/// Per banned card: (effective date, card, games it was played in before, games on or after). MTGO
/// applies bans the day they take effect, so any game after means the effective date is wrong.
pub fn ban_check(conn: &mut SqliteConnection, path: &Path) -> Result<Vec<(String, String, i64, i64)>, String> {
    #[derive(QueryableByName)]
    struct N {
        #[diesel(sql_type = BigInt)]
        before: i64,
        #[diesel(sql_type = BigInt)]
        after: i64,
    }
    let mut out = Vec::new();
    for e in load_timeline(path)?.into_iter().filter(|e| !e.banned.is_empty()) {
        for card in &e.banned {
            let n: N = diesel::sql_query(format!(
                "SELECT COUNT(DISTINCT CASE WHEN m.start_ts < '{d}' THEN c.game_id END) AS before, COUNT(DISTINCT CASE WHEN m.start_ts >= '{d}' THEN c.game_id END) AS after
                 FROM mtgo_card_events c JOIN mtgo_matches m ON m.id = c.match_id WHERE c.verb IN ('cast', 'land') AND c.card = '{}'",
                card.replace('\'', "''"),
                d = e.effective
            ))
            .get_result(conn)
            .map_err(|e| e.to_string())?;
            out.push((e.effective.clone(), card.clone(), n.before, n.after));
        }
    }
    Ok(out)
}
