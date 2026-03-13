use clap::Args;
use dialoguer::Confirm;
use diesel::prelude::*;
use rand::Rng;
use skim::prelude::*;
use std::collections::HashMap;
use std::io::Cursor;

use crate::db::schema::{cards, deck_types, decks};
use crate::db::{establish_connection, models::*};

mod catalog;
pub(crate) use catalog::*;

mod effects;
pub(crate) use effects::*;

mod predicates;
pub(crate) use predicates::*;

mod strategy;
use strategy::{decide_action, collect_on_board_actions};

#[cfg(test)]
mod tests;





// ── Game state ────────────────────────────────────────────────────────────────

// ── Stable object identity ────────────────────────────────────────────────────

/// Opaque game object identifier. Every player, card, token, and stack ability
/// gets one at construction time and keeps it through all zone changes.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub(crate) struct ObjId(u64);

impl ObjId {
    const UNSET: ObjId = ObjId(0);
    fn is_set(self) -> bool { self.0 != 0 }
}

/// Zone a card currently occupies. Changed by move_to_zone / move_to_battlefield.
#[derive(Clone, Copy, PartialEq, Debug)]
enum CardZone {
    Library,
    Hand { known: bool },   // known = identity visible to opponent
    Stack,
    Battlefield,
    Graveyard,
    Exile { on_adventure: bool },
}

/// In-play state for any permanent (land, creature, artifact, planeswalker, enchantment, token).
/// Replaces SimPermanent + SimLand. Whether a permanent is a land/creature/etc. is determined
/// by looking up its CardDef from the catalog.
#[derive(Clone)]
struct BattlefieldState {
    tapped: bool,
    damage: i32,
    entered_this_turn: bool,
    counters: i32,              // +1/+1 counters
    power_mod: i32,
    toughness_mod: i32,
    loyalty: i32,               // planeswalker loyalty (0 for non-PWs)
    pw_activated_this_turn: bool,
    attacking: bool,
    unblocked: bool,
    attack_target: Option<ObjId>,  // None = attacking player, Some = attacking planeswalker
    annotation: Option<String>,
    mana_abilities: Vec<ManaAbility>,  // cached from CardDef at entry
}

impl BattlefieldState {
    fn new(mana_abilities: Vec<ManaAbility>) -> Self {
        BattlefieldState {
            tapped: false, damage: 0, entered_this_turn: true, counters: 0,
            power_mod: 0, toughness_mod: 0, loyalty: 0, pw_activated_this_turn: false,
            attacking: false, unblocked: false, attack_target: None,
            annotation: None, mana_abilities,
        }
    }
}

/// State for a spell on the stack.
#[derive(Clone)]
struct StackSpellState {
    chosen_targets: Vec<Target>,
    effect: Option<Effect>,
    annotation: Option<String>,
    is_adventure_face: bool,    // resolve to Exile { on_adventure: true } instead of graveyard
}

/// A card as a game object — follows the card through all zone changes.
/// The immutable blueprint (CardDef) is looked up by name from the catalog.
#[derive(Clone)]
struct CardObject {
    id: ObjId,
    name: String,
    owner: String,        // "us" or "opp" — kept as String for compat with existing player(&str) API
    controller: String,
    zone: CardZone,
    bf: Option<BattlefieldState>,      // Some only when zone == Battlefield
    stack: Option<StackSpellState>,    // Some only when zone == Stack
}

impl CardObject {
    fn new(id: ObjId, name: impl Into<String>, owner: impl Into<String>) -> Self {
        let owner = owner.into();
        CardObject {
            id, name: name.into(), controller: owner.clone(), owner,
            zone: CardZone::Library, bf: None, stack: None,
        }
    }
}

/// An activated or triggered ability on the stack (not a card).
#[derive(Clone)]
struct StackAbility {
    id: ObjId,
    source_id: ObjId,
    source_name: String,
    owner: String,
    controller: String,
    ability_def: Option<AbilityDef>,
    trigger_context: Option<TriggerContext>,
    chosen_targets: Vec<Target>,
    effect: Option<Effect>,
    annotation: Option<String>,
}

/// An item on the spell stack: a spell or ability that has been declared and paid for
/// but not yet resolved.
///
/// Targets are chosen at cast time and stored here so resolution carries out effects
/// deterministically without needing to re-pick targets.
#[derive(Clone)]
struct StackItem {
    /// Stable stack-object identity. Freshly allocated for spells; ObjId::UNSET for abilities.
    id: ObjId,
    name: String,
    owner: ObjId,
    /// Stable ObjId of the physical card this stack item represents.
    /// Matches the id assigned when the card was placed in the library.
    /// ObjId::UNSET for ability stack items (not a card).
    card_id: ObjId,
    /// True for activated abilities; NAP skips countering these.
    is_ability: bool,
    /// For activated abilities: the ability definition, used to apply the effect at resolution.
    ability_def: Option<AbilityDef>,
    /// For counterspells: the ObjId of the stack item this spell is targeting.
    counters: Option<ObjId>,
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
    /// For triggered abilities: the trigger payload to apply at resolution.
    /// When Some, this overrides ability_def / spell resolution — `apply_trigger` is called instead.
    trigger_context: Option<TriggerContext>,
    /// Targets chosen when the trigger was put on the stack (from trigger_context.target_spec).
    chosen_targets: Vec<Target>,
    /// For ninjutsu abilities: the attack_target of the replaced attacker, so the ninja can inherit it.
    ninjutsu_attack_target: Option<ObjId>,
    /// Composable effect closure populated by `spell_effect` at cast time.
    /// When Some, the new Effect path is used at resolution. When None, falls back
    /// When None, the spell has no game text (e.g. test stubs).
    effect: Option<Effect>,
}


// ── Trigger system ────────────────────────────────────────────────────────────

/// Zones a card or permanent can occupy.
#[derive(Clone, Copy, PartialEq, Debug)]
enum ZoneId {
    Hand,
    Library,
    Battlefield,
    Graveyard,
    Exile,
    Stack,
}

/// A game event emitted at key moments. Handlers inspect this to decide whether their
/// trigger fires. Owned strings to avoid lifetime issues when pushing onto the stack.
#[derive(Clone)]
enum GameEvent {
    /// A card moved from one zone to another (ETB, GY→Exile, etc.).
    /// Does NOT include drawing — use `Draw` for that.
    ZoneChange {
        card: String,
        card_type: String,
        from: ZoneId,
        to: ZoneId,
        controller: String,
    },
    /// A player draws a card. `draw_index` is which draw this is this turn (1-based).
    /// `is_natural` is true only for the draw-step draw.
    Draw {
        controller: String,
        draw_index: u8,
        is_natural: bool,
    },
    /// Fired after step-specific actions complete and before priority begins.
    /// `active_player` is the player whose turn it is.
    EnteredStep {
        step: StepKind,
        active_player: String,
    },
    /// A creature was declared as an attacker.
    CreatureAttacked {
        attacker_id: ObjId,
        attacker: String,
        attacker_controller: String,
        attack_target: Option<ObjId>, // None = player, Some(id) = planeswalker
    },
    // Future variants: DamageDealt, SpellCast, SpellResolved, AbilityActivated,
    //                  CounterChanged, LifeChanged, TokenCreated.
}

/// Data stored with a triggered ability's `StackItem`.
/// The effect closure captures all context (targets, source data) at trigger-push time.
#[derive(Clone)]
struct TriggerContext {
    /// Card name of the triggering permanent. TODO(ids): replace with permanent ID.
    source: String,
    /// Player who controls that permanent.
    controller: String,
    /// Short string label for this trigger type — used in logging and test assertions.
    kind: &'static str,
    /// Legal targets this trigger may choose from. Resolved when pushed to the stack.
    target_spec: TargetSpec,
    /// The effect to apply when this trigger resolves. Receives the chosen targets.
    effect: EffectFn,
}

// ── Continuous effects ────────────────────────────────────────────────────────

/// When a continuous effect expires.
#[derive(Clone, PartialEq)]
enum EffectExpiry {
    /// Removed at the start of the controlling player's next Untap step.
    StartOfControllerNextTurn,
    /// Removed during the Cleanup step of the current turn.
    EndOfTurn,
}

/// A reversible power/toughness modification. Kept as structured data so
/// Cleanup can undo it precisely without the closure needing mutable access at expiry.
#[derive(Clone)]
struct StatModData {
    target_id: ObjId,
    power_delta: i32,
    toughness_delta: i32,
}

/// A continuous effect that persists across steps or turns.
#[derive(Clone)]
struct ContinuousEffect {
    /// Player who registered this effect (controls the source permanent).
    controller: String,
    expires: EffectExpiry,
    /// Called for each game event. Returns a TriggerContext if this effect fires a triggered
    /// ability in response to that event. None means the effect is purely passive.
    on_event: Option<std::sync::Arc<dyn Fn(&GameEvent, &str) -> Option<TriggerContext> + Send + Sync>>,
    /// If Some, a stat modification that Cleanup must reverse when this effect expires.
    stat_mod: Option<StatModData>,
}

// ── Effect system (new) ───────────────────────────────────────────────────────
//
// Target, TargetSpec, Who, Effect, and eff_* primitives live in effects.rs and predicates.rs,
// re-exported via `pub(crate) use effects::*` and `pub(crate) use predicates::*` above.

/// The closure type for all resolved effects: spells, abilities, and triggered abilities.
/// Targets were chosen at stack-push time and are passed in at resolution.
/// The function is responsible for re-checking target legality and doing nothing (fizzle)
/// if the target is no longer valid.
type EffectFn = std::sync::Arc<dyn Fn(&mut SimState, u8, &[Target], &HashMap<&str, &CardDef>) + Send + Sync>;

/// Builds a no-op EffectFn. Useful as a placeholder during migration.
#[allow(dead_code)]
fn no_effect() -> EffectFn {
    std::sync::Arc::new(|_state, _t, _targets, _catalog| {})
}

// ── Effect primitives and predicate functions are in effects.rs and predicates.rs ──

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
    Main,   // pre- and post-combat main phase priority window
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
    /// Activate a permanent ability. Carries source ObjId + ability def. Uses the stack, passes priority after.
    ActivateAbility(ObjId, AbilityDef),
    /// Intent to cast a spell. No resources are spent until `handle_priority_round` accepts and
    /// commits this action. The framework validates legality (sorcery-speed, etc.) there.
    ///
    /// `preferred_cost` — pre-selected alternate cost (used by `respond_with_counter`).
    /// `counters`       — ObjId of the stack item this spell will counter (counterspell only).
    CastSpell { name: String, preferred_cost: Option<AlternateCost>, counters: Option<ObjId> },
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
    id: ObjId,
    name: String,
    tapped: bool,
    basic: bool,
    #[allow(dead_code)]
    land_types: LandTypes,
    mana_abilities: Vec<ManaAbility>,
}

#[derive(Clone)]
struct SimPermanent {
    id: ObjId,
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
    /// Set when this creature was declared as an attacker this combat.
    attacking: bool,
    /// Set in DeclareBlockers if this attacking creature was not assigned a blocker.
    unblocked: bool,
    /// For planeswalkers: current loyalty (0 for non-planeswalkers).
    loyalty: i32,
    /// Set when a loyalty ability has been activated this turn; reset at Untap.
    pw_activated_this_turn: bool,
    /// Combat attack target: None = attacking the player, Some(id) = attacking that planeswalker.
    attack_target: Option<ObjId>,
    /// Temporary power/toughness modification from continuous effects (e.g. Tamiyo +2).
    /// Cleared at EndCombat.
    power_mod: i32,
    toughness_mod: i32,
}

impl SimPermanent {
    #[allow(dead_code)]
    fn new(name: impl Into<String>) -> Self {
        SimPermanent {
            id: ObjId::UNSET,
            name: name.into(),
            annotation: None,
            counters: 0,
            tapped: false,
            damage: 0,
            entered_this_turn: true,
            mana_abilities: vec![],
            attacking: false,
            unblocked: false,
            loyalty: 0,
            pw_activated_this_turn: false,
            attack_target: None,
            power_mod: 0,
            toughness_mod: 0,
        }
    }
}

impl SimLand {
    fn from_def(name: &str, def: &CardDef) -> Self {
        let land = def.as_land().expect("SimLand::from_def called with non-land");
        SimLand {
            id: ObjId::UNSET,
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
    id: ObjId,
    deck_name: String,
    life: i32,
    library: Vec<(ObjId, String, CardDef)>,
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
    /// Number of cards drawn this turn; reset each Untap. Used for Bowmasters / Tamiyo triggers.
    draws_this_turn: u8,
}

impl PlayerState {
    fn new(deck: &str, mulligans: u8) -> Self {
        PlayerState {
            id: ObjId::UNSET,
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
            draws_this_turn: 0,
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

pub(crate) struct SimState {
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
    current_ap: ObjId,
    /// Current phase/step label (for log context).
    current_phase: String,
    /// Attackers declared this combat (stable ObjIds); cleared at EndCombat.
    combat_attackers: Vec<ObjId>,
    /// Blocker assignments this combat: (attacker_id, blocker_id); cleared at EndCombat.
    combat_blocks: Vec<(ObjId, ObjId)>,
    /// Triggered abilities waiting to be pushed onto the stack at the next priority window.
    pending_triggers: Vec<TriggerContext>,
    /// Active continuous effects (from loyalty abilities, spells, etc.).
    active_effects: Vec<ContinuousEffect>,
    /// All cards in all zones, keyed by stable ObjId. Added as part of staged object model migration.
    cards: HashMap<ObjId, CardObject>,
    /// Activated/triggered abilities currently on the stack, keyed by ObjId.
    abilities: HashMap<ObjId, StackAbility>,
    /// ID allocator — starts at 1; 0 is reserved as ObjId::UNSET.
    next_id: u64,
}

impl SimState {
    fn new(us: PlayerState, opp: PlayerState) -> Self {
        let mut s = SimState {
            turn: 0,
            on_play: true,
            us,
            opp,
            log: Vec::new(),
            reroll: false,
            success: false,
            current_ap: ObjId::UNSET,
            current_phase: String::new(),
            combat_attackers: Vec::new(),
            combat_blocks: Vec::new(),
            pending_triggers: Vec::new(),
            active_effects: Vec::new(),
            cards: HashMap::new(),
            abilities: HashMap::new(),
            next_id: 0,
        };
        s.us.id = s.alloc_id();
        s.opp.id = s.alloc_id();
        s
    }

    fn alloc_id(&mut self) -> ObjId {
        self.next_id += 1;
        ObjId(self.next_id)
    }

    fn card(&self, id: ObjId) -> Option<&CardObject> {
        self.cards.get(&id)
    }

    fn card_mut(&mut self, id: ObjId) -> Option<&mut CardObject> {
        self.cards.get_mut(&id)
    }

    fn permanents_of<'a>(&'a self, who: &'a str) -> impl Iterator<Item = &'a CardObject> {
        self.cards.values().filter(move |c| c.controller == who && c.zone == CardZone::Battlefield)
    }

    fn lands_of<'a>(&'a self, who: &'a str, catalog: &'a HashMap<&str, &CardDef>) -> impl Iterator<Item = &'a CardObject> {
        self.cards.values().filter(move |c| {
            c.controller == who && c.zone == CardZone::Battlefield &&
            catalog.get(c.name.as_str()).map(|d| d.is_land()).unwrap_or(false)
        })
    }

    fn hand_of<'a>(&'a self, who: &'a str) -> impl Iterator<Item = &'a CardObject> {
        self.cards.values().filter(move |c| c.owner == who && matches!(c.zone, CardZone::Hand { .. }))
    }

    fn graveyard_of<'a>(&'a self, who: &'a str) -> impl Iterator<Item = &'a CardObject> {
        self.cards.values().filter(move |c| c.owner == who && c.zone == CardZone::Graveyard)
    }

    fn exile_of<'a>(&'a self, who: &'a str) -> impl Iterator<Item = &'a CardObject> {
        self.cards.values().filter(move |c| c.owner == who && matches!(c.zone, CardZone::Exile { .. }))
    }

    fn library_of<'a>(&'a self, who: &'a str) -> impl Iterator<Item = &'a CardObject> {
        self.cards.values().filter(move |c| c.owner == who && c.zone == CardZone::Library)
    }

    fn find_permanent_by_name<'a>(&'a self, name: &str, controller: &str) -> Option<&'a CardObject> {
        self.cards.values().find(|c| c.name == name && c.controller == controller && c.zone == CardZone::Battlefield)
    }

    fn find_card_by_name_in_zone<'a>(&'a self, name: &str, zone_pred: impl Fn(&CardZone) -> bool, owner: &str) -> Option<&'a CardObject> {
        self.cards.values().find(|c| c.name == name && c.owner == owner && zone_pred(&c.zone))
    }

    fn queue_triggers(&mut self, event: &GameEvent) {
        let triggers = fire_triggers(event, self);
        self.pending_triggers.extend(triggers);
    }

    /// Draw one card for `who`. Increments draws_this_turn and hand.hidden, then fires a Draw event.
    fn sim_draw(&mut self, who: &str, t: u8, is_natural: bool) {
        self.player_mut(who).draws_this_turn += 1;
        let draw_index = self.player(who).draws_this_turn;
        self.player_mut(who).hand.hidden += 1;
        let ev = GameEvent::Draw { controller: who.to_string(), draw_index, is_natural };
        self.queue_triggers(&ev);
        self.log(t, who, if is_natural { "Draw".to_string() } else { format!("draw ({})", draw_index) });
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

    /// Resolve a player name string to its stable ObjId.
    fn player_id(&self, who: &str) -> ObjId {
        if who == "us" { self.us.id } else { self.opp.id }
    }

    /// Resolve a player ObjId back to the "us"/"opp" name string.
    fn who_str(&self, id: ObjId) -> &str {
        if id == self.us.id { "us" } else { "opp" }
    }

    /// Return the controller name ("us"/"opp") of the permanent or land with the given id.
    fn permanent_controller(&self, id: ObjId) -> Option<&str> {
        if self.us.permanents.iter().any(|p| p.id == id) || self.us.lands.iter().any(|l| l.id == id) {
            Some("us")
        } else if self.opp.permanents.iter().any(|p| p.id == id) || self.opp.lands.iter().any(|l| l.id == id) {
            Some("opp")
        } else {
            None
        }
    }

    /// Return the name of the permanent or land with the given id.
    fn permanent_name(&self, id: ObjId) -> Option<String> {
        self.us.permanents.iter().find(|p| p.id == id).map(|p| p.name.clone())
            .or_else(|| self.us.lands.iter().find(|l| l.id == id).map(|l| l.name.clone()))
            .or_else(|| self.opp.permanents.iter().find(|p| p.id == id).map(|p| p.name.clone()))
            .or_else(|| self.opp.lands.iter().find(|l| l.id == id).map(|l| l.name.clone()))
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
        let ctx = if is_player && self.current_ap != ObjId::UNSET {
            format!("|{}/{}", self.who_str(self.current_ap), self.current_phase)
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

fn stage_label(turn: u8) -> &'static str {
    match turn {
        0..=3 => "Early",
        4..=5 => "Mid",
        _ => "Late",
    }
}

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
                if p.loyalty > 0 { tags.push(format!("loyalty: {}", p.loyalty)); }
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
            for card in &self.exile.visible {
                let tag = if self.on_adventure.contains(card) { " [on adventure]" } else { "" };
                writeln!(f, "    * {}{}", card, tag)?;
            }
            if self.exile.hidden > 0 {
                writeln!(f, "    * ({} hidden)", self.exile.hidden)?;
            }
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
    library: &[(ObjId, String, CardDef)],
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
        .filter_map(|(i, (_, _, def))| {
            let land = def.as_land()?;
            if need_black && !land.mana_abilities.iter().any(|ma| ma.produces.contains('B')) { return None; }
            Some((i, 1))
        })
        .collect();
    if weighted.is_empty() { return None; }
    Some(library[weighted_choice(&weighted, rng)].1.clone())
}

/// Play a specific, pre-chosen land from the library (removes the entry).
/// Fetches stay in play to be cracked later in the ability pass.
fn sim_play_land(
    state: &mut SimState,
    t: u8,
    who: &str,
    library: &mut Vec<(ObjId, String, CardDef)>,
    land_name: &str,
) {
    let Some(idx) = library.iter().position(|(_, n, _)| n == land_name) else { return; };
    let mut land = {
        let (_id, name, def) = &library[idx];
        SimLand::from_def(name, def)
    };
    let new_id = state.alloc_id();
    state.cards.insert(new_id, CardObject::new(new_id, land_name.to_string(), who));
    land.id = new_id;
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

// resolve_who, matches_target_type, has_valid_target are defined in predicates.rs

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
    library: &[(ObjId, String, CardDef)],
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
    library: &[(ObjId, String, CardDef)],
    catalog_map: &HashMap<&str, &CardDef>,
    stack: &[StackItem],
) -> Vec<PriorityAction> {
    if state.player(who).hand.hidden <= 0 {
        return Vec::new();
    }
    let permanents_in_play = &state.player(who).permanents;
    let opp_who = if who == "us" { "opp" } else { "us" };

    let mut actions: Vec<PriorityAction> = library
        .iter()
        .filter_map(|(_id, name, def)| {
            if def.is_land() {
                return None;
            }
            if !card_has_implementation(def) {
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
            if let Some(ct) = def.counter_target() {
                let owner_id = state.player_id(who);
                let has_target = stack.iter().any(|item| {
                    if item.owner == owner_id || item.is_ability { return false; }
                    match catalog_map.get(item.name.as_str()) {
                        Some(d) => matches_counter_target(ct, &d.kind),
                        None    => ct == "any",
                    }
                });
                if !has_target { return None; }
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
    for (card_id, _name, def) in library {
        for ab in def.abilities().iter().filter(|ab| ab.zone == "hand") {
            if hand_ability_affordable(ab, state, who) {
                actions.push(PriorityAction::ActivateAbility(*card_id, ab.clone()));
            }
        }
    }

    // Adventure spell face: offer casting the adventure (goes to exile on resolution).
    for (_id, name, def) in library {
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

// choose_permanent_target is defined in predicates.rs

/// Apply a targeted effect (destroy / exile) to a permanent identified by ObjId.
/// Looks up the controller and name from state. Used during resolution.
fn apply_effect_to(
    effect: &str,
    target_id: ObjId,
    state: &mut SimState,
    t: u8,
    log_who: &str,
) {
    let controller = match state.permanent_controller(target_id) {
        Some(c) => c.to_string(),
        None => return,
    };
    let target_name = match state.permanent_name(target_id) {
        Some(n) => n,
        None => return,
    };
    let is_land = state.player(&controller).lands.iter().any(|l| l.id == target_id);
    if is_land {
        state.player_mut(&controller).lands.retain(|l| l.id != target_id);
    } else {
        state.player_mut(&controller).permanents.retain(|p| p.id != target_id);
    }
    match effect {
        "exile" => state.player_mut(&controller).exile.visible.push(target_name.clone()),
        _ => state.player_mut(&controller).graveyard.visible.push(target_name.clone()),
    }
    state.log(t, log_who, format!("{} {} ({})", effect, target_name, controller));
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
    if let Some(target_id) = choose_permanent_target(target_str, log_who, state, catalog_map, rng) {
        apply_effect_to(effect, target_id, state, t, log_who);
    }
}

// matches_search_filter is defined in predicates.rs

/// Pay the activation cost of an ability: mana, life, tap, and/or sacrifice.
/// Effects are NOT applied here — they happen when the ability resolves off the stack.
fn pay_activation_cost(
    state: &mut SimState,
    t: u8,
    who: &str,
    source_id: ObjId,
    ability: &AbilityDef,
    library: &mut Vec<(ObjId, String, CardDef)>,
    _catalog_map: &HashMap<&str, &CardDef>,
) {
    let source_name = state.permanent_name(source_id)
        .or_else(|| library.iter().find(|(id, _, _)| *id == source_id).map(|(_, n, _)| n.clone()))
        .unwrap_or_default();
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
        if let Some(l) = state.player_mut(who).lands.iter_mut().find(|l| l.id == source_id) {
            l.tapped = true;
        }
    }

    // Pay sacrifice cost (in-play permanent or land).
    if ability.sacrifice_self && ability.zone != "hand" {
        let is_land = state.player(who).lands.iter().any(|l| l.id == source_id);
        if is_land {
            state.player_mut(who).lands.retain(|l| l.id != source_id);
        } else {
            state.player_mut(who).permanents.retain(|p| p.id != source_id);
        }
        state.player_mut(who).graveyard.visible.push(source_name.clone());
    }

    // Discard cost (zone="hand"): remove from library, send to graveyard.
    if ability.discard_self {
        if let Some(idx) = library.iter().position(|(_, n, _)| n == source_name.as_str()) {
            library.remove(idx);
            state.player_mut(who).hand.hidden -= 1;
            state.player_mut(who).graveyard.visible.push(source_name.clone());
        }
    }

    // Ninjutsu cost: remove ninja from library (hand) and return an unblocked attacker to hand.
    if ability.ninjutsu {
        if let Some(idx) = library.iter().position(|(_, n, _)| n == source_name.as_str()) {
            library.remove(idx);
            state.player_mut(who).hand.hidden -= 1;
        }
        let unblocked_attacker = state.player(who).permanents.iter()
            .find(|p| p.attacking && p.unblocked)
            .map(|p| (p.id, p.name.clone(), p.attack_target.clone()));
        if let Some((atk_id, atk_name, _atk_target)) = unblocked_attacker {
            state.player_mut(who).permanents.retain(|p| p.id != atk_id);
            state.combat_attackers.retain(|&a| a != atk_id);
            state.combat_blocks.retain(|(a, _)| *a != atk_id);
            state.player_mut(who).hand.hidden += 1;
            state.log(t, who, format!("→ return {} to hand (ninjutsu)", atk_name));
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

    // Loyalty ability: adjust planeswalker loyalty and mark activated this turn.
    if let Some(loyalty_delta) = ability.loyalty_cost {
        let new_loyalty = {
            if let Some(perm) = state.player_mut(who).permanents.iter_mut().find(|p| p.id == source_id) {
                perm.loyalty += loyalty_delta;
                perm.pw_activated_this_turn = true;
                Some(perm.loyalty)
            } else {
                None
            }
        };
        if let Some(new_loyalty) = new_loyalty {
            state.log(t, who, format!("→ {} loyalty {} → {}", source_name,
                if loyalty_delta >= 0 { format!("+{}", loyalty_delta) } else { loyalty_delta.to_string() },
                new_loyalty));
        }
    }
}

/// Apply the resolution effect of an activated ability.
/// Called when the ability stack item resolves (both players pass consecutively).
fn apply_ability_effect(
    state: &mut SimState,
    t: u8,
    who: &str,
    source_id: ObjId,
    ability: &AbilityDef,
    library: &mut Vec<(ObjId, String, CardDef)>,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
    ninjutsu_attack_target: Option<ObjId>,
) {
    let source_name = state.permanent_name(source_id)
        .or_else(|| {
            // For hand-zone abilities (ninjutsu/cycling), the source isn't in play yet;
            // fall back to looking up the name from the stack item's stored name.
            None
        })
        .unwrap_or_default();

    // Ninjutsu effect: ninja enters play tapped and attacking (unblocked).
    if ability.ninjutsu {
        // For ninjutsu, the ninja came from hand — look up the name from the library's
        // stored card def (the card was already removed by pay_activation_cost, so we
        // use the StackItem name which was stored at push time).
        // source_name may be empty here because the ninja left hand before entering play;
        // the stack item's `name` field holds the ninja's name (set at ability push time).
        // We need the name from the stack item — which is the caller's `top.name`.
        // Since apply_ability_effect gets called with the source_id from the StackItem,
        // and the ninja's permanent isn't in play yet, we use the source_name from the
        // ability activation's StackItem.name. For now, re-read from library is not possible
        // (already removed). The source_id from the ActivateAbility action was the ninja's
        // library card id — let's look that up in state.cards.
        let ninja_name = state.cards.get(&source_id)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| source_name.clone());
        let mana_abs = catalog_map.get(ninja_name.as_str())
            .map_or_else(Vec::new, |d| d.mana_abilities().to_vec());
        let new_id = state.alloc_id();
        state.cards.insert(new_id, CardObject::new(new_id, ninja_name.clone(), who));
        state.player_mut(who).permanents.push(SimPermanent {
            id: new_id,
            name: ninja_name.clone(),
            annotation: None,
            counters: 0,
            tapped: true,
            damage: 0,
            entered_this_turn: true,
            mana_abilities: mana_abs,
            attacking: true,
            unblocked: true,
            loyalty: 0,
            pw_activated_this_turn: false,
            attack_target: ninjutsu_attack_target,
            power_mod: 0,
            toughness_mod: 0,
        });
        state.combat_attackers.push(new_id);
        state.log(t, who, format!("{} enters play tapped and attacking (ninjutsu)", ninja_name));
        return;
    }

    // draw:N — draw N cards (cycling, clue, etc.). Fires a draw event per card.
    if let Some(rest) = ability.effect.strip_prefix("draw:") {
        let n: i32 = rest.parse().unwrap_or(1);
        for _ in 0..n {
            state.sim_draw(who, t, false);
        }
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
            .filter(|(_, (_, _, d))| matches_search_filter(filter, d))
            .map(|(i, _)| i)
            .collect();

        if !candidates.is_empty() {
            // Prefer black-producing lands so fetches reliably find a black source.
            let black_candidates: Vec<usize> = candidates.iter()
                .copied()
                .filter(|&i| library[i].2.as_land().map_or(false, |l| l.land_types.swamp || l.mana_abilities.iter().any(|ma| ma.produces.contains('B'))))
                .collect();
            let pool = if !black_candidates.is_empty() { &black_candidates } else { &candidates };
            let idx = pool[rng.gen_range(0..pool.len())];
            let mut land = {
                let (_lid, lname, ldef) = &library[idx];
                let ld = ldef.as_land().expect("search result should be a land");
                SimLand {
                    id: ObjId::UNSET,
                    name: lname.clone(),
                    tapped: false,
                    basic: ld.basic,
                    land_types: ld.land_types.clone(),
                    mana_abilities: ld.mana_abilities.clone(),
                }
            };
            let name = library[idx].1.clone();
            library.remove(idx);
            match dest {
                "play" => {
                    let new_id = state.alloc_id();
                    state.cards.insert(new_id, CardObject::new(new_id, name.clone(), who));
                    land.id = new_id;
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

    // tamiyo_plus_two: register a continuous effect until controller's next turn.
    if ability.effect == "tamiyo_plus_two" {
        state.active_effects.push(tamiyo_plus_two_effect(who));
        state.log(t, who, format!("{} +2: attackers get -1/-0 until your next turn", source_name));
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

// matches_counter_target is defined in predicates.rs

/// Check whether `cost` can be paid by `who` given current state.
/// `source_name` is the counterspell card name (excluded from blue pitch candidates).
fn can_pay_alternate_cost(
    cost: &AlternateCost,
    state: &SimState,
    who: &str,
    source_name: &str,
    library: &[(ObjId, String, CardDef)],
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
            .any(|(_, n, d)| n.as_str() != source_name && !d.is_land() && d.is_blue());
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
    library: &mut Vec<(ObjId, String, CardDef)>,
    _catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    if cost.exile_blue_from_hand {
        let pitch_indices: Vec<usize> = library
            .iter()
            .enumerate()
            .filter(|(_, (_, n, d))| n.as_str() != source_name && !d.is_land() && d.is_blue())
            .map(|(i, _)| i)
            .collect();
        let idx = pitch_indices[rng.gen_range(0..pitch_indices.len())];
        let pitch_name = library[idx].1.clone();
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
    library: &mut Vec<(ObjId, String, CardDef)>,
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

    // Remove the spell from library, capturing its stable ObjId.
    let pos = library.iter().position(|(_, n, _)| n.as_str() == name)?;
    let card_id = library[pos].0;
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
        // Fire exile-from-GY trigger (e.g. Murktide counter).
        let card_type = catalog_map.get(card.as_str())
            .map(|d| if d.is_instant() { "instant" } else if d.is_sorcery() { "sorcery" } else { "" })
            .unwrap_or("");
        let exile_ev = GameEvent::ZoneChange {
            card: card.clone(),
            card_type: card_type.to_string(),
            from: ZoneId::Graveyard,
            to: ZoneId::Exile,
            controller: who.to_string(),
        };
        state.queue_triggers(&exile_ev);
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

    let (spell_target_spec, spell_eff) = spell_effect(name, who, annotation.clone(), catalog_map);
    let spell_chosen_targets = choose_spell_target(&spell_target_spec, who, state, catalog_map)
        .into_iter()
        .collect::<Vec<_>>();

    Some(StackItem {
        id: state.alloc_id(),
        name: name.to_string(),
        owner: state.player_id(who),
        card_id,
        is_ability: false,
        ability_def: None,
        counters: None,

        annotation,
        adventure_exile: false,
        adventure_card_name: None,
        adventure_face: None,
        trigger_context: None,
        chosen_targets: spell_chosen_targets,
        ninjutsu_attack_target: None,
        effect: Some(spell_eff),
    })
}

/// Apply the resolution effects of a non-counter spell: puts it into play or graveyard,
/// and handles cantrip draw, targeted destroy, discard, and life-loss effects.
///
/// `actor_lib` is the caster's library; `other_lib` is the opponent's library.
/// In our model, non-counter spells are always cast by the active player, so
/// actor_lib/other_lib are actor-relative.
/// Build the effect for a non-adventure spell at cast time.
/// Returns `(TargetSpec, Effect)`: the TargetSpec describes the target requirement;
/// the Effect closure applies the card's game text when it resolves off the stack.
///
/// Handles all non-adventure, non-counterspell cards in pilegen.toml.
/// Permanents use `eff_enter_permanent`; spells use composed primitives.
fn spell_effect(
    name: &str,
    owner: &str,
    annotation: Option<String>,
    catalog_map: &HashMap<&str, &CardDef>,
) -> (TargetSpec, Effect) {
    let w = owner.to_string();
    match name {
        // ── Cantrips ──────────────────────────────────────────────────────────
        "Brainstorm" => (TargetSpec::None, eff_draw(w.clone(), 3).then(eff_put_back(w, 2))),
        "Ponder" | "Consider" | "Preordain" => (TargetSpec::None, eff_draw(w, 1)),

        // ── Rituals / mana ────────────────────────────────────────────────────
        "Dark Ritual" => (TargetSpec::None, eff_mana(w, "BBB")),

        // ── Win condition ─────────────────────────────────────────────────────
        "Doomsday" => (TargetSpec::None, eff_doomsday()),

        // ── Targeted removal ──────────────────────────────────────────────────
        "Fatal Push" => (TargetSpec::OpponentCreatureMvLt4, eff_destroy_target(w)),
        "Snuff Out"  => (TargetSpec::OpponentNonblackCreature, eff_destroy_target(w)),

        // ── Discard ───────────────────────────────────────────────────────────
        "Thoughtseize" => (
            TargetSpec::None,
            eff_discard(w.clone(), Who::Opp, 1, true).then(eff_life_loss(w, 2)),
        ),
        "Hymn to Tourach" => (TargetSpec::None, eff_discard(w, Who::Opp, 2, false)),

        // ── Reanimation ───────────────────────────────────────────────────────
        "Unearth" => (TargetSpec::None, eff_reanimate(w, Who::Actor, "creature")),

        // ── Bounce ────────────────────────────────────────────────────────────
        "Petty Theft" => (TargetSpec::AnyOpponentNonlandPermanent, eff_bounce_target(w)),

        // ── Permanents (catch-all: any creature / artifact / planeswalker / enchantment) ──
        _ => {
            if let Some(def) = catalog_map.get(name) {
                match &def.kind {
                    CardKind::Creature(_) | CardKind::Artifact(_)
                    | CardKind::Planeswalker(_) | CardKind::Enchantment => {
                        return (TargetSpec::None, eff_enter_permanent(w, name.to_string(), annotation));
                    }
                    _ => {}
                }
            }
            // Non-permanent spell with no registered game text: no-op Effect
            (TargetSpec::None, Effect(std::sync::Arc::new(|_state, _t, _targets, _catalog, _rng| {})))
        }
    }
}


/// Hypergeometric P(≥1 copy of a card is in the "in-hand" portion of the library pool).
///
/// `library_size` — total cards remaining in the pool (hand + undrawn).
/// `hand_size`    — how many of those are conceptually in hand.
/// `copies`       — how many copies of the card exist in the pool.
fn p_card_in_hand(library_size: usize, hand_size: i32, copies: usize) -> f64 {
    let t = library_size;
    let h = (hand_size.max(0) as usize).min(t);
    let n = copies;
    if n == 0 || h == 0 { return 0.0; }
    if n >= t { return 1.0; }
    // P(0 in hand) = ∏ᵢ₌₀ʰ⁻¹ (T-N-i)/(T-i)
    let mut p_none: f64 = 1.0;
    for i in 0..h {
        let num = t.saturating_sub(n + i);
        if num == 0 { return 1.0; }
        p_none *= num as f64 / (t - i) as f64;
    }
    (1.0 - p_none).max(0.0)
}

/// Try to respond to `stack[target_idx]` by casting a counterspell.
///
/// When `probabilistic = true`:
///   - Per counterspell: roll P(card in hand) via hypergeometric, then a strategic 50% choice
///     (overridden by `cost.prob` if set on the first cost option).
///   - For `exile_blue_from_hand` costs: also roll P(have a blue pitch card in hand).
/// When `probabilistic = false` the attempt is deterministic (used when protecting Doomsday).
///
/// On success, returns a `CastSpell` intent with `counters = Some(target_idx)`.
/// No resources are spent; the caller (`handle_priority_round`) commits the action.
fn respond_with_counter(
    state: &SimState,
    stack: &[StackItem],
    target_idx: usize,
    responding_who: &str,
    responding_library: &[(ObjId, String, CardDef)],
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
    probabilistic: bool,
) -> Option<PriorityAction> {
    let default_kind;
    let target_kind: &CardKind = match catalog_map.get(stack[target_idx].name.as_str()) {
        Some(d) => &d.kind,
        None => { default_kind = CardKind::Sorcery(SpellData::default()); &default_kind }
    };

    let target_has_untapped_lands = state.player(state.who_str(stack[target_idx].owner)).lands.iter().any(|l| !l.tapped);

    // Deduplicate counterspell names so each spell is evaluated once.
    let mut seen = std::collections::HashSet::new();
    let counterspells: Vec<String> = responding_library
        .iter()
        .filter(|(_, n, d)| {
            d.counter_target()
                .is_some_and(|ct| matches_counter_target(ct, target_kind))
                && !d.alternate_costs().is_empty()
                && !(n.as_str() == "Daze" && target_has_untapped_lands)
        })
        .filter_map(|(_, n, _)| seen.insert(n.clone()).then(|| n.clone()))
        .collect();

    if counterspells.is_empty() {
        return None;
    }

    let hand_size = state.player(responding_who).hand.hidden;
    let lib_size  = responding_library.len();

    for cs_name in &counterspells {
        if probabilistic {
            // Roll: is this counterspell in our hand?
            let copies = responding_library.iter().filter(|(_, n, _)| n == cs_name).count();
            let p_have = p_card_in_hand(lib_size, hand_size, copies);
            if !rng.gen_bool(p_have.max(f64::MIN_POSITIVE)) { continue; }

            // Roll: strategic choice to use it (default 50%; per-spell override via first cost.prob).
            let costs = catalog_map[cs_name.as_str()].alternate_costs();
            let strategic = costs.first().and_then(|c| c.prob).unwrap_or(0.5);
            if !rng.gen_bool(strategic) { continue; }
        }

        let costs = catalog_map[cs_name.as_str()].alternate_costs().to_vec();
        for cost in &costs {
            // For pitch costs, also roll whether we have a blue card to pitch.
            if probabilistic && cost.exile_blue_from_hand {
                let n_blue = responding_library.iter()
                    .filter(|(_, n, d)| n.as_str() != cs_name && !d.is_land() && d.is_blue())
                    .count();
                let p_have_blue = p_card_in_hand(lib_size, hand_size, n_blue);
                if !rng.gen_bool(p_have_blue.max(f64::MIN_POSITIVE)) { continue; }
            }
            if can_pay_alternate_cost(cost, state, responding_who, cs_name, responding_library) {
                return Some(PriorityAction::CastSpell {
                    name: cs_name.clone(),
                    preferred_cost: Some(cost.clone()),
                    counters: Some(stack[target_idx].id),
                });
            }
        }
    }
    None
}



// ── Combat helpers ────────────────────────────────────────────────────────────

/// Return (power, toughness) for a permanent, adding any +1/+1 counters and temporary mods.
fn creature_stats(perm: &SimPermanent, def: Option<&CardDef>) -> (i32, i32) {
    let power     = def.and_then(|d| d.as_creature()).map(|c| c.power).unwrap_or(1);
    let toughness = def.and_then(|d| d.as_creature()).map(|c| c.toughness).unwrap_or(1);
    (power + perm.counters + perm.power_mod, toughness + perm.counters + perm.toughness_mod)
}

/// Try to perform ninjutsu during a combat priority window (DeclareBlockers / CombatDamage / EndCombat).
///
/// Requires: unblocked attacker, ninjutsu card in library (treated probabilistically as in-hand),
/// and enough mana. Returns a `Ninjutsu` action or `None` if conditions aren't met.
fn try_ninjutsu(
    state: &SimState,
    who: &str,
    library: &[(ObjId, String, CardDef)],
    rng: &mut impl Rng,
) -> Option<PriorityAction> {
    if state.player(who).hand.hidden <= 0 { return None; }
    // Find an unblocked attacker controlled by `who`.
    let unblocked: Vec<&str> = state.player(who).permanents.iter()
        .filter(|p| p.attacking && p.unblocked)
        .map(|p| p.name.as_str())
        .collect();
    if unblocked.is_empty() { return None; }
    // Find ninjutsu cards in the library (hand + undrawn combined).
    let ninja_indices: Vec<usize> = library.iter()
        .enumerate()
        .filter(|(_, (_, _, def))| def.ninjutsu().is_some())
        .map(|(i, _)| i)
        .collect();
    if ninja_indices.is_empty() { return None; }
    // 35% roll: simulates probability of holding it and wanting to use it.
    if !rng.gen_bool(0.35) { return None; }
    // Pick a random ninja and verify mana.
    let idx = ninja_indices[rng.gen_range(0..ninja_indices.len())];
    let (ninja_id, _ninja_name, ninja_def) = &library[idx];
    let ninjutsu_cost = parse_mana_cost(&ninja_def.ninjutsu()?.mana_cost);
    if !state.player(who).potential_mana().can_pay(&ninjutsu_cost) { return None; }
    Some(PriorityAction::ActivateAbility(*ninja_id, ninja_def.ninjutsu()?.as_ability_def()))
}

// ── Keyword helpers ───────────────────────────────────────────────────────────

fn creature_has_keyword(name: &str, kw: &str, catalog_map: &HashMap<&str, &CardDef>) -> bool {
    catalog_map.get(name).map(|d| d.has_keyword(kw)).unwrap_or(false)
}


/// Deal 1 damage from Bowmasters to the best target on `target_player`'s side.

/// Remove creatures whose accumulated damage meets or exceeds their toughness (SBA).
fn check_lethal_damage(who: &str, state: &mut SimState, t: u8, catalog_map: &HashMap<&str, &CardDef>) {
    let dead: Vec<String> = state.player(who).permanents.iter()
        .filter_map(|p| {
            let def = catalog_map.get(p.name.as_str())?;
            let (_, tgh) = creature_stats(p, Some(def));
            if p.damage >= tgh { Some(p.name.clone()) } else { None }
        })
        .collect();
    for name in dead {
        state.player_mut(who).permanents.retain(|p| p.name != name);
        state.player_mut(who).graveyard.visible.push(name.clone());
        state.log(t, who, format!("{} dies (lethal damage)", name));
        let ev = GameEvent::ZoneChange {
            card: name,
            card_type: "creature".to_string(),
            from: ZoneId::Battlefield,
            to: ZoneId::Graveyard,
            controller: who.to_string(),
        };
        state.queue_triggers(&ev);
    }
}

fn opp_of(who: &str) -> &'static str {
    if who == "us" { "opp" } else { "us" }
}

fn do_amass_orc(controller: &str, n: i32, state: &mut SimState, t: u8) {
    if let Some(army) = state.player_mut(controller).permanents
        .iter_mut().find(|p| p.name == "Orc Army")
    {
        army.counters += n;
        let c = army.counters;
        state.log(t, controller, format!("Orc Army grows to {c}/{c}"));
    } else {
        let new_id = state.alloc_id();
        state.cards.insert(new_id, CardObject::new(new_id, "Orc Army".to_string(), controller));
        state.player_mut(controller).permanents.push(SimPermanent {
            id: new_id,
            name: "Orc Army".to_string(),
            counters: n,
            ..SimPermanent::new("Orc Army")
        });
        state.log(t, controller, format!("Orc Army token created {n}/{n}"));
    }
}

fn do_create_clue(controller: &str, state: &mut SimState, t: u8) {
    let new_id = state.alloc_id();
    state.cards.insert(new_id, CardObject::new(new_id, "Clue Token".to_string(), controller));
    let mut clue = SimPermanent::new("Clue Token");
    clue.id = new_id;
    state.player_mut(controller).permanents.push(clue);
    state.log(t, controller, "Clue Token created");
}

fn do_flip_tamiyo(controller: &str, state: &mut SimState, t: u8, catalog_map: &HashMap<&str, &CardDef>) {
    state.player_mut(controller).permanents
        .retain(|p| p.name != "Tamiyo, Inquisitive Student");
    let loyalty = catalog_map.get("Tamiyo, Seasoned Scholar")
        .and_then(|d| if let CardKind::Planeswalker(ref p) = d.kind { Some(p.loyalty) } else { None })
        .unwrap_or(2);
    let new_id = state.alloc_id();
    state.cards.insert(new_id, CardObject::new(new_id, "Tamiyo, Seasoned Scholar".to_string(), controller));
    state.player_mut(controller).permanents.push(SimPermanent {
        id: new_id,
        loyalty,
        ..SimPermanent::new("Tamiyo, Seasoned Scholar")
    });
    state.log(t, controller, format!("Tamiyo flips → Tamiyo, Seasoned Scholar [loyalty: {}]", loyalty));
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

/// Run a priority round. AP gets priority first; both players must pass consecutively
/// (with an empty stack) for the round to end. When both pass with a non-empty stack,
/// the entire stack resolves LIFO and AP regains priority.
fn handle_priority_round(
    state: &mut SimState,
    t: u8,
    ap: &str,
    dd_turn: u8,
    us_lib: &mut Vec<(ObjId, String, CardDef)>,
    opp_lib: &mut Vec<(ObjId, String, CardDef)>,
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
        // Drain any triggers queued since the last priority check (from step actions or
        // spell resolution). Triggers in no-priority steps are deferred until the next
        // priority window — acceptable because none of our current triggered abilities
        // fire during Untap or Cleanup steps.
        // Drain pending triggers onto the stack, then check SBAs before giving priority.
        let queued = std::mem::take(&mut state.pending_triggers);
        push_triggers(queued, &mut stack, state, catalog_map);
        check_lethal_damage("us",  state, t, catalog_map);
        check_lethal_damage("opp", state, t, catalog_map);

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
            PriorityAction::ActivateAbility(source_id, ref ability) => {
                // For loyalty abilities: sorcery-speed check.
                if ability.loyalty_cost.is_some() && !stack.is_empty() {
                    last_passer = Some(who.clone());
                    priority_holder = if who == ap { nap.to_string() } else { ap.to_string() };
                    continue;
                }
                // For ninjutsu: capture replaced attacker's attack_target BEFORE paying cost.
                let ninjutsu_attack_target = if ability.ninjutsu {
                    state.player(&who).permanents.iter()
                        .find(|p| p.attacking && p.unblocked)
                        .and_then(|p| p.attack_target)
                } else {
                    None
                };
                // Look up source name for the stack item name field (for display/logging).
                let source_name_for_stack = state.permanent_name(source_id)
                    .or_else(|| state.cards.get(&source_id).map(|c| c.name.clone()))
                    .unwrap_or_default();
                // Pay costs now; effect is deferred until the ability resolves.
                let actor_lib = if who == "us" { &mut *us_lib } else { &mut *opp_lib };
                pay_activation_cost(state, t, &who, source_id, ability, actor_lib, catalog_map);
                stack.push(StackItem {
                    id: ObjId::UNSET,
                    name: source_name_for_stack,
                    owner: state.player_id(&who),
                    card_id: source_id,
                    is_ability: true,
                    ability_def: Some(ability.clone()),
                    counters: None,

                    annotation: None,
                    adventure_exile: false,
                    adventure_card_name: None,
                    adventure_face: None,
                    trigger_context: None,
                    chosen_targets: vec![],
                    ninjutsu_attack_target,
                    effect: None,
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
                let Some(pos) = actor_lib.iter().position(|(_, n, _)| n == card_name) else {
                    last_passer = Some(who.clone());
                    priority_holder = if who == ap { nap.to_string() } else { ap.to_string() };
                    continue;
                };
                let adv_card_id = actor_lib[pos].0;
                actor_lib.remove(pos);
                // Pay mana.
                if !face.mana_cost.is_empty() {
                    let cost = parse_mana_cost(&face.mana_cost);
                    let mana_log = state.player_mut(&who).pay_mana(&cost);
                    state.log_mana_activations(t, &who, mana_log);
                }
                state.player_mut(&who).hand.hidden -= 1;
                // Build effect and choose target using the same Effect system as non-adventure spells.
                let (adv_spec, adv_eff) = spell_effect(&face.name, &who, None, catalog_map);
                let adv_targets = choose_spell_target(&adv_spec, &who, state, catalog_map)
                    .into_iter().collect::<Vec<_>>();
                state.log(t, &who, format!("Cast {} (adventure, {})", face.name, face.mana_cost));
                // Push StackItem.
                stack.push(StackItem {
                    id: state.alloc_id(),
                    name: face.name.clone(),
                    owner: state.player_id(&who),
                    card_id: adv_card_id,
                    is_ability: false,
                    ability_def: None,
                    counters: None,
                    annotation: None,
                    adventure_exile: true,
                    adventure_card_name: Some(card_name.clone()),
                    adventure_face: Some(face),
                    trigger_context: None,
                    chosen_targets: adv_targets,
                    ninjutsu_attack_target: None,
                    effect: Some(adv_eff),
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
                let (from_adv_spec, from_adv_eff) = spell_effect(card_name, &who, None, catalog_map);
                let from_adv_targets = choose_spell_target(&from_adv_spec, &who, state, catalog_map)
                    .into_iter()
                    .collect::<Vec<_>>();
                stack.push(StackItem {
                    id: state.alloc_id(),
                    name: card_name.clone(),
                    owner: state.player_id(&who),
                    card_id: ObjId::UNSET, // TODO: look up from state.cards when zone tracking is complete
                    is_ability: false,
                    ability_def: None,
                    counters: None,

                    annotation: None,
                    adventure_exile: false,
                    adventure_card_name: None,
                    adventure_face: None,
                    trigger_context: None,
                    chosen_targets: from_adv_targets,
                    ninjutsu_attack_target: None,
                    effect: Some(from_adv_eff),
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
                        let top_owner_str = state.who_str(top.owner).to_string();
                        if let Some(target_id) = top.counters {
                            if let Some(target_pos) = stack.iter().position(|s| s.id == target_id) {
                                // Target still on stack — counter resolves: remove and graveyard target.
                                let target = stack.remove(target_pos);
                                let target_owner_str = state.who_str(target.owner).to_string();
                                state.log(t, &top_owner_str, &format!("{} counters {}", top.name, target.name));
                                state.player_mut(&target_owner_str).graveyard.visible.push(target.name);
                                state.player_mut(&top_owner_str).graveyard.visible.push(top.name);
                            } else {
                                // Target already gone — counter fizzles.
                                state.player_mut(&top_owner_str).graveyard.visible.push(top.name.clone());
                                state.log(t, &top_owner_str, &format!("{} fizzles (target already resolved)", top.name));
                            }
                        } else if top.is_ability {
                            // Triggered ability resolves.
                            if let Some(ref ctx) = top.trigger_context.clone() {
                                apply_trigger(ctx, &top.chosen_targets, state, t, catalog_map);
                            // Activated ability resolves.
                            } else if let Some(ref ab) = top.ability_def {
                                let (actor_lib, _other_lib) = if top_owner_str == "us" {
                                    (&mut *us_lib, &mut *opp_lib)
                                } else {
                                    (&mut *opp_lib, &mut *us_lib)
                                };
                                apply_ability_effect(state, t, &top_owner_str, top.card_id, ab, actor_lib, catalog_map, rng, top.ninjutsu_attack_target);
                            }
                        } else if top.adventure_exile {
                            // Adventure spell: run the effect (e.g. bounce), then exile to on_adventure.
                            if let Some(ref eff) = top.effect {
                                let rng_dyn: &mut dyn rand::RngCore = rng;
                                eff.call(state, t, &top.chosen_targets, catalog_map, rng_dyn);
                            }
                            let card_name = top.adventure_card_name.as_deref().unwrap_or(&top.name).to_string();
                            state.player_mut(&top_owner_str).exile.visible.push(card_name.clone());
                            state.player_mut(&top_owner_str).on_adventure.push(card_name.clone());
                            state.log(t, &top_owner_str, format!("{} resolves → {} on adventure in exile", top.name, card_name));
                        } else if let Some(ref eff) = top.effect {
                            // New Effect path: used for all non-adventure spells.
                            let is_perm = catalog_map.get(top.name.as_str())
                                .map(|d| matches!(d.kind, CardKind::Creature(_) | CardKind::Artifact(_)
                                    | CardKind::Planeswalker(_) | CardKind::Enchantment))
                                .unwrap_or(false);
                            if !is_perm {
                                state.player_mut(&top_owner_str).graveyard.visible.push(top.name.clone());
                                state.log(t, &top_owner_str, format!("{} resolves", top.name));
                            }
                            let rng_dyn: &mut dyn rand::RngCore = rng;
                            eff.call(state, t, &top.chosen_targets, catalog_map, rng_dyn);
                        } else {
                            // effect: None — treat as no-op (should not occur in production).
                            state.player_mut(&top_owner_str).graveyard.visible.push(top.name.clone());
                            state.log(t, &top_owner_str, format!("{} resolves", top.name));
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
    us_lib: &mut Vec<(ObjId, String, CardDef)>,
    opp_lib: &mut Vec<(ObjId, String, CardDef)>,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) {
    state.current_phase = match step.kind {
        StepKind::Untap            => "Untap",
        StepKind::Upkeep           => "Upkeep",
        StepKind::Draw             => "Draw",
        StepKind::Main             => "Main",
        StepKind::BeginCombat      => "BeginCombat",
        StepKind::DeclareAttackers => "DeclareAttackers",
        StepKind::DeclareBlockers  => "DeclareBlockers",
        StepKind::CombatDamage     => "CombatDamage",
        StepKind::EndCombat        => "EndCombat",
        StepKind::End              => "EndStep",
        StepKind::Cleanup          => "Cleanup",
    }.to_string();
    match step.kind {
        StepKind::Untap => {
            for land in &mut state.player_mut(ap).lands {
                land.tapped = false;
            }
            for perm in &mut state.player_mut(ap).permanents {
                perm.tapped = false;
                perm.entered_this_turn = false;
                perm.pw_activated_this_turn = false;
            }
            state.player_mut(ap).land_drop_available = true;
            state.player_mut(ap).spells_cast_this_turn = 0;
            state.player_mut(ap).pending_actions.clear();
            state.player_mut(ap).draws_this_turn = 0;
            // Expire "until your next turn" effects whose controller is now the active player.
            state.active_effects.retain(|e| {
                !(e.expires == EffectExpiry::StartOfControllerNextTurn && e.controller == ap)
            });
        }
        StepKind::Draw => {
            let this_player_on_play = if ap == "us" { on_play } else { !on_play };
            let skip = this_player_on_play && t == 1;
            if skip {
                state.log(t, ap, "No draw (on the play)");
            } else {
                state.sim_draw(ap, t, true);
            }
        }
        StepKind::Cleanup => {
            sim_discard_to_limit(state, t, ap);
            for perm in &mut state.player_mut(ap).permanents {
                perm.damage = 0;
            }
            // Expire EndOfTurn continuous effects, undoing any StatMod they applied.
            let expiring: Vec<ContinuousEffect> = state.active_effects.iter()
                .filter(|e| e.expires == EffectExpiry::EndOfTurn)
                .cloned()
                .collect();
            state.active_effects.retain(|e| e.expires != EffectExpiry::EndOfTurn);
            for effect in expiring {
                if let Some(sm) = &effect.stat_mod {
                    let controller = state.permanent_controller(sm.target_id).map(|s| s.to_string());
                    if let Some(controller) = controller {
                        if let Some(p) = state.player_mut(&controller).permanents.iter_mut().find(|p| p.id == sm.target_id) {
                            p.power_mod -= sm.power_delta;
                            p.toughness_mod -= sm.toughness_delta;
                        }
                    }
                }
            }
        }
        StepKind::DeclareAttackers => {
            let nap = if ap == "us" { "opp" } else { "us" };
            // For each candidate attacker, compute the relevant NAP blocking power:
            // only untapped NAP creatures that *can* block that attacker count.
            // Flying attackers are only threatened by flying blockers.
            let nap_blockers: Vec<(String, i32)> = state.player(nap).permanents.iter()
                .filter(|p| !p.tapped)
                .filter_map(|p| {
                    let def = catalog_map.get(p.name.as_str());
                    if def.map(|d| d.is_creature()).unwrap_or(false) {
                        Some((p.name.clone(), creature_stats(p, def.copied()).0))
                    } else { None }
                })
                .collect();
            let attackers: Vec<ObjId> = state.player(ap).permanents.iter()
                .filter(|p| !p.tapped && !p.entered_this_turn)
                .filter_map(|p| {
                    let def = catalog_map.get(p.name.as_str());
                    if def.map(|d| d.is_creature()).unwrap_or(false) {
                        let atk_flies = creature_has_keyword(&p.name, "flying", catalog_map);
                        // Sum power of NAP creatures that can block this attacker.
                        let relevant_power: i32 = nap_blockers.iter()
                            .filter(|(blk_name, _)| {
                                // A flyer can only be blocked by flyers.
                                !atk_flies || creature_has_keyword(blk_name, "flying", catalog_map)
                            })
                            .map(|(_, pow)| pow)
                            .sum();
                        let (_, tgh) = creature_stats(p, def.copied());
                        if tgh > relevant_power { Some(p.id) } else { None }
                    } else { None }
                })
                .collect();
            // Assign attack targets: each attacker picks the player or a random NAP planeswalker.
            let nap_pw_ids: Vec<ObjId> = state.player(nap).permanents.iter()
                .filter(|p| catalog_map.get(p.name.as_str())
                    .map_or(false, |def| matches!(def.kind, CardKind::Planeswalker(_))))
                .map(|p| p.id)
                .collect();
            for &atk_id in &attackers {
                let target: Option<ObjId> = if !nap_pw_ids.is_empty() && rng.gen_bool(0.5) {
                    Some(nap_pw_ids[rng.gen_range(0..nap_pw_ids.len())])
                } else {
                    None
                };
                if let Some(p) = state.player_mut(ap).permanents.iter_mut().find(|p| p.id == atk_id) {
                    p.attack_target = target;
                    p.tapped = true;
                    p.attacking = true;
                }
            }
            if !attackers.is_empty() {
                let atk_descs: Vec<String> = attackers.iter().filter_map(|&atk_id| {
                    let p = state.player(ap).permanents.iter().find(|p| p.id == atk_id)?;
                    let target_name = p.attack_target
                        .and_then(|id| state.permanent_name(id))
                        .unwrap_or_else(|| "player".to_string());
                    Some(format!("{} → {}", p.name, target_name))
                }).collect();
                state.log(t, ap, format!("Declare attackers: {}", atk_descs.join(", ")));
            }
            state.combat_attackers = attackers.clone();

            // Fire per-creature and step events AFTER attackers are marked.
            for &atk_id in &attackers {
                if let Some(p) = state.player(ap).permanents.iter().find(|p| p.id == atk_id) {
                    let ev = GameEvent::CreatureAttacked {
                        attacker_id: p.id,
                        attacker: p.name.clone(),
                        attacker_controller: ap.to_string(),
                        attack_target: p.attack_target,
                    };
                    state.queue_triggers(&ev);
                }
            }
            let step_ev = GameEvent::EnteredStep {
                step: StepKind::DeclareAttackers,
                active_player: ap.to_string(),
            };
            state.queue_triggers(&step_ev);
        }
        StepKind::DeclareBlockers => {
            let nap = if ap == "us" { "opp" } else { "us" };
            let mut used_blockers: std::collections::HashSet<ObjId> = Default::default();
            let mut blocks: Vec<(ObjId, ObjId)> = Vec::new();
            for &atk_id in &state.combat_attackers.clone() {
                let (atk_name, atk_def, atk_pow, atk_tgh) = {
                    let atk_perm = state.player(ap).permanents.iter().find(|p| p.id == atk_id);
                    match atk_perm {
                        Some(p) => {
                            let def = catalog_map.get(p.name.as_str()).copied();
                            let (pow, tgh) = creature_stats(p, def);
                            (p.name.clone(), def, pow, tgh)
                        }
                        None => continue,
                    }
                };
                let atk_flies = creature_has_keyword(&atk_name, "flying", catalog_map);
                let blocker_id = state.player(nap).permanents.iter()
                    .filter(|p| !p.tapped && !used_blockers.contains(&p.id))
                    .filter_map(|p| {
                        let def = catalog_map.get(p.name.as_str()).copied();
                        if def.map(|d| d.is_creature()).unwrap_or(false) {
                            // Flying attackers can only be blocked by flying creatures.
                            if atk_flies && !creature_has_keyword(&p.name, "flying", catalog_map) {
                                return None;
                            }
                            let (blk_pow, blk_tgh) = creature_stats(p, def);
                            // Good block: kills attacker OR both survive (touch butts). Not a chump.
                            let good_block = blk_pow >= atk_tgh || atk_pow < blk_tgh;
                            if good_block { Some((p.id, p.name.clone())) } else { None }
                        } else { None }
                    })
                    .next();
                if let Some((blk_id, blk_name)) = blocker_id {
                    state.log(t, nap, format!("{} blocks {}", blk_name, atk_name));
                    used_blockers.insert(blk_id);
                    blocks.push((atk_id, blk_id));
                }
                let _ = atk_def; // suppress unused warning
            }
            state.combat_blocks = blocks;
            // Mark unblocked attackers so ninjutsu can target them.
            let blocked_atk_ids: std::collections::HashSet<ObjId> = state.combat_blocks.iter()
                .map(|(a, _)| *a).collect();
            let unblocked_ids: Vec<ObjId> = state.combat_attackers.iter()
                .filter(|&&a| !blocked_atk_ids.contains(&a))
                .copied()
                .collect();
            for id in unblocked_ids {
                if let Some(p) = state.player_mut(ap).permanents.iter_mut().find(|p| p.id == id) {
                    p.unblocked = true;
                }
            }
        }
        StepKind::CombatDamage => {
            if !state.combat_attackers.is_empty() {
                let nap = if ap == "us" { "opp" } else { "us" };
                let attackers   = state.combat_attackers.clone();
                let block_pairs = state.combat_blocks.clone();
                let blocked_atk_ids: std::collections::HashSet<ObjId> = block_pairs.iter()
                    .map(|(a, _)| *a).collect();

                let mut player_damage = 0i32;

                for &(atk_id, blk_id) in &block_pairs {
                    let atk_pow = {
                        let p = state.player(ap).permanents.iter().find(|p| p.id == atk_id);
                        p.map(|p| creature_stats(p, catalog_map.get(p.name.as_str()).copied()).0)
                         .unwrap_or(1)
                    };
                    let blk_pow = {
                        let p = state.player(nap).permanents.iter().find(|p| p.id == blk_id);
                        p.map(|p| creature_stats(p, catalog_map.get(p.name.as_str()).copied()).0)
                         .unwrap_or(1)
                    };
                    if let Some(p) = state.player_mut(ap).permanents.iter_mut().find(|p| p.id == atk_id) {
                        p.damage += blk_pow;
                    }
                    if let Some(p) = state.player_mut(nap).permanents.iter_mut().find(|p| p.id == blk_id) {
                        p.damage += atk_pow;
                    }
                }

                let mut pw_damage: HashMap<ObjId, i32> = HashMap::new();
                for &atk_id in &attackers {
                    if !blocked_atk_ids.contains(&atk_id) {
                        let atk_perm = state.player(ap).permanents.iter().find(|p| p.id == atk_id);
                        let atk_pow = atk_perm.map(|p| creature_stats(p, catalog_map.get(p.name.as_str()).copied()).0).unwrap_or(1);
                        let attack_target = atk_perm.and_then(|p| p.attack_target);
                        match attack_target {
                            None => player_damage += atk_pow,
                            Some(pw_id) => *pw_damage.entry(pw_id).or_insert(0) += atk_pow,
                        }
                    }
                }

                if player_damage > 0 {
                    state.lose_life(nap, player_damage);
                    state.log(t, ap, format!("Combat: {} unblocked damage to {} (life: {})", player_damage, nap, state.life_of(nap)));
                }
                for (&pw_id, &dmg) in &pw_damage {
                    let new_loyalty = {
                        if let Some(perm) = state.player_mut(nap).permanents.iter_mut().find(|p| p.id == pw_id) {
                            perm.loyalty -= dmg;
                            Some(perm.loyalty)
                        } else {
                            None
                        }
                    };
                    if let Some(new_loyalty) = new_loyalty {
                        let pw_name = state.permanent_name(pw_id).unwrap_or_default();
                        state.log(t, ap, format!("Combat: {} damage to {} (loyalty: {})", dmg, pw_name, new_loyalty));
                    }
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

                // SBA: destroy planeswalkers with loyalty ≤ 0.
                for owner in [ap, nap] {
                    let dying_pw: Vec<String> = state.player(owner).permanents.iter()
                        .filter(|p| {
                            catalog_map.get(p.name.as_str())
                                .map_or(false, |def| matches!(def.kind, CardKind::Planeswalker(_)))
                                && p.loyalty <= 0
                        })
                        .map(|p| p.name.clone())
                        .collect();
                    for name in dying_pw {
                        state.log(t, owner, format!("{} is destroyed (loyalty 0)", name));
                        state.player_mut(owner).permanents.retain(|p| p.name != name);
                        state.player_mut(owner).graveyard.visible.push(name);
                    }
                }
            }
        }
        StepKind::EndCombat => {
            state.combat_attackers.clear();
            state.combat_blocks.clear();
            for p in state.us.permanents.iter_mut() { p.attacking = false; p.unblocked = false; }
            for p in state.opp.permanents.iter_mut() { p.attacking = false; p.unblocked = false; }
        }
        StepKind::Upkeep | StepKind::BeginCombat | StepKind::End | StepKind::Main => {
            // No automatic actions.
        }
    }

    // Fire EnteredStep for all priority-bearing steps.
    // DeclareAttackers fires it inside its own arm (after p.attacking is set) so skip it here.
    if step.prio && step.kind != StepKind::DeclareAttackers {
        let step_ev = GameEvent::EnteredStep {
            step: step.kind,
            active_player: ap.to_string(),
        };
        state.queue_triggers(&step_ev);
    }

    if step.prio {
        handle_priority_round(state, t, ap, dd_turn, us_lib, opp_lib, catalog_map, rng);
    }
    // Mana pool drains at the end of every step.
    state.us.pool.drain();
    state.opp.pool.drain();
}

/// Activate each AP planeswalker's loyalty ability once (100% — never skip).
/// Called at the end of each main phase, after the regular priority round.
/// For each unactivated PW, picks a random available loyalty ability and runs a priority round.
fn activate_planeswalkers(
    state: &mut SimState,
    t: u8,
    ap: &str,
    dd_turn: u8,
    us_lib: &mut Vec<(ObjId, String, CardDef)>,
    opp_lib: &mut Vec<(ObjId, String, CardDef)>,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) {
    // Collect unactivated PW IDs + names + their current loyalty snapshot.
    let pw_data: Vec<(ObjId, String, i32)> = state.player(ap).permanents.iter()
        .filter(|p| !p.pw_activated_this_turn)
        .filter(|p| catalog_map.get(p.name.as_str())
            .map_or(false, |def| matches!(def.kind, CardKind::Planeswalker(_))))
        .map(|p| (p.id, p.name.clone(), p.loyalty))
        .collect();

    for (pw_id, name, loyalty) in pw_data {
        let def = match catalog_map.get(name.as_str()) { Some(d) => *d, None => continue };
        let available: Vec<AbilityDef> = def.abilities().iter()
            .filter(|ab| {
                let Some(cost) = ab.loyalty_cost else { return false; };
                !(cost < 0 && loyalty < -cost)
            })
            .cloned()
            .collect();
        if available.is_empty() { continue; }
        let ab = available[rng.gen_range(0..available.len())].clone();
        state.player_mut(ap).pending_actions = vec![PriorityAction::ActivateAbility(pw_id, ab)];
        handle_priority_round(state, t, ap, dd_turn, us_lib, opp_lib, catalog_map, rng);
        state.us.pool.drain();
        state.opp.pool.drain();
        if state.done() { return; }
    }
}

/// Execute a full phase: run each step, then optionally run a phase-level priority round.
fn do_phase(
    state: &mut SimState,
    t: u8,
    ap: &str,
    phase: &Phase,
    dd_turn: u8,
    on_play: bool,
    us_lib: &mut Vec<(ObjId, String, CardDef)>,
    opp_lib: &mut Vec<(ObjId, String, CardDef)>,
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
        let step_ev = GameEvent::EnteredStep {
            step: StepKind::Main,
            active_player: ap.to_string(),
        };
        state.queue_triggers(&step_ev);
        let on_board = collect_on_board_actions(state, ap, t, dd_turn, catalog_map, rng);
        state.player_mut(ap).pending_actions = on_board;
        handle_priority_round(state, t, ap, dd_turn, us_lib, opp_lib, catalog_map, rng);
        // Mana pool drains at the end of the main phase.
        state.us.pool.drain();
        state.opp.pool.drain();
        if state.done() { return; }
        // Activate each AP planeswalker's loyalty ability (100% — runs after all other actions).
        activate_planeswalkers(state, t, ap, dd_turn, us_lib, opp_lib, catalog_map, rng);
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
    us_lib: &mut Vec<(ObjId, String, CardDef)>,
    opp_lib: &mut Vec<(ObjId, String, CardDef)>,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) {
    state.current_ap = state.player_id(ap);
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
                .map(|d| std::iter::repeat((ObjId::UNSET, name.clone(), (*d).clone())).take(*qty as usize))
        })
        .flatten()
        .collect();

    state.opp.library = opp_cards
        .iter()
        .filter(|(_, _, b)| b == "main")
        .filter_map(|(name, qty, _)| {
            catalog_map
                .get(name.as_str())
                .map(|d| std::iter::repeat((ObjId::UNSET, name.clone(), (*d).clone())).take(*qty as usize))
        })
        .flatten()
        .collect();

    // Assign stable ObjIds to all library cards and register in state.cards.
    {
        let us_names: Vec<String> = state.us.library.iter().map(|(_, n, _)| n.clone()).collect();
        for (i, name) in us_names.iter().enumerate() {
            let id = state.alloc_id();
            state.us.library[i].0 = id;
            state.cards.insert(id, CardObject::new(id, name.clone(), "us"));
        }
    }
    {
        let opp_names: Vec<String> = state.opp.library.iter().map(|(_, n, _)| n.clone()).collect();
        for (i, name) in opp_names.iter().enumerate() {
            let id = state.alloc_id();
            state.opp.library[i].0 = id;
            state.cards.insert(id, CardObject::new(id, name.clone(), "opp"));
        }
    }

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
fn reveal_hand(zone: &mut Zone, library: &[(ObjId, String, CardDef)], count: i32, rng: &mut impl Rng) {
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
        zone.visible.push(library[idx].1.clone());
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
/// - Permanents (creatures, artifacts, planeswalkers, enchantments) are always castable.
/// - Spells need a counter_target, abilities, or a known entry in `spell_effect`.
fn card_has_implementation(def: &CardDef) -> bool {
    if def.is_land() { return true; }
    if !def.abilities().is_empty() { return true; }
    if def.counter_target().is_some() { return true; }
    match &def.kind {
        CardKind::Creature(_) | CardKind::Artifact(_)
        | CardKind::Planeswalker(_) | CardKind::Enchantment => true,
        CardKind::Instant(_) | CardKind::Sorcery(_) => {
            matches!(def.name.as_str(),
                "Brainstorm" | "Ponder" | "Consider" | "Preordain"
                | "Dark Ritual"
                | "Doomsday"
                | "Fatal Push" | "Snuff Out"
                | "Thoughtseize" | "Hymn to Tourach"
                | "Unearth"
                | "Petty Theft"
            )
        }
        CardKind::Land(_) => true,
    }
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

