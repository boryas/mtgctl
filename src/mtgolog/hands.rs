//! My card events that only the client's state snapshots show (see snapshot): the hands I was
//! dealt and kept, what I drew, how I sideboarded, and my Doomsday pile. Written into
//! mtgo_card_events beside the game log's own, for the games a session log links (mtgo_decklists).

use super::event::{CardRef, Event};
use super::model::{CardEvent, GameLog, Verb};
use super::snapshot::Snapshot;
use super::MtgoConfig;
use chrono::{NaiveDateTime, Timelike};
use diesel::prelude::*;
use diesel::sql_types::{Bool, Integer, Nullable, Text};
use diesel::sqlite::SqliteConnection;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::Path;

/// Version of what `derive` writes; bump it when that changes and every sync rewrites older games.
const HANDS: i32 = 1;

pub const VERBS: &[Verb] = &[Verb::OpeningHand, Verb::Mulliganed, Verb::Bottomed, Verb::Drawn, Verb::InHand, Verb::Pile, Verb::BoardedIn, Verb::BoardedOut];

/// A card of my registered list ("Deck Used"), the 75 of every game of the match.
pub struct ListCard {
    pub name: String,
    pub catalog_id: i64,
    pub qty: i32,
    pub sideboard: bool,
}

pub struct Derived {
    pub events: Vec<CardEvent>,
    /// the kept hand was found, the mulliganed hands number my mulligans, every logged draw of
    /// mine was matched to a card, and every sideboard card was named: a verb's absence is a fact
    pub hand_known: bool,
    /// my Doomsday pile came out at exactly five cards; see derive
    pub pile_known: bool,
}

/// Zones whose objects are not my deck's cards in play: the sideboard, the hidden library, and
/// the client's bookkeeping objects; my deck is every other card of mine plus the library.
const NOT_DECK: &[&str] = &["Sideboard", "Library", "Nowhere", "Effects", "Command", "LocalTriggers"];

pub fn derive(g: &GameLog, me: &str, snaps: &[Snapshot], names: &HashMap<i64, String>, list: &[ListCard]) -> Derived {
    // snapshot times are whole seconds, the game log's are not: compare the log's truncated
    let sec = |t: NaiveDateTime| t.with_nanosecond(0).unwrap_or(t);
    // a replay watched later writes the game's states again, outside the game's own time
    let snaps: Vec<&Snapshot> = snaps.iter().filter(|s| s.at >= sec(g.start) && s.at <= sec(g.end)).collect();
    let my_id = |s: &Snapshot| s.players.iter().find(|p| p.name == me).map(|p| p.id);
    let hand = |s: &Snapshot| -> BTreeMap<i64, i64> {
        let id = my_id(s);
        s.cards.iter().filter(|c| c.zone == "Hand" && Some(c.owner) == id).map(|c| (c.id, c.catalog_id)).collect()
    };
    let turns: Vec<u32> = g
        .events
        .iter()
        .scan(0, |t, e| {
            if let Event::Turn { number, .. } = e.event {
                *t = number;
            }
            Some(*t)
        })
        .collect();
    let seq_at = |t: NaiveDateTime| g.events.iter().rposition(|e| sec(e.ts) <= t).unwrap_or(0);
    let mut out: Vec<CardEvent> = Vec::new();
    let mut note = |seq: usize, card: CardRef, verb: Verb| out.push(CardEvent { seq: seq as u32, turn: turns.get(seq).copied().unwrap_or(0), owner: me.to_string(), actor: me.to_string(), card, verb, other: None });
    let named = |cat: i64, id: i64| names.get(&cat).map(|n| CardRef { name: n.clone(), catalog_id: cat as u32, instance_id: id as u32 });
    let mut known = true;

    // Before turn 1: every deal (London: seven cards), then the kept hand once the bottomed cards
    // are gone; pre-game Leylines leave the hand after that.
    let t1 = g.events.iter().find(|e| matches!(e.event, Event::Turn { .. })).map(|e| sec(e.ts));
    let pre: Vec<&&Snapshot> = snaps.iter().filter(|s| t1.map_or(true, |t| s.at <= t)).collect();
    let mut hands: Vec<BTreeMap<i64, i64>> = Vec::new();
    for s in &pre {
        let h = hand(s);
        if !h.is_empty() && hands.last() != Some(&h) {
            hands.push(h);
        }
    }
    let mulls: Vec<usize> = g.events.iter().enumerate().filter(|(_, e)| matches!(&e.event, Event::Mulligan { player, .. } if player == me)).map(|(i, _)| i).collect();
    let begins = g.events.iter().position(|e| matches!(&e.event, Event::BeginsGame { player, .. } if player == me)).unwrap_or(0);
    let kept_size = g.per_player.get(me).and_then(|p| p.hand_size).map_or(7 - mulls.len(), |h| h as usize);
    let last7 = hands.iter().rposition(|h| h.len() == 7);
    let kept = last7.and_then(|i| hands[i..].iter().find(|h| h.len() == kept_size));
    match (last7, kept) {
        (Some(last7), Some(kept)) => {
            let shipped: Vec<&BTreeMap<i64, i64>> = hands[..last7].iter().filter(|h| h.len() == 7).collect();
            known &= shipped.len() == mulls.len();
            for (k, h) in shipped.iter().enumerate() {
                for (&id, &cat) in *h {
                    match named(cat, id) {
                        Some(c) => note(mulls.get(k).copied().unwrap_or(begins), c, Verb::Mulliganed),
                        None => known = false,
                    }
                }
            }
            for (&id, &cat) in &hands[last7] {
                if !kept.contains_key(&id) {
                    match named(cat, id) {
                        Some(c) => note(begins, c, Verb::Bottomed),
                        None => known = false,
                    }
                }
            }
            for (&id, &cat) in kept {
                match named(cat, id) {
                    Some(c) => {
                        note(begins, c.clone(), Verb::OpeningHand);
                        note(begins, c, Verb::InHand);
                    }
                    None => known = false,
                }
            }
        }
        _ => known = false,
    }

    // During the game: a card is drawn when it arrives in my hand in a step that my logged draws,
    // not yet matched, account for all of; it takes that draw line's seq. A step that also brings
    // another card (a return, a Flow State dig) matches none of its arrivals to a draw, so the
    // game's draws are not known.
    let draws: Vec<(NaiveDateTime, usize, u32)> =
        g.events.iter().enumerate().filter_map(|(i, e)| if let Event::Draws { player, count, .. } = &e.event { (player == me).then_some((sec(e.ts), i, *count)) } else { None }).collect();
    let mut di = 0;
    let mut pending: VecDeque<usize> = VecDeque::new();
    let mut prev = pre.last().map(|s| hand(s)).unwrap_or_default();
    for s in snaps.iter().filter(|s| t1.is_some_and(|t| s.at > t)) {
        while di < draws.len() && draws[di].0 <= s.at {
            pending.extend(std::iter::repeat(draws[di].1).take(draws[di].2 as usize));
            di += 1;
        }
        let h = hand(s);
        let arrivals: Vec<(i64, i64)> = h.iter().filter(|(id, _)| !prev.contains_key(id)).map(|(&id, &cat)| (id, cat)).collect();
        let k = arrivals.len().min(pending.len());
        let all_drawn = k == arrivals.len();
        known &= all_drawn || k == 0;
        let at = seq_at(s.at);
        for (id, cat) in arrivals {
            let drawn = if all_drawn { pending.pop_front() } else { None };
            match named(cat, id) {
                Some(c) => {
                    if let Some(seq) = drawn {
                        note(seq, c.clone(), Verb::Drawn);
                    }
                    note(drawn.unwrap_or(at), c, Verb::InHand);
                }
                None => known = false,
            }
        }
        if !all_drawn {
            pending.drain(..k);
        }
        prev = h;
    }
    known &= pending.is_empty() && di == draws.len();

    let count = |it: &mut dyn Iterator<Item = (String, i32)>| -> BTreeMap<String, i32> {
        let mut m = BTreeMap::new();
        for (n, q) in it {
            *m.entry(n).or_insert(0) += q;
        }
        m
    };
    let catalog = |n: &str| list.iter().find(|c| c.name == n).map_or(0, |c| c.catalog_id);
    let registered_side = count(&mut list.iter().filter(|c| c.sideboard).map(|c| (c.name.clone(), c.qty)));
    let everything = count(&mut list.iter().map(|c| (c.name.clone(), c.qty)));
    let mut side_now: Option<BTreeMap<String, i32>> = None;
    if let Some(s) = snaps.iter().find(|s| s.cards.iter().any(|c| c.zone == "Sideboard" && Some(c.owner) == my_id(s))) {
        let id = my_id(s);
        let sb: Vec<Option<String>> = s.cards.iter().filter(|c| c.zone == "Sideboard" && Some(c.owner) == id).map(|c| names.get(&c.catalog_id).cloned()).collect();
        if sb.iter().all(Option::is_some) {
            let now = count(&mut sb.into_iter().flatten().map(|n| (n, 1)));
            for (n, q) in minus(&registered_side, &now) {
                for _ in 0..q {
                    note(0, CardRef { name: n.clone(), catalog_id: catalog(&n) as u32, instance_id: 0 }, Verb::BoardedIn);
                }
            }
            for (n, q) in minus(&now, &registered_side) {
                for _ in 0..q {
                    note(0, CardRef { name: n.clone(), catalog_id: catalog(&n) as u32, instance_id: 0 }, Verb::BoardedOut);
                }
            }
            side_now = Some(now);
        } else {
            known = false;
        }
    } else if registered_side.is_empty() {
        side_now = Some(BTreeMap::new());
    } else {
        known = false;
    }

    // The pile: Doomsday exiles the rest of my library and puts the pile on it one card at a time,
    // so the first state of the game proper at five cards after the library first falls to five
    // or fewer shows it (the log's clock can trail the session log's by a second, so time does
    // not pick the state): my deck (the registered 75 less the sideboard) minus my visible cards.
    // Kept only when that is exactly five cards; unnamed objects (tokens) are left out, which the
    // count would expose if one were a card of the deck.
    let mut pile_known = false;
    if let (Some(&dd), Some(side_now)) = (g.doomsday_resolved.get(me), &side_now) {
        let dd = dd as usize;
        let library = |s: &Snapshot| s.players.iter().find(|p| p.name == me).map_or(-1, |p| p.library);
        let game: Vec<&&Snapshot> = snaps.iter().filter(|s| t1.is_some_and(|t| s.at > t)).collect();
        let built = game.iter().position(|s| (0..=5).contains(&library(s))).and_then(|i| game[i..].iter().find(|s| library(s) == 5));
        if let Some(s) = built {
            let id = my_id(s);
            let vis = count(&mut s.cards.iter().filter(|c| Some(c.owner) == id && !NOT_DECK.contains(&c.zone.as_str())).filter_map(|c| names.get(&c.catalog_id)).map(|n| (n.clone(), 1)));
            let deck = minus(&everything, side_now);
            let pile = minus(&deck, &vis);
            if minus(&vis, &deck).is_empty() && pile.values().sum::<i32>() == 5 {
                pile_known = true;
                for (n, q) in pile {
                    for _ in 0..q {
                        note(dd, CardRef { name: n.clone(), catalog_id: catalog(&n) as u32, instance_id: 0 }, Verb::Pile);
                    }
                }
            }
        }
    }
    Derived { events: out, hand_known: known, pile_known }
}

/// Multiset difference a − b.
fn minus(a: &BTreeMap<String, i32>, b: &BTreeMap<String, i32>) -> BTreeMap<String, i32> {
    a.iter().filter_map(|(n, &q)| {
        let d = q - b.get(n).copied().unwrap_or(0);
        (d > 0).then(|| (n.clone(), d))
    })
    .collect()
}

#[derive(Default)]
pub struct HandStats {
    pub games: usize,
    pub hand_known: usize,
    pub pile_known: usize,
    pub no_snapshots: usize,
}

#[derive(QueryableByName)]
struct Todo {
    #[diesel(sql_type = Integer)]
    id: i32,
    #[diesel(sql_type = Integer)]
    match_id: i32,
    #[diesel(sql_type = Integer)]
    game_number: i32,
    #[diesel(sql_type = Text)]
    path: String,
    #[diesel(sql_type = Text)]
    start_ts: String,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    mtgo_game_id: i64,
    #[diesel(sql_type = Nullable<Integer>)]
    list_id: Option<i32>,
}

#[derive(QueryableByName)]
struct ListRow {
    #[diesel(sql_type = Text)]
    name: String,
    #[diesel(sql_type = Integer)]
    catalog_id: i32,
    #[diesel(sql_type = Integer)]
    qty: i32,
    #[diesel(sql_type = Bool)]
    sideboard: bool,
}

/// Derives the snapshot facts of `me`'s linked games not yet at HANDS, from the game logs under
/// `logs` and the session logs `cfg` names.
pub fn derive_all(conn: &mut SqliteConnection, cfg: &MtgoConfig, logs: &Path, me: &str) -> Result<HandStats, String> {
    let e = |e: diesel::result::Error| e.to_string();
    let todo: Vec<Todo> = diesel::sql_query(format!(
        "SELECT g.id, g.match_id, g.game_number, m.path, g.start_ts, d.mtgo_game_id, d.list_id FROM mtgo_games g JOIN mtgo_matches m ON m.id = g.match_id JOIN mtgo_decklists d ON d.game_db_id = g.id
         WHERE m.me = '{}' AND (g.hands IS NULL OR g.hands < {HANDS})",
        me.replace('\'', "''")
    ))
    .load(conn)
    .map_err(e)?;
    let mut st = HandStats::default();
    if todo.is_empty() {
        return Ok(st);
    }
    let day = |ts: &str| chrono::NaiveDate::parse_from_str(&ts[..10], "%Y-%m-%d").ok();
    let snaps = super::snapshot::games_snapshots(cfg, &todo.iter().filter_map(|t| Some((t.mtgo_game_id, day(&t.start_ts)?))).collect());
    let ids: BTreeSet<i64> = snaps
        .values()
        .flatten()
        .flat_map(|s| {
            let id = s.players.iter().find(|p| p.name == me).map(|p| p.id);
            s.cards.iter().filter(move |c| Some(c.owner) == id).map(|c| c.catalog_id)
        })
        .collect();
    let names = super::decklist::resolve_names(conn, cfg, &ids, true).map_err(e)?;
    // the stored path is where the log was when first imported; the file keeps its name
    let by_name: HashMap<String, std::path::PathBuf> = super::scan::find_game_logs(logs).into_iter().filter_map(|p| Some((p.file_name()?.to_string_lossy().to_string(), p))).collect();
    let mut matches: HashMap<String, Option<super::model::MatchLog>> = HashMap::new();
    let verbs = VERBS.iter().map(|v| format!("'{}'", v.name())).collect::<Vec<_>>().join(", ");
    conn.transaction::<_, diesel::result::Error, _>(|conn| {
        for t in &todo {
            let file = Path::new(&t.path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
            let m = matches.entry(file.clone()).or_insert_with(|| by_name.get(&file).and_then(|p| super::format::read_log(p).ok()).map(|raw| super::model::build_match(&raw)));
            let game = m.as_ref().and_then(|m| m.games.iter().find(|g| g.number as i32 == t.game_number));
            let list: Vec<ListCard> = match t.list_id {
                Some(l) => diesel::sql_query(format!("SELECT name, catalog_id, qty, sideboard FROM mtgo_list_cards WHERE list_id = {l}"))
                    .load::<ListRow>(conn)
                    ?
                    .into_iter()
                    .map(|r| ListCard { name: r.name, catalog_id: r.catalog_id as i64, qty: r.qty, sideboard: r.sideboard })
                    .collect(),
                None => vec![],
            };
            let d = match (game, snaps.get(&t.mtgo_game_id)) {
                (Some(g), Some(s)) if !list.is_empty() => derive(g, me, s, &names, &list),
                _ => {
                    st.no_snapshots += 1;
                    Derived { events: vec![], hand_known: false, pile_known: false }
                }
            };
            diesel::sql_query(format!("DELETE FROM mtgo_card_events WHERE game_id = {} AND verb IN ({verbs})", t.id)).execute(conn)?;
            super::db::insert_card_events(conn, t.id, t.match_id, me, None, &d.events)?;
            diesel::sql_query(format!("UPDATE mtgo_games SET hands = {HANDS}, hand_known = {}, pile_known = {} WHERE id = {}", d.hand_known as i32, d.pile_known as i32, t.id)).execute(conn)?;
            st.games += 1;
            st.hand_known += d.hand_known as usize;
            st.pile_known += d.pile_known as usize;
        }
        Ok(())
    })
    .map_err(e)?;
    Ok(st)
}
