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

    // ── Zone ──────────────────────────────────────────────────────────────────
    /// Zone the card must be in for this ability. Default "" / "play" = in play.
    /// Use "hand" for cycling/channel abilities.
    #[serde(default)]
    zone: String,
    /// Discard this card as part of the cost (zone="hand" abilities).
    #[serde(default)]
    discard_self: bool,
    /// Sacrifice a land you control as part of the cost (e.g. Edge of Autumn cycling).
    #[serde(default)]
    sacrifice_land: bool,

    // ── Effect ────────────────────────────────────────────────────────────────
    #[serde(default)]
    effect: String,
}

// ── Mana ability types ────────────────────────────────────────────────────────

/// How a permanent produces mana. `produces` is a string of color chars (e.g. "B", "U", "BU").
/// Empty produces → contributes to generic total only (e.g. Cavern of Souls).
#[derive(Deserialize, Clone, Default)]
struct ManaAbility {
    #[serde(default)] tap_self: bool,
    #[serde(default)] sacrifice_self: bool,
    #[serde(default)] produces: String,
}

/// The five basic land subtypes.
#[derive(Deserialize, Clone, Default)]
struct LandTypes {
    #[serde(default)] plains: bool,
    #[serde(default)] island: bool,
    #[serde(default)] swamp: bool,
    #[serde(default)] mountain: bool,
    #[serde(default)] forest: bool,
}

// ── Per-variant data structs ──────────────────────────────────────────────────

#[derive(Deserialize, Clone, Default)]
struct LandData {
    #[serde(default)] basic: bool,
    #[serde(default)] land_types: LandTypes,
    #[serde(default)] enters_tapped: bool,
    #[serde(default)] annotation_options: Vec<String>,
    #[serde(default)] mana_abilities: Vec<ManaAbility>,
    #[serde(default)] abilities: Vec<AbilityDef>,
}

#[derive(Deserialize, Clone)]
struct CreatureData {
    #[serde(default)] mana_cost: String,
    power: i32,
    toughness: i32,
    #[serde(default)] black: bool,
    #[serde(default)] blue: bool,
    #[allow(dead_code)]
    #[serde(default)] exileable: bool,
    #[serde(default)] legendary: bool,
    #[serde(default)] delve: bool,
    #[serde(default)] effects: Vec<String>,
    #[serde(default)] abilities: Vec<AbilityDef>,
    #[serde(default)] mana_abilities: Vec<ManaAbility>,
    #[serde(default)] adventure: Option<AdventureFace>,
}

#[derive(Deserialize, Clone, Default)]
struct ArtifactData {
    #[serde(default)] mana_cost: String,
    #[serde(default)] effects: Vec<String>,
    #[serde(default)] abilities: Vec<AbilityDef>,
    #[serde(default)] mana_abilities: Vec<ManaAbility>,
}

/// The adventure face of an adventure card (the instant/sorcery half).
#[derive(Deserialize, Clone)]
struct AdventureFace {
    name: String,
    #[serde(default)] card_type: String,  // "instant" or "sorcery"
    #[serde(default)] mana_cost: String,
    #[serde(default)] target: Option<String>,
    #[serde(default)] effects: Vec<String>,
}

/// Spell data shared by Instant and Sorcery variants.
#[derive(Deserialize, Clone, Default)]
struct SpellData {
    #[serde(default)] mana_cost: String,
    #[serde(default)] blue: bool,
    #[serde(default)] black: bool,
    #[allow(dead_code)]
    #[serde(default)] exileable: bool,
    #[serde(default)] target: Option<String>,
    #[serde(default)] counter_target: Option<String>,
    #[serde(default)] requires: Vec<String>,
    #[serde(default)] effects: Vec<String>,
    #[serde(default)] alternate_costs: Vec<AlternateCost>,
    #[serde(default)] delve: bool,
}

#[derive(Deserialize, Clone)]
struct PlaneswalkerData {
    #[serde(default)] mana_cost: String,
    #[allow(dead_code)]
    #[serde(default)] loyalty: i32,
    #[serde(default)] effects: Vec<String>,
    #[serde(default)] abilities: Vec<AbilityDef>,
}

#[derive(Clone)]
enum CardKind {
    Land(LandData),
    Creature(CreatureData),
    Artifact(ArtifactData),
    Instant(SpellData),
    Sorcery(SpellData),
    Planeswalker(PlaneswalkerData),
    Enchantment,
}

// ── CardDef wrapper ───────────────────────────────────────────────────────────

/// A card the generator knows about. Cards not in the catalog are treated as
/// generic non-land spells: hand-eligible, not permanent candidates, not exileable.
///
/// Wrapper struct preserving direct `.name` access and stable HashMap keys while
/// holding a typed `kind` that enforces card-category invariants.
#[derive(Clone)]
struct CardDef {
    name: String,
    /// Relative likelihood of appearing as a permanent in play (default 100).
    #[allow(dead_code)]
    play_weight: Option<u32>,
    kind: CardKind,
}

impl CardDef {
    fn is_land(&self) -> bool { matches!(self.kind, CardKind::Land(_)) }
    fn is_creature(&self) -> bool { matches!(self.kind, CardKind::Creature(_)) }
    fn is_instant(&self) -> bool { matches!(self.kind, CardKind::Instant(_)) }
    #[allow(dead_code)]
    fn is_sorcery(&self) -> bool { matches!(self.kind, CardKind::Sorcery(_)) }

    fn mana_cost(&self) -> &str {
        match &self.kind {
            CardKind::Land(_) | CardKind::Enchantment => "",
            CardKind::Creature(c) => &c.mana_cost,
            CardKind::Artifact(a) => &a.mana_cost,
            CardKind::Instant(s) | CardKind::Sorcery(s) => &s.mana_cost,
            CardKind::Planeswalker(p) => &p.mana_cost,
        }
    }

    fn effects(&self) -> &[String] {
        match &self.kind {
            CardKind::Creature(c) => &c.effects,
            CardKind::Artifact(a) => &a.effects,
            CardKind::Instant(s) | CardKind::Sorcery(s) => &s.effects,
            CardKind::Planeswalker(p) => &p.effects,
            CardKind::Land(_) | CardKind::Enchantment => &[],
        }
    }

    fn abilities(&self) -> &[AbilityDef] {
        match &self.kind {
            CardKind::Land(l) => &l.abilities,
            CardKind::Creature(c) => &c.abilities,
            CardKind::Artifact(a) => &a.abilities,
            CardKind::Planeswalker(p) => &p.abilities,
            CardKind::Instant(_) | CardKind::Sorcery(_) | CardKind::Enchantment => &[],
        }
    }

    fn alternate_costs(&self) -> &[AlternateCost] {
        match &self.kind {
            CardKind::Instant(s) | CardKind::Sorcery(s) => &s.alternate_costs,
            _ => &[],
        }
    }

    fn counter_target(&self) -> Option<&str> {
        match &self.kind {
            CardKind::Instant(s) | CardKind::Sorcery(s) => s.counter_target.as_deref(),
            _ => None,
        }
    }

    fn target(&self) -> Option<&str> {
        match &self.kind {
            CardKind::Instant(s) | CardKind::Sorcery(s) => s.target.as_deref(),
            _ => None,
        }
    }

    fn requires(&self) -> &[String] {
        match &self.kind {
            CardKind::Instant(s) | CardKind::Sorcery(s) => &s.requires,
            _ => &[],
        }
    }

    fn delve(&self) -> bool {
        match &self.kind {
            CardKind::Creature(c) => c.delve,
            CardKind::Instant(s) | CardKind::Sorcery(s) => s.delve,
            _ => false,
        }
    }

    fn legendary(&self) -> bool {
        match &self.kind {
            CardKind::Creature(c) => c.legendary,
            _ => false,
        }
    }

    fn annotation_options(&self) -> &[String] {
        match &self.kind {
            CardKind::Land(l) => &l.annotation_options,
            _ => &[],
        }
    }

    fn is_blue(&self) -> bool {
        self.mana_cost().contains('U')
            || match &self.kind {
                CardKind::Creature(c) => c.blue,
                CardKind::Instant(s) | CardKind::Sorcery(s) => s.blue,
                _ => false,
            }
    }

    fn is_black(&self) -> bool {
        self.mana_cost().contains('B')
            || match &self.kind {
                CardKind::Creature(c) => c.black,
                CardKind::Instant(s) | CardKind::Sorcery(s) => s.black,
                _ => false,
            }
    }

    fn mana_abilities(&self) -> &[ManaAbility] {
        match &self.kind {
            CardKind::Land(l) => &l.mana_abilities,
            CardKind::Creature(c) => &c.mana_abilities,
            CardKind::Artifact(a) => &a.mana_abilities,
            _ => &[],
        }
    }

    fn as_land(&self) -> Option<&LandData> {
        match &self.kind { CardKind::Land(l) => Some(l), _ => None }
    }

    fn as_creature(&self) -> Option<&CreatureData> {
        match &self.kind { CardKind::Creature(c) => Some(c), _ => None }
    }

    fn as_spell(&self) -> Option<&SpellData> {
        match &self.kind {
            CardKind::Instant(s) | CardKind::Sorcery(s) => Some(s),
            _ => None,
        }
    }

    fn adventure(&self) -> Option<&AdventureFace> {
        match &self.kind {
            CardKind::Creature(c) => c.adventure.as_ref(),
            _ => None,
        }
    }
}

// ── TOML deserialization: two-step via RawCardDef ─────────────────────────────

/// Typed card category used only during TOML deserialization.
#[derive(Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(rename_all = "snake_case")]
enum CardType {
    Land,
    Creature,
    Planeswalker,
    Artifact,
    #[default]
    Instant,
    Sorcery,
    Enchantment,
}

/// Flat deserialization target. Converted to `CardDef` by `From<RawCardDef>`.
#[derive(Deserialize, Clone, Default)]
struct RawCardDef {
    name: String,
    card_type: CardType,
    #[serde(default)] enters_tapped: bool,
    #[serde(default)] basic: bool,
    #[serde(default)] land_types: LandTypes,
    #[serde(default)] mana_abilities: Vec<ManaAbility>,
    #[serde(default)] annotation_options: Vec<String>,
    #[serde(default)] mana_cost: String,
    #[serde(default)] power: Option<i32>,
    #[serde(default)] toughness: Option<i32>,
    #[serde(default)] loyalty: Option<i32>,
    #[serde(default)] legendary: bool,
    #[serde(default)] blue: bool,
    #[serde(default)] black: bool,
    #[serde(default)] target: Option<String>,
    #[serde(default)] exileable: bool,
    #[serde(default)] play_weight: Option<u32>,
    #[serde(default)] requires: Vec<String>,
    #[serde(default)] effects: Vec<String>,
    #[serde(default)] abilities: Vec<AbilityDef>,
    #[serde(default)] delve: bool,
    #[serde(default)] counter_target: Option<String>,
    #[serde(default)] alternate_costs: Vec<AlternateCost>,
    #[serde(default)] adventure: Option<AdventureFace>,
}

impl From<RawCardDef> for CardDef {
    fn from(r: RawCardDef) -> Self {
        let kind = match r.card_type {
            CardType::Land => CardKind::Land(LandData {
                basic: r.basic,
                land_types: r.land_types,
                enters_tapped: r.enters_tapped,
                annotation_options: r.annotation_options,
                mana_abilities: r.mana_abilities.clone(),
                abilities: r.abilities,
            }),
            CardType::Creature => CardKind::Creature(CreatureData {
                mana_cost: r.mana_cost,
                power: r.power.unwrap_or(1),
                toughness: r.toughness.unwrap_or(1),
                black: r.black,
                blue: r.blue,
                exileable: r.exileable,
                legendary: r.legendary,
                delve: r.delve,
                effects: r.effects,
                abilities: r.abilities,
                mana_abilities: r.mana_abilities.clone(),
                adventure: r.adventure,
            }),
            CardType::Instant => CardKind::Instant(SpellData {
                mana_cost: r.mana_cost,
                blue: r.blue,
                black: r.black,
                exileable: r.exileable,
                target: r.target,
                counter_target: r.counter_target,
                requires: r.requires,
                effects: r.effects,
                alternate_costs: r.alternate_costs,
                delve: r.delve,
            }),
            CardType::Sorcery => CardKind::Sorcery(SpellData {
                mana_cost: r.mana_cost,
                blue: r.blue,
                black: r.black,
                exileable: r.exileable,
                target: r.target,
                counter_target: r.counter_target,
                requires: r.requires,
                effects: r.effects,
                alternate_costs: r.alternate_costs,
                delve: r.delve,
            }),
            CardType::Artifact => CardKind::Artifact(ArtifactData {
                mana_cost: r.mana_cost,
                effects: r.effects,
                abilities: r.abilities,
                mana_abilities: r.mana_abilities.clone(),
            }),
            CardType::Planeswalker => CardKind::Planeswalker(PlaneswalkerData {
                mana_cost: r.mana_cost,
                loyalty: r.loyalty.unwrap_or(0),
                effects: r.effects,
                abilities: r.abilities,
            }),
            CardType::Enchantment => CardKind::Enchantment,
        };
        CardDef { name: r.name, play_weight: r.play_weight, kind }
    }
}

impl<'de> Deserialize<'de> for CardDef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        RawCardDef::deserialize(d).map(CardDef::from)
    }
}


fn stage_label(turn: u8) -> &'static str {
    match turn {
        0..=3 => "Early",
        4..=5 => "Mid",
        _ => "Late",
    }
}

// ── Game state ────────────────────────────────────────────────────────────────

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
    /// Pre-computed annotation for permanents (e.g. Murktide size from delve count).
    /// If Some, overrides random annotation_options pick at resolution time.
    annotation: Option<String>,
    /// When true, the spell is an adventure face: on resolution it goes to exile + on_adventure
    /// instead of the graveyard.
    adventure_exile: bool,
    /// The physical card's name (creature half) when adventure_exile=true; used for exile placement.
    adventure_card_name: Option<String>,
    /// Adventure face data (name, effects, target) used at resolution when adventure_exile=true.
    adventure_face: Option<AdventureFace>,
}


// ── Turn structure ────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Debug)]
enum PhaseKind {
    Beginning,
    PreCombatMain,
    Combat,
    PostCombatMain,
    End,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum StepKind {
    Untap,
    Upkeep,
    Draw,
    BeginCombat,
    DeclareAttackers,
    DeclareBlockers,
    CombatDamage,
    EndCombat,
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
        matches!(self.kind, PhaseKind::PreCombatMain | PhaseKind::PostCombatMain)
    }
}

// ── Priority actions ──────────────────────────────────────────────────────────

#[derive(Clone)]
enum PriorityAction {
    /// Land drop: AP only, does NOT pass priority. Carries the chosen land name.
    LandDrop(String),
    /// Activate a permanent ability. Carries source name + ability def. Uses the stack, passes priority after.
    ActivateAbility(String, AbilityDef),
    /// Intent to cast a spell. No resources are spent until `handle_priority_round` accepts and
    /// commits this action. The framework validates legality (sorcery-speed, etc.) there.
    ///
    /// `preferred_cost` — pre-selected alternate cost (used by `respond_with_counter`).
    /// `counters`       — stack index this spell will counter (counterspell only).
    CastSpell { name: String, preferred_cost: Option<AlternateCost>, counters: Option<usize> },
    /// Cast the adventure face of a card in hand.
    CastAdventure { card_name: String },
    /// Cast the creature face of a card currently on adventure in exile.
    CastFromAdventure { card_name: String },
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

fn combat_phase() -> Phase {
    Phase {
        kind: PhaseKind::Combat,
        steps: vec![
            Step { kind: StepKind::BeginCombat,      prio: true },
            Step { kind: StepKind::DeclareAttackers, prio: true },
            Step { kind: StepKind::DeclareBlockers,  prio: true },
            Step { kind: StepKind::CombatDamage,     prio: true },
            Step { kind: StepKind::EndCombat,        prio: true },
        ],
    }
}

fn post_combat_main_phase() -> Phase {
    Phase { kind: PhaseKind::PostCombatMain, steps: vec![] }
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

// ── Mana cost ─────────────────────────────────────────────────────────────────

#[derive(Clone, Default, Debug)]
struct ManaCost {
    w: i32,
    u: i32,
    b: i32,
    r: i32,
    g: i32,
    c: i32,       // colorless pips {C}
    generic: i32, // any-color pips {1}, {2}, ...
}

impl ManaCost {
    fn total_specific(&self) -> i32 { self.w + self.u + self.b + self.r + self.g + self.c }
    fn mana_value(&self) -> i32 { self.total_specific() + self.generic }
}

/// Parse a mana cost string into a ManaCost.
/// Leading digits → generic; W/U/B/R/G/C → specific color pips.
/// Empty string = no castable mana cost (alt-cost-only or uncostable cards like Daze/FoW).
/// "0" = genuinely free (Lotus Petal, LED).
fn parse_mana_cost(cost: &str) -> ManaCost {
    let mut mc = ManaCost::default();
    let mut chars = cost.trim().chars().peekable();
    let mut num = String::new();
    while chars.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) {
        num.push(chars.next().unwrap());
    }
    if !num.is_empty() {
        mc.generic = num.parse().unwrap_or(0);
    }
    for c in chars {
        match c {
            'W' => mc.w += 1,
            'U' => mc.u += 1,
            'B' => mc.b += 1,
            'R' => mc.r += 1,
            'G' => mc.g += 1,
            'C' => mc.c += 1,
            _ => mc.generic += 1,
        }
    }
    mc
}

/// Total mana value (CMC) of a cost string.
fn mana_value(cost: &str) -> i32 {
    parse_mana_cost(cost).mana_value()
}

// ── Mana pool ─────────────────────────────────────────────────────────────────

/// Mana tracking: all 5 colors + colorless tracked separately; total covers all available mana.
#[derive(Clone, Default)]
struct ManaPool {
    w: i32,
    u: i32,
    b: i32,
    r: i32,
    g: i32,
    c: i32,
    total: i32,
}

impl ManaPool {
    fn can_pay(&self, cost: &ManaCost) -> bool {
        self.w >= cost.w && self.u >= cost.u && self.b >= cost.b &&
        self.r >= cost.r && self.g >= cost.g && self.c >= cost.c &&
        self.total >= cost.total_specific() + cost.generic
    }

    fn spend(&mut self, cost: &ManaCost) {
        self.w -= cost.w;
        self.u -= cost.u;
        self.b -= cost.b;
        self.r -= cost.r;
        self.g -= cost.g;
        self.c -= cost.c;
        self.total -= cost.total_specific() + cost.generic;
        // Generic costs may consume colored mana; reduce excess colored counters
        // proportionally so the invariant total >= sum_of_specifics holds.
        let color_sum = self.w + self.u + self.b + self.r + self.g + self.c;
        let excess = color_sum.saturating_sub(self.total);
        if excess > 0 {
            // Drain colors in priority: b, u, w, r, g, c
            let mut remaining = excess;
            for field in [&mut self.b, &mut self.u, &mut self.w, &mut self.r, &mut self.g, &mut self.c] {
                let drain = remaining.min(*field);
                *field -= drain;
                remaining -= drain;
                if remaining == 0 { break; }
            }
        }
    }

    fn drain(&mut self) {
        *self = ManaPool::default();
    }
}

// ── Mana potential accumulation ───────────────────────────────────────────────

/// Accumulate one source's potential contribution into the pool.
/// A source (land or permanent) contributes at most 1 to `total` because a single
/// tap or sacrifice produces one mana. The per-color fields reflect which colors
/// that source *can* produce (union across all available abilities).
fn accumulate_source_potential(abilities: &[ManaAbility], tapped: bool, p: &mut ManaPool) {
    let avail: Vec<_> = abilities.iter()
        .filter(|ma| !ma.tap_self || !tapped)
        .collect();
    if avail.is_empty() { return; }
    p.total += 1;
    let mut produced = [false; 6]; // W U B R G C
    for ma in &avail {
        for ch in ma.produces.chars() {
            match ch {
                'W' => produced[0] = true,
                'U' => produced[1] = true,
                'B' => produced[2] = true,
                'R' => produced[3] = true,
                'G' => produced[4] = true,
                'C' => produced[5] = true,
                _ => {}
            }
        }
    }
    let [w, u, b, r, g, c] = produced.map(|x| x as i32);
    p.w += w; p.u += u; p.b += b; p.r += r; p.g += g; p.c += c;
}

// ── Simulation types ──────────────────────────────────────────────────────────

#[derive(Clone)]
struct SimLand {
    name: String,
    tapped: bool,
    basic: bool,
    #[allow(dead_code)]
    land_types: LandTypes,
    mana_abilities: Vec<ManaAbility>,
}

#[derive(Clone)]
struct SimPermanent {
    name: String,
    /// Display annotation, e.g. "Wizard" for Cavern of Souls.
    annotation: Option<String>,
    /// +1/+1 counters on this permanent (e.g. from Murktide Regent's delve).
    counters: i32,
    tapped: bool,
    damage: i32,
    /// True on the turn this permanent entered play (summoning sickness).
    entered_this_turn: bool,
    mana_abilities: Vec<ManaAbility>,
}

impl SimPermanent {
    #[allow(dead_code)]
    fn new(name: impl Into<String>) -> Self {
        SimPermanent {
            name: name.into(),
            annotation: None,
            counters: 0,
            tapped: false,
            damage: 0,
            entered_this_turn: true,
            mana_abilities: vec![],
        }
    }
}

impl SimLand {
    fn from_def(name: &str, def: &CardDef) -> Self {
        let land = def.as_land().expect("SimLand::from_def called with non-land");
        SimLand {
            name: name.to_string(),
            tapped: land.enters_tapped,
            basic: land.basic,
            land_types: land.land_types.clone(),
            mana_abilities: land.mana_abilities.clone(),
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
    /// Set at the start of the fateful main phase when black mana is unavailable; ensures a
    /// black-producing land is played this turn (bypasses the probability roll).
    must_land_drop: bool,
    /// True once Doomsday has been cast this game (prevents double-cast).
    dd_cast: bool,
    /// Number of non-land spells cast this turn; reset each Untap. Used for multi-spell probability.
    spells_cast_this_turn: u8,
    /// Mana produced but not yet spent; drains at end of each step/phase.
    pool: ManaPool,
    /// On-board actions pre-collected at the start of each main phase (populated by
    /// `collect_on_board_actions`). `ap_proactive` pops from this list instead of scanning flags.
    pending_actions: Vec<PriorityAction>,
    /// Cards currently in exile with the "on adventure" status — the creature face can be cast
    /// from here. Does NOT clear on Untap (adventure status persists across turns).
    on_adventure: Vec<String>,
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
            must_land_drop: false,
            dd_cast: false,
            spells_cast_this_turn: 0,
            pool: ManaPool::default(),
            pending_actions: Vec::new(),
            on_adventure: Vec::new(),
        }
    }

    /// Mana accessible right now: pool + what untapped lands and permanents can still produce.
    fn potential_mana(&self) -> ManaPool {
        let mut p = self.pool.clone();
        for l in &self.lands {
            if l.tapped { continue; }
            accumulate_source_potential(&l.mana_abilities, false, &mut p);
        }
        for perm in &self.permanents {
            accumulate_source_potential(&perm.mana_abilities, perm.tapped, &mut p);
        }
        p
    }

    /// Tap/sac lands and permanents to produce mana for the given cost.
    /// Priority: B, U, W, R, G, C (specific pips), then generic.
    /// Returns a log of activations (e.g. "tap Underground Sea → B").
    fn produce_mana(&mut self, cost: &ManaCost) -> Vec<String> {
        let mut log: Vec<String> = Vec::new();
        // For each specific color: find a source that produces it and activate it.
        let color_specs: [(i32, char, fn(&mut ManaPool, i32)); 6] = [
            (cost.b, 'B', |p, _| p.b += 1),
            (cost.u, 'U', |p, _| p.u += 1),
            (cost.w, 'W', |p, _| p.w += 1),
            (cost.r, 'R', |p, _| p.r += 1),
            (cost.g, 'G', |p, _| p.g += 1),
            (cost.c, 'C', |p, _| p.c += 1),
        ];

        for (need, color_char, add_color) in color_specs {
            let mut remaining = need;
            while remaining > 0 {
                // Try lands first.
                let land_idx = self.lands.iter().position(|l| {
                    !l.tapped && l.mana_abilities.iter().any(|ma| {
                        (!ma.tap_self || true) && ma.produces.contains(color_char)
                    })
                });
                if let Some(idx) = land_idx {
                    self.lands[idx].tapped = true;
                    add_color(&mut self.pool, 0);
                    self.pool.total += 1;
                    log.push(format!("tap {} → {}", self.lands[idx].name, color_char));
                    remaining -= 1;
                    continue;
                }
                // Try permanents.
                let perm_idx = self.permanents.iter().position(|p| {
                    p.mana_abilities.iter().any(|ma| {
                        (!ma.tap_self || !p.tapped) && ma.produces.contains(color_char)
                    })
                });
                if let Some(idx) = perm_idx {
                    let sac = self.permanents[idx].mana_abilities.iter()
                        .find(|ma| (!ma.tap_self || !self.permanents[idx].tapped) && ma.produces.contains(color_char))
                        .map(|ma| ma.sacrifice_self)
                        .unwrap_or(false);
                    if sac {
                        let name = self.permanents[idx].name.clone();
                        log.push(format!("sac {} → {}", name, color_char));
                        self.permanents.remove(idx);
                        self.graveyard.visible.push(name);
                    } else {
                        log.push(format!("tap {} → {}", self.permanents[idx].name, color_char));
                        self.permanents[idx].tapped = true;
                    }
                    add_color(&mut self.pool, 0);
                    self.pool.total += 1;
                    remaining -= 1;
                    continue;
                }
                break; // No more sources available.
            }
        }

        // Generic: tap any remaining untapped source with any mana ability.
        let mut remaining_generic = cost.generic;
        while remaining_generic > 0 {
            let land_idx = self.lands.iter().position(|l| {
                !l.tapped && !l.mana_abilities.is_empty()
            });
            if let Some(idx) = land_idx {
                self.lands[idx].tapped = true;
                self.pool.total += 1;
                log.push(format!("tap {} → 1", self.lands[idx].name));
                remaining_generic -= 1;
                continue;
            }
            let perm_idx = self.permanents.iter().position(|p| {
                !p.mana_abilities.is_empty() && p.mana_abilities.iter().any(|ma| !ma.tap_self || !p.tapped)
            });
            if let Some(idx) = perm_idx {
                let sac = self.permanents[idx].mana_abilities.iter()
                    .find(|ma| !ma.tap_self || !self.permanents[idx].tapped)
                    .map(|ma| ma.sacrifice_self)
                    .unwrap_or(false);
                if sac {
                    let name = self.permanents[idx].name.clone();
                    log.push(format!("sac {} → 1", name));
                    self.permanents.remove(idx);
                    self.graveyard.visible.push(name);
                } else {
                    log.push(format!("tap {} → 1", self.permanents[idx].name));
                    self.permanents[idx].tapped = true;
                }
                self.pool.total += 1;
                remaining_generic -= 1;
                continue;
            }
            break;
        }
        log
    }

    /// Produce mana and immediately spend it (the common pay-a-cost pattern).
    /// Returns the activation log from produce_mana for callers that want to emit it.
    fn pay_mana(&mut self, cost: &ManaCost) -> Vec<String> {
        let log = self.produce_mana(cost);
        self.pool.spend(cost);
        log
    }

    /// True if the player can currently produce at least one black mana (pool + untapped sources).
    fn has_black_mana(&self) -> bool {
        self.potential_mana().b > 0
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
    /// Set when Doomsday resolved — simulation ends successfully.
    success: bool,
    /// Active player this phase/step (for log context).
    current_ap: String,
    /// Current phase/step label (for log context).
    current_phase: String,
    /// Attackers declared this combat; cleared at EndCombat.
    combat_attackers: Vec<String>,
    /// Blocker assignments this combat: (attacker_name, blocker_name); cleared at EndCombat.
    combat_blocks: Vec<(String, String)>,
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
            success: false,
            current_ap: String::new(),
            current_phase: String::new(),
            combat_attackers: Vec::new(),
            combat_blocks: Vec::new(),
        }
    }

    /// True when the simulation should stop (either success or reroll).
    fn done(&self) -> bool {
        self.reroll || self.success
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

    /// Log each mana activation returned by pay_mana/produce_mana.
    fn log_mana_activations(&mut self, t: u8, who: &str, activations: Vec<String>) {
        for entry in activations {
            self.log(t, who, format!("→ {}", entry));
        }
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
                let mut tags: Vec<String> = Vec::new();
                if let Some(ann) = &p.annotation { tags.push(ann.clone()); }
                if p.counters > 0 { tags.push(format!("+{} counters", p.counters)); }
                let suffix = if tags.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", tags.join(", "))
                };
                writeln!(f, "    * {}{}", p.name, suffix)?;
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

    warn_unimplemented_cards(&all_cards, &deck_name, &config);
    warn_unimplemented_cards(&opp_cards, &opp_display, &config);

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
    if state.player(who).hand.hidden <= 0 {
        return None;
    }
    // On the fateful turn, if we can't produce black mana yet, require a black source.
    let need_black = fateful && !state.player(who).has_black_mana();
    let weighted: Vec<(usize, u32)> = library
        .iter()
        .enumerate()
        .filter_map(|(i, (_, def))| {
            let land = def.as_land()?;
            if need_black && !land.mana_abilities.iter().any(|ma| ma.produces.contains('B')) { return None; }
            Some((i, 1))
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
    debug_assert!(state.player(who).hand.hidden >= 0, "hand.hidden went negative playing land");
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
    kind: &CardKind,
    basic: bool,
    def: Option<&CardDef>,
) -> bool {
    match type_str {
        "nonbasic_land" => matches!(kind, CardKind::Land(_)) && !basic,
        "land"          => matches!(kind, CardKind::Land(_)),
        "creature"      => matches!(kind, CardKind::Creature(_)),
        "planeswalker"  => matches!(kind, CardKind::Planeswalker(_)),
        "artifact"      => matches!(kind, CardKind::Artifact(_)),
        "any"           => true,
        "creature_mv_lt4" => {
            matches!(kind, CardKind::Creature(_))
                && def.map(|d| mana_value(d.mana_cost()) < 4).unwrap_or(true)
        }
        "creature_nonblack" => {
            matches!(kind, CardKind::Creature(_))
                && def.map(|d| !d.is_black()).unwrap_or(true)
        }
        // Non-land permanent: since all entries in `permanents` are already non-land,
        // this is true for any permanent (lands are in `lands`, not `permanents`).
        "permanent_nonland" => !matches!(kind, CardKind::Land(_)),
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
        .any(|l| matches_target_type(type_str, &CardKind::Land(LandData::default()), l.basic, None))
        || player.permanents.iter().any(|p| {
            match catalog_map.get(p.name.as_str()).copied() {
                Some(d) => matches_target_type(type_str, &d.kind, false, Some(d)),
                None    => type_str == "any",
            }
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
        let cost = parse_mana_cost(&ability.mana_cost);
        if !state.player(who).potential_mana().can_pay(&cost) {
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
/// Collect spells that can be proactively cast from an empty-stack main phase.
/// Counterspells are handled separately (via `respond_with_counter`); this only
/// covers cantrips, permanents, removal, and other proactive non-counter spells.
/// Returns true if the player can currently afford to cast `name` via any available cost.
fn spell_is_affordable(
    name: &str,
    def: &CardDef,
    state: &SimState,
    who: &str,
    library: &[(String, CardDef)],
) -> bool {
    let mut cost = parse_mana_cost(def.mana_cost());
    if def.delve() && cost.generic > 0 {
        let gy_len = state.player(who).graveyard.visible.len() as i32;
        cost.generic = (cost.generic - gy_len).max(0);
    }
    let mana_is_usable = !def.mana_cost().is_empty() && state.player(who).potential_mana().can_pay(&cost);
    if mana_is_usable { return true; }
    def.alternate_costs().iter().any(|c| can_pay_alternate_cost(c, state, who, name, library))
}

fn hand_ability_affordable(ability: &AbilityDef, state: &SimState, who: &str) -> bool {
    let player = state.player(who);
    if !ability.mana_cost.is_empty() {
        if !player.potential_mana().can_pay(&parse_mana_cost(&ability.mana_cost)) { return false; }
    }
    if ability.life_cost > 0 && player.life <= ability.life_cost { return false; }
    if ability.sacrifice_land && player.lands.is_empty() { return false; }
    true
}

fn collect_hand_actions(
    state: &SimState,
    who: &str,
    library: &[(String, CardDef)],
    catalog_map: &HashMap<&str, &CardDef>,
) -> Vec<PriorityAction> {
    if state.player(who).hand.hidden <= 0 {
        return Vec::new();
    }
    let permanents_in_play = &state.player(who).permanents;
    let opp_who = if who == "us" { "opp" } else { "us" };

    let mut actions: Vec<PriorityAction> = library
        .iter()
        .filter_map(|(name, def)| {
            if def.is_land() {
                return None;
            }
            let castable = def.effects().iter().any(|e| {
                e == "cantrip" || e == "permanent" || e == "destroy" || e == "doomsday"
                    || e.starts_with("discard:") || e.starts_with("reanimate:")
                    || e.starts_with("mana:")
            });
            if !castable {
                return None;
            }
            if def.legendary() && permanents_in_play.iter().any(|p| p.name == name.as_str()) {
                return None;
            }
            if let Some(tgt) = def.target() {
                if !has_valid_target(tgt, state, who, catalog_map) {
                    return None;
                }
            }
            let ok = def.requires().iter().all(|req| match req.as_str() {
                "opp_hand_nonempty" => state.player(opp_who).hand.hidden > 0,
                "us_gy_has_creature" => state.player(who).graveyard.visible.iter()
                    .any(|n| catalog_map.get(n.as_str())
                        .map(|d| d.is_creature())
                        .unwrap_or(false)),
                _ => true,
            });
            if !ok { return None; }
            if !spell_is_affordable(name, def, state, who, library) { return None; }
            Some(PriorityAction::CastSpell { name: name.clone(), preferred_cost: None, counters: None })
        })
        .collect();

    // In-hand abilities (cycling, channel, etc.) — one entry per card with a zone="hand" ability.
    for (name, def) in library {
        for ab in def.abilities().iter().filter(|ab| ab.zone == "hand") {
            if hand_ability_affordable(ab, state, who) {
                actions.push(PriorityAction::ActivateAbility(name.clone(), ab.clone()));
            }
        }
    }

    // Adventure spell face: offer casting the adventure (goes to exile on resolution).
    for (name, def) in library {
        let Some(face) = def.adventure() else { continue; };
        if !face.mana_cost.is_empty() {
            let cost = parse_mana_cost(&face.mana_cost);
            if !state.player(who).potential_mana().can_pay(&cost) { continue; }
        }
        if let Some(ref tgt) = face.target {
            if !has_valid_target(tgt, state, who, catalog_map) { continue; }
        }
        actions.push(PriorityAction::CastAdventure { card_name: name.clone() });
    }

    actions
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
        if matches_target_type(type_str, &CardKind::Land(LandData::default()), land.basic, None) {
            candidates.push(land.name.clone());
        }
    }
    for perm in &state.player(&target_who).permanents {
        let def = catalog_map.get(perm.name.as_str()).copied();
        let matched = match def {
            Some(d) => matches_target_type(type_str, &d.kind, false, Some(d)),
            None    => type_str == "any",
        };
        if matched { candidates.push(perm.name.clone()); }
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
/// Filter syntax: `"land"`, `"land-island"`, `"land-swamp"`, `"land-island|swamp"`.
fn matches_search_filter(filter: &str, def: &CardDef) -> bool {
    let Some(land) = def.as_land() else { return false; };
    if filter == "land" { return true; }
    if let Some(types_str) = filter.strip_prefix("land-") {
        return types_str.split('|').any(|t| match t {
            "island"   => land.land_types.island,
            "swamp"    => land.land_types.swamp,
            "plains"   => land.land_types.plains,
            "mountain" => land.land_types.mountain,
            "forest"   => land.land_types.forest,
            _          => false,
        });
    }
    false
}

/// Pay the activation cost of an ability: mana, life, tap, and/or sacrifice.
/// Effects are NOT applied here — they happen when the ability resolves off the stack.
fn pay_activation_cost(
    state: &mut SimState,
    t: u8,
    who: &str,
    source_name: &str,
    ability: &AbilityDef,
    library: &mut Vec<(String, CardDef)>,
    catalog_map: &HashMap<&str, &CardDef>,
) {
    state.log(t, who, format!("Activate {} ability", source_name));

    // Pay mana cost.
    if !ability.mana_cost.is_empty() {
        let cost = parse_mana_cost(&ability.mana_cost);
        let mana_log = state.player_mut(who).pay_mana(&cost);
        state.log_mana_activations(t, who, mana_log);
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

    // Pay sacrifice cost (in-play permanent or land).
    if ability.sacrifice_self && ability.zone != "hand" {
        let is_land = catalog_map.get(source_name).map(|d| d.is_land()).unwrap_or(false);
        if is_land {
            state.player_mut(who).lands.retain(|l| l.name != source_name);
        } else {
            state.player_mut(who).permanents.retain(|p| p.name != source_name);
        }
        state.player_mut(who).graveyard.visible.push(source_name.to_string());
    }

    // Discard cost (zone="hand"): remove from library, send to graveyard.
    if ability.discard_self {
        if let Some(idx) = library.iter().position(|(n, _)| n == source_name) {
            library.remove(idx);
            state.player_mut(who).hand.hidden -= 1;
            state.player_mut(who).graveyard.visible.push(source_name.to_string());
        }
    }

    // Sacrifice-a-land cost (e.g. Edge of Autumn cycling).
    if ability.sacrifice_land {
        // Prefer non-mana-producing lands to preserve mana sources.
        let idx = state.player(who).lands.iter()
            .position(|l| l.mana_abilities.is_empty())
            .unwrap_or(0);
        if !state.player(who).lands.is_empty() {
            let land_name = state.player(who).lands[idx].name.clone();
            state.player_mut(who).lands.remove(idx);
            state.player_mut(who).graveyard.visible.push(land_name);
        }
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
    // draw:N — draw N cards (cycling effect).
    if let Some(rest) = ability.effect.strip_prefix("draw:") {
        let n: i32 = rest.parse().unwrap_or(1);
        state.player_mut(who).hand.hidden += n;
        state.log(t, who, format!("{} → draw {}", source_name, n));
        return;
    }

    // search:*:* — generic library search (e.g. fetchland: "search:land-island|swamp:play").
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
            // Prefer black-producing lands so fetches reliably find a black source.
            let black_candidates: Vec<usize> = candidates.iter()
                .copied()
                .filter(|&i| library[i].1.as_land().map_or(false, |l| l.land_types.swamp || l.mana_abilities.iter().any(|ma| ma.produces.contains('B'))))
                .collect();
            let pool = if !black_candidates.is_empty() { &black_candidates } else { &candidates };
            let idx = pool[rng.gen_range(0..pool.len())];
            let land = {
                let (lname, ldef) = &library[idx];
                let ld = ldef.as_land().expect("search result should be a land");
                SimLand {
                    name: lname.clone(),
                    tapped: false,
                    basic: ld.basic,
                    land_types: ld.land_types.clone(),
                    mana_abilities: ld.mana_abilities.clone(),
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

/// Return true if `spell_kind` is a valid target for a counterspell with `counter_target`.
fn matches_counter_target(counter_target: &str, spell_kind: &CardKind) -> bool {
    match counter_target {
        "any"              => true,
        "noncreature"      => !matches!(spell_kind, CardKind::Creature(_)),
        "nonland"          => !matches!(spell_kind, CardKind::Land(_)),
        "instant_or_sorcery" => matches!(spell_kind, CardKind::Instant(_) | CardKind::Sorcery(_)),
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
        let cost_mc = parse_mana_cost(&cost.mana_cost);
        if !player.potential_mana().can_pay(&cost_mc) {
            return false;
        }
    }
    if cost.exile_blue_from_hand {
        let has_pitch = library
            .iter()
            .any(|(n, d)| n.as_str() != source_name && !d.is_land() && d.is_blue());
        if !has_pitch {
            return false;
        }
    }
    if cost.bounce_island {
        if !player.lands.iter().any(|l| l.mana_abilities.iter().any(|ma| ma.produces.contains('U'))) {
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
    t: u8,
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
            .filter(|(_, (n, d))| n.as_str() != source_name && !d.is_land() && d.is_blue())
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
            .position(|l| l.mana_abilities.iter().any(|ma| ma.produces.contains('U')))
            .unwrap();
        let land_name = state.player(who).lands[idx].name.clone();
        state.player_mut(who).lands.remove(idx);
        state.player_mut(who).hand.hidden += 1;
        parts.push(format!("bounce {}", land_name));
    }
    if !cost.mana_cost.is_empty() {
        let cost_mc = parse_mana_cost(&cost.mana_cost);
        let mana_log = state.player_mut(who).pay_mana(&cost_mc);
        state.log_mana_activations(t, who, mana_log);
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
    let def = *catalog_map.get(name)?;
    let mut cost = parse_mana_cost(def.mana_cost());

    // Delve: reduce generic cost by exiling cards from the caster's graveyard.
    let to_exile: Vec<String> = if def.delve() && cost.generic > 0 {
        let gy = state.player(who).graveyard.visible.clone();
        let mut cards = Vec::new();
        for card in &gy {
            if cards.len() as i32 >= cost.generic { break; }
            cards.push(card.clone());
        }
        cost.generic -= cards.len() as i32;
        cards
    } else {
        Vec::new()
    };

    // Empty mana_cost means the card has no castable mana cost (alt-cost-only, or truly uncostable).
    // Use mana_cost = "0" in the catalog for genuinely free spells (Lotus Petal, LED).
    let has_alt_costs = !def.alternate_costs().is_empty();
    let mana_is_usable = !def.mana_cost().is_empty() && state.player(who).potential_mana().can_pay(&cost);

    // Select cost.
    let alt_cost: Option<AlternateCost> = if let Some(pc) = preferred_cost {
        // Caller specified the exact cost to use.
        Some(pc.clone())
    } else if !mana_is_usable {
        // Can't pay mana (or mana_cost is empty / alt-cost-only): try alternate costs.
        def.alternate_costs()
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
    let permanent_target = def.target()
        .and_then(|tgt| choose_permanent_target(tgt, who, state, catalog_map, rng));

    // Remove the spell from library.
    let pos = library.iter().position(|(n, _)| n.as_str() == name)?;
    library.remove(pos);

    // Pay cost and build a log label.
    let cast_label = if let Some(ref cost) = alt_cost {
        let parts = apply_alt_cost_components(cost, state, t, who, name, library, catalog_map, rng);
        state.player_mut(who).hand.hidden -= 1;
        debug_assert!(state.player(who).hand.hidden >= 0, "hand.hidden went negative casting {} (alt cost)", name);
        parts.join(", ")
    } else {
        let mana_log = state.player_mut(who).pay_mana(&cost);
        state.log_mana_activations(t, who, mana_log);
        state.player_mut(who).hand.hidden -= 1;
        debug_assert!(state.player(who).hand.hidden >= 0, "hand.hidden went negative casting {}", name);
        def.mana_cost().to_string()
    };

    // Exile delve cards from graveyard (cost payment).
    for card in &to_exile {
        state.player_mut(who).graveyard.visible.retain(|n| n != card);
        state.player_mut(who).exile.visible.push(card.clone());
    }

    // For delve permanents: encode +1/+1 counter count as "+N" in annotation.
    // Counters come from instants/sorceries exiled via delve (e.g. Murktide Regent).
    let annotation: Option<String> = if def.delve() && def.is_creature() {
        let count = to_exile.iter()
            .filter(|n| catalog_map.get(n.as_str())
                .map(|d| d.as_spell().is_some())
                .unwrap_or(false))
            .count() as i32;
        if count > 0 { Some(format!("+{}", count)) } else { None }
    } else {
        None
    };

    let delve_label = if to_exile.is_empty() {
        String::new()
    } else {
        format!(", delve: {}", to_exile.join(", "))
    };
    state.log(t, who, format!("Cast {} ({}{})", name, cast_label, delve_label));

    Some(StackItem {
        name: name.to_string(),
        owner: who.to_string(),
        is_ability: false,
        ability_def: None,
        counters: None,
        permanent_target,
        annotation,
        adventure_exile: false,
        adventure_card_name: None,
        adventure_face: None,
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
    // Adventure spell resolution: apply adventure effects, then exile the physical card.
    if item.adventure_exile {
        if let Some(ref face) = item.adventure_face {
            // Apply bounce effect if present.
            for effect in &face.effects {
                if effect == "bounce" {
                    if let Some((ref tw, ref tn)) = item.permanent_target {
                        if let Some(perm_idx) = state.player(tw).permanents.iter().position(|p| &p.name == tn) {
                            state.player_mut(tw).permanents.remove(perm_idx);
                            state.player_mut(tw).hand.hidden += 1;
                            state.log(t, &item.owner, format!("→ {} returned to {}'s hand", tn, tw));
                        }
                    }
                }
            }
            let card_name = item.adventure_card_name.as_deref().unwrap_or(&item.name);
            state.player_mut(&item.owner).exile.visible.push(card_name.to_string());
            state.player_mut(&item.owner).on_adventure.push(card_name.to_string());
            state.log(t, &item.owner, format!("{} resolves → {} on adventure in exile", face.name, card_name));
        }
        return;
    }

    let Some(&def) = catalog_map.get(item.name.as_str()) else {
        state.player_mut(&item.owner).graveyard.visible.push(item.name.clone());
        return;
    };

    let is_permanent = def.effects().iter().any(|e| e == "permanent");
    let is_cantrip = def.effects().iter().any(|e| e == "cantrip");

    // Destination: permanents or graveyard.
    if is_permanent {
        // Decode annotation: "+N" encodes +1/+1 counters; anything else is a display label.
        let (counters, annotation) = match item.annotation.as_deref() {
            Some(s) if s.starts_with('+') => {
                (s[1..].parse::<i32>().unwrap_or(0), None)
            }
            _ => {
                let ann = if item.annotation.is_some() {
                    item.annotation.clone()
                } else if !def.annotation_options().is_empty() {
                    Some(def.annotation_options()[rng.gen_range(0..def.annotation_options().len())].clone())
                } else {
                    None
                };
                (0, ann)
            }
        };
        state.player_mut(&item.owner).permanents.push(SimPermanent {
            name: item.name.clone(),
            annotation,
            counters,
            tapped: false,
            damage: 0,
            entered_this_turn: true,
            mana_abilities: catalog_map.get(item.name.as_str())
                .map_or_else(Vec::new, |d| d.mana_abilities().to_vec()),
        });
    } else {
        state.player_mut(&item.owner).graveyard.visible.push(item.name.clone());
    }

    // Apply all effects and collect secondary log lines, before logging resolution.
    // Each entry is (who, msg) so discard lines can be attributed to the discarding player.
    let mut secondary_logs: Vec<(String, String)> = Vec::new();
    for effect in def.effects() {
        let parts: Vec<&str> = effect.splitn(3, ':').collect();
        match parts.as_slice() {
            [e] if *e == "doomsday" => {
                state.success = true;
            }
            [e] if *e == "cantrip" => {
                state.player_mut(&item.owner).hand.hidden += 1;
            }
            ["discard", who_rel, n_str_and_flags] => {
                let (n_str, nonland_only) = match n_str_and_flags.split_once(':') {
                    Some((n, "nonland")) => (n, true),
                    _ => (*n_str_and_flags, false),
                };
                let n: i32 = n_str.parse().unwrap_or(0);
                let target_who = resolve_who(who_rel, &item.owner).to_string();
                let current = state.player(&target_who).hand.hidden.max(0);
                let actual = n.min(current);
                if actual > 0 {
                    let mut discarded: Vec<String> = Vec::new();
                    for _ in 0..actual {
                        let candidates: Vec<usize> = other_lib.iter().enumerate()
                            .filter(|(_, (_, d))| !nonland_only || !d.is_land())
                            .map(|(i, _)| i)
                            .collect();
                        if let Some(&idx) = candidates.get(rng.gen_range(0..candidates.len().max(1))) {
                            let (card, _) = other_lib.remove(idx);
                            state.player_mut(&target_who).hand.hidden -= 1;
                            state.player_mut(&target_who).graveyard.visible.push(card.clone());
                            discarded.push(card);
                        }
                    }
                    if !discarded.is_empty() {
                        secondary_logs.push((target_who.clone(), format!("→ {} discards: {}", target_who, discarded.join(", "))));
                    }
                }
            }
            ["life_loss", n_str] => {
                let n: i32 = n_str.parse().unwrap_or(0);
                state.lose_life(&item.owner, n);
                secondary_logs.push((item.owner.clone(), format!("→ lose {} life (now {})", n, state.life_of(&item.owner))));
            }
            ["mana", spec] => {
                // Parse MTG mana notation using parse_mana_cost.
                let mc = parse_mana_cost(spec);
                let owner = item.owner.clone();
                let pool = &mut state.player_mut(&owner).pool;
                pool.w += mc.w;
                pool.u += mc.u;
                pool.b += mc.b;
                pool.r += mc.r;
                pool.g += mc.g;
                pool.c += mc.c;
                pool.total += mc.mana_value();
                secondary_logs.push((item.owner.clone(), format!("→ add {} to pool", spec)));
            }
            ["reanimate", who_rel, type_filter] => {
                let target_who = resolve_who(who_rel, &item.owner).to_string();
                let candidates: Vec<String> = state.player(&target_who).graveyard.visible.iter()
                    .filter(|n| catalog_map.get(n.as_str())
                        .map(|d| matches_target_type(type_filter, &d.kind, false, Some(*d)))
                        .unwrap_or(false))
                    .cloned()
                    .collect();
                if !candidates.is_empty() {
                    let chosen = candidates[rng.gen_range(0..candidates.len())].clone();
                    state.player_mut(&target_who).graveyard.visible.retain(|n| n != &chosen);
                    state.player_mut(&target_who).permanents.push(SimPermanent {
                        name: chosen.clone(),
                        annotation: None,
                        counters: 0,
                        tapped: false,
                        damage: 0,
                        entered_this_turn: true,
                        mana_abilities: catalog_map.get(chosen.as_str())
                            .map_or_else(Vec::new, |d| d.mana_abilities().to_vec()),
                    });
                    secondary_logs.push((item.owner.clone(), format!("→ {} returns from graveyard", chosen)));
                }
            }
            _ => {}
        }
    }

    // Targeted destroy effect: applied before log so resolution line reflects final state.
    if def.effects().iter().any(|e| e == "destroy") {
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
    for (who, msg) in secondary_logs {
        state.log(t, &who, msg);
    }
}


/// Try to respond to `stack[target_idx]` by casting a counterspell.
///
/// When `probabilistic = true` a 35% base check is applied and per-cost `prob` rolls are
/// honoured (used for the opponent's optional counter decisions).
/// When `probabilistic = false` the attempt is deterministic — all payable options are
/// tried in order (used when we must protect Doomsday).
///
/// On success, returns a `CastSpell` intent with `counters = Some(target_idx)`.
/// No resources are spent; the caller (`handle_priority_round`) commits the action.
fn respond_with_counter(
    state: &SimState,
    stack: &[StackItem],
    target_idx: usize,
    responding_who: &str,
    responding_library: &[(String, CardDef)],
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
    probabilistic: bool,
) -> Option<PriorityAction> {
    let default_kind;
    let target_kind: &CardKind = match catalog_map.get(stack[target_idx].name.as_str()) {
        Some(d) => &d.kind,
        None => { default_kind = CardKind::Sorcery(SpellData::default()); &default_kind }
    };

    let target_owner = &stack[target_idx].owner;
    let target_has_untapped_lands = state.player(target_owner).lands.iter().any(|l| !l.tapped);

    let counterspells: Vec<String> = responding_library
        .iter()
        .filter(|(n, d)| {
            d.counter_target()
                .is_some_and(|ct| matches_counter_target(ct, target_kind))
                && !d.alternate_costs().is_empty()
                // Daze is useless if the opponent can pay the 1-mana tax.
                && !(n.as_str() == "Daze" && target_has_untapped_lands)
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
        let costs = catalog_map[cs_name.as_str()].alternate_costs().to_vec();
        for cost in &costs {
            if probabilistic {
                if let Some(p) = cost.prob {
                    if !rng.gen_bool(p) {
                        continue;
                    }
                }
            }
            if can_pay_alternate_cost(cost, state, responding_who, cs_name, responding_library) {
                return Some(PriorityAction::CastSpell {
                    name: cs_name.clone(),
                    preferred_cost: Some(cost.clone()),
                    counters: Some(target_idx),
                });
            }
        }
    }
    None
}



// ── Combat helpers ────────────────────────────────────────────────────────────

/// Return (power, toughness) for a permanent, adding any +1/+1 counters to base stats.
fn creature_stats(perm: &SimPermanent, def: Option<&CardDef>) -> (i32, i32) {
    let power     = def.and_then(|d| d.as_creature()).map(|c| c.power).unwrap_or(1);
    let toughness = def.and_then(|d| d.as_creature()).map(|c| c.toughness).unwrap_or(1);
    (power + perm.counters, toughness + perm.counters)
}

// ── New turn-structure functions ──────────────────────────────────────────────

/// Collect on-board actions (ability activations) to potentially take this main phase.
///
/// Performs a 75% roll per land/permanent with an available ability and returns the resulting
/// `Vec<PriorityAction>`. This replaces the old `want_to_activate` flag system: instead of
/// marking flags on `SimLand`/`SimPermanent`, we pre-collect a list of actions the player
/// intends to take, which `ap_proactive` then pops in order.
///
/// On the fateful (Doomsday) turn, if we can't produce any black mana, fetches that can search
/// for a black land are force-added (bypassing the 75% roll). If no such fetch exists,
/// `state.us.must_land_drop` is set as a side effect so the land drop is guaranteed.
fn collect_on_board_actions(
    state: &mut SimState,
    ap: &str,
    t: u8,
    dd_turn: u8,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> Vec<PriorityAction> {
    let mut actions: Vec<PriorityAction> = Vec::new();

    // 75% roll per land with an available ability.
    let land_names: Vec<String> = state.player(ap).lands.iter()
        .filter(|l| !l.tapped)
        .filter(|l| catalog_map.get(l.name.as_str())
            .map_or(false, |def| def.abilities().iter()
                .any(|ab| ability_available(ab, state, ap, true, catalog_map))))
        .map(|l| l.name.clone())
        .collect();
    for name in land_names {
        if rng.gen_bool(0.75) {
            if let Some(def) = catalog_map.get(name.as_str()) {
                if let Some(ab) = def.abilities().iter()
                    .find(|ab| ability_available(ab, state, ap, true, catalog_map))
                    .cloned()
                {
                    actions.push(PriorityAction::ActivateAbility(name, ab));
                }
            }
        }
    }

    // 75% roll per permanent with an available ability.
    let perm_names: Vec<(String, bool)> = state.player(ap).permanents.iter()
        .filter(|p| catalog_map.get(p.name.as_str())
            .map_or(false, |def| def.abilities().iter()
                .any(|ab| ability_available(ab, state, ap, !p.tapped, catalog_map))))
        .map(|p| (p.name.clone(), p.tapped))
        .collect();
    for (name, tapped) in perm_names {
        if rng.gen_bool(0.75) {
            if let Some(def) = catalog_map.get(name.as_str()) {
                if let Some(ab) = def.abilities().iter()
                    .find(|ab| ability_available(ab, state, ap, !tapped, catalog_map))
                    .cloned()
                {
                    actions.push(PriorityAction::ActivateAbility(name, ab));
                }
            }
        }
    }

    // Adventure creatures in exile: 75% roll to cast the creature face.
    let on_adventure_names: Vec<String> = state.player(ap).on_adventure.clone();
    for card_name in on_adventure_names {
        if let Some(&def) = catalog_map.get(card_name.as_str()) {
            let cost = parse_mana_cost(def.mana_cost());
            if !state.player(ap).potential_mana().can_pay(&cost) { continue; }
            if rng.gen_bool(0.75) {
                actions.push(PriorityAction::CastFromAdventure { card_name });
            }
        }
    }

    // Fateful turn override: force-include fetch lands that can search for a black source,
    // if we have no black mana. (These bypass the 75% roll.)
    if ap == "us" && t == dd_turn && !state.us.has_black_mana() {
        let can_search_black = |name: &str| catalog_map.get(name).map_or(false, |def|
            def.abilities().iter().any(|ab|
                ab.effect.starts_with("search:land-swamp")
                    || ab.effect.starts_with("search:land-island|swamp")
            )
        );
        let fetch_names: Vec<String> = state.us.lands.iter()
            .filter(|l| !l.tapped && can_search_black(&l.name))
            .map(|l| l.name.clone())
            .collect();
        if !fetch_names.is_empty() {
            for name in &fetch_names {
                // Add if not already in the list.
                if !actions.iter().any(|a| matches!(a, PriorityAction::ActivateAbility(n, _) if n == name)) {
                    if let Some(def) = catalog_map.get(name.as_str()) {
                        if let Some(ab) = def.abilities().iter()
                            .find(|ab| ability_available(ab, state, "us", true, catalog_map))
                            .cloned()
                        {
                            actions.push(PriorityAction::ActivateAbility(name.clone(), ab));
                        }
                    }
                }
            }
        } else {
            // No fetch available — ensure the land drop fires.
            state.us.must_land_drop = true;
        }
    }

    actions
}

/// NAP decision: if AP just acted, try to counter the top opposing spell; otherwise pass.
fn nap_action(
    state: &SimState,
    who: &str,
    last_action: &PriorityAction,
    stack: &[StackItem],
    us_lib: &mut Vec<(String, CardDef)>,
    opp_lib: &mut Vec<(String, CardDef)>,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> PriorityAction {
    let other_acted = matches!(last_action, PriorityAction::CastSpell { .. } | PriorityAction::ActivateAbility(..) | PriorityAction::CastAdventure { .. } | PriorityAction::CastFromAdventure { .. });
    if other_acted {
        let actor_lib: &[_] = if who == "us" { us_lib } else { opp_lib };
        for idx in (0..stack.len()).rev() {
            if stack[idx].owner != who && !stack[idx].is_ability {
                if let Some(action) = respond_with_counter(state, stack, idx, who, actor_lib, catalog_map, rng, true) {
                    if let PriorityAction::CastSpell { ref name, .. } = action {
                        eprintln!("[decision] {}: NAP counter {} targeting {}", who, name, stack[idx].name);
                    }
                    return action;
                }
                eprintln!("[decision] {}: NAP passes (no counter available for {})", who, stack[idx].name);
                break;
            }
        }
    }
    PriorityAction::Pass
}

/// AP reactive decision: respond to threats already on the stack.
/// Currently handles protecting our Doomsday if the opponent has countered it.
/// Returns Some(action) if we should respond, None to continue to proactive logic.
fn ap_react(
    state: &mut SimState,
    t: u8,
    who: &str,
    stack: &[StackItem],
    us_lib: &[(String, CardDef)],
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> Option<PriorityAction> {
    if who != "us" || stack.is_empty() {
        return None;
    }
    let top_idx = stack.len() - 1;
    let top = &stack[top_idx];
    let dd_countered = !top.is_ability
        && top.owner != "us"
        && top.counters
            .map(|ti| stack[ti].name == "Doomsday" && stack[ti].owner == "us")
            .unwrap_or(false);
    if !dd_countered {
        return None;
    }
    Some(
        if let Some(action) = respond_with_counter(state, stack, top_idx, "us", us_lib, catalog_map, rng, false) {
            action
        } else {
            state.log(t, "us", "⚠ Doomsday countered — could not protect");
            state.reroll = true;
            PriorityAction::Pass
        },
    )
}

/// AP proactive decision: land drop, abilities, Doomsday setup, and general spells.
/// Only called when the AP is in the main phase.
fn ap_proactive(
    state: &mut SimState,
    t: u8,
    who: &str,
    dd_turn: u8,
    stack: &[StackItem],
    us_lib: &mut Vec<(String, CardDef)>,
    opp_lib: &mut Vec<(String, CardDef)>,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> PriorityAction {
    // Land drop (sorcery speed: requires empty stack).
    if stack.is_empty() && state.player(who).land_drop_available {
        let fateful = who == "us" && t == dd_turn;
        // On the fateful turn, skip the land drop if Doomsday is already castable — playing
        // a land might spend our last card in hand and leave us unable to cast Doomsday.
        let dd_already_castable = fateful && !state.us.dd_cast
            && state.us.potential_mana().can_pay(&ManaCost { b: 3, ..Default::default() })
            && us_lib.iter().any(|(n, _)| n == "Doomsday");
        if !dd_already_castable {
            let force = state.player(who).must_land_drop;
            let lib: &[(String, CardDef)] = if who == "us" { us_lib } else { opp_lib };
            let land_count = lib.iter().filter(|(_, d)| d.is_land()).count();
            if land_count > 0 {
                // T1=100%, T2=90%, T3=80%, T4+=70%; forced to 100% when must_land_drop is set.
                let prob = if force { 1.0 } else { match t { 1 => 1.0, 2 => 0.9, 3 => 0.80, _ => 0.70 } };
                if rng.gen::<f64>() < prob {
                    if let Some(name) = choose_land_name(state, who, lib, fateful, rng) {
                        state.player_mut(who).must_land_drop = false;
                        return PriorityAction::LandDrop(name);
                    }
                }
            }
            state.player_mut(who).must_land_drop = false;
            state.player_mut(who).land_drop_available = false;
        }
    }

    // On-board actions: pop the first pending action (pre-rolled at phase start).
    if let Some(action) = state.player(who).pending_actions.first().cloned() {
        // Verify it's still valid before committing (source might have been tapped/sacrificed).
        let still_valid = match &action {
            PriorityAction::ActivateAbility(source, ab) => {
                let source_untapped = state.player(who).lands.iter().any(|l| l.name == *source && !l.tapped)
                    || state.player(who).permanents.iter().any(|p| p.name == *source && (!p.tapped || ab.sacrifice_self));
                ability_available(ab, state, who, source_untapped, catalog_map)
            }
            _ => false,
        };
        state.player_mut(who).pending_actions.remove(0);
        if still_valid {
            return action;
        }
        // Fall through to hand actions.
    }

    // Hand actions: only on empty stack.
    if !stack.is_empty() {
        return PriorityAction::Pass;
    }

    let actor_lib: &[(String, CardDef)] = if who == "us" { us_lib } else { opp_lib };
    let actions = collect_hand_actions(state, who, actor_lib, catalog_map);
    if actions.is_empty() {
        let pool = &state.player(who).pool;
        let hand = state.player(who).hand.hidden;
        eprintln!("[decision] {}: no castable spells (pool B={} U={} tot={}, hand={})",
            who, pool.b, pool.u, pool.total, hand);
        if who == "us" && t == dd_turn && !state.us.dd_cast {
            let dd_in_lib = actor_lib.iter().filter(|(n, _)| n == "Doomsday").count();
            eprintln!("[decision] fateful turn: Doomsday not cast — hand={}, dd_in_lib={}, potential B={} tot={}",
                hand, dd_in_lib, pool.b, pool.total);
        }
        return PriorityAction::Pass;
    }

    // Fateful turn prioritization: Doomsday > Dark Ritual > anything else.
    let fateful = who == "us" && t == dd_turn && !state.us.dd_cast;
    let action = if fateful && actions.iter().any(|a| matches!(a, PriorityAction::CastSpell { name, .. } if name == "Doomsday")) {
        PriorityAction::CastSpell { name: "Doomsday".to_string(), preferred_cost: None, counters: None }
    } else if fateful && actions.iter().any(|a| matches!(a, PriorityAction::CastSpell { name, .. } if name == "Dark Ritual")) {
        PriorityAction::CastSpell { name: "Dark Ritual".to_string(), preferred_cost: None, counters: None }
    } else {
        // General casting — decaying probability for multi-spell turns.
        // 1st spell: always; 2nd: 30%; 3rd+: 10%.
        // Override to 1.0 if mana is floating in the pool: we generated it on purpose.
        let has_floating = state.player(who).pool.total > 0;
        let cast_prob = if has_floating { 1.0 } else {
            match state.player(who).spells_cast_this_turn { 0 => 1.0, 1 => 0.30, _ => 0.10 }
        };
        if rng.gen::<f64>() >= cast_prob {
            return PriorityAction::Pass;
        }
        actions[rng.gen_range(0..actions.len())].clone()
    };

    if let PriorityAction::CastSpell { ref name, .. } = action {
        eprintln!("[decision] {}: proactive cast {} (options: {})", who, name,
            actions.iter().filter_map(|a| if let PriorityAction::CastSpell { name, .. } = a { Some(name.as_str()) } else { None }).collect::<Vec<_>>().join(", "));
    }
    action
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
    if who != ap {
        if stack.is_empty() { return PriorityAction::Pass; }
        return nap_action(state, who, last_action, stack, us_lib, opp_lib, catalog_map, rng);
    }
    if state.current_phase != "Main" {
        return PriorityAction::Pass;
    }
    if let Some(action) = ap_react(state, t, who, stack, us_lib, catalog_map, rng) {
        return action;
    }
    ap_proactive(state, t, who, dd_turn, stack, us_lib, opp_lib, catalog_map, rng)
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
                let actor_lib = if who == "us" { &mut *us_lib } else { &mut *opp_lib };
                pay_activation_cost(state, t, &who, source_name, ability, actor_lib, catalog_map);
                // Push ability stack item; ability_def carries the effect for resolution.
                stack.push(StackItem {
                    name: source_name.clone(),
                    owner: who.clone(),
                    is_ability: true,
                    ability_def: Some(ability.clone()),
                    counters: None,
                    permanent_target: None,
                    annotation: None,
                    adventure_exile: false,
                    adventure_card_name: None,
                    adventure_face: None,
                });
                let next = if who == ap { nap } else { ap };
                priority_holder = next.to_string();
                last_passer = None;
            }
            PriorityAction::CastAdventure { ref card_name } => {
                // Get adventure face.
                let face = catalog_map.get(card_name.as_str())
                    .and_then(|d| d.adventure())
                    .cloned();
                let Some(face) = face else {
                    last_passer = Some(who.clone());
                    priority_holder = if who == ap { nap.to_string() } else { ap.to_string() };
                    continue;
                };
                // Sorcery-speed check.
                let is_sorcery = face.card_type == "sorcery";
                if is_sorcery && !stack.is_empty() {
                    eprintln!("[priority] adventure sorcery {} on non-empty stack, treating as Pass", face.name);
                    last_passer = Some(who.clone());
                    priority_holder = if who == ap { nap.to_string() } else { ap.to_string() };
                    continue;
                }
                // Remove card from library (hand).
                let actor_lib = if who == "us" { &mut *us_lib } else { &mut *opp_lib };
                let Some(pos) = actor_lib.iter().position(|(n, _)| n == card_name) else {
                    last_passer = Some(who.clone());
                    priority_holder = if who == ap { nap.to_string() } else { ap.to_string() };
                    continue;
                };
                actor_lib.remove(pos);
                // Pay mana.
                if !face.mana_cost.is_empty() {
                    let cost = parse_mana_cost(&face.mana_cost);
                    let mana_log = state.player_mut(&who).pay_mana(&cost);
                    state.log_mana_activations(t, &who, mana_log);
                }
                state.player_mut(&who).hand.hidden -= 1;
                // Choose target.
                let permanent_target = face.target.as_deref()
                    .and_then(|tgt| choose_permanent_target(tgt, &who, state, catalog_map, rng));
                state.log(t, &who, format!("Cast {} (adventure, {})", face.name, face.mana_cost));
                // Push StackItem.
                stack.push(StackItem {
                    name: face.name.clone(),
                    owner: who.clone(),
                    is_ability: false,
                    ability_def: None,
                    counters: None,
                    permanent_target,
                    annotation: None,
                    adventure_exile: true,
                    adventure_card_name: Some(card_name.clone()),
                    adventure_face: Some(face),
                });
                state.player_mut(&who).spells_cast_this_turn += 1;
                let next = if who == ap { nap } else { ap };
                priority_holder = next.to_string();
                last_passer = None;
            }
            PriorityAction::CastFromAdventure { ref card_name } => {
                // Verify still on adventure.
                if !state.player(&who).on_adventure.iter().any(|n| n == card_name) {
                    last_passer = Some(who.clone());
                    priority_holder = if who == ap { nap.to_string() } else { ap.to_string() };
                    continue;
                }
                let Some(&def) = catalog_map.get(card_name.as_str()) else {
                    last_passer = Some(who.clone());
                    priority_holder = if who == ap { nap.to_string() } else { ap.to_string() };
                    continue;
                };
                // Check still affordable.
                let cost = parse_mana_cost(def.mana_cost());
                if !state.player(&who).potential_mana().can_pay(&cost) {
                    last_passer = Some(who.clone());
                    priority_holder = if who == ap { nap.to_string() } else { ap.to_string() };
                    continue;
                }
                // Remove from on_adventure and exile.
                state.player_mut(&who).on_adventure.retain(|n| n != card_name);
                state.player_mut(&who).exile.visible.retain(|n| n != card_name);
                // Pay mana (no hand.hidden change — comes from exile).
                let mana_log = state.player_mut(&who).pay_mana(&cost);
                state.log_mana_activations(t, &who, mana_log);
                state.log(t, &who, format!("Cast {} from adventure ({})", card_name, def.mana_cost()));
                stack.push(StackItem {
                    name: card_name.clone(),
                    owner: who.clone(),
                    is_ability: false,
                    ability_def: None,
                    counters: None,
                    permanent_target: None,
                    annotation: None,
                    adventure_exile: false,
                    adventure_card_name: None,
                    adventure_face: None,
                });
                state.player_mut(&who).spells_cast_this_turn += 1;
                let next = if who == ap { nap } else { ap };
                priority_holder = next.to_string();
                last_passer = None;
            }
            PriorityAction::CastSpell { ref name, ref preferred_cost, counters } => {
                // Framework legality check: non-instant spells require an empty stack.
                // No resources have been spent yet, so it is safe to drop an illegal action.
                let is_instant = catalog_map.get(name.as_str())
                    .map(|d| d.is_instant())
                    .unwrap_or(false);
                if !is_instant && !stack.is_empty() {
                    eprintln!("[priority] BUG: sorcery-speed {} on non-empty stack (stack={}), treating as Pass", name,
                        stack.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", "));
                    debug_assert!(false, "BUG: sorcery-speed cast of {} on non-empty stack", name);
                    // Treat as Pass — no resources spent, no card removed.
                    last_passer = Some(who.clone());
                    let other = if who == ap { nap } else { ap };
                    priority_holder = other.to_string();
                } else {
                    // Commit: pay costs and create the StackItem.
                    let actor_lib = if who == "us" { &mut *us_lib } else { &mut *opp_lib };
                    if let Some(mut item) = cast_spell(state, t, &who, name, actor_lib,
                                                       preferred_cost.as_ref(), catalog_map, rng) {
                        item.counters = counters;
                        // Bookkeeping that was previously in the decision functions.
                        if name == "Doomsday" && who == "us" { state.us.dd_cast = true; }
                        state.player_mut(&who).spells_cast_this_turn += 1;
                        stack.push(item);
                        let next = if who == ap { nap } else { ap };
                        priority_holder = next.to_string();
                        last_passer = None;
                    } else {
                        // cast_spell failed (can't afford or card unavailable) — treat as Pass
                        // to avoid an infinite loop where the same failing action repeats.
                        let pool = &state.player(&who).pool;
                        eprintln!("[priority] BUG: cast_spell failed for {} by {} (pool B={} U={} tot={}, hand={})",
                            name, who, pool.b, pool.u, pool.total, state.player(&who).hand.hidden);
                        debug_assert!(false, "BUG: cast_spell failed for {} — decision function returned unaffordable/unavailable spell", name);
                        last_passer = Some(who.clone());
                        let other = if who == ap { nap } else { ap };
                        priority_holder = other.to_string();
                    }
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

        if state.done() {
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
        StepKind::Untap           => "Untap",
        StepKind::Upkeep          => "Upkeep",
        StepKind::Draw            => "Draw",
        StepKind::BeginCombat     => "BeginCombat",
        StepKind::DeclareAttackers => "DeclareAttackers",
        StepKind::DeclareBlockers  => "DeclareBlockers",
        StepKind::CombatDamage    => "CombatDamage",
        StepKind::EndCombat       => "EndCombat",
        StepKind::End             => "EndStep",
        StepKind::Cleanup         => "Cleanup",
    }.to_string();
    match step.kind {
        StepKind::Untap => {
            for land in &mut state.player_mut(ap).lands {
                land.tapped = false;
            }
            for perm in &mut state.player_mut(ap).permanents {
                perm.tapped = false;
                perm.entered_this_turn = false;
            }
            state.player_mut(ap).land_drop_available = true;
            state.player_mut(ap).spells_cast_this_turn = 0;
            state.player_mut(ap).pending_actions.clear();
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
            for perm in &mut state.player_mut(ap).permanents {
                perm.damage = 0;
            }
        }
        StepKind::DeclareAttackers => {
            let nap = if ap == "us" { "opp" } else { "us" };
            // Sum of untapped NAP creature power (tapped creatures cannot block).
            let nap_power: i32 = state.player(nap).permanents.iter()
                .filter(|p| !p.tapped)
                .filter_map(|p| {
                    let def = catalog_map.get(p.name.as_str());
                    if def.map(|d| d.is_creature()).unwrap_or(false) {
                        Some(creature_stats(p, def.copied()).0)
                    } else { None }
                })
                .sum();
            let attackers: Vec<String> = state.player(ap).permanents.iter()
                .filter(|p| !p.tapped && !p.entered_this_turn)
                .filter_map(|p| {
                    let def = catalog_map.get(p.name.as_str());
                    if def.map(|d| d.is_creature()).unwrap_or(false) {
                        let (_, tgh) = creature_stats(p, def.copied());
                        if tgh > nap_power { Some(p.name.clone()) } else { None }
                    } else { None }
                })
                .collect();
            if !attackers.is_empty() {
                state.log(t, ap, format!("Declare attackers: {}", attackers.join(", ")));
                for name in &attackers {
                    if let Some(p) = state.player_mut(ap).permanents.iter_mut().find(|p| &p.name == name) {
                        p.tapped = true;
                    }
                }
            }
            state.combat_attackers = attackers;
        }
        StepKind::DeclareBlockers => {
            let nap = if ap == "us" { "opp" } else { "us" };
            let mut used_blockers: std::collections::HashSet<String> = Default::default();
            let mut blocks: Vec<(String, String)> = Vec::new();
            for atk_name in state.combat_attackers.clone() {
                let atk_def = catalog_map.get(atk_name.as_str()).copied();
                let (atk_pow, atk_tgh) = {
                    let atk_perm = state.player(ap).permanents.iter().find(|p| p.name == atk_name);
                    match atk_perm {
                        Some(p) => creature_stats(p, atk_def),
                        None    => continue,
                    }
                };
                let blocker = state.player(nap).permanents.iter()
                    .filter(|p| !p.tapped && !used_blockers.contains(&p.name))
                    .filter_map(|p| {
                        let def = catalog_map.get(p.name.as_str()).copied();
                        if def.map(|d| d.is_creature()).unwrap_or(false) {
                            let (blk_pow, blk_tgh) = creature_stats(p, def);
                            // Good block: kills attacker OR both survive (touch butts). Not a chump.
                            let good_block = blk_pow >= atk_tgh || atk_pow < blk_tgh;
                            if good_block { Some(p.name.clone()) } else { None }
                        } else { None }
                    })
                    .next();
                if let Some(blk_name) = blocker {
                    state.log(t, nap, format!("{} blocks {}", blk_name, atk_name));
                    used_blockers.insert(blk_name.clone());
                    blocks.push((atk_name, blk_name));
                }
            }
            state.combat_blocks = blocks;
        }
        StepKind::CombatDamage => {
            if !state.combat_attackers.is_empty() {
                let nap = if ap == "us" { "opp" } else { "us" };
                let attackers   = state.combat_attackers.clone();
                let block_pairs = state.combat_blocks.clone();
                let blocked: std::collections::HashSet<&str> = block_pairs.iter()
                    .map(|(a, _)| a.as_str()).collect();

                let mut player_damage = 0i32;

                for (atk_name, blk_name) in &block_pairs {
                    let atk_pow = {
                        let p = state.player(ap).permanents.iter().find(|p| p.name == *atk_name);
                        p.map(|p| creature_stats(p, catalog_map.get(atk_name.as_str()).copied()).0)
                         .unwrap_or(1)
                    };
                    let blk_pow = {
                        let p = state.player(nap).permanents.iter().find(|p| p.name == *blk_name);
                        p.map(|p| creature_stats(p, catalog_map.get(blk_name.as_str()).copied()).0)
                         .unwrap_or(1)
                    };
                    if let Some(p) = state.player_mut(ap).permanents.iter_mut().find(|p| p.name == *atk_name) {
                        p.damage += blk_pow;
                    }
                    if let Some(p) = state.player_mut(nap).permanents.iter_mut().find(|p| p.name == *blk_name) {
                        p.damage += atk_pow;
                    }
                }

                for atk_name in &attackers {
                    if !blocked.contains(atk_name.as_str()) {
                        let atk_pow = {
                            let p = state.player(ap).permanents.iter().find(|p| p.name == *atk_name);
                            p.map(|p| creature_stats(p, catalog_map.get(atk_name.as_str()).copied()).0)
                             .unwrap_or(1)
                        };
                        player_damage += atk_pow;
                    }
                }

                if player_damage > 0 {
                    state.lose_life(nap, player_damage);
                    state.log(t, ap, format!("Combat: {} unblocked damage to {} (life: {})", player_damage, nap, state.life_of(nap)));
                }

                // SBAs: lethal damage check on both boards.
                for owner in [ap, nap] {
                    let dying: Vec<String> = state.player(owner).permanents.iter()
                        .filter_map(|p| {
                            if p.damage == 0 { return None; }
                            let def = catalog_map.get(p.name.as_str()).copied();
                            let (_, tgh) = creature_stats(p, def);
                            if p.damage >= tgh { Some(p.name.clone()) } else { None }
                        })
                        .collect();
                    for name in dying {
                        state.log(t, owner, format!("{} dies", name));
                        state.player_mut(owner).permanents.retain(|p| p.name != name);
                        state.player_mut(owner).graveyard.visible.push(name);
                    }
                }
            }
        }
        StepKind::EndCombat => {
            state.combat_attackers.clear();
            state.combat_blocks.clear();
        }
        StepKind::Upkeep | StepKind::BeginCombat | StepKind::End => {
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
        if state.done() {
            return;
        }
    }
    if phase.is_main_phase() {
        state.current_phase = "Main".to_string();
        let on_board = collect_on_board_actions(state, ap, t, dd_turn, catalog_map, rng);
        state.player_mut(ap).pending_actions = on_board;
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
    if state.done() { return; }

    do_phase(state, t, ap, &main_phase(), dd_turn, on_play, us_lib, opp_lib, catalog_map, rng);
    if state.done() { return; }

    do_phase(state, t, ap, &combat_phase(), dd_turn, on_play, us_lib, opp_lib, catalog_map, rng);
    if state.done() { return; }

    do_phase(state, t, ap, &post_combat_main_phase(), dd_turn, on_play, us_lib, opp_lib, catalog_map, rng);
    if state.done() { return; }

    do_phase(state, t, ap, &end_phase(), dd_turn, on_play, us_lib, opp_lib, catalog_map, rng);
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
            if state.done() { break; }
        }
        {
            let mut us_lib = std::mem::take(&mut state.us.library);
            let mut opp_lib = std::mem::take(&mut state.opp.library);
            do_turn(&mut state, t, "us", turn, on_play, &mut us_lib, &mut opp_lib, &catalog_map, rng);
            state.us.library = us_lib;
            state.opp.library = opp_lib;
            if state.done() { break; }
        }
        if on_play && t < turn {
            let mut us_lib = std::mem::take(&mut state.us.library);
            let mut opp_lib = std::mem::take(&mut state.opp.library);
            do_turn(&mut state, t, "opp", turn, on_play, &mut us_lib, &mut opp_lib, &catalog_map, rng);
            state.us.library = us_lib;
            state.opp.library = opp_lib;
            if state.done() { break; }
        }
    }

    if !state.success {
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

// ── Implementation checking ───────────────────────────────────────────────────

/// True if `def` has enough simulation implementation to do something during a game.
///
/// - Lands are always actionable (played via land-drop logic).
/// - Spells need at least one castable effect, a counter_target, or abilities.
fn card_has_implementation(def: &CardDef) -> bool {
    if def.is_land() { return true; }
    if !def.abilities().is_empty() { return true; }
    if def.counter_target().is_some() { return true; }
    def.effects().iter().any(|e| {
        e == "cantrip" || e == "permanent" || e == "destroy" || e == "doomsday"
            || e.starts_with("discard:") || e.starts_with("mana:") || e.starts_with("reanimate:")
    })
}

/// Print a warning for mainboard cards that lack a simulation implementation.
///
/// Two categories:
///   ✗ not in pilegen.toml — excluded from simulation entirely (silently dropped)
///   ~ in catalog but no actionable effects — drawn but never played/cast
fn warn_unimplemented_cards(
    cards: &[(String, i32, String)],
    deck_label: &str,
    config: &PilegenConfig,
) {
    let catalog: std::collections::HashMap<&str, &CardDef> =
        config.cards.iter().map(|c| (c.name.as_str(), c)).collect();

    let mut missing: Vec<(&str, i32)> = Vec::new();
    let mut no_effects: Vec<(&str, i32)> = Vec::new();

    for (name, qty, board) in cards {
        if board != "main" { continue; }
        match catalog.get(name.as_str()) {
            None => missing.push((name, *qty)),
            Some(def) if !card_has_implementation(def) => no_effects.push((name, *qty)),
            _ => {}
        }
    }

    if missing.is_empty() && no_effects.is_empty() { return; }

    println!("\n⚠  {} — unimplemented cards:", deck_label);
    for (name, qty) in &missing {
        println!("   ✗ {}×{} — not in pilegen.toml (excluded from simulation)", qty, name);
    }
    for (name, qty) in &no_effects {
        println!("   ~ {}×{} — no simulation effects (drawn but never cast)", qty, name);
    }
}

// ── Card rule helpers ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use rand::{SeedableRng, rngs::StdRng};

    // ── Test helpers ──────────────────────────────────────────────────────────

    fn make_state() -> SimState {
        let us = PlayerState::new("us_deck", 0);
        let opp = PlayerState::new("opp_deck", 0);
        SimState::new(us, opp)
    }

    fn seeded_rng() -> StdRng {
        StdRng::seed_from_u64(42)
    }

    fn empty_libs() -> (Vec<(String, CardDef)>, Vec<(String, CardDef)>) {
        (vec![], vec![])
    }

    fn creature(name: &str, power: i32, toughness: i32) -> CardDef {
        let toml = format!(
            "name = {:?}\ncard_type = \"creature\"\npower = {}\ntoughness = {}\n",
            name, power, toughness
        );
        toml::from_str(&toml).unwrap()
    }

    fn make_land(name: &str, tapped: bool) -> SimLand {
        SimLand {
            name: name.to_string(),
            tapped,
            basic: false,
            land_types: LandTypes::default(),
            mana_abilities: vec![],
        }
    }

    fn stack_item(name: &str, owner: &str) -> StackItem {
        StackItem {
            name: name.to_string(),
            owner: owner.to_string(),
            is_ability: false,
            ability_def: None,
            counters: None,
            permanent_target: None,
            annotation: None,
            adventure_exile: false,
            adventure_card_name: None,
            adventure_face: None,
        }
    }

    // ── Section 1: Pure Function Tests ────────────────────────────────────────

    #[test]
    fn test_parse_mana_cost_black() {
        let mc = parse_mana_cost("BBB");
        assert_eq!(mc.b, 3);
        assert_eq!(mc.u, 0);
        assert_eq!(mc.generic, 0);
    }

    #[test]
    fn test_parse_mana_cost_mixed() {
        // "1UB" → b=1, u=1, generic=1
        let mc = parse_mana_cost("1UB");
        assert_eq!(mc.b, 1);
        assert_eq!(mc.u, 1);
        assert_eq!(mc.generic, 1);
    }

    #[test]
    fn test_parse_mana_cost_zero() {
        let mc = parse_mana_cost("0");
        assert_eq!(mc.mana_value(), 0);
    }

    #[test]
    fn test_mana_value() {
        assert_eq!(mana_value("2BB"), 4);
        assert_eq!(mana_value("0"), 0);
        assert_eq!(mana_value("U"), 1);
    }

    #[test]
    fn test_creature_stats_counters() {
        let perm = SimPermanent { counters: 3, ..SimPermanent::new("Murktide Regent") };
        let def = creature("Murktide Regent", 3, 3);
        assert_eq!(creature_stats(&perm, Some(&def)), (6, 6));
    }

    #[test]
    fn test_creature_stats_from_def() {
        let def = creature("Ragavan", 2, 1);
        let perm = SimPermanent::new("Ragavan");
        assert_eq!(creature_stats(&perm, Some(&def)), (2, 1));
    }

    #[test]
    fn test_creature_stats_defaults() {
        let perm = SimPermanent::new("Unknown");
        assert_eq!(creature_stats(&perm, None), (1, 1));
    }

    #[test]
    fn test_stage_label() {
        assert_eq!(stage_label(1), "Early");
        assert_eq!(stage_label(4), "Mid");
        assert_eq!(stage_label(8), "Late");
    }

    // ── Section 2: Step Tests ─────────────────────────────────────────────────

    #[test]
    fn test_untap_step_resets_permanents() {
        let mut state = make_state();
        state.us.lands.push(make_land("Island", true));
        let mut ragavan = SimPermanent::new("Ragavan");
        ragavan.tapped = true;
        ragavan.entered_this_turn = true;
        state.us.permanents.push(ragavan);
        state.us.spells_cast_this_turn = 2;

        let step = Step { kind: StepKind::Untap, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(!state.us.lands[0].tapped, "land should be untapped");
        assert!(!state.us.permanents[0].tapped, "permanent should be untapped");
        assert!(!state.us.permanents[0].entered_this_turn, "summoning sickness should clear");
        assert!(state.us.land_drop_available, "land drop should reset");
        assert_eq!(state.us.spells_cast_this_turn, 0);
    }

    #[test]
    fn test_draw_step_skipped_on_play_turn1() {
        let mut state = make_state();
        let initial_hidden = state.us.hand.hidden;

        let step = Step { kind: StepKind::Draw, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        // on_play=true, t=1, ap="us" → this_player_on_play=true → skip
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert_eq!(state.us.hand.hidden, initial_hidden, "no draw on the play turn 1");
    }

    #[test]
    fn test_draw_step_draws_card() {
        let mut state = make_state();
        let initial_hidden = state.us.hand.hidden;

        let step = Step { kind: StepKind::Draw, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        // on_play=false → this_player_on_play=false → no skip
        do_step(&mut state, 1, "us", &step, 3, false, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert_eq!(state.us.hand.hidden, initial_hidden + 1, "should draw one card");
    }

    #[test]
    fn test_cleanup_removes_damage() {
        let mut state = make_state();
        let mut perm = SimPermanent::new("Ragavan");
        perm.damage = 3;
        state.us.permanents.push(perm);

        let step = Step { kind: StepKind::Cleanup, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert_eq!(state.us.permanents[0].damage, 0);
    }

    #[test]
    fn test_declare_attackers_safe_to_attack() {
        let mut state = make_state();
        let ragavan_def = creature("Ragavan", 2, 4);
        let mut ragavan = SimPermanent::new("Ragavan");
        ragavan.entered_this_turn = false;
        state.us.permanents.push(ragavan);

        let catalog = vec![ragavan_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareAttackers, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(state.combat_attackers.contains(&"Ragavan".to_string()), "should attack");
        assert!(state.us.permanents[0].tapped, "attacker should be tapped");
    }

    #[test]
    fn test_declare_attackers_too_risky() {
        let mut state = make_state();
        let attacker_def = creature("Ragavan", 2, 2);
        let blocker_def = creature("Mosscoat Construct", 3, 3);
        let mut ragavan = SimPermanent::new("Ragavan");
        ragavan.entered_this_turn = false;
        state.us.permanents.push(ragavan);
        state.opp.permanents.push(SimPermanent::new("Mosscoat Construct"));

        let catalog = vec![attacker_def, blocker_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareAttackers, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(state.combat_attackers.is_empty(), "should not attack into 3/3");
    }

    #[test]
    fn test_declare_attackers_summoning_sickness() {
        let mut state = make_state();
        let def = creature("Ragavan", 2, 4);
        // entered_this_turn = true (default from SimPermanent::new)
        state.us.permanents.push(SimPermanent::new("Ragavan"));

        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareAttackers, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(state.combat_attackers.is_empty(), "sickness prevents attack");
    }

    #[test]
    fn test_declare_blockers_good_block() {
        let mut state = make_state();
        state.combat_attackers = vec!["Ragavan".to_string()];
        let atk_def = creature("Ragavan", 2, 2);
        let blk_def = creature("Mosscoat Construct", 3, 3);
        let mut ragavan = SimPermanent::new("Ragavan");
        ragavan.entered_this_turn = false;
        ragavan.tapped = false;
        state.us.permanents.push(ragavan);
        state.opp.permanents.push(SimPermanent::new("Mosscoat Construct"));

        let catalog = vec![atk_def, blk_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareBlockers, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert_eq!(state.combat_blocks.len(), 1);
        assert_eq!(state.combat_blocks[0], ("Ragavan".to_string(), "Mosscoat Construct".to_string()));
    }

    #[test]
    fn test_declare_blockers_no_chump() {
        let mut state = make_state();
        state.combat_attackers = vec!["Beast".to_string()];
        let atk_def = creature("Beast", 4, 4);
        let blk_def = creature("Squirrel Token", 1, 1);
        let mut beast = SimPermanent::new("Beast");
        beast.entered_this_turn = false;
        state.us.permanents.push(beast);
        state.opp.permanents.push(SimPermanent::new("Squirrel Token"));

        let catalog = vec![atk_def, blk_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareBlockers, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(state.combat_blocks.is_empty(), "should not chump block");
    }

    #[test]
    fn test_combat_damage_unblocked_hits_player() {
        let mut state = make_state();
        let initial_life = state.opp.life;
        state.combat_attackers = vec!["Ragavan".to_string()];
        let atk_def = creature("Ragavan", 2, 1);
        let mut ragavan = SimPermanent::new("Ragavan");
        ragavan.tapped = true;
        state.us.permanents.push(ragavan);

        let catalog = vec![atk_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::CombatDamage, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert_eq!(state.opp.life, initial_life - 2);
    }

    #[test]
    fn test_combat_damage_blocked_no_player_damage() {
        let mut state = make_state();
        let initial_life = state.opp.life;
        state.combat_attackers = vec!["Ragavan".to_string()];
        state.combat_blocks = vec![("Ragavan".to_string(), "Mosscoat Construct".to_string())];
        let atk_def = creature("Ragavan", 2, 2);
        let blk_def = creature("Mosscoat Construct", 3, 3);
        let mut ragavan = SimPermanent::new("Ragavan");
        ragavan.tapped = true;
        state.us.permanents.push(ragavan);
        state.opp.permanents.push(SimPermanent::new("Mosscoat Construct"));

        let catalog = vec![atk_def, blk_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::CombatDamage, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert_eq!(state.opp.life, initial_life, "blocked — no player damage");
    }

    #[test]
    fn test_combat_damage_sba_kills_both_2_2s() {
        let mut state = make_state();
        state.combat_attackers = vec!["Ragavan".to_string()];
        state.combat_blocks = vec![("Ragavan".to_string(), "Mosscoat Construct".to_string())];
        let atk_def = creature("Ragavan", 2, 2);
        let blk_def = creature("Mosscoat Construct", 2, 2);
        let mut ragavan = SimPermanent::new("Ragavan");
        ragavan.tapped = true;
        state.us.permanents.push(ragavan);
        state.opp.permanents.push(SimPermanent::new("Mosscoat Construct"));

        let catalog = vec![atk_def, blk_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::CombatDamage, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(state.us.permanents.is_empty(), "attacker should die");
        assert!(state.opp.permanents.is_empty(), "blocker should die");
        assert!(state.us.graveyard.visible.contains(&"Ragavan".to_string()));
        assert!(state.opp.graveyard.visible.contains(&"Mosscoat Construct".to_string()));
    }

    #[test]
    fn test_combat_damage_outclassed_attacker_dies() {
        let mut state = make_state();
        state.combat_attackers = vec!["Ragavan".to_string()];
        state.combat_blocks = vec![("Ragavan".to_string(), "Troll".to_string())];
        let atk_def = creature("Ragavan", 2, 2);
        let blk_def = creature("Troll", 3, 3);
        let mut ragavan = SimPermanent::new("Ragavan");
        ragavan.tapped = true;
        state.us.permanents.push(ragavan);
        state.opp.permanents.push(SimPermanent::new("Troll"));

        let catalog = vec![atk_def, blk_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::CombatDamage, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(state.us.permanents.is_empty(), "attacker dies");
        assert!(!state.opp.permanents.is_empty(), "blocker survives");
    }

    #[test]
    fn test_end_combat_clears_fields() {
        let mut state = make_state();
        state.combat_attackers = vec!["Ragavan".to_string()];
        state.combat_blocks = vec![("Ragavan".to_string(), "Construct".to_string())];

        let step = Step { kind: StepKind::EndCombat, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(state.combat_attackers.is_empty());
        assert!(state.combat_blocks.is_empty());
    }

    // ── Section 3: Phase Tests ────────────────────────────────────────────────

    #[test]
    fn test_beginning_phase_untaps_and_draws() {
        let mut state = make_state();
        state.us.lands.push(SimLand {
            name: "Island".to_string(),
            tapped: true,
            basic: true,
            land_types: LandTypes { island: true, ..Default::default() },
            mana_abilities: vec![ManaAbility { tap_self: true, produces: "U".into(), ..Default::default() }],
        });
        let initial_hidden = state.us.hand.hidden; // 7

        let (mut us_lib, mut opp_lib) = empty_libs();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        // t=2, on_play=false → draw fires (this_player_on_play=false)
        do_phase(&mut state, 2, "us", &beginning_phase(), 3, false, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(!state.us.lands[0].tapped, "land should be untapped");
        assert_eq!(state.us.hand.hidden, initial_hidden + 1, "should have drawn one card");
    }

    #[test]
    fn test_combat_phase_full_cycle() {
        let mut state = make_state();
        let (mut us_lib, mut opp_lib) = empty_libs();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        do_phase(&mut state, 1, "us", &combat_phase(), 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(state.combat_attackers.is_empty());
        assert!(state.combat_blocks.is_empty());
    }

    // ── Section 4: Priority Action Cycle ─────────────────────────────────────

    #[test]
    fn test_priority_round_both_pass_empty_stack() {
        let mut state = make_state();
        // current_phase is "" (not "Main") → both players pass immediately
        let (mut us_lib, mut opp_lib) = empty_libs();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        handle_priority_round(&mut state, 1, "us", 3, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert_eq!(state.us.life, 20);
        assert_eq!(state.opp.life, 20);
    }

    // ── Section 5: Spell Casting ──────────────────────────────────────────────

    #[test]
    fn test_cast_spell_normal_cost_removes_from_library() {
        let mut state = make_state();
        let def: CardDef = toml::from_str(r#"
            name = "Dark Ritual"
            card_type = "instant"
            mana_cost = "B"
            effects = ["mana:BBB"]
        "#).unwrap();
        let mut us_lib = vec![("Dark Ritual".to_string(), def.clone())];
        state.us.pool.b = 1;
        state.us.pool.total = 1;
        // hand.hidden = 7 (from PlayerState::new)

        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        let item = cast_spell(&mut state, 1, "us", "Dark Ritual", &mut us_lib, None, &catalog_map, &mut seeded_rng());

        assert!(item.is_some(), "spell should be cast");
        let item = item.unwrap();
        assert_eq!(item.name, "Dark Ritual");
        assert_eq!(item.owner, "us");
        assert!(!us_lib.iter().any(|(n, _)| n == "Dark Ritual"), "removed from library");
        assert_eq!(state.us.pool.b, 0, "mana spent");
    }

    #[test]
    fn test_cast_spell_unaffordable_returns_none() {
        let mut state = make_state();
        let def: CardDef = toml::from_str(r#"
            name = "Doomsday"
            card_type = "instant"
            mana_cost = "BBB"
            effects = ["doomsday"]
        "#).unwrap();
        let mut us_lib = vec![("Doomsday".to_string(), def.clone())];
        // No mana in pool, no lands

        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let item = cast_spell(&mut state, 1, "us", "Doomsday", &mut us_lib, None, &catalog_map, &mut seeded_rng());

        assert!(item.is_none(), "can't cast with no mana");
    }

    #[test]
    fn test_cast_spell_alt_cost_exiles_pitch_card() {
        let mut state = make_state();
        let fow_def: CardDef = toml::from_str(r#"
            name = "Force of Will"
            card_type = "instant"
            mana_cost = "3UU"
            blue = true
            [[alternate_costs]]
            mana_cost = ""
            exile_blue_from_hand = true
            life_cost = 1
        "#).unwrap();
        let brainstorm_def: CardDef = toml::from_str(r#"
            name = "Brainstorm"
            card_type = "instant"
            mana_cost = "U"
        "#).unwrap();
        let mut us_lib = vec![
            ("Force of Will".to_string(), fow_def.clone()),
            ("Brainstorm".to_string(), brainstorm_def.clone()),
        ];
        let catalog = vec![fow_def.clone(), brainstorm_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        let alt_cost = &fow_def.alternate_costs()[0];
        let initial_life = state.us.life;

        let item = cast_spell(&mut state, 1, "us", "Force of Will", &mut us_lib, Some(alt_cost), &catalog_map, &mut seeded_rng());

        assert!(item.is_some(), "FoW should be cast via pitch");
        assert_eq!(state.us.life, initial_life - 1, "paid 1 life");
        assert!(!us_lib.iter().any(|(n, _)| n == "Brainstorm"), "pitch card removed from library");
        assert!(state.us.exile.visible.contains(&"Brainstorm".to_string()), "pitch card exiled");
    }

    // ── Section 6: Spell Resolution ───────────────────────────────────────────

    #[test]
    fn test_effect_doomsday_sets_success() {
        let mut state = make_state();
        let def: CardDef = toml::from_str(r#"
            name = "Doomsday"
            card_type = "instant"
            mana_cost = "BBB"
            effects = ["doomsday"]
        "#).unwrap();
        let item = stack_item("Doomsday", "us");
        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let (mut us_lib, mut opp_lib) = empty_libs();
        apply_spell_effects(&item, &mut state, 1, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(state.success);
    }

    #[test]
    fn test_effect_cantrip_increments_hand() {
        let mut state = make_state();
        let initial_hidden = state.us.hand.hidden;
        let def: CardDef = toml::from_str(r#"
            name = "Preordain"
            card_type = "instant"
            mana_cost = "U"
            effects = ["cantrip"]
        "#).unwrap();
        let item = stack_item("Preordain", "us");
        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let (mut us_lib, mut opp_lib) = empty_libs();
        apply_spell_effects(&item, &mut state, 1, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert_eq!(state.us.hand.hidden, initial_hidden + 1, "cantrip increments hand count");
    }

    #[test]
    fn test_effect_life_loss_reduces_caster_life() {
        let mut state = make_state();
        let initial = state.us.life;
        // life_loss:N reduces the caster's life
        let def: CardDef = toml::from_str(r#"
            name = "Dark Confidant"
            card_type = "creature"
            mana_cost = "1B"
            effects = ["life_loss:2"]
        "#).unwrap();
        let item = stack_item("Dark Confidant", "us");
        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let (mut us_lib, mut opp_lib) = empty_libs();
        apply_spell_effects(&item, &mut state, 1, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert_eq!(state.us.life, initial - 2);
    }

    #[test]
    fn test_effect_mana_adds_to_pool() {
        let mut state = make_state();
        let def: CardDef = toml::from_str(r#"
            name = "Dark Ritual"
            card_type = "instant"
            mana_cost = "B"
            effects = ["mana:BBB"]
        "#).unwrap();
        let item = stack_item("Dark Ritual", "us");
        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let (mut us_lib, mut opp_lib) = empty_libs();
        apply_spell_effects(&item, &mut state, 1, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert_eq!(state.us.pool.b, 3, "should add 3 black mana");
        assert_eq!(state.us.pool.total, 3);
    }

    #[test]
    fn test_effect_discard_removes_opp_card() {
        let mut state = make_state();
        state.opp.hand.hidden = 1;
        let def: CardDef = toml::from_str(r#"
            name = "Thoughtseize"
            card_type = "sorcery"
            mana_cost = "B"
            effects = ["discard:opp:1"]
        "#).unwrap();
        let target_card: CardDef = toml::from_str(r#"
            name = "Counterspell"
            card_type = "instant"
            mana_cost = "UU"
        "#).unwrap();
        let item = stack_item("Thoughtseize", "us");
        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let mut us_lib: Vec<(String, CardDef)> = vec![];
        // other_lib = opp_lib: the card pool to discard from
        let mut opp_lib: Vec<(String, CardDef)> = vec![("Counterspell".to_string(), target_card)];
        apply_spell_effects(&item, &mut state, 1, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert_eq!(state.opp.hand.hidden, 0, "opp hand decremented");
        assert!(state.opp.graveyard.visible.contains(&"Counterspell".to_string()));
        assert!(opp_lib.is_empty(), "card removed from opp library");
    }

    // ── Section 7: Ability Activation ─────────────────────────────────────────

    #[test]
    fn test_pay_activation_cost_mana() {
        let mut state = make_state();
        state.us.pool.b = 2;
        state.us.pool.total = 2;
        let ability: AbilityDef = toml::from_str(r#"
            mana_cost = "B"
            effect = "cantrip"
        "#).unwrap();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        pay_activation_cost(&mut state, 1, "us", "SourceCard", &ability, &mut vec![], &catalog_map);

        assert_eq!(state.us.pool.b, 1, "1 black spent");
        assert_eq!(state.us.pool.total, 1);
    }

    #[test]
    fn test_pay_activation_cost_life() {
        let mut state = make_state();
        let initial = state.us.life;
        let ability: AbilityDef = toml::from_str(r#"
            mana_cost = ""
            life_cost = 2
            effect = "cantrip"
        "#).unwrap();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        pay_activation_cost(&mut state, 1, "us", "SourceCard", &ability, &mut vec![], &catalog_map);

        assert_eq!(state.us.life, initial - 2);
    }

    #[test]
    fn test_pay_activation_cost_sacrifice_self() {
        let mut state = make_state();
        state.us.permanents.push(SimPermanent::new("Lotus Petal"));
        let ability: AbilityDef = toml::from_str(r#"
            mana_cost = ""
            sacrifice_self = true
            effect = "mana:B"
        "#).unwrap();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        pay_activation_cost(&mut state, 1, "us", "Lotus Petal", &ability, &mut vec![], &catalog_map);

        assert!(state.us.permanents.is_empty(), "Lotus Petal should be sacrificed");
        assert!(state.us.graveyard.visible.contains(&"Lotus Petal".to_string()));
    }

    // ── Section 8: Destruction Effects ───────────────────────────────────────

    // Spell resolution: destroy uses item.permanent_target set at cast time.

    #[test]
    fn test_effect_destroy_spell_removes_opp_land() {
        let mut state = make_state();
        state.opp.lands.push(make_land("Bayou", false));
        let def: CardDef = toml::from_str(r#"
            name = "Sinkhole"
            card_type = "sorcery"
            mana_cost = "BB"
            target = "opp:land"
            effects = ["destroy"]
        "#).unwrap();
        let item = StackItem {
            permanent_target: Some(("opp".to_string(), "Bayou".to_string())),
            ..stack_item("Sinkhole", "us")
        };
        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let (mut us_lib, mut opp_lib) = empty_libs();
        apply_spell_effects(&item, &mut state, 1, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(state.opp.lands.is_empty(), "Bayou should be destroyed");
        assert!(state.opp.graveyard.visible.contains(&"Bayou".to_string()));
    }

    #[test]
    fn test_effect_destroy_spell_removes_opp_creature() {
        let mut state = make_state();
        state.opp.permanents.push(SimPermanent::new("Troll"));
        let troll_def = creature("Troll", 2, 2);
        let swords_def: CardDef = toml::from_str(r#"
            name = "Swords to Plowshares"
            card_type = "instant"
            mana_cost = "W"
            target = "opp:creature"
            effects = ["destroy"]
        "#).unwrap();
        let item = StackItem {
            permanent_target: Some(("opp".to_string(), "Troll".to_string())),
            ..stack_item("Swords to Plowshares", "us")
        };
        let catalog = vec![swords_def, troll_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let (mut us_lib, mut opp_lib) = empty_libs();
        apply_spell_effects(&item, &mut state, 1, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(state.opp.permanents.is_empty(), "Troll should be destroyed");
        assert!(state.opp.graveyard.visible.contains(&"Troll".to_string()));
    }

    // Ability resolution: target is chosen at resolution via sim_apply_targeted_effect.

    #[test]
    fn test_effect_destroy_ability_removes_nonbasic_land() {
        let mut state = make_state();
        state.opp.lands.push(SimLand { basic: false, ..make_land("Bayou", false) });
        let ability: AbilityDef = toml::from_str(r#"
            mana_cost = ""
            target = "opp:nonbasic_land"
            effect = "destroy"
        "#).unwrap();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        let (mut us_lib, mut _opp_lib) = empty_libs();
        apply_ability_effect(&mut state, 1, "us", "Wasteland", &ability, &mut us_lib, &catalog_map, &mut seeded_rng());

        assert!(state.opp.lands.is_empty(), "Bayou should be destroyed");
        assert!(state.opp.graveyard.visible.contains(&"Bayou".to_string()));
    }

    #[test]
    fn test_effect_destroy_ability_ignores_basic_land() {
        let mut state = make_state();
        state.opp.lands.push(SimLand { basic: true, ..make_land("Forest", false) });
        let ability: AbilityDef = toml::from_str(r#"
            mana_cost = ""
            target = "opp:nonbasic_land"
            effect = "destroy"
        "#).unwrap();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        let (mut us_lib, mut _opp_lib) = empty_libs();
        apply_ability_effect(&mut state, 1, "us", "Wasteland", &ability, &mut us_lib, &catalog_map, &mut seeded_rng());

        assert!(!state.opp.lands.is_empty(), "basic Forest should survive");
        assert!(state.opp.graveyard.visible.is_empty());
    }

    // ── Section 9: Delve ──────────────────────────────────────────────────────

    #[test]
    fn test_cast_delve_spell_exiles_graveyard_cards() {
        // Spell costs 3 generic + U. Two graveyard cards reduce generic to 1.
        // Pool supplies the remaining 1 generic + 1 blue.
        let mut state = make_state();
        let def: CardDef = toml::from_str(r#"
            name = "Treasure Cruise"
            card_type = "instant"
            mana_cost = "7U"
            delve = true
            effects = ["cantrip"]
        "#).unwrap();
        state.us.graveyard.visible = vec!["A".into(), "B".into(), "C".into(),
                                          "D".into(), "E".into(), "F".into(), "G".into()];
        state.us.pool.u  = 1;
        state.us.pool.total = 1; // only 1 mana in pool — delve pays the other 7

        let mut us_lib = vec![("Treasure Cruise".to_string(), def.clone())];
        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        let item = cast_spell(&mut state, 1, "us", "Treasure Cruise", &mut us_lib, None, &catalog_map, &mut seeded_rng());

        assert!(item.is_some(), "should cast with full delve");
        assert_eq!(state.us.graveyard.visible.len(), 0, "all 7 graveyard cards exiled");
        assert_eq!(state.us.exile.visible.len(), 7, "exiled by delve");
        assert_eq!(state.us.pool.u, 0, "blue pip paid");
    }

    #[test]
    fn test_cast_delve_spell_partial_delve() {
        // Spell costs 3 generic. Graveyard has 2 cards — reduces cost to 1.
        // Pool must cover the remaining 1 generic.
        let mut state = make_state();
        let def: CardDef = toml::from_str(r#"
            name = "Dead Drop"
            card_type = "sorcery"
            mana_cost = "3"
            delve = true
            effects = ["destroy"]
        "#).unwrap();
        state.us.graveyard.visible = vec!["Ritual".into(), "Ponder".into()];
        state.us.pool.total = 1; // covers the 1 remaining generic after delve

        let mut us_lib = vec![("Dead Drop".to_string(), def.clone())];
        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        let item = cast_spell(&mut state, 1, "us", "Dead Drop", &mut us_lib, None, &catalog_map, &mut seeded_rng());

        assert!(item.is_some(), "should cast with partial delve + 1 mana");
        assert_eq!(state.us.graveyard.visible.len(), 0, "both graveyard cards exiled");
        assert_eq!(state.us.exile.visible.len(), 2);
        assert_eq!(state.us.pool.total, 0, "remaining generic pip paid");
    }

    #[test]
    fn test_murktide_counters_from_exiled_instants_sorceries() {
        // Murktide exiles 4 cards via delve; 3 are instants/sorceries → enters as 6/6.
        let mut state = make_state();
        let murktide_def: CardDef = toml::from_str(r#"
            name = "Murktide Regent"
            card_type = "creature"
            mana_cost = "5UU"
            delve = true
            power = 3
            toughness = 3
            effects = ["permanent"]
        "#).unwrap();
        let ritual_def: CardDef   = toml::from_str("name = \"Dark Ritual\"\ncard_type = \"instant\"\nmana_cost = \"B\"").unwrap();
        let ponder_def: CardDef   = toml::from_str("name = \"Ponder\"\ncard_type = \"sorcery\"\nmana_cost = \"U\"").unwrap();
        let consider_def: CardDef = toml::from_str("name = \"Consider\"\ncard_type = \"instant\"\nmana_cost = \"U\"").unwrap();
        let ragavan_def  = creature("Ragavan", 2, 1); // creature — does NOT count

        state.us.graveyard.visible = vec![
            "Dark Ritual".into(), "Ponder".into(), "Consider".into(), "Ragavan".into(),
        ];
        // After delving all 4, generic cost = 5-4 = 1. Need UU + 1 generic.
        state.us.pool.u  = 2;
        state.us.pool.total = 3;

        let catalog = vec![murktide_def.clone(), ritual_def, ponder_def, consider_def, ragavan_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let mut us_lib = vec![("Murktide Regent".to_string(), murktide_def)];

        let item = cast_spell(&mut state, 1, "us", "Murktide Regent", &mut us_lib, None, &catalog_map, &mut seeded_rng()).unwrap();
        // annotation encodes "+3" (3 instants/sorceries: Ritual, Ponder, Consider)
        assert_eq!(item.annotation.as_deref(), Some("+3"));

        // Resolve and check permanent
        let (mut us_lib2, mut opp_lib) = empty_libs();
        apply_spell_effects(&item, &mut state, 1, &mut us_lib2, &mut opp_lib, &catalog_map, &mut seeded_rng());

        let murktide = &state.us.permanents[0];
        assert_eq!(murktide.counters, 3, "3 instants/sorceries exiled → 3 counters");
        assert!(murktide.annotation.is_none(), "counter annotation consumed");

        // creature_stats reflects counters in damage calculations
        let def = catalog_map["Murktide Regent"];
        assert_eq!(creature_stats(murktide, Some(def)), (6, 6));
    }

    #[test]
    fn test_murktide_zero_counters_when_no_instants_exiled() {
        // Delve only exiles a creature — no instants/sorceries → enters as base 3/3.
        let mut state = make_state();
        let murktide_def: CardDef = toml::from_str(r#"
            name = "Murktide Regent"
            card_type = "creature"
            mana_cost = "5UU"
            delve = true
            power = 3
            toughness = 3
            effects = ["permanent"]
        "#).unwrap();
        let ragavan_def = creature("Ragavan", 2, 1);

        state.us.graveyard.visible = vec!["Ragavan".into()];
        // 5 - 1 = 4 generic remaining; need UU + 4 generic
        state.us.pool.u  = 2;
        state.us.pool.total = 6;

        let catalog = vec![murktide_def.clone(), ragavan_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let mut us_lib = vec![("Murktide Regent".to_string(), murktide_def)];

        let item = cast_spell(&mut state, 1, "us", "Murktide Regent", &mut us_lib, None, &catalog_map, &mut seeded_rng()).unwrap();
        assert!(item.annotation.is_none(), "no instants/sorceries → no counter annotation");

        let (mut us_lib2, mut opp_lib) = empty_libs();
        apply_spell_effects(&item, &mut state, 1, &mut us_lib2, &mut opp_lib, &catalog_map, &mut seeded_rng());

        let murktide = &state.us.permanents[0];
        assert_eq!(murktide.counters, 0);
        let def = catalog_map["Murktide Regent"];
        assert_eq!(creature_stats(murktide, Some(def)), (3, 3));
    }

    #[test]
    fn test_murktide_attacks_with_counter_boosted_stats() {
        // A 6/6 Murktide (base 3/3 + 3 counters) should survive attacking into a 5-power blocker.
        let mut state = make_state();
        let murktide_def = creature("Murktide Regent", 3, 3);
        let mut murktide = SimPermanent::new("Murktide Regent");
        murktide.counters = 3;
        murktide.entered_this_turn = false;
        // Opponent has a 5/5 blocker — Murktide's toughness 6 > opp power 5, safe to attack.
        let blocker_def = creature("Dragon", 5, 5);
        state.opp.permanents.push(SimPermanent::new("Dragon"));
        state.us.permanents.push(murktide);

        let catalog = vec![murktide_def, blocker_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareAttackers, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(state.combat_attackers.contains(&"Murktide Regent".to_string()),
            "6/6 Murktide should attack into a 5-power blocker");
    }

    #[test]
    fn test_cast_delve_spell_insufficient_mana_after_delve() {
        // Spell costs 3 generic. Graveyard has 2 cards — reduces cost to 1.
        // Pool is empty — still can't cast.
        let mut state = make_state();
        let def: CardDef = toml::from_str(r#"
            name = "Dead Drop"
            card_type = "sorcery"
            mana_cost = "3"
            delve = true
            effects = ["destroy"]
        "#).unwrap();
        state.us.graveyard.visible = vec!["Ritual".into(), "Ponder".into()];
        // no mana

        let mut us_lib = vec![("Dead Drop".to_string(), def.clone())];
        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        let item = cast_spell(&mut state, 1, "us", "Dead Drop", &mut us_lib, None, &catalog_map, &mut seeded_rng());

        assert!(item.is_none(), "can't cast — 1 generic still unpaid");
        assert_eq!(state.us.graveyard.visible.len(), 2, "graveyard unchanged on failed cast");
        assert!(state.us.exile.visible.is_empty(), "nothing exiled on failed cast");
    }

    #[test]
    fn test_effect_exile_ability_removes_creature() {
        let mut state = make_state();
        state.opp.permanents.push(SimPermanent::new("Troll"));
        let troll_def = creature("Troll", 2, 2);
        let ability: AbilityDef = toml::from_str(r#"
            mana_cost = ""
            target = "opp:creature"
            effect = "exile"
        "#).unwrap();
        let catalog = vec![troll_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let (mut us_lib, mut _opp_lib) = empty_libs();
        apply_ability_effect(&mut state, 1, "us", "Karakas", &ability, &mut us_lib, &catalog_map, &mut seeded_rng());

        assert!(state.opp.permanents.is_empty(), "Troll should be exiled");
        assert!(state.opp.exile.visible.contains(&"Troll".to_string()));
        assert!(state.opp.graveyard.visible.is_empty(), "exiled, not dead");
    }
}

