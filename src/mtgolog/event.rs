//! Typed events parsed from the text of MTGO game-log records.
//!
//! Lines are system messages prefixed with "@P" (occasionally "@P@P"). Card references are
//! embedded as `@[Name@:catalog_id,instance_id:@]`. Anything we do not recognise becomes
//! `Event::Other` — `mtgctl mtgo scan --unparsed` lists the most common such shapes so the
//! vocabulary can be grown deliberately.

use regex::Regex;
use std::sync::LazyLock;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CardRef {
    pub name: String,
    /// MTGO catalog id (== Scryfall `mtgo_id`).
    pub catalog_id: u32,
    /// Per-game object instance id.
    pub instance_id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Card(CardRef),
    Player(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameEnd {
    Conceded,
    Left,
    LostConnection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Rolled { player: String, value: u32 },
    Joined { player: String },
    PlaysFirst { player: String },
    DrawsFirst { player: String },
    /// "mulligans to six cards" — `to` is the resulting hand size (after the London bottom).
    Mulligan { player: String, to: u32 },
    /// "begins the game with seven cards in hand" — final kept hand size.
    BeginsGame { player: String, hand: u32 },
    Turn { number: u32, player: String },
    SkipsDraw { player: String },
    Draws { player: String, count: u32, via: Option<CardRef> },
    Casts { player: String, card: CardRef, targets: Vec<Target>, rest: String },
    PlaysLand { player: String, card: CardRef },
    Activates { player: String, source: CardRef, targets: Vec<Target>, text: String },
    Triggers { player: String, source: Option<CardRef>, source_name: String, targets: Vec<Target>, text: String },
    Attacks { defender: String, defender_card: Option<CardRef>, attackers: Vec<CardRef> },
    Blocks { blocker: CardRef, attacker: CardRef },
    Discards { player: String, cards: Vec<CardRef> },
    Mills { player: String, cards: Vec<CardRef> },
    Exiles { player: String, cards: Vec<CardRef>, rest: String },
    Sacrifices { player: String, cards: Vec<CardRef> },
    ToGraveyard { player: String, cards: Vec<CardRef> },
    Reveals { player: String, cards: Vec<CardRef>, rest: String },
    Returns { player: String, cards: Vec<CardRef>, rest: String },
    Cycles { player: String, card: CardRef },
    Counters { player: String, spell: CardRef, with: String },
    Shuffles { player: String },
    GainsLife { player: String, amount: u32 },
    DamagePrevented { amount: u32, source: Option<CardRef>, to: String },
    Concedes { player: String },
    LeavesGame { player: String, how: GameEnd },
    WinsGame { player: String },
    LosesGame { player: String },
    LeadsMatch { player: String, wins: u32, losses: u32 },
    WinsMatch { player: String, wins: u32, losses: u32 },
    MatchTied { a: u32, b: u32 },
    System(String),
    Other(String),
}

static CARD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"@\[(.+?)@:(\d+),(\d+):@\]").unwrap());

macro_rules! re {
    ($name:ident, $pat:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($pat).unwrap());
    };
}

// A single card ref as a sub-pattern (non-capturing wrapper; we re-parse with CARD_RE).
const C: &str = r"@\[.+?@:\d+,\d+:@\]";

re!(TURN, r"^Turn (\d+): ([^@:]+?)(?: - Extra Turn)?$");
re!(ROLLED, r"^([^@:]+?) rolled a (\d+)\.$");
re!(JOINED, r"^([^@:]+?) joined the game\.$");
re!(PLAYFIRST, r"^([^@:]+?) chooses to (play|draw) first\.$");
re!(MULLIGAN, r"^([^@:]+?) mulligans to (\w+) cards?\.$");
re!(BEGINS, r"^([^@:]+?) (?:puts .*? on the bottom of their library and )?begins the game with (\w+) cards? in hand\.$");
re!(SKIPDRAW, r"^([^@:]+?) skips their draw step\.$");
re!(DRAWS, r"^([^@:]+?) draws (a|\w+) cards?(?: with (.+?)(?:'s ability)?)?\.$");
re!(DRAWS_NEXT, r"^([^@:]+?) draws their next card\.$");
static CASTS: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"^([^@:]+?) casts ({C})(.*)$")).unwrap());
static PLAYS: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"^([^@:]+?) plays ({C})\.$")).unwrap());
static ACTIVATES: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"^([^@:]+?) activates an ability of ({C})(.*)$")).unwrap());
re!(TRIGGERS, r"^([^@:]+?) puts (?:a )?triggered ability from (.+?) onto the stack(.*)$");
static ATTACKED_PW: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"^([^@:]+?)'s ({C}) is being attacked by (.*)$")).unwrap());
re!(ATTACKED, r"^([^@:]+?) is being attacked by (.*)$");
static BLOCKS: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"^({C}) blocks ({C})\.$")).unwrap());
re!(DISCARDS, r"^([^@:]+?) discards (.+)\.$");
re!(MILLS, r"^([^@:]+?) mills (.+)\.$");
re!(EXILES, r"^([^@:]+?) exiles (.+?)( with .*| from .*)?\.$");
re!(SACRIFICES, r"^([^@:]+?) sacrifices (.+)\.$");
re!(TOGY, r"^([^@:]+?) puts (.+) into their graveyard\.$");
re!(REVEALS, r"^([^@:]+?) reveals (.+?)( with .*)?\.$");
re!(RETURNS, r"^([^@:]+?) returns (.+?) to (?:their|its owner's) hand( with .*)?\.$");
re!(CYCLES, r"^([^@:]+?) cycles (.+)\.$");
static COUNTERS: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"^([^@:]+?) counters ({C}) with (.+)\.$")).unwrap());
re!(SHUFFLES, r"^([^@:]+?) shuffles their library\.$");
re!(GAINS, r"^([^@:]+?) gains (\d+) life\.$");
re!(PREVENTED, r"^(\d+) damage caused by (.+?) to ([^@:]+?) is prevented\.$");
re!(CONCEDES, r"^([^@:]+?) has conceded from the game\.$");
re!(LEFT, r"^([^@:]+?) has left the game\.$");
re!(LOSTCONN, r"^([^@:]+?) has lost connection to the game\.$");
re!(WINSGAME, r"^([^@:]+?) wins the game\.$");
re!(LOSESGAME, r"^([^@:]+?) loses the game\.$");
re!(LEADS, r"^([^@:]+?) leads the match (\d+)-(\d+)$");
re!(WINSMATCH, r"^([^@:]+?) wins the match (\d+)-(\d+)$");
re!(TIED, r"^Match Tied (\d+)-(\d+)$");
re!(TARGETING, r" targeting (.+?)(?: \(|\.?$)");

pub fn card_refs(text: &str) -> Vec<CardRef> {
    CARD_RE
        .captures_iter(text)
        .map(|c| CardRef {
            name: c[1].to_string(),
            catalog_id: c[2].parse().unwrap_or(0),
            instance_id: c[3].parse().unwrap_or(0),
        })
        .collect()
}

fn first_card(text: &str) -> Option<CardRef> {
    card_refs(text).into_iter().next()
}

fn word_number(w: &str) -> Option<u32> {
    Some(match w {
        "a" | "one" => 1,
        "two" => 2,
        "three" => 3,
        "four" => 4,
        "five" => 5,
        "six" => 6,
        "seven" => 7,
        "eight" => 8,
        "nine" => 9,
        "ten" => 10,
        _ => return w.parse().ok(),
    })
}

/// Targets named in a " targeting X, Y, and Z" clause: cards as refs, bare words as players.
fn targets_in(rest: &str) -> Vec<Target> {
    let Some(m) = TARGETING.captures(rest) else {
        return Vec::new();
    };
    let clause = &m[1];
    let mut out: Vec<Target> = card_refs(clause).into_iter().map(Target::Card).collect();
    // Player targets: what remains between separators once card refs are stripped; a name may
    // contain spaces, so only "," and " and " separate.
    let stripped = CARD_RE.replace_all(clause, "");
    for tok in stripped.split(',').flat_map(|s| s.split(" and ")) {
        let tok = tok.trim().trim_start_matches("and ").trim();
        let tok = tok.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-' && c != '.');
        if tok.is_empty() || tok == "and" {
            continue;
        }
        out.push(Target::Player(tok.to_string()));
    }
    out
}

/// Strip the "@P" prefix(es) that mark system lines.
pub fn strip_prefix(text: &str) -> (&str, bool) {
    let mut t = text;
    let mut had = false;
    while let Some(rest) = t.strip_prefix("@P") {
        t = rest;
        had = true;
    }
    (t, had)
}

pub fn parse_event(text: &str) -> Event {
    let (t, system) = strip_prefix(text);
    if !system {
        // "Match Tied 1-1" is emitted without the @P prefix.
        if let Some(c) = TIED.captures(t) {
            return Event::MatchTied { a: c[1].parse().unwrap_or(0), b: c[2].parse().unwrap_or(0) };
        }
        return Event::System(t.to_string());
    }
    macro_rules! cap {
        ($re:ident) => {
            $re.captures(t)
        };
    }
    if let Some(c) = cap!(TURN) {
        return Event::Turn { number: c[1].parse().unwrap_or(0), player: c[2].to_string() };
    }
    if let Some(c) = cap!(CASTS) {
        let rest = c[3].to_string();
        return Event::Casts { player: c[1].to_string(), card: first_card(&c[2]).unwrap(), targets: targets_in(&rest), rest };
    }
    if let Some(c) = cap!(PLAYS) {
        return Event::PlaysLand { player: c[1].to_string(), card: first_card(&c[2]).unwrap() };
    }
    if let Some(c) = cap!(DRAWS) {
        let via = c.get(3).and_then(|m| first_card(m.as_str()));
        return Event::Draws { player: c[1].to_string(), count: word_number(&c[2]).unwrap_or(1), via };
    }
    if let Some(c) = cap!(DRAWS_NEXT) {
        return Event::Draws { player: c[1].to_string(), count: 1, via: None };
    }
    if let Some(c) = cap!(ACTIVATES) {
        let text = c[3].to_string();
        return Event::Activates { player: c[1].to_string(), source: first_card(&c[2]).unwrap(), targets: targets_in(&text), text };
    }
    if let Some(c) = cap!(TRIGGERS) {
        let text = c[3].to_string();
        return Event::Triggers {
            player: c[1].to_string(),
            source: first_card(&c[2]),
            source_name: CARD_RE.replace_all(&c[2], "$1").to_string(),
            targets: targets_in(&text),
            text,
        };
    }
    if let Some(c) = cap!(ATTACKED_PW) {
        return Event::Attacks { defender: c[1].to_string(), defender_card: first_card(&c[2]), attackers: card_refs(&c[3]) };
    }
    if let Some(c) = cap!(ATTACKED) {
        return Event::Attacks { defender: c[1].to_string(), defender_card: None, attackers: card_refs(&c[2]) };
    }
    if let Some(c) = cap!(BLOCKS) {
        return Event::Blocks { blocker: first_card(&c[1]).unwrap(), attacker: first_card(&c[2]).unwrap() };
    }
    if let Some(c) = cap!(JOINED) {
        return Event::Joined { player: c[1].to_string() };
    }
    if let Some(c) = cap!(ROLLED) {
        return Event::Rolled { player: c[1].to_string(), value: c[2].parse().unwrap_or(0) };
    }
    if let Some(c) = cap!(PLAYFIRST) {
        return if &c[2] == "play" { Event::PlaysFirst { player: c[1].to_string() } } else { Event::DrawsFirst { player: c[1].to_string() } };
    }
    if let Some(c) = cap!(MULLIGAN) {
        return Event::Mulligan { player: c[1].to_string(), to: word_number(&c[2]).unwrap_or(0) };
    }
    if let Some(c) = cap!(BEGINS) {
        return Event::BeginsGame { player: c[1].to_string(), hand: word_number(&c[2]).unwrap_or(0) };
    }
    if let Some(c) = cap!(SKIPDRAW) {
        return Event::SkipsDraw { player: c[1].to_string() };
    }
    if let Some(c) = cap!(DISCARDS) {
        return Event::Discards { player: c[1].to_string(), cards: card_refs(&c[2]) };
    }
    if let Some(c) = cap!(MILLS) {
        return Event::Mills { player: c[1].to_string(), cards: card_refs(&c[2]) };
    }
    if let Some(c) = cap!(SACRIFICES) {
        return Event::Sacrifices { player: c[1].to_string(), cards: card_refs(&c[2]) };
    }
    if let Some(c) = cap!(TOGY) {
        return Event::ToGraveyard { player: c[1].to_string(), cards: card_refs(&c[2]) };
    }
    if let Some(c) = cap!(CYCLES) {
        if let Some(card) = first_card(&c[2]) {
            return Event::Cycles { player: c[1].to_string(), card };
        }
    }
    if let Some(c) = cap!(COUNTERS) {
        return Event::Counters { player: c[1].to_string(), spell: first_card(&c[2]).unwrap(), with: c[3].to_string() };
    }
    if let Some(c) = cap!(RETURNS) {
        return Event::Returns { player: c[1].to_string(), cards: card_refs(&c[2]), rest: c.get(3).map(|m| m.as_str().to_string()).unwrap_or_default() };
    }
    if let Some(c) = cap!(EXILES) {
        return Event::Exiles { player: c[1].to_string(), cards: card_refs(&c[2]), rest: c.get(3).map(|m| m.as_str().to_string()).unwrap_or_default() };
    }
    if let Some(c) = cap!(REVEALS) {
        return Event::Reveals { player: c[1].to_string(), cards: card_refs(&c[2]), rest: c.get(3).map(|m| m.as_str().to_string()).unwrap_or_default() };
    }
    if let Some(c) = cap!(SHUFFLES) {
        return Event::Shuffles { player: c[1].to_string() };
    }
    if let Some(c) = cap!(GAINS) {
        return Event::GainsLife { player: c[1].to_string(), amount: c[2].parse().unwrap_or(0) };
    }
    if let Some(c) = cap!(PREVENTED) {
        return Event::DamagePrevented { amount: c[1].parse().unwrap_or(0), source: first_card(&c[2]), to: c[3].to_string() };
    }
    if let Some(c) = cap!(CONCEDES) {
        return Event::Concedes { player: c[1].to_string() };
    }
    if let Some(c) = cap!(LEFT) {
        return Event::LeavesGame { player: c[1].to_string(), how: GameEnd::Left };
    }
    if let Some(c) = cap!(LOSTCONN) {
        return Event::LeavesGame { player: c[1].to_string(), how: GameEnd::LostConnection };
    }
    if let Some(c) = cap!(WINSGAME) {
        return Event::WinsGame { player: c[1].to_string() };
    }
    if let Some(c) = cap!(LOSESGAME) {
        return Event::LosesGame { player: c[1].to_string() };
    }
    if let Some(c) = cap!(LEADS) {
        return Event::LeadsMatch { player: c[1].to_string(), wins: c[2].parse().unwrap_or(0), losses: c[3].parse().unwrap_or(0) };
    }
    if let Some(c) = cap!(WINSMATCH) {
        return Event::WinsMatch { player: c[1].to_string(), wins: c[2].parse().unwrap_or(0), losses: c[3].parse().unwrap_or(0) };
    }
    if let Some(c) = cap!(TIED) {
        return Event::MatchTied { a: c[1].parse().unwrap_or(0), b: c[2].parse().unwrap_or(0) };
    }
    Event::Other(t.to_string())
}

/// Collapse a line into a template (card refs → <CARD>, known players → <P>, digits → <N>) for
/// vocabulary censuses.
pub fn template(text: &str, players: &[String]) -> String {
    let (t, _) = strip_prefix(text);
    let mut s = CARD_RE.replace_all(t, "<CARD>").to_string();
    let mut ps: Vec<&String> = players.iter().collect();
    ps.sort_by_key(|p| std::cmp::Reverse(p.len()));
    for p in ps {
        if !p.is_empty() {
            s = s.replace(p.as_str(), "<P>");
        }
    }
    static DIGITS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d+").unwrap());
    DIGITS.replace_all(&s, "<N>").to_string()
}

/// Actor of an event (the player the line is about), if any.
pub fn actor(e: &Event) -> Option<&str> {
    use Event::*;
    Some(match e {
        Rolled { player, .. } | Joined { player } | PlaysFirst { player } | DrawsFirst { player } | Mulligan { player, .. }
        | BeginsGame { player, .. } | Turn { player, .. } | SkipsDraw { player } | Draws { player, .. } | Casts { player, .. }
        | PlaysLand { player, .. } | Activates { player, .. } | Triggers { player, .. } | Discards { player, .. }
        | Mills { player, .. } | Exiles { player, .. } | Sacrifices { player, .. } | ToGraveyard { player, .. }
        | Reveals { player, .. } | Returns { player, .. } | Cycles { player, .. } | Counters { player, .. }
        | Shuffles { player } | GainsLife { player, .. } | Concedes { player } | LeavesGame { player, .. }
        | WinsGame { player } | LosesGame { player } | LeadsMatch { player, .. } | WinsMatch { player, .. } => player,
        Attacks { defender, .. } => defender,
        DamagePrevented { to, .. } => to,
        Blocks { .. } | MatchTied { .. } | System(_) | Other(_) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_core_lines() {
        assert_eq!(parse_event("@PTurn 4: boryas"), Event::Turn { number: 4, player: "boryas".into() });
        assert_eq!(parse_event("@P@Pboryas joined the game."), Event::Joined { player: "boryas".into() });
        assert_eq!(parse_event("@PDydyk95 mulligans to six cards."), Event::Mulligan { player: "Dydyk95".into(), to: 6 });
        assert_eq!(
            parse_event("@PDydyk95 puts a card on the bottom of their library and begins the game with six cards in hand."),
            Event::BeginsGame { player: "Dydyk95".into(), hand: 6 }
        );
        match parse_event("@Pboryas casts @[Duress@:24634,469:@] targeting Dydyk95.") {
            Event::Casts { player, card, targets, .. } => {
                assert_eq!(player, "boryas");
                assert_eq!(card.name, "Duress");
                assert_eq!(card.catalog_id, 24634);
                assert_eq!(targets, vec![Target::Player("Dydyk95".into())]);
            }
            e => panic!("{e:?}"),
        }
        match parse_event("@PDydyk95 activates an ability of @[Wasteland@:20070,452:@] (Destroy target nonbasic land.) targeting @[Underground Sea@:77248,473:@].") {
            Event::Activates { source, targets, .. } => {
                assert_eq!(source.name, "Wasteland");
                assert!(matches!(&targets[0], Target::Card(c) if c.name == "Underground Sea"));
            }
            e => panic!("{e:?}"),
        }
        match parse_event("@Pboryas is being attacked by @[Anointed Peacekeeper@:204960,457:@] and @[Thalia, Guardian of Thraben@:86938,453:@]") {
            Event::Attacks { defender, attackers, .. } => {
                assert_eq!(defender, "boryas");
                assert_eq!(attackers.len(), 2);
            }
            e => panic!("{e:?}"),
        }
        assert_eq!(parse_event("@Pboryas wins the match 2-1"), Event::WinsMatch { player: "boryas".into(), wins: 2, losses: 1 });
        assert_eq!(parse_event("Match Tied 1-1"), Event::MatchTied { a: 1, b: 1 });
        assert_eq!(parse_event("@PMatch Tied 1-1"), Event::MatchTied { a: 1, b: 1 });
        match parse_event("@PDydyk95 puts a triggered ability from Mishra's Bauble onto the stack (Draw a card.).") {
            Event::Triggers { source, source_name, .. } => {
                assert!(source.is_none());
                assert_eq!(source_name, "Mishra's Bauble");
            }
            e => panic!("{e:?}"),
        }
        assert!(matches!(parse_event("@Pboryas draws three cards with @[Brainstorm@:1,2:@]."), Event::Draws { count: 3, via: Some(_), .. }));
    }

    #[test]
    fn parses_names_with_spaces() {
        assert_eq!(parse_event("@P@Pwhiffy penguin joined the game."), Event::Joined { player: "whiffy penguin".into() });
        assert_eq!(parse_event("@PTurn 2: whiffy penguin"), Event::Turn { number: 2, player: "whiffy penguin".into() });
        assert_eq!(parse_event("@PTurn 5: whiffy penguin - Extra Turn"), Event::Turn { number: 5, player: "whiffy penguin".into() });
        assert!(matches!(parse_event("@PChosen mode: Target player draws a card."), Event::Other(_)));
        match parse_event("@Pwhiffy penguin casts @[Goblin Welder@:25122,429:@].") {
            Event::Casts { player, card, .. } => {
                assert_eq!(player, "whiffy penguin");
                assert_eq!(card.name, "Goblin Welder");
            }
            e => panic!("{e:?}"),
        }
        match parse_event("@Pboryas casts @[Thoughtseize@:100512,467:@] targeting whiffy penguin.") {
            Event::Casts { targets, .. } => assert_eq!(targets, vec![Target::Player("whiffy penguin".into())]),
            e => panic!("{e:?}"),
        }
        match parse_event("@Pa b casts @[Fireblast@:1,2:@] targeting @[Goblin Welder@:3,4:@] and whiffy penguin.") {
            Event::Casts { targets, .. } => {
                assert!(matches!(&targets[0], Target::Card(c) if c.name == "Goblin Welder"));
                assert_eq!(targets[1], Target::Player("whiffy penguin".into()));
            }
            e => panic!("{e:?}"),
        }
    }
}
