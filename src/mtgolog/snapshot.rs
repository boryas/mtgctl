//! Game-state snapshots from the client's session log: on every state change of a game on screen
//! (live or replay) it writes
//! `HH:MM:SS [INF] (Twitch Info|Game Play Status Update for Game ID: G, Match ID: M, Event ID: E) {json}`.
//! Cards are listed only where the viewer can see them: the viewer's own hand, public zones, and an
//! opponent's hand or library while a reveal (Thoughtseize, …) is resolving; libraries and hidden
//! hands appear only as counts.

use super::decklist::{read_session_log, session_end_date, session_logs, SessionClock};
use super::MtgoConfig;
use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Clone, PartialEq, Debug)]
pub struct SnapPlayer {
    #[serde(rename = "Id")]
    pub id: i64,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "LibraryCount")]
    pub library: i32,
    #[serde(rename = "HandCount")]
    pub hand: i32,
    #[serde(rename = "Life")]
    pub life: i32,
}

#[derive(Deserialize, Serialize, Clone, PartialEq, Debug)]
pub struct SnapCard {
    /// Object id; a new one on every zone change, so it names an object, not a card.
    #[serde(rename = "Id")]
    pub id: i64,
    #[serde(rename = "CatalogID")]
    pub catalog_id: i64,
    #[serde(rename = "Zone")]
    pub zone: String,
    /// `SnapPlayer::id`
    #[serde(rename = "Owner")]
    pub owner: i64,
    #[serde(rename = "Controller")]
    pub controller: i64,
}

#[derive(Deserialize)]
struct Payload {
    #[serde(rename = "Players")]
    players: Vec<SnapPlayer>,
    #[serde(rename = "Cards")]
    cards: Vec<SnapCard>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Snapshot {
    pub at: NaiveDateTime,
    pub players: Vec<SnapPlayer>,
    pub cards: Vec<SnapCard>,
}

/// Every distinct state of MTGO game `game_id` (played on `day`) found in the player's session
/// logs, in time order. A game seen by several files (the live log and its archived copy, or a
/// replay watched later) yields each state once: consecutive equal states collapse.
pub fn game_snapshots(cfg: &MtgoConfig, game_id: i64, day: NaiveDate) -> Vec<Snapshot> {
    let needle = format!("Game Play Status Update for Game ID: {game_id},");
    let mut out: Vec<Snapshot> = Vec::new();
    for path in session_logs(cfg) {
        // a session log is named after its last day; a game can only be in one ending on or after it
        let end = session_end_date(&path);
        if end < day || end > day + chrono::Days::new(3) {
            continue;
        }
        let Some(text) = read_session_log(&path) else { continue };
        if !text.contains(&needle) {
            continue;
        }
        let mut clock = SessionClock::new(&path, &text);
        for line in text.lines() {
            let at = clock.stamp(line);
            let (Some(at), true) = (at, line.contains(&needle)) else { continue };
            let Some(json) = line.find(") {").map(|i| &line[i + 2..]) else { continue };
            if let Ok(p) = serde_json::from_str::<Payload>(json) {
                out.push(Snapshot { at, players: p.players, cards: p.cards });
            }
        }
    }
    out.sort_by_key(|s| s.at);
    out.dedup_by(|b, a| a.players == b.players && a.cards == b.cards);
    out
}
