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
#[derive(Clone)]
struct StackItem {
    name: String,
    owner: String,
    /// True for activated abilities; NAP skips countering these.
    is_ability: bool,
    /// For activated abilities: the ability definition, used to apply the effect at resolution.
    ability_def: Option<AbilityDef>,
    /// For counterspells: which index in the stack this spell is targeting.
    counters: Option<usize>,
    /// For spells with a permanent target (`CardDef.target`): `(target_who, target_name)`.
    /// Resolved at cast time and locked in; used directly by `apply_spell_effects`.
    permanent_target: Option<(String, String)>,
}


// ── Turn structure ────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Debug)]
enum PhaseKind {
    Beginning,
    PreCombatMain,
    End,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum StepKind {
    Untap,
    Upkeep,
    Draw,
    End,
    Cleanup,
}

struct Step {
    kind: StepKind,
    prio: bool,
}

struct Phase {
    kind: PhaseKind,
    steps: Vec<Step>,
}

impl Phase {
    fn is_main_phase(&self) -> bool {
        matches!(self.kind, PhaseKind::PreCombatMain)
    }
}

// ── Priority actions ──────────────────────────────────────────────────────────

#[derive(Clone)]
enum PriorityAction {
    /// Land drop: AP only, does NOT pass priority. Carries the chosen land name.
    LandDrop(String),
    /// Activate a permanent ability. Carries source name + ability def. Uses the stack, passes priority after.
    ActivateAbility(String, AbilityDef),
    /// Cast a spell onto the stack; passes priority after.
    CastSpell(StackItem),
    /// Pass priority.
    Pass,
}

// ── Phase constructors ────────────────────────────────────────────────────────

fn beginning_phase() -> Phase {
    Phase {
        kind: PhaseKind::Beginning,
        steps: vec![
            Step { kind: StepKind::Untap,  prio: false },
            Step { kind: StepKind::Upkeep, prio: true  },
            Step { kind: StepKind::Draw,   prio: true  },
        ],
    }
}

fn main_phase() -> Phase {
    Phase { kind: PhaseKind::PreCombatMain, steps: vec![] }
}

fn end_phase() -> Phase {
    Phase {
        kind: PhaseKind::End,
        steps: vec![
            Step { kind: StepKind::End,     prio: true  },
            Step { kind: StepKind::Cleanup, prio: false },
        ],
    }
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

    fn spend(&mut self, black: i32, blue: i32, generic: i32) {
        self.black -= black;
        self.blue  -= blue;
        self.total -= black + blue + generic;
        // Generic costs may consume colored mana from the pool; reduce the excess colored
        // counters (black first, arbitrarily) so the invariant total >= black + blue holds.
        let excess = (self.black + self.blue).saturating_sub(self.total);
        if excess > 0 {
            let from_black = excess.min(self.black);
            self.black -= from_black;
            self.blue  -= (excess - from_black).min(self.blue);
        }
    }

    fn drain(&mut self) {
        *self = ManaPool::default();
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
    want_to_activate: bool,
}

#[derive(Clone)]
struct SimPermanent {
    name: String,
    want_to_activate: bool,
}

impl SimPermanent {
    fn new(name: impl Into<String>) -> Self {
        SimPermanent { name: name.into(), want_to_activate: false }
    }
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
            want_to_activate: false,
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
    life: i32,
    library: Vec<(String, CardDef)>,
    hand: Zone,
    lands: Vec<SimLand>,
    permanents: Vec<SimPermanent>,
    graveyard: Zone,
    exile: Zone,
    /// Reset to true each Untap step; false once a land has been played or the 85% roll failed.
    land_drop_available: bool,
    /// True once Doomsday has been cast this game (prevents double-cast).
    dd_cast: bool,
    /// Number of non-land spells cast this turn; reset each Untap. Used for multi-spell probability.
    spells_cast_this_turn: u8,
    /// Mana produced but not yet spent; drains at end of each step/phase.
    pool: ManaPool,
}

impl PlayerState {
    fn new(deck: &str, mulligans: u8) -> Self {
        PlayerState {
            life: 20,
            deck_name: deck.to_string(),
            library: Vec::new(),
            hand: Zone::new_hidden((7 - mulligans as i32).max(0)),
            lands: Vec::new(),
            permanents: Vec::new(),
            graveyard: Zone { visible: Vec::new(), hidden: 0 },
            exile: Zone { visible: Vec::new(), hidden: 0 },
            land_drop_available: false, // set true by Untap step
            dd_cast: false,
            spells_cast_this_turn: 0,
            pool: ManaPool::default(),
        }
    }

    /// Mana accessible right now: pool + what untapped non-fetch lands can still produce.
    fn potential_mana(&self) -> ManaPool {
        let mut p = self.pool.clone();
        for l in &self.lands {
            if l.tapped || l.is_fetch { continue; }
            if l.produces_black { p.black += 1; }
            if l.produces_blue  { p.blue  += 1; }
            p.total += 1;
        }
        p
    }

    /// Tap lands to add mana to the pool. Black sources first, then blue, then any.
    /// For generic costs only `pool.total` is incremented (color is not committed).
    /// Fetches are never tapped for mana.
    fn produce_mana(&mut self, black: i32, blue: i32, generic: i32) {
        let mut b = black;
        for l in &mut self.lands {
            if !l.tapped && !l.is_fetch && b > 0 && l.produces_black {
                l.tapped = true;
                self.pool.black += 1;
                self.pool.total += 1;
                b -= 1;
            }
        }
        let mut u = blue;
        for l in &mut self.lands {
            if !l.tapped && !l.is_fetch && u > 0 && l.produces_blue {
                l.tapped = true;
                self.pool.blue  += 1;
                self.pool.total += 1;
                u -= 1;
            }
        }
        let mut g = generic;
        for l in &mut self.lands {
            if !l.tapped && !l.is_fetch && g > 0 {
                l.tapped = true;
                self.pool.total += 1; // color not committed for generic
                g -= 1;
            }
        }
    }

    /// Produce mana from lands and immediately spend it (the common pay-a-cost pattern).
    fn pay_mana(&mut self, black: i32, blue: i32, generic: i32) {
        self.produce_mana(black, blue, generic);
        self.pool.spend(black, blue, generic);
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
    /// Active player this phase/step (for log context).
    current_ap: String,
    /// Current phase/step label (for log context).
    current_phase: String,
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
            current_ap: String::new(),
            current_phase: String::new(),
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
        let is_player = who == "us" || who == "opp";
        let hand = if is_player { self.player(who).hand.hidden } else { 0 };
        let suffix = if is_player { format!(" [hand: {}]", hand) } else { String::new() };
        let ctx = if is_player && !self.current_ap.is_empty() {
            format!("|{}/{}", self.current_ap, self.current_phase)
        } else {
            String::new()
        };
        self.log.push(format!("T{} [{}{}] {}{}", t, who, ctx, msg.into(), suffix));
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
                writeln!(f, "    * {}", p.name)?;
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

/// Choose a land to play from the library. Returns the chosen land name, or `None` if no eligible
/// land exists. Weights black-producing lands 3× when the player has no black source.
/// `fateful` = Doomsday turn: skip cracked-land entries (e.g. Wasteland) to avoid mana issues.
fn choose_land_name(
    state: &SimState,
    who: &str,
    library: &[(String, CardDef)],
    fateful: bool,
    rng: &mut impl Rng,
) -> Option<String> {
    let has_black = state.player(who).lands.iter().any(|l| !l.is_fetch && l.produces_black);
    let weighted: Vec<(usize, u32)> = library
        .iter()
        .enumerate()
        .filter_map(|(i, (_, def))| {
            if def.card_type != "land" { return None; }
            if fateful && def.cracked_land { return None; }
            let w = 1;
            Some((i, w))
        })
        .collect();
    if weighted.is_empty() { return None; }
    Some(library[weighted_choice(&weighted, rng)].0.clone())
}

/// Play a specific, pre-chosen land from the library (removes the entry).
/// Fetches stay in play to be cracked later in the ability pass.
fn sim_play_land(
    state: &mut SimState,
    t: u8,
    who: &str,
    library: &mut Vec<(String, CardDef)>,
    land_name: &str,
) {
    let Some(idx) = library.iter().position(|(n, _)| n == land_name) else { return; };
    let land = {
        let (name, def) = &library[idx];
        SimLand::from_def(name, def)
    };
    state.player_mut(who).hand.hidden -= 1;
    state.player_mut(who).lands.push(land);
    state.log(t, who, format!("Play {}", land_name));
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
            let def = catalog_map.get(p.name.as_str()).copied();
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
        if !state.player(who).potential_mana().can_pay(b, bl, g) {
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
    sorcery_speed: bool,
) -> Vec<String> {
    let permanents_in_play = &state.player(who).permanents;
    let opp_who = if who == "us" { "opp" } else { "us" };
    library
        .iter()
        .filter_map(|(name, def)| {
            if def.card_type == "land" {
                return None;
            }
            // Sorceries (and creature/planeswalker permanents) can only be cast at sorcery speed:
            // AP's main phase with an empty stack.
            if !sorcery_speed && def.card_type != "instant" {
                return None;
            }
            let castable = def.effects.iter().any(|e| {
                e == "cantrip" || e == "permanent" || e == "destroy" || e.starts_with("discard:")
            });
            if !castable {
                return None;
            }
            if def.legendary && permanents_in_play.iter().any(|p| p.name == name.as_str()) {
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
        let def = catalog_map.get(perm.name.as_str()).copied();
        let ct = def.map(|d| d.card_type.as_str()).unwrap_or("");
        if matches_target_type(type_str, ct, false, def) {
            candidates.push(perm.name.clone());
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
        state.player_mut(target_who).permanents.retain(|p| p.name != target_name);
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

/// Pay the activation cost of an ability: mana, life, tap, and/or sacrifice.
/// Effects are NOT applied here — they happen when the ability resolves off the stack.
fn pay_activation_cost(
    state: &mut SimState,
    t: u8,
    who: &str,
    source_name: &str,
    ability: &AbilityDef,
    catalog_map: &HashMap<&str, &CardDef>,
) {
    state.log(t, who, format!("Activate {} ability", source_name));

    // Pay mana cost.
    if !ability.mana_cost.is_empty() {
        let (b, bl, g) = parse_mana_cost(&ability.mana_cost);
        state.player_mut(who).pay_mana(b, bl, g);
    }

    // Pay life cost.
    if ability.life_cost > 0 {
        state.lose_life(who, ability.life_cost);
    }

    // Pay tap cost.
    if ability.tap_self && !ability.sacrifice_self {
        if let Some(l) = state.player_mut(who).lands.iter_mut().find(|l| l.name == source_name) {
            l.tapped = true;
        }
    }

    // Pay sacrifice cost.
    if ability.sacrifice_self {
        let is_land = catalog_map.get(source_name).map(|d| d.card_type == "land").unwrap_or(false);
        if is_land {
            state.player_mut(who).lands.retain(|l| l.name != source_name);
        } else {
            state.player_mut(who).permanents.retain(|p| p.name != source_name);
        }
        state.player_mut(who).graveyard.visible.push(source_name.to_string());
    }
}

/// Apply the resolution effect of an activated ability.
/// Called when the ability stack item resolves (both players pass consecutively).
fn apply_ability_effect(
    state: &mut SimState,
    t: u8,
    who: &str,
    source_name: &str,
    ability: &AbilityDef,
    library: &mut Vec<(String, CardDef)>,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) {
    // search:*:* — generic library search (e.g. fetchland: "search:land-ub:play").
    if ability.effect.starts_with("search:") {
        let mut parts = ability.effect.splitn(3, ':');
        parts.next(); // "search"
        let filter = parts.next().unwrap_or("");
        let dest   = parts.next().unwrap_or("play");

        let candidates: Vec<usize> = library
            .iter()
            .enumerate()
            .filter(|(_, (_, d))| matches_search_filter(filter, d))
            .map(|(i, _)| i)
            .collect();

        if !candidates.is_empty() {
            let idx = candidates[rng.gen_range(0..candidates.len())];
            let land = {
                let (name, def) = &library[idx];
                SimLand {
                    name: name.clone(),
                    tapped: false,
                    is_fetch: false,
                    basic: def.basic_land,
                    produces_black: def.produces_black,
                    produces_blue: def.produces_blue,
                    want_to_activate: false,
                }
            };
            let name = library[idx].0.clone();
            library.remove(idx);
            match dest {
                "play" => {
                    state.player_mut(who).lands.push(land);
                    state.log(t, who, format!("{} ability → {}", source_name, name));
                }
                "hand" => {
                    state.player_mut(who).hand.hidden += 1;
                    state.log(t, who, format!("{} ability → {} (to hand)", source_name, name));
                }
                _ => {}
            }
        }
        return;
    }

    // Targeted non-search effect (e.g. Wasteland: destroy target nonbasic land).
    if !ability.effect.is_empty() {
        if let Some(target_str) = &ability.target {
            sim_apply_targeted_effect(&ability.effect, target_str, state, t, who, catalog_map, rng);
        }
    }

    state.log(t, who, format!("{} ability resolves", source_name));
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
        if !player.potential_mana().can_pay(b, bl, g) {
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
    _catalog_map: &HashMap<&str, &CardDef>,
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
        state.player_mut(who).pay_mana(b, bl, g);
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
    let mana_is_usable = !def.mana_cost.is_empty() && state.player(who).potential_mana().can_pay(b, bl, g);

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
        state.player_mut(who).pay_mana(b, bl, g);
        state.player_mut(who).hand.hidden -= 1;
        def.mana_cost.clone()
    };

    state.log(t, who, format!("Cast {} ({})", name, cast_label));

    Some(StackItem {
        name: name.to_string(),
        owner: who.to_string(),
        is_ability: false,
        ability_def: None,
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
        state.player_mut(&item.owner).permanents.push(SimPermanent::new(item.name.clone()));
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



// ── New turn-structure functions ──────────────────────────────────────────────

/// At the start of the main phase, roll 75% per land/permanent with an available ability
/// to decide whether it wants to activate this turn.
fn roll_want_to_activate(
    state: &mut SimState,
    ap: &str,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) {
    // Collect names with available abilities (immutable borrow first).
    let sources: Vec<String> = {
        let player = state.player(ap);
        let mut v = Vec::new();
        for land in &player.lands {
            if land.tapped || land.want_to_activate { continue; }
            if let Some(def) = catalog_map.get(land.name.as_str()) {
                if def.abilities.iter().any(|ab| ability_available(ab, state, ap, true, catalog_map)) {
                    v.push(land.name.clone());
                }
            }
        }
        for perm in &player.permanents {
            if perm.want_to_activate { continue; }
            if let Some(def) = catalog_map.get(perm.name.as_str()) {
                if def.abilities.iter().any(|ab| ability_available(ab, state, ap, true, catalog_map)) {
                    v.push(perm.name.clone());
                }
            }
        }
        v
    };
    // Apply 75% roll (mutable borrow).
    for source in sources {
        if rng.gen_bool(0.75) {
            if let Some(land) = state.player_mut(ap).lands.iter_mut().find(|l| l.name == source) {
                land.want_to_activate = true;
            } else if let Some(perm) = state.player_mut(ap).permanents.iter_mut().find(|p| p.name == source) {
                perm.want_to_activate = true;
            }
        }
    }
}

/// Decide what action the player `who` takes when they hold priority.
/// `ap` is the active player (whose turn it is). Phase context is read from
/// `state.current_phase` (set by `do_turn`/`do_step`/`do_phase` before each priority window).
fn decide_action(
    state: &mut SimState,
    t: u8,
    ap: &str,
    who: &str,
    dd_turn: u8,
    last_action: &PriorityAction,
    stack: &[StackItem],
    us_lib: &mut Vec<(String, CardDef)>,
    opp_lib: &mut Vec<(String, CardDef)>,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> PriorityAction {
    let is_ap = who == ap;
    let in_main_phase = state.current_phase == "Main";

    // NAP with an empty stack: nothing to react to.
    if !is_ap && stack.is_empty() {
        return PriorityAction::Pass;
    }

    // NAP: only counter when AP just acted and there's an opposing spell on the stack.
    if !is_ap {
        let other_acted = matches!(last_action, PriorityAction::CastSpell(_) | PriorityAction::ActivateAbility(..));
        if other_acted {
            let actor_lib: &mut Vec<(String, CardDef)> =
                if who == "us" { us_lib } else { opp_lib };
            for idx in (0..stack.len()).rev() {
                if stack[idx].owner != who && !stack[idx].is_ability {
                    if let Some(counter) = respond_with_counter(
                        state, t, stack, idx, who, actor_lib, catalog_map, rng, true,
                    ) {
                        return PriorityAction::CastSpell(counter);
                    }
                    break;
                }
            }
        }
        return PriorityAction::Pass;
    }

    // AP outside main phase: no proactive actions.
    if !in_main_phase {
        return PriorityAction::Pass;
    }

    // AP in main phase: land drop (requires empty stack), abilities, Doomsday, spells.

    if stack.is_empty() && state.player(who).land_drop_available {
        let fateful = who == "us" && t == dd_turn;
        let lib: &Vec<(String, CardDef)> = if who == "us" { us_lib } else { opp_lib };
        let land_count = lib.iter().filter(|(_, d)| d.card_type == "land").count();
        if land_count > 0 {
            // T1 ≈ 100%, T2 ≈ 80%, T3+ ≈ land density in remaining library.
            let prob = match t {
                1 => 1.0,
                2 => 0.8,
                _ => land_count as f64 / lib.len() as f64,
            };
            if rng.gen::<f64>() < prob {
                if let Some(name) = choose_land_name(state, who, lib, fateful, rng) {
                    return PriorityAction::LandDrop(name);
                }
            }
        }
        state.player_mut(who).land_drop_available = false;
    }

    // Activate abilities (drains all pending before moving to spells).
    let source_name = state.player(who).lands.iter()
        .find(|l| l.want_to_activate).map(|l| l.name.clone())
        .or_else(|| state.player(who).permanents.iter()
            .find(|p| p.want_to_activate).map(|p| p.name.clone()));
    if let Some(source_name) = source_name {
        let ab = catalog_map.get(source_name.as_str())
            .and_then(|def| def.abilities.iter()
                .find(|ab| ability_available(ab, state, who, true, catalog_map))
                .cloned());
        if let Some(ab) = ab {
            return PriorityAction::ActivateAbility(source_name, ab);
        }
    }

    // Doomsday turn: cast Doomsday as primary action.
    if who == "us" && t == dd_turn && !state.us.dd_cast {
        if let Some(item) = sim_cast_doomsday(state, t) {
            state.us.dd_cast = true;
            state.us.spells_cast_this_turn += 1;
            return PriorityAction::CastSpell(item);
        }
    }

    // Cast non-Doomsday spells — with decaying multi-spell probability.
    // 1st spell: always attempt; 2nd: 30%; 3rd+: 10%.
    let cast_prob = match state.player(who).spells_cast_this_turn {
        0 => 1.0,
        1 => 0.30,
        _ => 0.10,
    };
    if rng.gen::<f64>() < cast_prob {
        let (actor_lib, _other_lib): (&mut Vec<(String, CardDef)>, &mut Vec<(String, CardDef)>) =
            if who == "us" { (us_lib, opp_lib) } else { (opp_lib, us_lib) };
        let spells = collect_spells(state, who, actor_lib, catalog_map, true);
        if !spells.is_empty() {
            let idx = rng.gen_range(0..spells.len());
            let spell_name = spells[idx].clone();
            if let Some(item) = cast_spell(state, t, who, &spell_name, actor_lib, None, catalog_map, rng) {
                state.player_mut(who).spells_cast_this_turn += 1;
                return PriorityAction::CastSpell(item);
            }
        }
    }

    PriorityAction::Pass
}

/// Run a priority round. AP gets priority first; both players must pass consecutively
/// (with an empty stack) for the round to end. When both pass with a non-empty stack,
/// the entire stack resolves LIFO and AP regains priority.
fn handle_priority_round(
    state: &mut SimState,
    t: u8,
    ap: &str,
    dd_turn: u8,
    us_lib: &mut Vec<(String, CardDef)>,
    opp_lib: &mut Vec<(String, CardDef)>,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) {
    let nap = if ap == "us" { "opp" } else { "us" };
    let mut priority_holder = ap.to_string();
    let mut last_passer: Option<String> = None;
    let mut stack: Vec<StackItem> = Vec::new();
    // What the *other* player did on their last priority window.
    let mut last_action: PriorityAction = PriorityAction::Pass;

    loop {
        let who = priority_holder.clone();
        let action = decide_action(
            state, t, ap, &who, dd_turn, &last_action, &stack,
            us_lib, opp_lib, catalog_map, rng,
        );
        last_action = action.clone();

        match action {
            PriorityAction::LandDrop(ref land_name) => {
                // Land drop — does not pass priority, does not use the stack.
                let actor_lib = if who == "us" { &mut *us_lib } else { &mut *opp_lib };
                sim_play_land(state, t, &who, actor_lib, land_name);
                state.player_mut(&who).land_drop_available = false;
                last_passer = None;
                // Priority stays with the same player.
            }
            PriorityAction::ActivateAbility(ref source_name, ref ability) => {
                // Pay costs now; effect is deferred until the ability resolves.
                pay_activation_cost(state, t, &who, source_name, ability, catalog_map);
                // Clear want_to_activate flag on the source.
                if let Some(l) = state.player_mut(&who).lands.iter_mut().find(|l| l.name == *source_name) {
                    l.want_to_activate = false;
                } else if let Some(p) = state.player_mut(&who).permanents.iter_mut().find(|p| p.name == *source_name) {
                    p.want_to_activate = false;
                }
                // Push ability stack item; ability_def carries the effect for resolution.
                stack.push(StackItem {
                    name: source_name.clone(),
                    owner: who.clone(),
                    is_ability: true,
                    ability_def: Some(ability.clone()),
                    counters: None,
                    permanent_target: None,
                });
                let next = if who == ap { nap } else { ap };
                priority_holder = next.to_string();
                last_passer = None;
            }
            PriorityAction::CastSpell(item) => {
                // Check if this is Doomsday: if opponent counters and we can't protect, mark reroll.
                let is_dd = item.name == "Doomsday" && item.owner == "us";
                stack.push(item);
                let next = if who == ap { nap } else { ap };
                priority_holder = next.to_string();
                last_passer = None;

                // If we just cast Doomsday, immediately run the full Doomsday priority exchange
                // (opponent responds, we counter-counter) then resolve and return.
                if is_dd {
                    // Opponent gets priority to counter.
                    let dd_idx = stack.len() - 1;
                    let opp_counter = respond_with_counter(
                        state, t, &stack, dd_idx, "opp", opp_lib, catalog_map, rng, true,
                    );
                    if let Some(opp_item) = opp_counter {
                        stack.push(opp_item);
                        // We get priority back — try to protect Doomsday deterministically.
                        let counter_idx = stack.len() - 1;
                        let our_counter = respond_with_counter(
                            state, t, &stack, counter_idx, "us", us_lib, catalog_map, rng, false,
                        );
                        if let Some(our_item) = our_counter {
                            stack.push(our_item);
                        } else {
                            state.log(t, "us", "⚠ Doomsday countered — could not protect");
                            state.reroll = true;
                        }
                    }
                    // Resolve the full stack and return from this priority round.
                    resolve_stack(&stack, state, t, us_lib, opp_lib, catalog_map, rng);
                    return;
                }
            }
            PriorityAction::Pass => {
                let other = if who == ap { nap } else { ap };
                if last_passer.as_deref() == Some(other) {
                    // Both players passed consecutively.
                    if stack.is_empty() {
                        // Empty stack — priority round ends.
                        break;
                    } else {
                        // Resolve top item only, then AP gets priority again.
                        let top = stack.pop().unwrap();
                        if let Some(target_idx) = top.counters {
                            if target_idx < stack.len() {
                                // Target still on stack — counter resolves: remove and graveyard target.
                                let target = stack.remove(target_idx);
                                state.log(t, &top.owner, &format!("{} counters {}", top.name, target.name));
                                state.player_mut(&target.owner).graveyard.visible.push(target.name);
                                state.player_mut(&top.owner).graveyard.visible.push(top.name);
                            } else {
                                // Target already gone — counter fizzles.
                                state.player_mut(&top.owner).graveyard.visible.push(top.name.clone());
                                state.log(t, &top.owner, &format!("{} fizzles (target already resolved)", top.name));
                            }
                        } else if top.is_ability {
                            // Ability resolves: apply the deferred effect now.
                            if let Some(ref ab) = top.ability_def {
                                let (actor_lib, _other_lib) = if top.owner == "us" {
                                    (&mut *us_lib, &mut *opp_lib)
                                } else {
                                    (&mut *opp_lib, &mut *us_lib)
                                };
                                apply_ability_effect(state, t, &top.owner, &top.name, ab, actor_lib, catalog_map, rng);
                            }
                        } else {
                            let (actor_lib, other_lib) = if top.owner == "us" {
                                (&mut *us_lib, &mut *opp_lib)
                            } else {
                                (&mut *opp_lib, &mut *us_lib)
                            };
                            apply_spell_effects(&top, state, t, actor_lib, other_lib, catalog_map, rng);
                        }
                        // AP gets priority with remaining stack.
                        priority_holder = ap.to_string();
                        last_passer = None;
                        last_action = PriorityAction::Pass; // fresh slate after resolution
                    }
                } else {
                    last_passer = Some(who.clone());
                    priority_holder = other.to_string();
                }
            }
        }

        if state.reroll {
            break;
        }
    }
}

/// Execute a single step: apply automatic effects, then optionally run a priority round.
fn do_step(
    state: &mut SimState,
    t: u8,
    ap: &str,
    step: &Step,
    dd_turn: u8,
    on_play: bool,
    us_lib: &mut Vec<(String, CardDef)>,
    opp_lib: &mut Vec<(String, CardDef)>,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) {
    state.current_phase = match step.kind {
        StepKind::Untap   => "Untap",
        StepKind::Upkeep  => "Upkeep",
        StepKind::Draw    => "Draw",
        StepKind::End     => "EndStep",
        StepKind::Cleanup => "Cleanup",
    }.to_string();
    match step.kind {
        StepKind::Untap => {
            for land in &mut state.player_mut(ap).lands {
                land.tapped = false;
            }
            state.player_mut(ap).land_drop_available = true;
            state.player_mut(ap).spells_cast_this_turn = 0;
        }
        StepKind::Draw => {
            let this_player_on_play = if ap == "us" { on_play } else { !on_play };
            let skip = this_player_on_play && t == 1;
            if skip {
                state.log(t, ap, "No draw (on the play)");
            } else {
                state.player_mut(ap).hand.hidden += 1;
                state.log(t, ap, "Draw");
            }
        }
        StepKind::Cleanup => {
            sim_discard_to_limit(state, t, ap);
        }
        StepKind::Upkeep | StepKind::End => {
            // No automatic actions.
        }
    }

    if step.prio {
        handle_priority_round(state, t, ap, dd_turn, us_lib, opp_lib, catalog_map, rng);
    }
    // Mana pool drains at the end of every step.
    state.us.pool.drain();
    state.opp.pool.drain();
}

/// Execute a full phase: run each step, then optionally run a phase-level priority round.
fn do_phase(
    state: &mut SimState,
    t: u8,
    ap: &str,
    phase: &Phase,
    dd_turn: u8,
    on_play: bool,
    us_lib: &mut Vec<(String, CardDef)>,
    opp_lib: &mut Vec<(String, CardDef)>,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) {
    for step in &phase.steps {
        do_step(state, t, ap, step, dd_turn, on_play, us_lib, opp_lib, catalog_map, rng);
        if state.reroll {
            return;
        }
    }
    if phase.is_main_phase() {
        state.current_phase = "Main".to_string();
        roll_want_to_activate(state, ap, catalog_map, rng);
        handle_priority_round(state, t, ap, dd_turn, us_lib, opp_lib, catalog_map, rng);
        // Mana pool drains at the end of the main phase.
        state.us.pool.drain();
        state.opp.pool.drain();
    }
}

/// Simulate one full turn for the active player `ap`.
fn do_turn(
    state: &mut SimState,
    t: u8,
    ap: &str,
    dd_turn: u8,
    on_play: bool,
    us_lib: &mut Vec<(String, CardDef)>,
    opp_lib: &mut Vec<(String, CardDef)>,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) {
    state.current_ap = ap.to_string();
    do_phase(state, t, ap, &beginning_phase(), dd_turn, on_play, us_lib, opp_lib, catalog_map, rng);
    if state.reroll { return; }

    do_phase(state, t, ap, &main_phase(), dd_turn, on_play, us_lib, opp_lib, catalog_map, rng);
    if state.reroll { return; }

    do_phase(state, t, ap, &end_phase(), dd_turn, on_play, us_lib, opp_lib, catalog_map, rng);
}


/// Sacrifice Lotus Petal for 1 black mana, adding it directly to the pool.
fn sacrifice_petal(ps: &mut PlayerState) {
    ps.permanents.retain(|p| p.name != "Lotus Petal");
    ps.graveyard.visible.push("Lotus Petal".to_string());
    ps.pool.black += 1;
    ps.pool.total += 1;
}

/// Remove Dark Ritual from the library, add BBB to the pool, send it to graveyard.
/// The caller is responsible for paying the B cost first (via pay_mana or sacrifice_petal).
fn resolve_ritual(ps: &mut PlayerState) {
    if let Some(idx) = ps.library.iter().position(|(n, _)| n == "Dark Ritual") {
        ps.library.remove(idx);
    }
    ps.hand.hidden -= 1;
    ps.graveyard.visible.push("Dark Ritual".to_string());
    ps.pool.black += 3;
    ps.pool.total += 3;
}

/// Cast Doomsday on the fateful turn. Pays costs via the mana pool and logs the payment path.
/// Returns the Doomsday StackItem if a payment path was found, None otherwise.
/// The actual resolution effect (pile construction / life loss) is handled by the caller
/// via resolve_stack.
fn sim_cast_doomsday(state: &mut SimState, t: u8) -> Option<StackItem> {
    let dd = || StackItem {
        name: "Doomsday".to_string(),
        owner: "us".to_string(),
        is_ability: false,
        ability_def: None,
        counters: None,
        permanent_target: None,
    };
    let has_petal  = state.us.permanents.iter().any(|p| p.name == "Lotus Petal");
    let has_ritual = state.us.library.iter().any(|(n, _)| n == "Dark Ritual");
    let potential  = state.us.potential_mana();

    // Path 1: BBB directly from lands/pool.
    if potential.can_pay(3, 0, 0) {
        state.us.pay_mana(3, 0, 0);
        state.us.hand.hidden -= 1;
        state.log(t, "us", "Cast Doomsday (BBB)");
        return Some(dd());
    }
    // Path 2: BB from lands + Lotus Petal → B (accumulate all three into pool, then pay).
    if has_petal && potential.can_pay(2, 0, 0) {
        state.us.produce_mana(2, 0, 0); // 2B now in pool
        sacrifice_petal(&mut state.us);  // +1B → pool has 3B
        state.log(t, "us", "Sacrifice Lotus Petal (→ B)");
        state.us.pool.spend(3, 0, 0);   // pay BBB for Doomsday
        state.us.hand.hidden -= 1;
        state.log(t, "us", "Cast Doomsday (BB + Lotus Petal)");
        return Some(dd());
    }
    // Path 3: B from lands → Dark Ritual → BBB → Doomsday.
    if has_ritual && potential.can_pay(1, 0, 0) && state.us.hand.hidden > 1 {
        state.us.pay_mana(1, 0, 0); // pay B for Ritual
        resolve_ritual(&mut state.us); // adds BBB to pool
        state.log(t, "us", "Cast Dark Ritual (B → BBB)");
        state.us.pool.spend(3, 0, 0);
        state.us.hand.hidden -= 1;
        state.log(t, "us", "Cast Doomsday (BBB from Ritual)");
        return Some(dd());
    }
    // Path 4: Lotus Petal → B → Dark Ritual → BBB → Doomsday (0 lands needed).
    if has_petal && has_ritual && state.us.hand.hidden > 1 {
        sacrifice_petal(&mut state.us); // adds B to pool
        state.log(t, "us", "Sacrifice Lotus Petal (→ B)");
        state.us.pool.spend(1, 0, 0); // pay B for Ritual
        resolve_ritual(&mut state.us); // adds BBB to pool
        state.log(t, "us", "Cast Dark Ritual (Petal → BBB)");
        state.us.pool.spend(3, 0, 0);
        state.us.hand.hidden -= 1;
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
            let mut us_lib = std::mem::take(&mut state.us.library);
            let mut opp_lib = std::mem::take(&mut state.opp.library);
            do_turn(&mut state, t, "opp", turn, on_play, &mut us_lib, &mut opp_lib, &catalog_map, rng);
            state.us.library = us_lib;
            state.opp.library = opp_lib;
            if state.reroll { break; }
        }
        {
            let mut us_lib = std::mem::take(&mut state.us.library);
            let mut opp_lib = std::mem::take(&mut state.opp.library);
            do_turn(&mut state, t, "us", turn, on_play, &mut us_lib, &mut opp_lib, &catalog_map, rng);
            state.us.library = us_lib;
            state.opp.library = opp_lib;
            if state.reroll { break; }
        }
        if on_play && t < turn {
            let mut us_lib = std::mem::take(&mut state.us.library);
            let mut opp_lib = std::mem::take(&mut state.opp.library);
            do_turn(&mut state, t, "opp", turn, on_play, &mut us_lib, &mut opp_lib, &catalog_map, rng);
            state.us.library = us_lib;
            state.opp.library = opp_lib;
            if state.reroll { break; }
        }
    }

    if state.reroll {
        return None;
    }

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

