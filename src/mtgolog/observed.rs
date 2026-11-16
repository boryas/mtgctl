//! What a deck did to me in my games against it, by its own turn: the damage I took, from the
//! client's state snapshots (see snapshot) with my own life payments taken out, and its casts.

use super::{snapshot, MtgoConfig};
use chrono::NaiveDateTime;
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Integer, Nullable, Text};
use diesel::sqlite::SqliteConnection;
use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;

#[derive(Serialize)]
pub struct Turn {
    pub turn: i32,
    /// games that reached this turn of theirs
    pub games: usize,
    /// damage on this turn: 50th, 75th and 90th percentile
    pub dmg: [i32; 3],
    /// damage through this turn: 50th, 75th, 90th percentile and the most
    pub cum: [i32; 4],
}

#[derive(Serialize)]
pub struct Paid {
    pub games: usize,
    pub p50: i32,
    pub p75: i32,
    pub max: i32,
}

#[derive(Serialize)]
pub struct Observed {
    pub since: String,
    pub games: usize,
    /// games whose session log holds state snapshots: the ones the numbers come from
    pub with_states: usize,
    pub won: usize,
    pub lost: usize,
    /// (their turn, games) where their damage killed me on that turn
    pub dead_on: Vec<(i32, usize)>,
    /// my own life paid before Doomsday was cast, over the games it was
    pub paid_before_doomsday: Option<Paid>,
    pub turns: Vec<Turn>,
    /// their casts on each of their first turns, most frequent first: (turn, [(card, games)])
    pub casts: Vec<(i32, Vec<(String, usize)>)>,
}

#[derive(QueryableByName)]
struct GameRow {
    #[diesel(sql_type = Integer)]
    game_id: i32,
    #[diesel(sql_type = Text)]
    start: String,
    #[diesel(sql_type = Nullable<Text>)]
    winner: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    win_by: Option<String>,
    #[diesel(sql_type = Nullable<BigInt>)]
    mtgo_game_id: Option<i64>,
}

#[derive(QueryableByName)]
struct EventRow {
    #[diesel(sql_type = Integer)]
    seq: i32,
    #[diesel(sql_type = Text)]
    ts: String,
    #[diesel(sql_type = Integer)]
    turn: i32,
    #[diesel(sql_type = Nullable<Text>)]
    side: Option<String>,
    #[diesel(sql_type = Text)]
    kind: String,
    #[diesel(sql_type = Text)]
    text: String,
}

static CARD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"@\[([^@]*)@:[0-9]*,?[0-9]*:@\]").unwrap());
static PAYING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"by paying (\d+) life").unwrap());
static FETCH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"Delta|Strand|Mire|Tarn|Foothills|Mesa|Rainforest|Catacombs|Heath|Expanse").unwrap());

const MAX_TURN: i32 = 8;
/// A turn is reported from this many games reaching it.
const MIN_GAMES: usize = 5;
const CAST_TURNS: i32 = 3;

fn first_card(text: &str) -> Option<&str> {
    CARD.captures(text).map(|c| c.get(1).unwrap().as_str())
}

/// The life the event cost me: fetching, cycling Street Wraith, Thoughtseize and the like,
/// "by paying N life", and Doomsday's half of `life` unless a counter follows within `next`.
fn self_paid(e: &EventRow, life: i32, next: &[EventRow]) -> i32 {
    let card = first_card(&e.text).unwrap_or("");
    match e.kind.as_str() {
        "Activates" if e.text.contains("Search your library for") && FETCH.is_match(card) => 1,
        "Cycles" if card == "Street Wraith" => 2,
        "Casts" => {
            if let Some(c) = PAYING.captures(&e.text) {
                return c[1].parse().unwrap_or(0);
            }
            match card {
                "Thoughtseize" | "Gitaxian Probe" => 2,
                "Dismember" => 4,
                "Doomsday" if !next.iter().take(8).any(|x| x.kind == "Counters" && x.text.contains("Doomsday")) => (life + 1) / 2,
                _ => 0,
            }
        }
        _ => 0,
    }
}

fn pct(sorted: &[i32], p: f64) -> i32 {
    sorted[((sorted.len() as f64 * p) as usize).min(sorted.len() - 1)]
}

struct Game {
    winner: Option<String>,
    win_by: Option<String>,
    /// their turn → (damage that round, their casts that turn, whether it was the last turn of the game)
    turns: BTreeMap<i32, (i32, Vec<String>, bool)>,
    paid_before_doomsday: Option<i32>,
}

/// One game read off its event log and snapshots; None without a snapshot.
fn game(me: &str, events: &[EventRow], snaps: &[snapshot::Snapshot]) -> Option<Game> {
    let life: Vec<(NaiveDateTime, i32)> = snaps.iter().filter_map(|s| Some((s.at, s.players.iter().find(|p| p.name == me)?.life))).collect();
    if life.is_empty() {
        return None;
    }
    let at = |ts: &str| NaiveDateTime::parse_from_str(ts, "%Y-%m-%d %H:%M:%S").ok();
    let life_at = |t: NaiveDateTime| life.iter().take_while(|(a, _)| *a <= t).last().map_or(20, |x| x.1);
    let lowest = |a: NaiveDateTime, b: NaiveDateTime| life.iter().filter(|(t, _)| a <= *t && *t <= b).map(|x| x.1).min().unwrap_or(i32::MAX).min(life_at(b));
    let mut paid: Vec<(i32, i32)> = Vec::new();
    let mut before_dd = None;
    for (i, e) in events.iter().enumerate() {
        if e.side.as_deref() != Some("me") {
            continue;
        }
        let Some(t) = at(&e.ts) else { continue };
        let p = self_paid(e, life_at(t), &events[i + 1..]);
        if p > 0 {
            if before_dd.is_none() && e.kind == "Casts" && first_card(&e.text) == Some("Doomsday") {
                before_dd = Some(paid.iter().map(|x| x.1).sum());
            }
            paid.push((e.seq, p));
        }
    }
    let marks: Vec<&EventRow> = events.iter().filter(|e| e.kind == "Turn").collect();
    let theirs: Vec<(usize, &EventRow)> = marks.iter().enumerate().filter(|(_, m)| !m.text.ends_with(&format!(": {me}"))).map(|(i, m)| (i, *m)).collect();
    let end = events.last().and_then(|e| at(&e.ts))?;
    let mut turns = BTreeMap::new();
    for (j, (i, m)) in theirs.iter().enumerate() {
        let Some(start) = at(&m.ts) else { continue };
        let (next_t, next_seq) = theirs.get(j + 1).map_or((end, i32::MAX), |(_, n)| (at(&n.ts).unwrap_or(end), n.seq));
        let loss = life_at(start) - lowest(start, next_t);
        let own: i32 = paid.iter().filter(|(s, _)| m.seq < *s && *s < next_seq).map(|x| x.1).sum();
        let my_next = marks.get(i + 1).map_or(i32::MAX, |n| n.seq);
        let casts = events.iter().filter(|e| m.seq < e.seq && e.seq < my_next && e.side.as_deref() == Some("opp") && e.kind == "Casts").filter_map(|e| first_card(&e.text).map(str::to_string)).collect();
        turns.insert(m.turn, ((loss - own).max(0), casts, j + 1 == theirs.len()));
    }
    Some(Game { winner: None, win_by: None, turns, paid_before_doomsday: before_dd })
}

/// My games against `deck` from `since` (YYYY-MM-DD): see `Observed`.
pub fn observed(conn: &mut SqliteConnection, cfg: &MtgoConfig, me: &str, deck: &str, since: &str) -> Result<Observed, String> {
    let rows: Vec<GameRow> = diesel::sql_query(
        "SELECT v.game_id, v.game_start AS start, v.winner, v.win_by, (SELECT d.mtgo_game_id FROM mtgo_decklists d WHERE d.game_db_id = v.game_id) AS mtgo_game_id
         FROM mtgo_game_view v WHERE v.player = ? AND v.opp_deck = ? AND v.date >= ? ORDER BY v.game_start",
    )
    .bind::<Text, _>(me)
    .bind::<Text, _>(deck)
    .bind::<Text, _>(since)
    .load(conn)
    .map_err(|e| e.to_string())?;
    let days: HashMap<i64, chrono::NaiveDate> = rows.iter().filter_map(|r| Some((r.mtgo_game_id?, chrono::NaiveDate::parse_from_str(r.start.get(..10)?, "%Y-%m-%d").ok()?))).collect();
    let snaps = snapshot::games_snapshots(cfg, &days);
    let mut games = Vec::new();
    for r in &rows {
        let Some(s) = r.mtgo_game_id.and_then(|id| snaps.get(&id)) else { continue };
        let events: Vec<EventRow> = diesel::sql_query("SELECT seq, ts, turn, side, kind, text FROM mtgo_events WHERE game_id = ? ORDER BY seq").bind::<Integer, _>(r.game_id).load(conn).map_err(|e| e.to_string())?;
        if let Some(mut g) = game(me, &events, s) {
            g.winner = r.winner.clone();
            g.win_by = r.win_by.clone();
            games.push(g);
        }
    }
    let mut dead: BTreeMap<i32, usize> = BTreeMap::new();
    for g in &games {
        if g.winner.as_deref() == Some("opp") && g.win_by.as_deref() == Some("normal") {
            if let Some((t, _)) = g.turns.iter().last().filter(|(_, v)| v.2) {
                *dead.entry(*t).or_default() += 1;
            }
        }
    }
    let mut turns = Vec::new();
    for t in 1..=MAX_TURN {
        let reached: Vec<&Game> = games.iter().filter(|g| g.turns.contains_key(&t)).collect();
        if reached.len() < MIN_GAMES {
            break;
        }
        let mut dmg: Vec<i32> = reached.iter().map(|g| g.turns[&t].0).collect();
        let mut cum: Vec<i32> = reached.iter().map(|g| g.turns.range(1..=t).map(|(_, v)| v.0).sum()).collect();
        dmg.sort_unstable();
        cum.sort_unstable();
        turns.push(Turn { turn: t, games: reached.len(), dmg: [pct(&dmg, 0.5), pct(&dmg, 0.75), pct(&dmg, 0.9)], cum: [pct(&cum, 0.5), pct(&cum, 0.75), pct(&cum, 0.9), cum[cum.len() - 1]] });
    }
    let casts = (1..=CAST_TURNS)
        .map(|t| {
            let mut n: HashMap<&str, usize> = HashMap::new();
            for g in &games {
                if let Some((_, cs, _)) = g.turns.get(&t) {
                    let mut seen: Vec<&str> = cs.iter().map(String::as_str).collect();
                    seen.sort_unstable();
                    seen.dedup();
                    for c in seen {
                        *n.entry(c).or_default() += 1;
                    }
                }
            }
            let mut v: Vec<(String, usize)> = n.into_iter().map(|(c, k)| (c.to_string(), k)).collect();
            v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            (t, v)
        })
        .collect();
    let mut paid: Vec<i32> = games.iter().filter_map(|g| g.paid_before_doomsday).collect();
    paid.sort_unstable();
    Ok(Observed {
        since: since.to_string(),
        games: rows.len(),
        with_states: games.len(),
        won: games.iter().filter(|g| g.winner.as_deref() == Some("me")).count(),
        lost: games.iter().filter(|g| g.winner.as_deref() == Some("opp")).count(),
        dead_on: dead.into_iter().collect(),
        paid_before_doomsday: (!paid.is_empty()).then(|| Paid { games: paid.len(), p50: pct(&paid, 0.5), p75: pct(&paid, 0.75), max: paid[paid.len() - 1] }),
        turns,
        casts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: i32, ts: &str, turn: i32, side: Option<&str>, kind: &str, text: &str) -> EventRow {
        EventRow { seq, ts: format!("2026-09-30 {ts}"), turn, side: side.map(str::to_string), kind: kind.into(), text: text.into() }
    }
    fn snap(ts: &str, me: i32) -> snapshot::Snapshot {
        let at = NaiveDateTime::parse_from_str(&format!("2026-09-30 {ts}"), "%Y-%m-%d %H:%M:%S").unwrap();
        let p = |id, name: &str, life| snapshot::SnapPlayer { id, name: name.into(), library: 50, hand: 7, life };
        snapshot::Snapshot { at, players: vec![p(1, "me", me), p(2, "opp", 20)], cards: vec![] }
    }

    #[test]
    fn damage_by_their_turn_leaves_out_what_i_paid() {
        // on the draw: their T1 nothing; my T1 fetch (1); their T2 bolt me for 3 and attack for 2 next
        // round while I Thoughtseize (2); their T3 I die
        let events = vec![
            ev(1, "10:00:00", 1, Some("opp"), "Turn", "Turn 1: opp"),
            ev(2, "10:00:10", 1, Some("me"), "Turn", "Turn 1: me"),
            ev(3, "10:00:20", 1, Some("me"), "Activates", "me activates an ability of @[Polluted Delta@:1,2:@] (Search your library for an Island or Swamp card, put it onto the battlefield, then shuffle.)"),
            ev(4, "10:01:00", 2, Some("opp"), "Turn", "Turn 2: opp"),
            ev(5, "10:01:10", 2, Some("opp"), "Casts", "opp casts @[Lightning Bolt@:3,4:@] targeting me."),
            ev(6, "10:02:00", 2, Some("me"), "Turn", "Turn 2: me"),
            ev(7, "10:02:10", 2, Some("me"), "Casts", "me casts @[Thoughtseize@:5,6:@] targeting opp."),
            ev(8, "10:03:00", 3, Some("opp"), "Turn", "Turn 3: opp"),
            ev(9, "10:03:10", 3, Some("opp"), "Casts", "opp casts @[Fireblast@:7,8:@] targeting me."),
            ev(10, "10:03:20", 3, None, "Wins", "opp wins the game."),
        ];
        let snaps = vec![snap("09:59:00", 20), snap("10:00:25", 19), snap("10:01:15", 16), snap("10:02:15", 14), snap("10:03:15", 0)];
        let g = game("me", &events, &snaps).unwrap();
        let t: Vec<(i32, i32, Vec<String>, bool)> = g.turns.iter().map(|(k, v)| (*k, v.0, v.1.clone(), v.2)).collect();
        assert_eq!(t, vec![(1, 0, vec![], false), (2, 3, vec!["Lightning Bolt".to_string()], false), (3, 14, vec!["Fireblast".to_string()], true)]);
        assert_eq!(g.paid_before_doomsday, None);
    }

    #[test]
    fn doomsday_pays_half_unless_countered() {
        let cast = ev(1, "10:00:00", 1, Some("me"), "Casts", "me casts @[Doomsday@:1,2:@].");
        let counter = ev(2, "10:00:05", 1, Some("opp"), "Counters", "opp counters @[Doomsday@:1,2:@] with @[Force of Will@:3,4:@].");
        assert_eq!(self_paid(&cast, 17, &[]), 9);
        assert_eq!(self_paid(&cast, 17, &[counter]), 0);
        let force = ev(3, "10:00:00", 1, Some("me"), "Casts", "me casts @[Force of Will@:3,4:@] by paying 1 life and exiling a blue card from your hand targeting @[Ponder@:5,6:@].");
        assert_eq!(self_paid(&force, 17, &[]), 1);
    }
}
