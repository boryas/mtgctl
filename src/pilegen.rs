use clap::Args;
use dialoguer::Confirm;
use diesel::prelude::*;
use rand::Rng;
use serde::Deserialize;
use skim::prelude::*;
use std::collections::HashMap;
use std::io::Cursor;

use crate::db::schema::{cards, deck_types, decks};
use crate::db::{establish_connection, models::*};

// ── Config deserialization ────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct PilegenConfig {
    #[serde(default)]
    cards: Vec<CardDef>,
}

/// One way to pay for a counterspell. Each option is tried in order; the first
/// affordable one is taken.
///
/// Components (all optional, combined additively):
///   mana_cost           — standard mana (e.g. "3UU" for FoW hard cost)
///   exile_blue_from_hand — exile another blue card from hand as pitch cost
///   life_cost           — life paid alongside (e.g. 1 for FoW alternate)
///   bounce_island       — return any blue-producing land from play to hand
///   hand_min            — minimum hand size required (inclusive of this spell)
///   prob                — probability this option is even attempted (default 1.0)
#[derive(Deserialize, Clone, Default)]
struct AlternateCost {
    #[serde(default)]
    mana_cost: String,
    #[serde(default)]
    exile_blue_from_hand: bool,
    #[serde(default)]
    life_cost: i32,
    #[serde(default)]
    bounce_island: bool,
    #[serde(default)]
    hand_min: i32,
    #[serde(default)]
    prob: Option<f64>,
}

/// An activated ability a permanent can use during its controller's turn.
///
/// Preconditions are derived automatically: ability is available iff
/// the cost can be paid and a valid target exists (if one is required).
///
/// target syntax: "<who>:<type>"
///   who  ∈ { opp, us }
///   type ∈ { nonbasic_land, land, creature, planeswalker, artifact, any }
///
/// effect ∈ { destroy, bounce, exile, fetch_land }
#[derive(Deserialize, Clone, Default)]
struct AbilityDef {
    // ── Cost ──────────────────────────────────────────────────────────────────
    /// Mana cost to activate (empty = no mana required).
    #[serde(default)]
    mana_cost: String,
    /// Whether the source is tapped as part of the cost.
    #[serde(default)]
    tap_self: bool,
    /// Whether the source is sacrificed as part of the cost.
    #[serde(default)]
    sacrifice_self: bool,
    /// Life paid as part of the cost (e.g. 1 for fetchlands).
    #[serde(default)]
    life_cost: i32,

    // ── Target (optional) ─────────────────────────────────────────────────────
    /// If set, a valid target must exist for the ability to be available,
    /// and the effect is applied to a randomly chosen valid target.
    #[serde(default)]
    target: Option<String>,

    // ── Effect ────────────────────────────────────────────────────────────────
    #[serde(default)]
    effect: String,
}

/// A card the generator knows about. Cards not in the catalog are treated as
/// generic non-land spells: hand-eligible, not permanent candidates, not exileable.
#[derive(Deserialize, Clone, Default)]
struct CardDef {
    name: String,
    /// "land" | "creature" | "planeswalker" | "artifact" | "instant" | "sorcery" | "enchantment"
    card_type: String,

    // Land-specific
    #[serde(default)]
    is_fetch: bool,
    #[serde(default)]
    enters_tapped: bool,
    #[serde(default)]
    basic_land: bool,
    #[serde(default)]
    produces_black: bool,
    #[serde(default)]
    produces_blue: bool,
    #[serde(default)]
    annotation_options: Vec<String>,

    /// Mana cost in WURBG notation, e.g. "BBB", "1U", "UB", "2B".
    /// Empty string = free (alternate-cost cards like Force of Will).
    #[serde(default)]
    mana_cost: String,

    // Creature power (for combat damage fudge)
    #[serde(default)]
    power: Option<i32>,

    // DFC: back-face properties
    #[serde(default)]
    flipped_name: Option<String>,
    #[serde(default)]
    flipped_card_type: Option<String>,
    #[serde(default)]
    flipped_loyalty: Option<i32>,

    // Planeswalker
    #[serde(default)]
    loyalty: Option<i32>,

    // Legendary rule
    #[serde(default)]
    legendary: bool,

    // Colors (for cards whose mana_cost doesn't fully express them, e.g. FoW, Daze, Snuff Out)
    #[serde(default)]
    blue: bool,
    #[serde(default)]
    black: bool,

    /// If set, this spell targets a permanent of this type when cast.
    /// Uses the same "<who>:<type>" syntax as AbilityDef.target.
    #[serde(default)]
    target: Option<String>,

    // Hand / exile rules
    #[serde(default)]
    exileable: bool,

    /// Relative likelihood of appearing as a permanent in play.
    /// Default 100. Set to 1 for cards that are almost never played out (100:1 odds).
    #[serde(default)]
    play_weight: Option<u32>,

    /// Land is usually cracked before Doomsday — filtered by stage probability.
    #[serde(default)]
    cracked_land: bool,

    /// Token creature name placed when this permanent enters (e.g. "Orc Army (1/1)").
    #[serde(default)]
    entry_token: Option<String>,
    /// Percent probability of entry token appearing (default 62).
    #[serde(default)]
    entry_token_prob: Option<u32>,

    /// Card may leave clue tokens even if no longer in play (e.g. Tamiyo).
    #[serde(default)]
    generates_clues: bool,

    /// Pre-conditions beyond mana cost (e.g. "opp_hand_nonempty", "hand_min:2").
    #[serde(default)]
    requires: Vec<String>,

    /// Post-effects beyond card-type defaults (e.g. "cantrip", "add_mana_black:2").
    #[serde(default)]
    effects: Vec<String>,

    /// Activated abilities this permanent has.
    #[serde(default)]
    abilities: Vec<AbilityDef>,

    /// If set, this card is a counterspell that can target spells of the given type.
    /// Values: "any" | "noncreature" | "nonland" | "instant_or_sorcery"
    /// Must also have `alternate_costs` defined to be usable.
    #[serde(default)]
    counter_target: Option<String>,

    /// Alternate ways to pay for this spell (used both for reactive counters and
    /// proactive alternate-cost spells like Snuff Out).
    /// Tried in order; first affordable option is taken.
    #[serde(default)]
    alternate_costs: Vec<AlternateCost>,
}


fn stage_label(turn: u8) -> &'static str {
    match turn {
        0..=3 => "Early",
        4..=5 => "Mid",
        _ => "Late",
    }
}

// ── Game state ────────────────────────────────────────────────────────────────

enum PermanentKind {
    Creature { tapped: bool },
    Planeswalker { loyalty: i32, activated: bool },
    Artifact,
}

struct Permanent {
    name: String,
    kind: PermanentKind,
}

/// An item on the spell stack: a spell or ability that has been declared and paid for
/// but not yet resolved.
///
/// Targets are chosen at cast time and stored here so resolution carries out effects
/// deterministically without needing to re-pick targets.
struct StackItem {
    name: String,
    owner: String,
    /// For counterspells: which index in the stack this spell is targeting.
    counters: Option<usize>,
    /// For spells with a permanent target (`CardDef.target`): `(target_who, target_name)`.
    /// Resolved at cast time and locked in; used directly by `apply_spell_effects`.
    permanent_target: Option<(String, String)>,
}


// ── Mana cost parsing ─────────────────────────────────────────────────────────

/// Parse a mana cost string (e.g. "BBB", "1U", "UB", "2B") into (black, blue, generic).
/// `generic` = leading number + W/R/G/C pips (can be paid with any color).
/// Empty string = free (alternate-cost cards).
fn parse_mana_cost(cost: &str) -> (i32, i32, i32) {
    let mut generic = 0i32;
    let mut black = 0i32;
    let mut blue = 0i32;
    let mut other = 0i32;
    let mut chars = cost.trim().chars().peekable();
    let mut num = String::new();
    while chars.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) {
        num.push(chars.next().unwrap());
    }
    if !num.is_empty() {
        generic = num.parse().unwrap_or(0);
    }
    for c in chars {
        match c {
            'B' => black += 1,
            'U' => blue += 1,
            'W' | 'R' | 'G' | 'C' => other += 1,
            _ => {}
        }
    }
    (black, blue, generic + other)
}

/// Total mana value (CMC) of a cost string.
fn mana_value(cost: &str) -> i32 {
    let (b, bl, g) = parse_mana_cost(cost);
    b + bl + g
}

// ── Mana pool ─────────────────────────────────────────────────────────────────

/// Mana tracking: black and blue are tracked separately (specific pip requirements);
/// total covers generic costs. Dual lands contribute to multiple colors.
#[derive(Clone, Default)]
struct ManaPool {
    black: i32,
    blue: i32,
    total: i32,
}

impl ManaPool {
    fn can_pay(&self, black: i32, blue: i32, generic: i32) -> bool {
        self.black >= black && self.blue >= blue && self.total >= black + blue + generic
    }
}

// ── Simulation types ──────────────────────────────────────────────────────────

#[derive(Clone)]
struct SimLand {
    name: String,
    tapped: bool,
    produces_black: bool,
    produces_blue: bool,
    is_fetch: bool,
    basic: bool,
}

impl SimLand {
    fn from_def(name: &str, def: &CardDef) -> Self {
        SimLand {
            name: name.to_string(),
            tapped: def.enters_tapped,
            produces_black: def.produces_black,
            produces_blue: def.produces_blue,
            is_fetch: def.is_fetch,
            basic: def.basic_land,
        }
    }
}

/// A game zone (hand, graveyard, exile). `visible` lists known card names;
/// `hidden` counts cards whose identity is unknown to both players (normally 0
/// for graveyard/exile, hand-size for hand).
struct Zone {
    visible: Vec<String>,
    hidden: i32,
}

impl Zone {
    fn new_hidden(count: i32) -> Self {
        Zone { visible: Vec::new(), hidden: count }
    }

    fn len(&self) -> i32 {
        self.visible.len() as i32 + self.hidden
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl std::fmt::Display for Zone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for card in &self.visible {
            writeln!(f, "    * {}", card)?;
        }
        if self.hidden > 0 {
            writeln!(f, "    ({} hidden card{})", self.hidden, if self.hidden == 1 { "" } else { "s" })?;
        }
        Ok(())
    }
}

struct PlayerState {
    deck_name: String,
    mulligans: u8,
    life: i32,
    library: Vec<(String, CardDef)>,
    hand: Zone,
    lands: Vec<SimLand>,
    permanents: Vec<String>,
    graveyard: Zone,
    exile: Zone,
}

impl PlayerState {
    fn new(deck: &str, mulligans: u8) -> Self {
        PlayerState {
            life: 20,
            deck_name: deck.to_string(),
            mulligans: mulligans,
            library: Vec::new(),
            hand: Zone::new_hidden((7 - mulligans as i32).max(0)),
            lands: Vec::new(),
            permanents: Vec::new(),
            graveyard: Zone { visible: Vec::new(), hidden: 0 },
            exile: Zone { visible: Vec::new(), hidden: 0 },
        }
    }

    fn mana(&self) -> ManaPool {
        let mut p = ManaPool::default();
        for l in &self.lands {
            if l.tapped || l.is_fetch {
                continue;
            }
            if l.produces_black {
                p.black += 1;
            }
            if l.produces_blue {
                p.blue += 1;
            }
            p.total += 1;
        }
        p
    }

    /// Tap lands: black sources first, then blue, then any. Fetches are never tapped for mana.
    fn tap(&mut self, black: i32, blue: i32, generic: i32) {
        let mut b = black;
        let mut u = blue;
        let mut g = generic;
        for l in &mut self.lands {
            if !l.tapped && !l.is_fetch && b > 0 && l.produces_black {
                l.tapped = true;
                b -= 1;
            }
        }
        for l in &mut self.lands {
            if !l.tapped && !l.is_fetch && u > 0 && l.produces_blue {
                l.tapped = true;
                u -= 1;
            }
        }
        for l in &mut self.lands {
            if !l.tapped && !l.is_fetch && g > 0 {
                l.tapped = true;
                g -= 1;
            }
        }
    }
}

struct SimState {
    turn: u8,
    on_play: bool,
    us: PlayerState,
    opp: PlayerState,
    log: Vec<String>,
    /// Set when Doomsday was countered and we couldn't protect it — scenario must be re-rolled.
    reroll: bool,
}

impl SimState {
    fn new(us: PlayerState, opp: PlayerState) -> Self {
        SimState {
            turn: 0,
            on_play: true,
            us: us,
            opp: opp,
            log: Vec::new(),
            reroll: false,
        }
    }

    fn player(&self, who: &str) -> &PlayerState {
        if who == "us" { &self.us } else { &self.opp }
    }

    fn player_mut(&mut self, who: &str) -> &mut PlayerState {
        if who == "us" { &mut self.us } else { &mut self.opp }
    }

    fn life_of(&self, who: &str) -> i32 {
        self.player(who).life
    }

    fn lose_life(&mut self, who: &str, n: i32) {
        self.player_mut(who).life -= n;
    }

    fn log(&mut self, t: u8, who: &str, msg: impl Into<String>) {
        let hand = self.player(who).hand.hidden;
        let suffix = if who == "us" || who == "opp" {
            format!(" [hand: {}]", hand)
        } else {
            String::new()
        };
        self.log
            .push(format!("T{} [{}] {}{}", t, who, msg.into(), suffix));
    }
}

// ── Display ───────────────────────────────────────────────────────────────────

fn sec(label: &str) -> String {
    let total = 50usize;
    let label_with_spaces = format!(" {} ", label);
    let padding = total.saturating_sub(label_with_spaces.chars().count() + 2);
    format!("  ──{}{}", label_with_spaces, "─".repeat(padding))
}

impl std::fmt::Display for SimLand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.tapped {
            write!(f, "{} (tapped)", self.name)
        } else {
            write!(f, "{}", self.name)
        }
    }
}

impl std::fmt::Display for PlayerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !self.lands.is_empty() {
            writeln!(f, "  Lands      :")?;
            for land in &self.lands {
                writeln!(f, "    * {}", land)?;
            }
        }

        if !self.permanents.is_empty() {
            writeln!(f, "  Permanents :")?;
            for p in &self.permanents {
                writeln!(f, "    * {}", p)?;
            }
        }

        if !self.hand.is_empty() {
            writeln!(f, "  Hand       :")?;
            write!(f, "{}", self.hand)?;
        }

        if !self.graveyard.is_empty() {
            writeln!(f, "  Graveyard  :")?;
            write!(f, "{}", self.graveyard)?;
        }

        if !self.exile.is_empty() {
            writeln!(f, "  Exile      :")?;
            write!(f, "{}", self.exile)?;
        }

        Ok(())
    }
}

impl std::fmt::Display for SimState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let dbar = "═".repeat(50);
        writeln!(f)?;
        writeln!(f, "  ╔{}╗", dbar)?;
        writeln!(f, "  ║{:^50}║", " DOOMSDAY PILE SCENARIO ")?;
        writeln!(f, "  ╚{}╝", dbar)?;
        writeln!(f)?;
        writeln!(f, "  Deck    : {}", self.us.deck_name)?;
        writeln!(f, "  Opponent: {}", self.opp.deck_name)?;
        writeln!(
            f,
            "  Turn    : {} ({}, {})",
            self.turn,
            stage_label(self.turn),
            if self.on_play { "on the play" } else { "on the draw" }
        )?;

        if !self.log.is_empty() {
            writeln!(f)?;
            writeln!(f, "{}", sec("TURN LOG"))?;
            writeln!(f)?;
            for entry in &self.log {
                writeln!(f, "  {}", entry)?;
            }
        }

        writeln!(f)?;
        writeln!(f, "{}", sec("MY BOARD"))?;
        writeln!(f)?;
        writeln!(f, "  Life       : {} -> {}", self.us.life, self.us.life / 2)?;
        write!(f, "{}", self.us)?;
        writeln!(f)?;

        let opp_label = format!("OPPONENT: {}", self.opp.deck_name);
        writeln!(f, "{}", sec(&opp_label))?;
        writeln!(f)?;
        writeln!(f, "  Life       : {}", self.opp.life)?;
        write!(f, "{}", self.opp)?;

        Ok(())
    }
}

// ── Clap args ─────────────────────────────────────────────────────────────────

#[derive(Args)]
pub struct PilegenArgs {}

// ── Public entry point ────────────────────────────────────────────────────────

pub fn run(_args: PilegenArgs) {
    let config = load_config();

    let (deck_name, _) =
        match select_deck("Select your Doomsday deck", Some("doomsday")) {
            Some(d) => d,
            None => return,
        };

    let (opp_deck_name, opp_display) =
        match select_deck("Select opponent deck", None) {
            Some(o) => o,
            None => return,
        };

    let all_cards = load_deck_cards(&deck_name);
    let opp_cards = load_deck_cards(&opp_deck_name);

    loop {
        let state = generate_scenario(&deck_name, &opp_display, &config, &all_cards, &opp_cards);
        println!("{}", state);

        println!();
        if !Confirm::new()
            .with_prompt("Generate another scenario?")
            .default(true)
            .interact()
            .unwrap_or(false)
        {
            break;
        }
    }
}

// ── Config loading ────────────────────────────────────────────────────────────

fn load_config() -> PilegenConfig {
    let path = "definitions/pilegen.toml";
    match std::fs::read_to_string(path) {
        Ok(content) => toml::from_str(&content).unwrap_or_else(|e| {
            eprintln!("Warning: could not parse {}: {}", path, e);
            PilegenConfig::default()
        }),
        Err(_) => {
            eprintln!("Warning: {} not found, using defaults", path);
            PilegenConfig::default()
        }
    }
}

// ── Selection helpers ─────────────────────────────────────────────────────────

fn fuzzy_select(prompt: &str, options: &[String]) -> Option<String> {
    if options.is_empty() {
        return None;
    }
    let prompt_str = format!("{}: ", prompt);
    let skim_options = SkimOptionsBuilder::default()
        .prompt(Some(&prompt_str))
        .build()
        .unwrap();
    let input = options.join("\n");
    let item_reader = SkimItemReader::default();
    let items = item_reader.of_bufread(Cursor::new(input));
    match Skim::run_with(&skim_options, Some(items)) {
        Some(output) if !output.is_abort => {
            if output.selected_items.is_empty() {
                let q = output.query.trim().to_string();
                if q.is_empty() {
                    None
                } else {
                    Some(q)
                }
            } else {
                Some(output.selected_items[0].output().to_string())
            }
        }
        _ => None,
    }
}

/// Select a deck with imported cards. If `flow_type_filter` is Some, restrict to that flow type.
/// Returns `(list_name, display_name)`.
fn select_deck(prompt: &str, flow_type_filter: Option<&str>) -> Option<(String, String)> {
    let conn = &mut establish_connection();

    // Load all deck types (optionally filtered) to get their IDs.
    let type_ids: Vec<i32> = if let Some(flow) = flow_type_filter {
        deck_types::table
            .filter(deck_types::flow_type.eq(flow))
            .select(deck_types::type_id)
            .load(conn)
            .unwrap_or_default()
    } else {
        deck_types::table
            .select(deck_types::type_id)
            .load(conn)
            .unwrap_or_default()
    };

    let all_decks: Vec<Deck> = decks::table
        .filter(decks::type_id.eq_any(&type_ids))
        .load(conn)
        .unwrap_or_default();

    // Load all deck types for display name lookup.
    let all_types: Vec<DeckType> = deck_types::table.load(conn).unwrap_or_default();
    let type_map: HashMap<i32, &DeckType> = all_types.iter().map(|dt| (dt.type_id, dt)).collect();

    // Keep only decks that have at least one card imported; build display names.
    let card_counts: HashMap<i32, i64> = {
        let ids: Vec<i32> = all_decks.iter().map(|d| d.deck_id).collect();
        cards::table
            .filter(cards::deck_id.eq_any(&ids))
            .select(cards::deck_id)
            .load::<i32>(conn)
            .unwrap_or_default()
            .into_iter()
            .fold(HashMap::new(), |mut m, id| {
                *m.entry(id).or_insert(0) += 1;
                m
            })
    };

    let candidates: Vec<(String, String)> = all_decks
        .into_iter()
        .filter(|d| card_counts.get(&d.deck_id).copied().unwrap_or(0) > 0)
        .map(|d| {
            let display = d
                .type_id
                .and_then(|tid| type_map.get(&tid))
                .map(|dt| format!("{} ({})", dt.display(), d.list_name))
                .unwrap_or_else(|| d.list_name.clone());
            (d.list_name, display)
        })
        .collect();

    if candidates.is_empty() {
        println!("No decks with imported cards found. Run: mtgctl deck import");
        return None;
    }

    if candidates.len() == 1 {
        println!("Using deck: {}", candidates[0].1);
        return Some(candidates[0].clone());
    }

    let display_names: Vec<String> = candidates.iter().map(|(_, d)| d.clone()).collect();
    fuzzy_select(prompt, &display_names).and_then(|chosen| {
        candidates.into_iter().find(|(_, d)| d == &chosen)
    })
}

// ── Deck card loading ─────────────────────────────────────────────────────────

// ── Turn simulation ───────────────────────────────────────────────────────────

/// Play a land from the pool (without replacement — the entry is removed).
/// Fetches stay in play to be cracked later in the ability pass.
fn sim_play_land(
    state: &mut SimState,
    t: u8,
    who: &str,
    library: &mut Vec<(String, CardDef)>,
    fateful: bool, // true = Doomsday turn; avoid mana-neutral lands (Wasteland)
    rng: &mut impl Rng,
) {
    let has_black = state
        .player(who)
        .lands
        .iter()
        .any(|l| !l.is_fetch && l.produces_black);

    let weighted: Vec<(usize, u32)> = library
        .iter()
        .enumerate()
        .filter_map(|(i, (_, def))| {
            if def.card_type != "land" {
                return None;
            }
            if fateful && def.cracked_land {
                return None;
            }
            let w = if !has_black && def.produces_black {
                3u32
            } else {
                1u32
            };
            Some((i, w))
        })
        .collect();
    if weighted.is_empty() {
        return;
    }
    let idx = weighted_choice(&weighted, rng);
    let land = {
        let (name, def) = &library[idx];
        SimLand::from_def(name, def)
    };
    let name = library[idx].0.clone();
    state.player_mut(who).hand.hidden -= 1;
    state.player_mut(who).lands.push(land);
    state.log(t, who, format!("Play {}", name));
    library.remove(idx);
}


/// Discard down to 7 at end of turn.
fn sim_discard_to_limit(state: &mut SimState, t: u8, who: &str) {
    let hand = state.player(who).hand.hidden;
    if hand > 7 {
        let n = hand - 7;
        state.player_mut(who).hand.hidden = 7;
        state.log(t, who, format!("Discard {} to hand limit", n));
    }
}

// ── Action system ─────────────────────────────────────────────────────────────

/// Resolve `"<who>"` relative to the acting player.
fn resolve_who<'a>(who_rel: &str, actor: &'a str) -> &'a str {
    if who_rel == "opp" {
        if actor == "us" {
            "opp"
        } else {
            "us"
        }
    } else {
        actor
    }
}

/// Check whether `type_str` matches a permanent. `def` is the target card's definition,
/// required for MV and color checks (may be None for lands or unknown cards).
fn matches_target_type(
    type_str: &str,
    card_type: &str,
    basic: bool,
    def: Option<&CardDef>,
) -> bool {
    match type_str {
        "nonbasic_land" => card_type == "land" && !basic,
        "land" => card_type == "land",
        "creature" => card_type == "creature",
        "planeswalker" => card_type == "planeswalker",
        "artifact" => card_type == "artifact",
        "any" => true,
        "creature_mv_lt4" => {
            card_type == "creature" && def.map(|d| mana_value(&d.mana_cost) < 4).unwrap_or(true)
        }
        "creature_nonblack" => {
            card_type == "creature"
                && def
                    .map(|d| !d.black && !d.mana_cost.contains('B'))
                    .unwrap_or(true)
        }
        _ => false,
    }
}

/// Return true if at least one valid target exists for `target_str`.
fn has_valid_target(
    target_str: &str,
    state: &SimState,
    actor: &str,
    catalog_map: &HashMap<&str, &CardDef>,
) -> bool {
    let (who_rel, type_str) = match target_str.split_once(':') {
        Some(pair) => pair,
        None => return false,
    };
    let target_who = resolve_who(who_rel, actor);
    let player = state.player(target_who);
    player
        .lands
        .iter()
        .any(|l| matches_target_type(type_str, "land", l.basic, None))
        || player.permanents.iter().any(|p| {
            let def = catalog_map.get(p.as_str()).copied();
            let ct = def.map(|d| d.card_type.as_str()).unwrap_or("");
            matches_target_type(type_str, ct, false, def)
        })
}

/// Check whether an ability can be activated (cost payable + valid target exists).
/// `source_untapped` must be true when the source is an untapped land/permanent.
fn ability_available(
    ability: &AbilityDef,
    state: &SimState,
    who: &str,
    source_untapped: bool,
    catalog_map: &HashMap<&str, &CardDef>,
) -> bool {
    if ability.tap_self && !source_untapped {
        return false;
    }
    if !ability.mana_cost.is_empty() {
        let (b, bl, g) = parse_mana_cost(&ability.mana_cost);
        if !state.player(who).mana().can_pay(b, bl, g) {
            return false;
        }
    }
    if let Some(tgt) = &ability.target {
        if !has_valid_target(tgt, state, who, catalog_map) {
            return false;
        }
    }
    true
}

/// Collect spells from hand that can be cast this turn (cantrips / permanents / discard).
/// Abilities are handled separately in the ability pass.
fn collect_spells(
    state: &SimState,
    who: &str,
    library: &[(String, CardDef)],
    catalog_map: &HashMap<&str, &CardDef>,
) -> Vec<String> {
    let permanents_in_play = &state.player(who).permanents;
    let opp_who = if who == "us" { "opp" } else { "us" };
    library
        .iter()
        .filter_map(|(name, def)| {
            if def.card_type == "land" {
                return None;
            }
            let castable = def.effects.iter().any(|e| {
                e == "cantrip" || e == "permanent" || e == "destroy" || e.starts_with("discard:")
            });
            if !castable {
                return None;
            }
            if def.legendary && permanents_in_play.iter().any(|p| p == name.as_str()) {
                return None;
            }
            // Targeted spells need a valid target.
            if let Some(tgt) = &def.target {
                if !has_valid_target(tgt, state, who, catalog_map) {
                    return None;
                }
            }
            // Check requires conditions.
            let ok = def.requires.iter().all(|req| match req.as_str() {
                "opp_hand_nonempty" => state.player(opp_who).hand.hidden > 0,
                _ => true,
            });
            if !ok {
                return None;
            }
            Some(name.to_string())
        })
        .collect()
}

/// Pick a random valid permanent target for `target_str` (e.g. "opp:creature_mv_lt4").
/// Returns `(target_who, target_name)` or `None` if no valid target exists.
fn choose_permanent_target(
    target_str: &str,
    actor: &str,
    state: &SimState,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> Option<(String, String)> {
    let (who_rel, type_str) = target_str.split_once(':')?;
    let target_who = resolve_who(who_rel, actor).to_string();

    let mut candidates: Vec<String> = Vec::new();
    for land in &state.player(&target_who).lands {
        if matches_target_type(type_str, "land", land.basic, None) {
            candidates.push(land.name.clone());
        }
    }
    for perm in &state.player(&target_who).permanents {
        let def = catalog_map.get(perm.as_str()).copied();
        let ct = def.map(|d| d.card_type.as_str()).unwrap_or("");
        if matches_target_type(type_str, ct, false, def) {
            candidates.push(perm.clone());
        }
    }
    if candidates.is_empty() {
        return None;
    }
    let name = candidates.remove(rng.gen_range(0..candidates.len()));
    Some((target_who, name))
}

/// Apply a targeted effect (destroy / exile) to a specific named permanent.
/// `target_who` must be "us" or "opp" (global). Used during resolution.
fn apply_effect_to(
    effect: &str,
    target_who: &str,
    target_name: &str,
    state: &mut SimState,
    t: u8,
    log_who: &str,
) {
    let is_land = state.player(target_who).lands.iter().any(|l| l.name == target_name);
    if is_land {
        state.player_mut(target_who).lands.retain(|l| l.name != target_name);
    } else {
        state.player_mut(target_who).permanents.retain(|p| p != target_name);
    }
    match effect {
        "exile" => state.player_mut(target_who).exile.visible.push(target_name.to_string()),
        _ => state.player_mut(target_who).graveyard.visible.push(target_name.to_string()),
    }
    state.log(t, log_who, format!("{} {} ({})", effect, target_name, target_who));
}

/// Apply a targeted effect to a randomly chosen valid target.
/// Used for activated abilities (Wasteland, etc.) where the target is chosen at activation time.
fn sim_apply_targeted_effect(
    effect: &str,
    target_str: &str,
    state: &mut SimState,
    t: u8,
    log_who: &str,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) {
    if let Some((target_who, target_name)) =
        choose_permanent_target(target_str, log_who, state, catalog_map, rng)
    {
        apply_effect_to(effect, &target_who, &target_name, state, t, log_who);
    }
}

/// Match a search filter string against a card definition.
/// Filter syntax: `"land"`, `"land-u"`, `"land-b"`, `"land-ub"`.
fn matches_search_filter(filter: &str, def: &CardDef) -> bool {
    match filter {
        "land"    => def.card_type == "land",
        "land-u"  => def.card_type == "land" && def.produces_blue,
        "land-b"  => def.card_type == "land" && def.produces_black,
        "land-ub" => def.card_type == "land" && (def.produces_blue || def.produces_black),
        _         => false,
    }
}

/// Execute an activated ability: apply effect, then pay the cost.
fn sim_activate_ability(
    state: &mut SimState,
    t: u8,
    who: &str,
    source_name: &str,
    ability: &AbilityDef,
    land_pool: &mut Vec<(String, CardDef)>,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) {
    // search:*:* — generic library search (e.g. "search:land-ub:play").
    // Format: "search:<filter>:<dest>" where dest is "play" or "hand".
    if ability.effect.starts_with("search:") {
        let mut parts = ability.effect.splitn(3, ':');
        parts.next(); // "search"
        let filter = parts.next().unwrap_or("");
        let dest   = parts.next().unwrap_or("play");

        // Pay costs.
        if ability.sacrifice_self {
            state.player_mut(who).lands.retain(|l| l.name != source_name);
            state.player_mut(who).graveyard.visible.push(source_name.to_string());
        }
        if ability.life_cost > 0 {
            state.lose_life(who, ability.life_cost);
        }

        // Find all matching cards in the library and pick one at random.
        let candidates: Vec<usize> = land_pool
            .iter()
            .enumerate()
            .filter(|(_, (_, d))| matches_search_filter(filter, d))
            .map(|(i, _)| i)
            .collect();

        if !candidates.is_empty() {
            let idx = candidates[rng.gen_range(0..candidates.len())];
            let land = {
                let (name, def) = &land_pool[idx];
                SimLand {
                    name: name.clone(),
                    tapped: false,
                    is_fetch: false,
                    basic: def.basic_land,
                    produces_black: def.produces_black,
                    produces_blue: def.produces_blue,
                }
            };
            let name = land_pool[idx].0.clone();
            land_pool.remove(idx);
            match dest {
                "play" => {
                    state.player_mut(who).lands.push(land);
                    state.log(t, who, format!("Crack {} → {}", source_name, name));
                }
                "hand" => {
                    state.player_mut(who).hand.hidden += 1;
                    state.log(t, who, format!("Search {} → {} (to hand)", source_name, name));
                }
                _ => {}
            }
        }
        return;
    }

    state.log(t, who, format!("Activate {}", source_name));

    // Apply targeted effect
    if !ability.effect.is_empty() {
        if let Some(target_str) = &ability.target {
            sim_apply_targeted_effect(&ability.effect, target_str, state, t, who, catalog_map, rng);
        }
    }

    // Pay mana cost
    if !ability.mana_cost.is_empty() {
        let (b, bl, g) = parse_mana_cost(&ability.mana_cost);
        state.player_mut(who).tap(b, bl, g);
    }

    // Pay life cost
    if ability.life_cost > 0 {
        state.lose_life(who, ability.life_cost);
    }

    // Pay tap cost
    if ability.tap_self && !ability.sacrifice_self {
        if let Some(l) = state
            .player_mut(who)
            .lands
            .iter_mut()
            .find(|l| l.name == source_name)
        {
            l.tapped = true;
        }
    }

    // Pay sacrifice cost
    if ability.sacrifice_self {
        let is_land = catalog_map
            .get(source_name)
            .map(|d| d.card_type == "land")
            .unwrap_or(false);
        if is_land {
            state
                .player_mut(who)
                .lands
                .retain(|l| l.name != source_name);
        } else {
            state
                .player_mut(who)
                .permanents
                .retain(|p| p != source_name);
        }
        state
            .player_mut(who)
            .graveyard
            .visible
            .push(source_name.to_string());
    }
}

/// Return true if a card is blue (U pip in mana_cost or explicit `blue` flag).
fn is_blue(def: &CardDef) -> bool {
    def.mana_cost.contains('U') || def.blue
}

/// Return true if `spell_type` is a valid target for a counterspell with `counter_target`.
fn matches_counter_target(counter_target: &str, spell_type: &str) -> bool {
    match counter_target {
        "any" => true,
        "noncreature" => spell_type != "creature",
        "nonland" => spell_type != "land",
        "instant_or_sorcery" => spell_type == "instant" || spell_type == "sorcery",
        _ => false,
    }
}

/// Check whether `cost` can be paid by `who` given current state.
/// `source_name` is the counterspell card name (excluded from blue pitch candidates).
fn can_pay_alternate_cost(
    cost: &AlternateCost,
    state: &SimState,
    who: &str,
    source_name: &str,
    library: &[(String, CardDef)],
) -> bool {
    let player = state.player(who);
    if player.hand.hidden < cost.hand_min {
        return false;
    }
    if !cost.mana_cost.is_empty() {
        let (b, bl, g) = parse_mana_cost(&cost.mana_cost);
        if !player.mana().can_pay(b, bl, g) {
            return false;
        }
    }
    if cost.exile_blue_from_hand {
        let has_pitch = library
            .iter()
            .any(|(n, d)| n.as_str() != source_name && d.card_type != "land" && is_blue(d));
        if !has_pitch {
            return false;
        }
    }
    if cost.bounce_island {
        if !player.lands.iter().any(|l| !l.is_fetch && l.produces_blue) {
            return false;
        }
    }
    true
}

/// Pay the component parts of `cost` (life, mana, exile, bounce). Returns description parts.
/// Does NOT handle the spell card itself leaving hand — that is the caller's responsibility.
fn apply_alt_cost_components(
    cost: &AlternateCost,
    state: &mut SimState,
    who: &str,
    source_name: &str,
    library: &mut Vec<(String, CardDef)>,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    if cost.exile_blue_from_hand {
        let pitch_indices: Vec<usize> = library
            .iter()
            .enumerate()
            .filter(|(_, (n, d))| n.as_str() != source_name && d.card_type != "land" && is_blue(d))
            .map(|(i, _)| i)
            .collect();
        let idx = pitch_indices[rng.gen_range(0..pitch_indices.len())];
        let pitch_name = library[idx].0.clone();
        library.remove(idx);
        state.player_mut(who).hand.hidden -= 1;
        state.player_mut(who).exile.visible.push(pitch_name.clone());
        parts.push(format!("exile {}", pitch_name));
    }
    if cost.bounce_island {
        let idx = state
            .player(who)
            .lands
            .iter()
            .position(|l| !l.is_fetch && l.produces_blue)
            .unwrap();
        let land_name = state.player(who).lands[idx].name.clone();
        state.player_mut(who).lands.remove(idx);
        state.player_mut(who).hand.hidden += 1;
        parts.push(format!("bounce {}", land_name));
    }
    if !cost.mana_cost.is_empty() {
        let (b, bl, g) = parse_mana_cost(&cost.mana_cost);
        state.player_mut(who).tap(b, bl, g);
        parts.push(cost.mana_cost.clone());
    }
    if cost.life_cost > 0 {
        state.lose_life(who, cost.life_cost);
        parts.push(format!("-{} life", cost.life_cost));
    }
    parts
}

/// Cast a spell: pay its cost, choose any permanent target, remove from library, log,
/// and return a `StackItem` ready to be placed on the stack.
///
/// Cost selection: if `preferred_cost` is `Some`, that specific alternate cost is used
/// (caller already verified it's payable, e.g. `respond_with_counter` after prob checks).
/// Otherwise the standard mana cost is tried first; if unpayable (or mana_cost is empty
/// and the card has alternate costs), the first payable alternate cost is used instead.
///
/// Permanent targets (from `CardDef.target`) are chosen randomly at cast time and
/// locked into the `StackItem`; resolution uses the stored target directly.
///
/// Returns `None` if the spell can't be cast (cost unpayable or card not in library).
fn cast_spell(
    state: &mut SimState,
    t: u8,
    who: &str,
    name: &str,
    library: &mut Vec<(String, CardDef)>,
    preferred_cost: Option<&AlternateCost>,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> Option<StackItem> {
    let def = catalog_map.get(name)?;
    let (b, bl, g) = parse_mana_cost(&def.mana_cost);

    // A spell with an empty mana_cost string and alternate costs is "alt-cost only":
    // it can't be cast for free, it must use one of its alternate costs.
    let has_alt_costs = !def.alternate_costs.is_empty();
    let mana_is_usable = !def.mana_cost.is_empty() && state.player(who).mana().can_pay(b, bl, g);

    // Select cost.
    let alt_cost: Option<AlternateCost> = if let Some(pc) = preferred_cost {
        // Caller specified the exact cost to use.
        Some(pc.clone())
    } else if !mana_is_usable {
        // Can't pay mana (or mana_cost is empty / alt-cost-only): try alternate costs.
        def.alternate_costs
            .iter()
            .find(|c| can_pay_alternate_cost(c, state, who, name, library))
            .cloned()
    } else if has_alt_costs {
        // Mana is payable but there are also alternate costs — prefer mana by default.
        None
    } else {
        None
    };

    if alt_cost.is_none() && !mana_is_usable {
        return None; // no payable cost
    }

    // Choose permanent target (if the spell has one) before paying cost.
    let permanent_target = def.target.as_deref()
        .and_then(|tgt| choose_permanent_target(tgt, who, state, catalog_map, rng));

    // Remove the spell from library.
    let pos = library.iter().position(|(n, _)| n.as_str() == name)?;
    library.remove(pos);

    // Pay cost and build a log label.
    let cast_label = if let Some(ref cost) = alt_cost {
        let parts = apply_alt_cost_components(cost, state, who, name, library, catalog_map, rng);
        state.player_mut(who).hand.hidden -= 1;
        parts.join(", ")
    } else {
        state.player_mut(who).tap(b, bl, g);
        state.player_mut(who).hand.hidden -= 1;
        def.mana_cost.clone()
    };

    state.log(t, who, format!("Cast {} ({})", name, cast_label));

    Some(StackItem {
        name: name.to_string(),
        owner: who.to_string(),
        counters: None,
        permanent_target,
    })
}

/// Apply the resolution effects of a non-counter spell: puts it into play or graveyard,
/// and handles cantrip draw, targeted destroy, discard, and life-loss effects.
///
/// `actor_lib` is the caster's library; `other_lib` is the opponent's library.
/// In our model, non-counter spells are always cast by the active player, so
/// actor_lib/other_lib are actor-relative.
fn apply_spell_effects(
    item: &StackItem,
    state: &mut SimState,
    t: u8,
    _actor_lib: &mut Vec<(String, CardDef)>,
    other_lib: &mut Vec<(String, CardDef)>,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) {
    let Some(def) = catalog_map.get(item.name.as_str()) else {
        state.player_mut(&item.owner).graveyard.visible.push(item.name.clone());
        return;
    };

    let is_permanent = def.effects.iter().any(|e| e == "permanent");
    let is_cantrip = def.effects.iter().any(|e| e == "cantrip");

    // Destination: permanents or graveyard.
    if is_permanent {
        state.player_mut(&item.owner).permanents.push(item.name.clone());
    } else {
        state.player_mut(&item.owner).graveyard.visible.push(item.name.clone());
    }

    // Apply all effects and collect secondary log lines, before logging resolution.
    let mut secondary_logs: Vec<String> = Vec::new();
    for effect in &def.effects {
        let parts: Vec<&str> = effect.splitn(3, ':').collect();
        match parts.as_slice() {
            [e] if *e == "cantrip" => {
                state.player_mut(&item.owner).hand.hidden += 1;
            }
            ["discard", who_rel, n_str] => {
                let n: i32 = n_str.parse().unwrap_or(0);
                let target_who = resolve_who(who_rel, &item.owner).to_string();
                let current = state.player(&target_who).hand.hidden;
                let actual = n.min(current);
                if actual > 0 {
                    let mut discarded: Vec<String> = Vec::new();
                    for _ in 0..actual {
                        if !other_lib.is_empty() {
                            let idx = rng.gen_range(0..other_lib.len());
                            let (card, _) = other_lib.remove(idx);
                            state.player_mut(&target_who).hand.hidden -= 1;
                            state.player_mut(&target_who).graveyard.visible.push(card.clone());
                            discarded.push(card);
                        }
                    }
                    secondary_logs.push(format!("→ {} discards: {}", target_who, discarded.join(", ")));
                }
            }
            ["life_loss", n_str] => {
                let n: i32 = n_str.parse().unwrap_or(0);
                state.lose_life(&item.owner, n);
                secondary_logs.push(format!("→ lose {} life (now {})", n, state.life_of(&item.owner)));
            }
            _ => {}
        }
    }

    // Targeted destroy effect: applied before log so resolution line reflects final state.
    if def.effects.iter().any(|e| e == "destroy") {
        if let Some((ref target_who, ref target_name)) = item.permanent_target {
            apply_effect_to("destroy", target_who, target_name, state, t, &item.owner);
        }
    }

    // Log resolution (hand count now reflects cantrip draw; life reflects life_loss).
    let resolve_label = if is_permanent {
        "enters play"
    } else if is_cantrip {
        "resolves (draw)"
    } else {
        "resolves"
    };
    state.log(t, &item.owner, format!("{} {}", item.name, resolve_label));

    // Secondary effect detail logs.
    for msg in secondary_logs {
        let owner = item.owner.clone();
        state.log(t, &owner, msg);
    }
}

/// Resolve the stack from top (last) to bottom (first).
///
/// A counterspell (item.counters = Some(idx)) fizzles its target; a fizzled item goes
/// to graveyard without applying effects. Returns a mask: `true` = item was fizzled.
///
/// `actor_lib` / `other_lib` are relative to the active player (who started the spell cascade).
fn resolve_stack(
    stack: &[StackItem],
    state: &mut SimState,
    t: u8,
    actor_lib: &mut Vec<(String, CardDef)>,
    other_lib: &mut Vec<(String, CardDef)>,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> Vec<bool> {
    let mut fizzled = vec![false; stack.len()];

    // Process top-to-bottom (last element first = LIFO resolution).
    for i in (0..stack.len()).rev() {
        let item = &stack[i];

        if fizzled[i] {
            // Countered before it could resolve: goes to graveyard with no effect.
            state.player_mut(&item.owner).graveyard.visible.push(item.name.clone());
            continue;
        }

        if let Some(target_idx) = item.counters {
            // Counterspell resolves: fizzle the target if it's still live.
            if !fizzled[target_idx] {
                fizzled[target_idx] = true;
                state.log(t, &item.owner,
                    format!("{} counters {}", item.name, stack[target_idx].name));
            }
            // Counterspell itself goes to graveyard.
            state.player_mut(&item.owner).graveyard.visible.push(item.name.clone());
        } else {
            // Normal spell resolves.
            apply_spell_effects(item, state, t, actor_lib, other_lib, catalog_map, rng);
        }
    }

    fizzled
}

/// Try to respond to `stack[target_idx]` by casting a counterspell.
///
/// When `probabilistic = true` a 35% base check is applied and per-cost `prob` rolls are
/// honoured (used for the opponent's optional counter decisions).
/// When `probabilistic = false` the attempt is deterministic — all payable options are
/// tried in order (used when we must protect Doomsday).
///
/// On success, returns a StackItem with `counters = Some(target_idx)` and the
/// responding player's costs already paid. Returns None if no counter is possible.
fn respond_with_counter(
    state: &mut SimState,
    t: u8,
    stack: &[StackItem],
    target_idx: usize,
    responding_who: &str,
    responding_library: &mut Vec<(String, CardDef)>,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
    probabilistic: bool,
) -> Option<StackItem> {
    let target_type = catalog_map
        .get(stack[target_idx].name.as_str())
        .map(|d| d.card_type.as_str())
        .unwrap_or("sorcery");

    let counterspells: Vec<String> = responding_library
        .iter()
        .filter(|(_, d)| {
            d.counter_target
                .as_deref()
                .is_some_and(|ct| matches_counter_target(ct, target_type))
                && !d.alternate_costs.is_empty()
        })
        .map(|(n, _)| n.to_string())
        .collect();

    if counterspells.is_empty() {
        return None;
    }

    // Probabilistic: 35% base chance the opponent even tries.
    if probabilistic && !rng.gen_bool(0.35) {
        return None;
    }

    for cs_name in &counterspells {
        let costs = catalog_map[cs_name.as_str()].alternate_costs.clone();
        for cost in &costs {
            if probabilistic {
                if let Some(p) = cost.prob {
                    if !rng.gen_bool(p) {
                        continue;
                    }
                }
            }
            if can_pay_alternate_cost(cost, state, responding_who, cs_name, responding_library) {
                let cost = cost.clone();
                if let Some(mut item) = cast_spell(
                    state, t, responding_who, cs_name,
                    responding_library, Some(&cost), catalog_map, rng,
                ) {
                    item.counters = Some(target_idx);
                    return Some(item);
                }
            }
        }
    }
    None
}


/// Activate all available abilities for `who` (fetches, Wasteland, etc.).
/// Snapshot then walk — each source appears at most once so sacrifice is safe.
/// Each ability is independently rolled at 75%.
fn sim_activate_abilities_for_turn(
    state: &mut SimState,
    t: u8,
    who: &str,
    library: &mut Vec<(String, CardDef)>,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) {
    let mut available: Vec<(String, usize)> = Vec::new();
    for perm in state.player(who).permanents.clone() {
        if let Some(def) = catalog_map.get(perm.as_str()) {
            for (idx, ab) in def.abilities.iter().enumerate() {
                if ability_available(ab, state, who, true, catalog_map) {
                    available.push((perm.clone(), idx));
                }
            }
        }
    }
    for land in state.player(who).lands.clone() {
        if land.tapped {
            continue;
        }
        if let Some(def) = catalog_map.get(land.name.as_str()) {
            for (idx, ab) in def.abilities.iter().enumerate() {
                if ability_available(ab, state, who, true, catalog_map) {
                    available.push((land.name.clone(), idx));
                }
            }
        }
    }
    for (source, ability_idx) in available {
        let Some(ab) = catalog_map
            .get(source.as_str())
            .and_then(|d| d.abilities.get(ability_idx))
            .cloned()
        else {
            continue;
        };
        if !ability_available(&ab, state, who, true, catalog_map) {
            continue;
        }
        if rng.gen_bool(0.75) {
            sim_activate_ability(state, t, who, &source, &ab, library, catalog_map, rng);
        }
    }
}

/// Take actions during a player's main phase:
///   1. Ability pass — fetches cracked, Wasteland activated, etc. (75% each).
///   2. Spell cascade — up to 3 spells cast with stack/priority:
///        a. Active player casts a spell (cost paid, goes on stack).
///        b. Opponent gets priority and may cast a counterspell (probabilistic).
///        c. Stack resolves LIFO; a counter fizzles its target.
///        d. If the active spell was countered, the cascade ends.
///
/// `library` is the acting player's library; `opp_library` is the other player's.
fn sim_cast_spells_for_turn(
    state: &mut SimState,
    t: u8,
    who: &str,
    library: &mut Vec<(String, CardDef)>,
    opp_library: &mut Vec<(String, CardDef)>,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) {
    let opp_who = if who == "us" { "opp" } else { "us" };

    // ── Ability pass (before spells so fetches are cracked first) ─────────────
    sim_activate_abilities_for_turn(state, t, who, library, catalog_map, rng);

    // ── Spell cascade ─────────────────────────────────────────────────────────
    for &continue_prob in &[0.90f64, 0.30, 0.10] {
        if !rng.gen_bool(continue_prob) {
            break;
        }

        let spells = collect_spells(state, who, library, catalog_map);
        if spells.is_empty() {
            break;
        }
        let name = spells[rng.gen_range(0..spells.len())].clone();

        // Cast the spell: pay costs, remove from library, put on stack.
        let Some(spell_item) = cast_spell(state, t, who, &name, library, None, catalog_map, rng)
        else {
            continue;
        };
        let mut stack = vec![spell_item];

        // Opponent gets priority and may respond with a counterspell.
        if let Some(counter) =
            respond_with_counter(state, t, &stack, 0, opp_who, opp_library, catalog_map, rng, true)
        {
            stack.push(counter);
            // Active player could also respond here; for non-DD spells we don't model that.
        }

        // Resolve the stack LIFO and check if the active spell was countered.
        let fizzled = resolve_stack(&stack, state, t, library, opp_library, catalog_map, rng);
        if fizzled[0] {
            break; // spell was countered — counter disrupts the cascade
        }
    }
}

/// Simulate one player's turn. Both players use the same logic; the only special
/// case is when `who == "us"` on the Doomsday turn (`t == dd_turn`).
fn sim_turn(
    state: &mut SimState,
    t: u8,
    who: &str,
    dd_turn: u8,
    on_play: bool,
    library: &mut Vec<(String, CardDef)>,
    opp_library: &mut Vec<(String, CardDef)>,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) {
    // Untap. Clean up ritual mana sentinels for us.
    for l in &mut state.player_mut(who).lands {
        l.tapped = false;
    }
    if who == "us" {
        state.us.lands.retain(|l| l.name != "Ritual mana");
    }

    // Draw — skip on turn 1 for whichever player is on the play.
    let this_player_on_play = if who == "us" { on_play } else { !on_play };
    if this_player_on_play && t == 1 {
        state.log(t, who, "No draw (on the play)");
    } else {
        state.player_mut(who).hand.hidden += 1;
        state.log(t, who, "Draw");
    }

    // Land drop.
    let has_lands = library.iter().any(|(_, d)| d.card_type == "land");
    // TODO decay from 100% -> 20% over 3-4 turns (100, 75, %lands-in-deck)?
    if has_lands && rng.gen_bool(0.85) {
        let fateful = who == "us" && t == dd_turn;
        sim_play_land(state, t, who, library, fateful, rng);
    }

    // Main phase.
    if who == "us" && t == dd_turn {
        // Ability pass first so fetches are cracked before casting spells.
        sim_activate_abilities_for_turn(state, t, who, library, catalog_map, rng);

        match sim_cast_doomsday(state, t) {
            None => {
                state.log(t, "us", "⚠ Could not find Doomsday payment path");
            }
            Some(dd_item) => {
                let mut stack = vec![dd_item];

                // Opponent gets priority — may try to counter.
                if let Some(opp_counter) = respond_with_counter(
                    state, t, &stack, 0, "opp", opp_library, catalog_map, rng, true,
                ) {
                    stack.push(opp_counter);

                    // We get priority back — try to protect Doomsday deterministically.
                    if let Some(our_counter) = respond_with_counter(
                        state, t, &stack, 1, "us", library, catalog_map, rng, false,
                    ) {
                        stack.push(our_counter);
                    } else {
                        // Couldn't protect — mark reroll before resolving.
                        state.log(t, "us", "⚠ Doomsday countered — could not protect");
                        state.reroll = true;
                    }
                }

                // Resolve the stack. Doomsday goes to graveyard here.
                resolve_stack(&stack, state, t, library, opp_library, catalog_map, rng);
            }
        }
    } else {
        sim_cast_spells_for_turn(state, t, who, library, opp_library, catalog_map, rng);
    }

    sim_discard_to_limit(state, t, who);
}

/// Convert sim-tracked permanent names into Permanent structs, applying card-specific side effects:
/// - Tamiyo: generates 0–2 clue tokens; 50% chance she's flipped to her planeswalker face.
/// - Orcish Bowmasters: generates an Orc Army token with a size biased toward 1/1 (1–4).
/// Returns (permanents, bonus_clue_tokens).
fn apply_sim_entry_effects(
    names: &[String],
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> (Vec<Permanent>, i32) {
    let mut result: Vec<Permanent> = Vec::new();
    let mut bonus_clues = 0i32;
    for name in names {
        match name.as_str() {
            "Tamiyo, Inquisitive Student" => {
                bonus_clues += rng.gen_range(0..=2);
                if rng.gen_bool(0.5) {
                    result.push(Permanent {
                        name: "Tamiyo, Seasoned Scholar".to_string(),
                        kind: PermanentKind::Planeswalker {
                            loyalty: 4,
                            activated: false,
                        },
                    });
                } else {
                    result.push(Permanent {
                        name: name.clone(),
                        kind: PermanentKind::Creature { tapped: false },
                    });
                }
            }
            "Orcish Bowmasters" => {
                result.push(Permanent {
                    name: name.clone(),
                    kind: PermanentKind::Creature { tapped: false },
                });
                let size = weighted_choice(&[(1i32, 50), (2, 25), (3, 15), (4, 10)], rng);
                result.push(Permanent {
                    name: format!("Orc Army ({}/{})", size, size),
                    kind: PermanentKind::Creature { tapped: false },
                });
            }
            _ => {
                let kind = match catalog_map.get(name.as_str()).map(|d| d.card_type.as_str()) {
                    Some("planeswalker") => {
                        let loyalty = catalog_map
                            .get(name.as_str())
                            .and_then(|d| d.loyalty)
                            .unwrap_or(3);
                        PermanentKind::Planeswalker {
                            loyalty,
                            activated: false,
                        }
                    }
                    Some("artifact") => PermanentKind::Artifact,
                    _ => PermanentKind::Creature { tapped: false },
                };
                result.push(Permanent {
                    name: name.clone(),
                    kind,
                });
            }
        }
    }
    (result, bonus_clues)
}

/// Same as apply_sim_entry_effects but returns flat strings (for opponent_permanents in GameState).
fn apply_sim_entry_effects_opp(
    names: &[String],
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> (Vec<String>, i32) {
    let mut result: Vec<String> = Vec::new();
    let mut bonus_clues = 0i32;
    for name in names {
        match name.as_str() {
            "Tamiyo, Inquisitive Student" => {
                bonus_clues += rng.gen_range(0..=2);
                if rng.gen_bool(0.5) {
                    result.push("Tamiyo, Seasoned Scholar".to_string());
                } else {
                    result.push(name.clone());
                }
            }
            "Orcish Bowmasters" => {
                result.push(name.clone());
                let size = weighted_choice(&[(1i32, 50), (2, 25), (3, 15), (4, 10)], rng);
                result.push(format!("Orc Army ({}/{})", size, size));
            }
            _ => {
                result.push(name.clone());
            }
        }
    }
    (result, bonus_clues)
}

/// Cast Doomsday on the fateful turn. Pays costs and logs the payment path.
/// Returns the Doomsday StackItem if a payment path was found, None otherwise.
/// The actual resolution effect (pile construction / life loss) is handled by the caller
/// via resolve_stack.
fn sim_cast_doomsday(state: &mut SimState, t: u8) -> Option<StackItem> {
    let dd = || StackItem {
        name: "Doomsday".to_string(),
        owner: "us".to_string(),
        counters: None,
        permanent_target: None,
    };
    let mana = state.us.mana();
    // Path 1: BBB from lands
    if mana.can_pay(3, 0, 0) {
        state.us.tap(3, 0, 0);
        state.us.hand.hidden -= 1;
        state.log(t, "us", "Cast Doomsday (paid BBB from lands)");
        return Some(dd());
    }
    // Path 2: BB + Lotus Petal (Petal provides 1 of any color → treated as 1 generic)
    let has_petal = state.us.permanents.iter().any(|p| p == "Lotus Petal");
    if has_petal && mana.can_pay(2, 0, 0) {
        state.us.tap(2, 0, 0);
        state.us.permanents.retain(|p| p != "Lotus Petal");
        state.us.graveyard.visible.push("Lotus Petal".to_string());
        state.us.hand.hidden -= 1;
        state.log(t, "us", "Cast Doomsday (BB + Lotus Petal)");
        return Some(dd());
    }
    // Path 3: 1 black land → Ritual (B→BBB) → Doomsday (BBB)
    if mana.can_pay(1, 0, 0) && state.us.hand.hidden > 1 {
        state.us.tap(1, 0, 0); // pay B for Ritual
                               // Ritual produces BBB — add 3 virtual black mana
        for _ in 0..3 {
            state.us.lands.push(SimLand {
                name: "Ritual mana".into(),
                tapped: false,
                produces_black: true,
                produces_blue: false,
                is_fetch: false,
                basic: false,
            });
        }
        // TODO: make this a real "cast a dark ritual" using the cast function
        state.us.hand.hidden -= 1; // ritual
        state.us.graveyard.visible.push("Dark Ritual".to_string());
        state.log(t, "us", "Cast Dark Ritual (B → BBB)");
        state.us.tap(3, 0, 0); // pay BBB for Doomsday
        state.us.hand.hidden -= 1; // doomsday
        state.log(t, "us", "Cast Doomsday (BBB from Ritual)");
        return Some(dd());
    }
    // Path 4: Lotus Petal pays for Ritual (B→BBB) → Doomsday (BBB), 0 lands needed
    if has_petal && state.us.hand.hidden > 1 {
        state.us.permanents.retain(|p| p != "Lotus Petal"); // Petal pays B for Ritual
        state.us.graveyard.visible.push("Lotus Petal".to_string());
        state.us.hand.hidden -= 1; // ritual
        state.us.graveyard.visible.push("Dark Ritual".to_string());
        // Ritual produces BBB — add 3 virtual black mana
        for _ in 0..3 {
            state.us.lands.push(SimLand {
                name: "Ritual mana".into(),
                tapped: false,
                produces_black: true,
                produces_blue: false,
                is_fetch: false,
                basic: false,
            });
        }
        state.log(t, "us", "Cast Dark Ritual via Lotus Petal (BBB)");
        state.us.tap(3, 0, 0); // pay BBB for Doomsday
        state.us.hand.hidden -= 1; // doomsday
        state.log(t, "us", "Cast Doomsday (BBB from Ritual)");
        return Some(dd());
    }
    None
}

/// Simulate the full game up to the Doomsday turn.
/// Returns `None` if Doomsday was countered and could not be protected — caller should retry.
fn simulate_game(
    deck_name: &str,
    opponent: &str,
    config: &PilegenConfig,
    all_cards: &[(String, i32, String)],
    opp_cards: &[(String, i32, String)],
    rng: &mut impl Rng,
) -> Option<SimState> {
    let turn = gen_turn(rng);
    let on_play = rng.gen_bool(0.5);
    let our_mulligans = gen_mulligans(rng);
    let opp_mulligans = gen_mulligans(rng);

    let catalog_map: HashMap<&str, &CardDef> =
        config.cards.iter().map(|c| (c.name.as_str(), c)).collect();

    let us = PlayerState::new(deck_name, our_mulligans);
    let opp = PlayerState::new(opponent, opp_mulligans);
    let mut state = SimState::new(us, opp);
    state.on_play = on_play;
    state.turn = turn;

    state.log(
        0,
        "—",
        format!(
            "Turn {} — {} ({}) | us: {} cards (-{} mulligans), opp: {} cards (-{} mulligans)",
            turn,
            opponent,
            if on_play { "play" } else { "draw" },
            state.us.hand.hidden,
            our_mulligans,
            state.opp.hand.hidden,
            opp_mulligans
        ),
    );

    // Unified library for each player: all mainboard cards expanded by quantity.
    // Represents hand + undrawn library combined; hand.hidden tracks drawn count.
    state.us.library = all_cards
        .iter()
        .filter(|(_, _, b)| b == "main")
        .filter_map(|(name, qty, _)| {
            catalog_map
                .get(name.as_str())
                .map(|d| std::iter::repeat((name.clone(), (*d).clone())).take(*qty as usize))
        })
        .flatten()
        .collect();

    state.opp.library = opp_cards
        .iter()
        .filter(|(_, _, b)| b == "main")
        .filter_map(|(name, qty, _)| {
            catalog_map
                .get(name.as_str())
                .map(|d| std::iter::repeat((name.clone(), (*d).clone())).take(*qty as usize))
        })
        .flatten()
        .collect();

    // ── Turn loop ────────────────────────────────────────────────────────────

    for t in 1..=turn {
        if !on_play {
            let mut opp_lib = std::mem::take(&mut state.opp.library);
            let mut us_lib = std::mem::take(&mut state.us.library);
            sim_turn(&mut state, t, "opp", turn, on_play, &mut opp_lib, &mut us_lib, &catalog_map, rng);
            state.opp.library = opp_lib;
            state.us.library = us_lib;
        }
        {
            let mut us_lib = std::mem::take(&mut state.us.library);
            let mut opp_lib = std::mem::take(&mut state.opp.library);
            sim_turn(&mut state, t, "us", turn, on_play, &mut us_lib, &mut opp_lib, &catalog_map, rng);
            state.us.library = us_lib;
            state.opp.library = opp_lib;
        }
        if on_play && t < turn {
            let mut opp_lib = std::mem::take(&mut state.opp.library);
            let mut us_lib = std::mem::take(&mut state.us.library);
            sim_turn(&mut state, t, "opp", turn, on_play, &mut opp_lib, &mut us_lib, &catalog_map, rng);
            state.opp.library = opp_lib;
            state.us.library = us_lib;
        }
    }

    if state.reroll {
        return None;
    }

    // Clean up ritual sentinels before converting to output
    // TODO: real mana pool
    state.us.lands.retain(|l| l.name != "Ritual mana");

    Some(state)
}

fn load_deck_cards(deck_name: &str) -> Vec<(String, i32, String)> {
    let conn = &mut establish_connection();

    let deck: Deck = decks::table
        .filter(decks::list_name.eq(deck_name))
        .first(conn)
        .expect("Deck not found");

    let deck_cards: Vec<Card> = cards::table
        .filter(cards::deck_id.eq(deck.deck_id))
        .load(conn)
        .unwrap_or_default();

    deck_cards
        .into_iter()
        .map(|c| (c.card_name, c.quantity, c.board))
        .collect()
}

// ── Scenario generation ───────────────────────────────────────────────────────

/// Sample `count` cards from `library` without replacement into `zone.visible`,
/// decrementing `zone.hidden` by the same amount.
fn reveal_hand(zone: &mut Zone, library: &[(String, CardDef)], count: i32, rng: &mut impl Rng) {
    let n = (count as usize).min(library.len());
    if n == 0 {
        return;
    }
    let mut indices: Vec<usize> = (0..library.len()).collect();
    for i in 0..n {
        let j = rng.gen_range(i..library.len());
        indices.swap(i, j);
    }
    for &idx in &indices[..n] {
        zone.visible.push(library[idx].0.clone());
        zone.hidden -= 1;
    }
    zone.visible.sort();
}

fn generate_scenario(
    deck_name: &str,
    opp_display: &str,
    config: &PilegenConfig,
    all_cards: &[(String, i32, String)],
    opp_cards: &[(String, i32, String)],
) -> SimState {
    let mut rng = rand::thread_rng();
    loop {
        if let Some(mut state) =
            simulate_game(deck_name, opp_display, config, all_cards, opp_cards, &mut rng)
        {
            // Reveal our full hand from remaining library (we know our own hand).
            let count = state.us.hand.hidden;
            let lib = std::mem::take(&mut state.us.library);
            reveal_hand(&mut state.us.hand, &lib, count, &mut rng);
            state.us.library = lib;
            return state;
        }
    }
}

fn gen_turn(rng: &mut impl Rng) -> u8 {
    weighted_choice(
        &[(2u8, 10), (3, 25), (4, 30), (5, 20), (6, 10), (7, 5)],
        rng,
    )
}

fn gen_mulligans(rng: &mut impl Rng) -> u8 {
    weighted_choice(&[(0u8, 55), (1, 35), (2, 10)], rng)
}

fn weighted_choice<T: Clone>(options: &[(T, u32)], rng: &mut impl Rng) -> T {
    let total: u32 = options.iter().map(|(_, w)| w).sum();
    assert!(total > 0);
    let mut pick = rng.gen_range(0..total);
    for (val, weight) in options {
        if pick < *weight {
            return val.clone();
        }
        pick -= weight;
    }
    options.last().unwrap().0.clone()
}

// ── Card rule helpers ─────────────────────────────────────────────────────────

/// Stage-based probability that a cracked_land is still in play.
fn gen_clue_tokens(rng: &mut impl Rng) -> i32 {
    weighted_choice(&[(0i32, 90), (1, 9), (2, 1)], rng)
}
