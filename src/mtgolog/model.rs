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

#[derive(Debug, Clone, Default)]
pub struct CardUse {
    pub catalog_id: u32,
    pub name: String,
    /// Distinct object instances observed for this card name (approx. copies seen).
    pub instances: BTreeSet<u32>,
    /// Number of cast/play events.
    pub plays: u32,
    /// Earliest turn (game turn number) the card was cast/played; None if only seen passively.
    pub first_turn: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct PlayerGame {
    pub name: String,
    pub mulligans: u32,
    pub hand_size: Option<u32>,
    /// Cards this player cast or played (land drops), keyed by name.
    pub played: BTreeMap<String, CardUse>,
    /// Any card attributed to this player in any way (played ∪ revealed/discarded/milled/sacrificed/…).
    pub seen: BTreeMap<String, CardUse>,
    pub lands_played: u32,
    pub spells_cast: u32,
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

fn note_card(map: &mut BTreeMap<String, CardUse>, c: &CardRef, turn: u32, played: bool) {
    let u = map.entry(c.name.clone()).or_insert_with(|| CardUse { catalog_id: c.catalog_id, name: c.name.clone(), ..Default::default() });
    u.instances.insert(c.instance_id);
    if played {
        u.plays += 1;
        if u.first_turn.map_or(true, |t| turn < t) {
            u.first_turn = Some(turn);
        }
    }
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
            Event::Casts { player, card, .. } => {
                self.claim(player, card);
                let pg = self.pg(player);
                pg.spells_cast += 1;
                note_card(&mut pg.played, card, turn, true);
                note_card(&mut pg.seen, card, turn, true);
            }
            Event::PlaysLand { player, card } => {
                self.claim(player, card);
                let pg = self.pg(player);
                pg.lands_played += 1;
                note_card(&mut pg.played, card, turn, true);
                note_card(&mut pg.seen, card, turn, true);
            }
            Event::Activates { player, source, .. } => {
                // activating = controlling; treat as own unless known foreign (stolen permanents are rare)
                if !self.foreign(player, source) {
                    self.claim(player, source);
                    let pg = self.pg(player);
                    note_card(&mut pg.seen, source, turn, false);
                }
            }
            Event::Triggers { player, source: Some(source), .. } => {
                if !self.foreign(player, source) {
                    self.claim(player, source);
                    let pg = self.pg(player);
                    note_card(&mut pg.seen, source, turn, false);
                }
            }
            // Own-zone evidence: these act on the actor's own cards.
            Event::Discards { player, cards } | Event::Mills { player, cards } | Event::Reveals { player, cards, .. } => {
                for c in cards {
                    self.claim(player, c);
                }
                let pg = self.pg(player);
                for c in cards {
                    note_card(&mut pg.seen, c, turn, false);
                }
            }
            Event::Cycles { player, card } => {
                self.claim(player, card);
                let pg = self.pg(player);
                note_card(&mut pg.seen, card, turn, false);
            }
            // Mixed evidence: sacrifice / to-graveyard / exile / return may act on the other
            // player's cards (Ghost Vacuum, bounce, edicts). Only count instances not known to be foreign.
            Event::Sacrifices { player, cards } | Event::ToGraveyard { player, cards } | Event::Exiles { player, cards, .. } | Event::Returns { player, cards, .. } => {
                let own: Vec<CardRef> = cards.iter().filter(|c| self.owns(player, c)).cloned().collect();
                let pg = self.pg(player);
                for c in &own {
                    note_card(&mut pg.seen, c, turn, false);
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
