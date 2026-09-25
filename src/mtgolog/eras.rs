//! Era timeline: Legacy B&R changes and set releases (definitions/mtgo/legacy_timeline.toml) →
//! eras, each opened by the changes that took effect on one date.

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

