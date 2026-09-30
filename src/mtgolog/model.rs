//! Structure a raw game log into a match with games, and derive per-game facts.

use super::event::{actor, parse_event, CardRef, Event, GameEnd, Target};
use super::format::RawLog;
use chrono::NaiveDateTime;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub struct LogEvent {
    pub ts: NaiveDateTime,
    pub event: Event,
    pub raw: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameOutcome {
    /// Someone was declared the winner ("wins the game").
    Won { winner: String, by: WinBy },
    /// No "wins the game" line (disconnect, crash, log ended).
    Unfinished,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WinBy {
    /// Opponent conceded.
    Concession,
    /// Opponent left / lost connection and the game was awarded.
    OpponentLeft(GameEnd),
    /// Loser got a "loses the game" line (e.g. drew from empty library, poison, state-based).
    LossLine,
    /// Plain "wins the game" (damage / life).
    Normal,
}

/// What happened to a card in one log line (`CardEvent::verb`); stored by `Verb::name`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Cast,
    Land,
    Cycled,
    Activated,
    Triggered,
    Attacked,
    /// not the discard that pays for cycling, see `GameBuilder::push`
    Discarded,
    Milled,
    Revealed,
    Sacrificed,
    ToGraveyard,
    Exiled,
    Returned,
}

impl Verb {
    pub fn name(self) -> &'static str {
        match self {
            Verb::Cast => "cast",
            Verb::Land => "land",
            Verb::Cycled => "cycled",
            Verb::Activated => "activated",
            Verb::Triggered => "triggered",
            Verb::Attacked => "attacked",
            Verb::Discarded => "discarded",
            Verb::Milled => "milled",
            Verb::Revealed => "revealed",
            Verb::Sacrificed => "sacrificed",
            Verb::ToGraveyard => "to_graveyard",
            Verb::Exiled => "exiled",
            Verb::Returned => "returned",
        }
    }
}

/// One card in one log line. A card whose owner the game gives no evidence of is left out.
#[derive(Debug, Clone)]
pub struct CardEvent {
    /// index into `GameLog::events`
    pub seq: u32,
    pub turn: u32,
    /// the player whose card it is
    pub owner: String,
    /// the player the line is about; differs from `owner` for an opponent's exile or edict
    pub actor: String,
    pub card: CardRef,
    pub verb: Verb,
    /// the other card involved: a cast or ability's first card target, the source an exile or
    /// return names
    pub other: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PlayerGame {
    pub name: String,
    pub mulligans: u32,
    pub hand_size: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct GameLog {
    pub number: u32,
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
    pub players: Vec<String>,
    pub first_player: Option<String>,
    pub die_rolls: Vec<(String, u32)>,
    pub turns: u32,
    pub outcome: GameOutcome,
    pub per_player: BTreeMap<String, PlayerGame>,
    pub events: Vec<LogEvent>,
    pub card_events: Vec<CardEvent>,
    /// player → seq of the fifth consecutive put-on-top line of theirs after they cast Doomsday,
    /// for the first Doomsday that resolves (Doomsday's pile is the only run of five)
    pub doomsday_resolved: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchResult {
    Won { winner: String, wins: u32, losses: u32 },
    Tied { a: u32, b: u32 },
    Unknown,
}

#[derive(Debug, Clone)]
pub struct MatchLog {
    pub guid: String,
    pub start: Option<NaiveDateTime>,
    pub end: Option<NaiveDateTime>,
    pub players: Vec<String>,
    pub games: Vec<GameLog>,
    pub result: MatchResult,
    /// Events not inside any game (pre-game chatter, between-game lines, system notices).
    pub extra: Vec<LogEvent>,
    pub record_count: usize,
    /// Replay logs (opened from the client's game history) re-emit every line with the viewer's
    /// name in the record's sender field and millisecond spacing; live logs have an empty sender.
    pub replay: bool,
}

impl GameLog {
    pub fn winner(&self) -> Option<&str> {
        match &self.outcome {
            GameOutcome::Won { winner, .. } => Some(winner),
            GameOutcome::Unfinished => None,
        }
    }

    pub fn duration_secs(&self) -> i64 {
        (self.end - self.start).num_seconds()
    }
}

impl MatchLog {
    pub fn opponent_of<'a>(&'a self, me: &str) -> Option<&'a str> {
        self.players.iter().map(String::as_str).find(|p| *p != me)
    }

    pub fn game_score(&self, me: &str) -> (u32, u32) {
        let mut w = 0;
        let mut l = 0;
        for g in &self.games {
            match g.winner() {
                Some(p) if p == me => w += 1,
                Some(_) => l += 1,
                None => {}
            }
        }
        (w, l)
    }
}

fn is_game_start(e: &Event) -> bool {
    matches!(e, Event::Joined { .. } | Event::Rolled { .. })
}

/// The card an exile or return line names after "with": a card ref, else "Triggered ability
/// from X".
fn other_card(rest: &str) -> Option<String> {
    if let Some(c) = super::event::card_refs(rest).into_iter().next() {
        return Some(c.name);
    }
    rest.split_once("ability from ").map(|(_, x)| x.trim_end_matches('.').to_string())
}

struct GameBuilder {
    number: u32,
    start: NaiveDateTime,
    end: NaiveDateTime,
    players: BTreeSet<String>,
    first_player: Option<String>,
    die_rolls: Vec<(String, u32)>,
    turn: u32,
    turns: u32,
    winner: Option<String>,
    concession_by: Option<String>,
    left: Option<(String, GameEnd)>,
    loss_line: Option<String>,
    per_player: BTreeMap<String, PlayerGame>,
    events: Vec<LogEvent>,
    card_events: Vec<CardEvent>,
    /// players with a Doomsday cast that has not resolved yet
    doomsday_cast: BTreeSet<String>,
    /// player → (seq of the latest put-on-top line, length of the run of them it ends)
    puts_run: BTreeMap<String, (u32, u32)>,
    doomsday_resolved: BTreeMap<String, u32>,
    /// card name → players with positive own-evidence (cast/played/discarded/milled/revealed/cycled).
    /// MTGO re-numbers an object's instance id on every zone change, so ownership is tracked by name.
    owner: BTreeMap<String, BTreeSet<String>>,
}

impl GameBuilder {
    fn new(number: u32, ts: NaiveDateTime) -> Self {
        GameBuilder {
            number,
            start: ts,
            end: ts,
            players: BTreeSet::new(),
            first_player: None,
            die_rolls: Vec::new(),
            turn: 0,
            turns: 0,
            winner: None,
            concession_by: None,
            left: None,
            loss_line: None,
            per_player: BTreeMap::new(),
            events: Vec::new(),
            card_events: Vec::new(),
            doomsday_cast: BTreeSet::new(),
            puts_run: BTreeMap::new(),
            doomsday_resolved: BTreeMap::new(),
            owner: BTreeMap::new(),
        }
    }

    fn claim(&mut self, player: &str, c: &CardRef) {
        self.owner.entry(c.name.clone()).or_default().insert(player.to_string());
    }

    /// True if `player` has positive evidence of owning a card of this name in this game
    /// (cast / played / discarded / milled / revealed / cycled it). Used for events that may act
    /// on the other player's cards ("boryas exiles Atraxa", "returns Omniscience").
    fn owns(&self, player: &str, c: &CardRef) -> bool {
        self.owner.get(&c.name).is_some_and(|ps| ps.contains(player))
    }

    fn foreign(&self, player: &str, c: &CardRef) -> bool {
        self.owner.get(&c.name).is_some_and(|ps| !ps.contains(player) && !ps.is_empty())
    }

    /// The other player of the game, once both have appeared.
    fn opponent(&self, player: &str) -> Option<String> {
        self.players.iter().find(|p| *p != player).cloned()
    }

    fn note(&mut self, turn: u32, owner: &str, actor: &str, card: &CardRef, verb: Verb, other: Option<String>) {
        let seq = self.events.len() as u32;
        self.card_events.push(CardEvent { seq, turn, owner: owner.to_string(), actor: actor.to_string(), card: card.clone(), verb, other });
    }

    fn pg(&mut self, name: &str) -> &mut PlayerGame {
        self.players.insert(name.to_string());
        self.per_player.entry(name.to_string()).or_insert_with(|| PlayerGame { name: name.to_string(), ..Default::default() })
    }

    fn push(&mut self, le: LogEvent) {
        self.end = le.ts;
        let turn = self.turn;
        match &le.event {
            Event::Rolled { player, value } => {
                self.die_rolls.push((player.clone(), *value));
                self.pg(player);
            }
            Event::Joined { player } => {
                self.pg(player);
            }
            Event::PlaysFirst { player } => {
                self.first_player = Some(player.clone());
                self.pg(player);
            }
            Event::DrawsFirst { player } => {
                self.pg(player);
            }
            Event::Mulligan { player, .. } => {
                self.pg(player).mulligans += 1;
            }
            Event::BeginsGame { player, hand } => {
                self.pg(player).hand_size = Some(*hand);
            }
            Event::Turn { number, player } => {
                self.turn = *number;
                self.turns = self.turns.max(*number);
                if self.first_player.is_none() && *number == 1 {
                    self.first_player = Some(player.clone());
                }
                self.pg(player);
            }
            Event::Casts { player, card, targets, .. } => {
                self.claim(player, card);
                self.pg(player);
                self.note(turn, player, player, card, Verb::Cast, target_cards(targets).next().map(|c| c.name.clone()));
                if card.name == "Doomsday" {
                    self.doomsday_cast.insert(player.clone());
                }
            }
            Event::PlaysLand { player, card } => {
                self.claim(player, card);
                self.pg(player);
                self.note(turn, player, player, card, Verb::Land, None);
            }
            Event::Activates { player, source, targets, .. } => {
                self.pg(player);
                // activating = controlling; treat as own unless known foreign (stolen permanents are rare)
                if !self.foreign(player, source) {
                    self.claim(player, source);
                    self.note(turn, player, player, source, Verb::Activated, target_cards(targets).next().map(|c| c.name.clone()));
                }
            }
            Event::Triggers { player, source: Some(source), targets, .. } => {
                self.pg(player);
                if !self.foreign(player, source) {
                    self.claim(player, source);
                    self.note(turn, player, player, source, Verb::Triggered, target_cards(targets).next().map(|c| c.name.clone()));
                }
            }
            Event::Attacks { defender, attackers, .. } => {
                self.pg(defender);
                if let Some(attacker) = self.opponent(defender) {
                    for c in attackers {
                        self.note(turn, &attacker, &attacker, c, Verb::Attacked, None);
                    }
                }
            }
            // Own-zone evidence: these act on the actor's own cards.
            Event::Discards { player, cards } | Event::Mills { player, cards } | Event::Reveals { player, cards, .. } => {
                self.pg(player);
                let verb = match &le.event {
                    Event::Discards { .. } => Verb::Discarded,
                    Event::Mills { .. } => Verb::Milled,
                    _ => Verb::Revealed,
                };
                for c in cards {
                    self.claim(player, c);
                    self.note(turn, player, player, c, verb, None);
                }
            }
            Event::Cycles { player, card } => {
                self.claim(player, card);
                self.pg(player);
                // the log writes the discard that pays for cycling first, as its own line
                if self.card_events.last().is_some_and(|e| e.verb == Verb::Discarded && e.owner == *player && e.card.instance_id == card.instance_id) {
                    self.card_events.pop();
                }
                self.note(turn, player, player, card, Verb::Cycled, None);
            }
            // Mixed evidence: sacrifice / to-graveyard / exile / return may act on the other
            // player's cards (Ghost Vacuum, bounce, edicts); a card neither player has shown is left out.
            Event::Sacrifices { player, cards } | Event::ToGraveyard { player, cards } | Event::Exiles { player, cards, .. } | Event::Returns { player, cards, .. } => {
                self.pg(player);
                let (verb, other) = match &le.event {
                    Event::Sacrifices { .. } => (Verb::Sacrificed, None),
                    Event::ToGraveyard { .. } => (Verb::ToGraveyard, None),
                    Event::Exiles { rest, .. } => (Verb::Exiled, other_card(rest)),
                    Event::Returns { rest, .. } => (Verb::Returned, other_card(rest)),
                    _ => unreachable!(),
                };
                let opp = self.opponent(player);
                for c in cards {
                    let owner = if self.owns(player, c) {
                        player.clone()
                    } else if let Some(o) = opp.as_ref().filter(|o| self.owns(o, c)) {
                        o.clone()
                    } else {
                        continue;
                    };
                    self.note(turn, &owner, player, c, verb, other.clone());
                }
            }
            Event::PutsOnTop { player, .. } => {
                self.pg(player);
                let seq = self.events.len() as u32;
                let run = match self.puts_run.get(player) {
                    Some(&(last, n)) if last + 1 == seq => n + 1,
                    _ => 1,
                };
                self.puts_run.insert(player.clone(), (seq, run));
                if run == 5 && self.doomsday_cast.remove(player) {
                    self.doomsday_resolved.entry(player.clone()).or_insert(seq);
                }
            }
            Event::Concedes { player } => {
                self.concession_by = Some(player.clone());
                self.pg(player);
            }
            Event::LeavesGame { player, how } => {
                self.left = Some((player.clone(), how.clone()));
                self.pg(player);
            }
            Event::LosesGame { player } => {
                self.loss_line = Some(player.clone());
                self.pg(player);
            }
            Event::WinsGame { player } => {
                self.winner = Some(player.clone());
                self.pg(player);
            }
            _ => {
                if let Some(p) = actor(&le.event) {
                    self.pg(p);
                }
            }
        }
        self.events.push(le);
    }

    fn finish(self) -> GameLog {
        let outcome = match self.winner {
            Some(winner) => {
                let by = if self.concession_by.as_deref().is_some_and(|p| p != winner) {
                    WinBy::Concession
                } else if let Some((p, how)) = &self.left {
                    if p != &winner { WinBy::OpponentLeft(how.clone()) } else { WinBy::Normal }
                } else if self.loss_line.as_deref().is_some_and(|p| p != winner) {
                    WinBy::LossLine
                } else {
                    WinBy::Normal
                };
                GameOutcome::Won { winner, by }
            }
            None => GameOutcome::Unfinished,
        };
        GameLog {
            number: self.number,
            start: self.start,
            end: self.end,
            players: self.players.into_iter().collect(),
            first_player: self.first_player,
            die_rolls: self.die_rolls,
            turns: self.turns,
            outcome,
            per_player: self.per_player,
            events: self.events,
            card_events: self.card_events,
            doomsday_resolved: self.doomsday_resolved,
        }
    }
}

pub fn build_match(raw: &RawLog) -> MatchLog {
    let mut games: Vec<GameLog> = Vec::new();
    let mut extra: Vec<LogEvent> = Vec::new();
    let mut cur: Option<GameBuilder> = None;
    let mut result = MatchResult::Unknown;
    let mut players: BTreeSet<String> = BTreeSet::new();
    let mut start = None;
    let mut end = None;

    for r in &raw.records {
        let event = parse_event(&r.text);
        let le = LogEvent { ts: r.ts, event, raw: r.text.clone() };
        start.get_or_insert(r.ts);
        end = Some(r.ts);
        if let Some(p) = actor(&le.event) {
            players.insert(p.to_string());
        }

        // Match-level lines live outside games.
        match &le.event {
            Event::LeadsMatch { .. } => {
                extra.push(le);
                continue;
            }
            Event::WinsMatch { player, wins, losses } => {
                result = MatchResult::Won { winner: player.clone(), wins: *wins, losses: *losses };
                extra.push(le);
                continue;
            }
            Event::MatchTied { a, b } => {
                result = MatchResult::Tied { a: *a, b: *b };
                extra.push(le);
                continue;
            }
            _ => {}
        }

        // Game boundaries: a Joined/Rolled line when no game is open — or when the open game is
        // already decided — starts a new game.
        let open_decided = cur.as_ref().is_some_and(|g| g.winner.is_some());
        if is_game_start(&le.event) && (cur.is_none() || open_decided) {
            if let Some(g) = cur.take() {
                games.push(g.finish());
            }
            cur = Some(GameBuilder::new(games.len() as u32 + 1, r.ts));
        }

        match cur.as_mut() {
            Some(g) => g.push(le),
            None => extra.push(le),
        }
    }
    if let Some(g) = cur.take() {
        games.push(g.finish());
    }

    // Fill in match-level players from games too (e.g. chat-only lines).
    for g in &games {
        for p in &g.players {
            players.insert(p.clone());
        }
    }

    let replay = !raw.records.is_empty() && raw.records.iter().all(|r| !r.sender.is_empty());
    MatchLog { guid: raw.guid.clone(), start, end, players: players.into_iter().collect(), games, result, extra, record_count: raw.records.len(), replay }
}

/// Card targets helper (used by analyses).
pub fn target_cards(targets: &[Target]) -> impl Iterator<Item = &CardRef> {
    targets.iter().filter_map(|t| match t {
        Target::Card(c) => Some(c),
        Target::Player(_) => None,
    })
}
