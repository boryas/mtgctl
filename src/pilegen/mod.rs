use clap::Args;
use dialoguer::Confirm;
use diesel::prelude::*;
use rand::Rng;
use skim::prelude::*;
use std::collections::{HashMap, HashSet};
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
use strategy::{decide_action, declare_attackers, declare_blockers};
#[cfg(test)] use strategy::try_ninjutsu;

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

}

/// Zone a card currently occupies.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum CardZone {
    Library,
    Hand { known: bool },   // known = identity visible to opponent
    Stack,
    Battlefield,
    Graveyard,
    Exile { on_adventure: bool },
}

/// Spell-on-stack state for a card while it's on the stack.
/// Populated at cast time; cleared when the spell resolves or is countered.
#[derive(Clone)]
struct SpellState {
    effect: Option<Effect>,
    chosen_targets: Vec<Target>,
    /// True when the back face of a split card was cast (e.g. an adventure instant).
    is_back_face: bool,
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
    /// Active face index for double-faced cards (0 = front, 1 = back). Flip sets this to 1.
    active_face: u8,
}

impl BattlefieldState {
    fn new() -> Self {
        BattlefieldState {
            tapped: false, damage: 0, entered_this_turn: true, counters: 0,
            power_mod: 0, toughness_mod: 0, loyalty: 0, pw_activated_this_turn: false,
            attacking: false, unblocked: false, attack_target: None,
            active_face: 0,
        }
    }
}

/// A card as a game object — follows the card through all zone changes.
/// Carries only game-accumulated state. The card's characteristics are derived
/// by looking up `catalog_key` in the catalog and applying continuous effects.
#[derive(Clone)]
struct GameObject {
    id: ObjId,
    catalog_key: String,  // foreign key into the CardDef catalog
    owner: String,        // "us" or "opp" — kept as String for compat with existing player(&str) API
    controller: String,
    zone: CardZone,
    is_token: bool,
    bf: Option<BattlefieldState>,      // Some only when zone == Battlefield
    spell: Option<SpellState>,         // Some only when zone == Stack (spell on stack)
}

impl GameObject {
    fn new(id: ObjId, catalog_key: impl Into<String>, owner: impl Into<String>) -> Self {
        let owner = owner.into();
        GameObject {
            id, catalog_key: catalog_key.into(), controller: owner.clone(), owner,
            zone: CardZone::Library, is_token: false, bf: None, spell: None,
        }
    }
}


/// An activated or triggered ability on the stack. Not counterable.
#[derive(Clone)]
pub(crate) struct StackAbility {
    /// The stable ObjId for this ability (also the key in `SimState::abilities`).
    #[allow(dead_code)]
    pub(crate) id: ObjId,
    pub(crate) source_name: String,
    pub(crate) owner: ObjId,         // player id
    pub(crate) effect: Effect,
    pub(crate) chosen_targets: Vec<Target>,
}

// ── Trigger system ────────────────────────────────────────────────────────────

/// Zones a card or permanent can occupy.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum ZoneId {
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
#[allow(dead_code)]
pub(super) enum GameEvent {
    /// A card moved from one zone to another (ETB, GY→Exile, etc.).
    /// Does NOT include drawing — use `Draw` for that.
    ZoneChange {
        id: ObjId,
        actor: String,
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
    /// Only fires for named steps that have a priority round (not Untap or Cleanup).
    EnteredStep {
        step: StepKind,
        active_player: String,
    },
    /// Fired at the start of a phase-level priority window (main phases, which have no named steps).
    EnteredPhase {
        phase: PhaseKind,
    },
    /// A creature was declared as an attacker.
    CreatureAttacked {
        attacker_id: ObjId,
        attacker_controller: String,
    },
    // Future variants: DamageDealt, SpellCast, SpellResolved, AbilityActivated,
    //                  CounterChanged, LifeChanged, TokenCreated.
}

/// Data stored with a triggered ability waiting to be pushed onto the stack.
/// The effect closure captures all context (targets, source data) at trigger-push time.
#[derive(Clone)]
pub(crate) struct TriggerContext {
    /// Display name of the source — used for stack item naming and logging.
    pub(crate) source_name: String,
    /// Player who controls that permanent.
    pub(crate) controller: String,
    /// Legal targets this trigger may choose from. Resolved when pushed to the stack.
    pub(crate) target_spec: TargetSpec,
    /// The effect to apply when this trigger resolves. Receives the chosen targets.
    pub(crate) effect: Effect,
}

// ── Triggers and replacement effects ─────────────────────────────────────────

/// Signature for a per-card trigger check function.
/// Inspects the event, and if a trigger fires, appends a `TriggerContext` to `pending`.
pub(super) type TriggerCheckFn =
    std::sync::Arc<dyn Fn(&GameEvent, ObjId, &str, &mut Vec<TriggerContext>) + Send + Sync>;

/// Signature for a per-card replacement check function.
/// Returns Some(targets) if this replacement applies to the event; None otherwise.
/// `source_id` is passed so self-ETB checks work without string dispatch.
pub(super) type ReplacementCheckFn = fn(&GameEvent, ObjId, &str) -> Option<Vec<Target>>;

/// One trigger registration per card object in the simulation.
/// Created at sim init (`active: false`); flipped to `true` when the card enters the battlefield.
/// Dynamically-created triggers (e.g. Tamiyo +2 watcher) start with `active: true`.
pub(super) struct TriggerInstance {
    pub(super) source_id: ObjId,
    pub(super) controller: String,
    pub(super) check: TriggerCheckFn,
    /// None for permanent (card-based) triggers; Some for floating triggers created by abilities.
    pub(super) expiry: Option<ContinuousExpiry>,
    pub(super) active: bool,
}

/// One replacement registration per card object in the simulation.
/// Created at sim init (`active: false`); flipped to `true` when the card enters the battlefield.
/// `id` is stable across the whole simulation — used for loop prevention in `repl_applied`.
pub(super) struct ReplacementInstance {
    pub(super) id: ObjId,
    pub(super) source_id: ObjId,
    pub(super) controller: String,
    pub(super) check: ReplacementCheckFn,
    pub(super) effect: Effect,
    pub(super) active: bool,
}

// ── Continuous effects (new model) ───────────────────────────────────────────

/// The seven layers in which continuous effects are applied (MTG rule 613).
/// Ordering is derived: effects in earlier layers apply before later ones.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(super) enum ContinuousLayer {
    L1CopyEffects      = 1,
    L2ControlEffects   = 2,
    L3TextEffects      = 3,
    L4TypeEffects      = 4,
    L5ColorEffects     = 5,
    L6AbilityEffects   = 6,
    L7PowerToughness   = 7,
}

/// Closure that mutates a cloned `CardDef` to apply a continuous effect modifier.
/// Receives `&SimState` so CDAs (characteristic-defining abilities) can read live game state.
pub(super) type ContinuousModFn =
    std::sync::Arc<dyn Fn(&mut CardDef, &SimState) + Send + Sync>;

/// Predicate that decides whether a continuous effect applies to a given object.
/// Receives (target_id, target_controller).
pub(super) type ContinuousFilterFn =
    std::sync::Arc<dyn Fn(ObjId, &str) -> bool + Send + Sync>;

/// When a `ContinuousInstance` expires and should be removed.
#[derive(Clone, PartialEq, Debug)]
pub(super) enum ContinuousExpiry {
    /// Removed during the Cleanup step of the current turn.
    EndOfTurn,
    /// Removed at the start of the controlling player's next Untap step.
    StartOfControllerNextTurn,
    /// Tied to a permanent being on the battlefield; removed when it leaves play.
    /// Used for static abilities (intrinsic CEs that a card grants to itself).
    WhileSourceOnBattlefield,
}

/// A single registered continuous-effect instance.
/// Created when a spell or ability that grants a CE resolves.
/// Removed when `expiry` is met.
pub(super) struct ContinuousInstance {
    /// Object that generated this effect (for expiry tracking and logging).
    pub(super) source_id: ObjId,
    /// Controller of the source at the time the effect was created.
    pub(super) controller: String,
    /// Which layer this modifier applies in (determines application order).
    pub(super) layer: ContinuousLayer,
    /// Determines which game objects this CE affects.
    pub(super) filter: ContinuousFilterFn,
    /// Mutates the target object's cloned `CardDef`.
    pub(super) modifier: ContinuousModFn,
    /// When this instance should be removed.
    pub(super) expiry: ContinuousExpiry,
}

/// Snapshot of all game objects' effective `CardDef` after continuous effects are applied.
/// Covers every zone (battlefield, hand, library, GY, exile, stack) so that zone-spanning CEs
/// (e.g. Painter's Servant, Mycosynth Lattice) are reflected everywhere.
/// Produced by `recompute` after each state-mutating tick (generation advance).
/// Strategy and display code read from this; they never access raw `CardDef` fields directly.
pub(super) struct MaterializedState {
    /// Generation counter from the `SimState` at the time of recompute.
    pub(super) generation: u64,
    /// Effective `CardDef` per game object (all zones), post-CE application.
    pub(super) defs: HashMap<ObjId, CardDef>,
}

// ── Recompute ─────────────────────────────────────────────────────────────────

/// Fold game-accumulated object state (counters, temporary P/T mods) into a cloned `CardDef`
/// before continuous-effect modifiers run. This makes counters and other game-state
/// deltas visible to layer modifiers that inspect P/T (e.g. Tarmogoyf's self-referential
/// P/T which would interact with a CE modifying it).
fn fold_game_state_into_def(def: &mut CardDef, obj: &GameObject) {
    let Some(bf) = &obj.bf else { return };
    if let CardKind::Creature(c) = &mut def.kind {
        c.adjust_pt(bf.counters + bf.power_mod, bf.counters + bf.toughness_mod);
    }
}

/// Produce a `MaterializedState` snapshot by applying all active `ContinuousInstance`s
/// to clones of each game object's `CardDef`.
///
/// All zones are covered: CEs such as Painter's Servant and Mycosynth Lattice can modify
/// per-card characteristics in every zone (hand, library, GY, exile, stack, battlefield).
/// Objects with no entry in the catalog (e.g. naked stack abilities) are silently skipped
/// by the `catalog.get()` guard below.
///
/// Called after every `fire_event` at recursion depth 0 (each "tick"). Strategy and display
/// code use the resulting snapshot; they never read raw `CardDef` fields directly.
///
/// Application order: instances sorted by `layer` ascending (L1 before L7), then by
/// insertion order within the same layer (stable sort preserves registration order).
pub(super) fn recompute(state: &SimState, catalog: &HashMap<&str, &CardDef>) -> MaterializedState {
    let mut defs: HashMap<ObjId, CardDef> = HashMap::new();

    for (id, obj) in &state.objects {
        // Objects with no catalog entry (naked stack abilities) are excluded.
        let Some(&base) = catalog.get(obj.catalog_key.as_str()) else { continue };
        let mut def = base.clone();

        // Step 0: for double-faced cards on their back face, substitute the back-face kind
        // and name before folding. This ensures fold sees the correct variant (e.g. Planeswalker
        // instead of Creature for flipped Tamiyo) and CEs apply to the active face.
        if obj.bf.as_ref().map_or(false, |bf| bf.active_face == 1) {
            if let Some(ref back) = def.back.take() {
                def.name = back.name.clone();
                def.kind = back.kind.clone();
            }
        }

        // Step 1: fold game-accumulated state (counters, temporary mods) into the clone.
        fold_game_state_into_def(&mut def, obj);

        // Step 2: collect applicable continuous instances and sort by layer.
        let mut applicable: Vec<&ContinuousInstance> = state.continuous_instances
            .iter()
            .filter(|ci| (ci.filter)(*id, &obj.controller))
            .collect();
        applicable.sort_by_key(|ci| ci.layer);

        // Step 3: apply each modifier in layer order. Pass `state` so CDAs can read live data.
        for ci in applicable {
            (ci.modifier)(&mut def, state);
        }

        defs.insert(*id, def);
    }

    MaterializedState { generation: state.generation, defs }
}

// ── Turn structure ────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum PhaseKind {
    Beginning,
    PreCombatMain,
    Combat,
    PostCombatMain,
    End,
}

#[derive(Clone, Copy, Debug)]
enum TurnPosition {
    Step(StepKind),
    Phase(PhaseKind),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum StepKind {
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

/// Which face of a card to cast. `Back` = adventure/split second half.
#[derive(Clone, Copy, PartialEq, Debug)]
enum SpellFace { Main, Back }

#[derive(Clone)]
enum PriorityAction {
    /// Land drop: AP only, does NOT pass priority. Carries the chosen land name.
    LandDrop(ObjId),
    /// Activate a permanent ability. Carries source ObjId + ability def. Uses the stack, passes priority after.
    ActivateAbility(ObjId, AbilityDef),
    /// Intent to cast a spell. No resources are spent until `handle_priority_round` accepts and
    /// commits this action. The framework validates legality (sorcery-speed, etc.) there.
    /// `face` selects main vs adventure face/cost; card zone identifies the source zone.
    /// `preferred_cost` — pre-selected alternate cost (used by `respond_with_counter`).
    CastSpell { card_id: ObjId, face: SpellFace, preferred_cost: Option<AlternateCost> },
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


struct PlayerState {
    id: ObjId,
    deck_name: String,
    life: i32,
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
    /// Number of cards drawn this turn; reset each Untap. Used for Bowmasters / Tamiyo triggers.
    draws_this_turn: u8,
}

impl PlayerState {
    fn new(deck: &str, _mulligans: u8) -> Self {
        PlayerState {
            id: ObjId::UNSET,
            life: 20,
            deck_name: deck.to_string(),
            land_drop_available: false, // set true by Untap step
            must_land_drop: false,
            dd_cast: false,
            spells_cast_this_turn: 0,
            pool: ManaPool::default(),
            draws_this_turn: 0,
        }
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
    current_phase: Option<TurnPosition>,
    /// Attackers declared this combat (stable ObjIds); cleared at EndCombat.
    combat_attackers: Vec<ObjId>,
    /// Blocker assignments this combat: (attacker_id, blocker_id); cleared at EndCombat.
    combat_blocks: Vec<(ObjId, ObjId)>,
    /// Triggered abilities waiting to be pushed onto the stack at the next priority window.
    pending_triggers: Vec<TriggerContext>,
    /// Spell/ability stack. Items are resolved last-in-first-out. Populated by
    /// handle_priority_round; empty between priority rounds.
    pub(crate) stack: Vec<ObjId>,
    /// Activated and triggered abilities on the stack, keyed by their allocated ObjId.
    abilities: HashMap<ObjId, StackAbility>,
    /// All cards in all zones, keyed by stable ObjId. Added as part of staged object model migration.
    objects: HashMap<ObjId, GameObject>,
    /// ID allocator — starts at 1; 0 is reserved as ObjId::UNSET.
    next_id: u64,
    /// Order in which cards entered each player's graveyard (oldest first). Used for display.
    graveyard_order: Vec<ObjId>,
    /// All trigger instances for card objects in the simulation (pre-registered at init).
    /// `active` is false until the card enters the battlefield.
    pub(super) trigger_instances: Vec<TriggerInstance>,
    /// All replacement instances for card objects in the simulation (pre-registered at init).
    /// `active` is false until the card enters the battlefield.
    pub(super) replacement_instances: Vec<ReplacementInstance>,
    /// Replacements already applied in the current fire_event call chain (prevents loops).
    repl_applied: HashSet<ObjId>,
    /// Recursion depth for fire_event (used to clear repl_applied at the top level).
    repl_depth: u32,
    /// Monotonically increasing counter — incremented by every `fire_event` call at depth 0.
    /// `MaterializedState.generation` must match before the snapshot is trusted.
    pub(super) generation: u64,
    /// All active continuous-effect instances. Checked at `recompute` time; expired entries
    /// are removed at Cleanup / start-of-turn as appropriate.
    pub(super) continuous_instances: Vec<ContinuousInstance>,
    /// Cached post-CE snapshot of every battlefield permanent's effective CardDef.
    /// Rebuilt by `recompute` at the end of every top-level `fire_event` call.
    /// Always current when strategy or display code runs.
    pub(super) materialized: MaterializedState,
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
            current_phase: None,
            combat_attackers: Vec::new(),
            combat_blocks: Vec::new(),
            pending_triggers: Vec::new(),
            stack: Vec::new(),
            abilities: HashMap::new(),
            objects: HashMap::new(),
            next_id: 0,
            graveyard_order: Vec::new(),
            trigger_instances: Vec::new(),
            replacement_instances: Vec::new(),
            repl_applied: HashSet::new(),
            repl_depth: 0,
            generation: 0,
            continuous_instances: Vec::new(),
            materialized: MaterializedState { generation: 0, defs: HashMap::new() },
        };
        s.us.id = s.alloc_id();
        s.opp.id = s.alloc_id();
        s
    }

    fn alloc_id(&mut self) -> ObjId {
        self.next_id += 1;
        ObjId(self.next_id)
    }

    fn permanents_of<'a>(&'a self, who: &'a str) -> impl Iterator<Item = &'a GameObject> {
        self.objects.values().filter(move |c| c.controller == who && c.zone == CardZone::Battlefield)
    }

    fn permanent_bf(&self, id: ObjId) -> Option<&BattlefieldState> {
        self.objects.get(&id)
            .filter(|c| c.zone == CardZone::Battlefield)
            .and_then(|c| c.bf.as_ref())
    }

    fn permanent_bf_mut(&mut self, id: ObjId) -> Option<&mut BattlefieldState> {
        self.objects.get_mut(&id)
            .filter(|c| c.zone == CardZone::Battlefield)
            .and_then(|c| c.bf.as_mut())
    }

    fn hand_of<'a>(&'a self, who: &'a str) -> impl Iterator<Item = &'a GameObject> {
        self.objects.values().filter(move |c| c.owner == who && matches!(c.zone, CardZone::Hand { .. }))
    }

    fn graveyard_of<'a>(&'a self, who: &'a str) -> impl Iterator<Item = &'a GameObject> {
        self.objects.values().filter(move |c| c.owner == who && c.zone == CardZone::Graveyard)
    }

    fn exile_of<'a>(&'a self, who: &'a str) -> impl Iterator<Item = &'a GameObject> {
        self.objects.values().filter(move |c| c.owner == who && matches!(c.zone, CardZone::Exile { .. }))
    }

    /// Cards owned by `who` that are currently in exile with adventure status.
    fn on_adventure_of<'a>(&'a self, who: &'a str) -> impl Iterator<Item = &'a GameObject> {
        self.objects.values().filter(move |c| c.owner == who && c.zone == (CardZone::Exile { on_adventure: true }))
    }

    fn library_of<'a>(&'a self, who: &'a str) -> impl Iterator<Item = &'a GameObject> {
        self.objects.values().filter(move |c| c.owner == who && c.zone == CardZone::Library)
    }

    fn hand_size(&self, who: &str) -> i32 {
        self.hand_of(who).count() as i32
    }

    fn library_size(&self, who: &str) -> usize {
        self.library_of(who).count()
    }

    /// Mutate zone field only — no triggers, no logging. Use `change_zone` for that.
    fn set_card_zone(&mut self, id: ObjId, zone: CardZone) {
        if let Some(card) = self.objects.get_mut(&id) {
            card.zone = zone;
            if !matches!(zone, CardZone::Battlefield) {
                card.bf = None;
            }
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

    /// Resolve a player name string to its stable ObjId.
    fn player_id(&self, who: &str) -> ObjId {
        if who == "us" { self.us.id } else { self.opp.id }
    }

    /// Resolve a player ObjId back to the "us"/"opp" name string.
    fn who_str(&self, id: ObjId) -> &str {
        if id == self.us.id { "us" } else { "opp" }
    }

    /// Return the controller name ("us"/"opp") of the permanent with the given id.
    fn permanent_controller(&self, id: ObjId) -> Option<&str> {
        self.objects.get(&id)
            .filter(|c| c.zone == CardZone::Battlefield)
            .map(|c| c.controller.as_str())
    }

    /// Return the name of the permanent with the given id.
    fn permanent_name(&self, id: ObjId) -> Option<String> {
        self.objects.get(&id)
            .filter(|c| c.zone == CardZone::Battlefield)
            .map(|c| c.catalog_key.clone())
    }

    /// Mana accessible right now for `who`: pool + what untapped permanents can still produce.
    fn potential_mana(&self, who: &str) -> ManaPool {
        let mut p = self.player(who).pool.clone();
        for card in self.permanents_of(who) {
            if let Some(bf) = &card.bf {
                let mas = self.materialized.defs.get(&card.id).map(|d| d.mana_abilities()).unwrap_or(&[]);
                accumulate_source_potential(mas, bf.tapped, &mut p);
            }
        }
        p
    }

    /// Tap/sac permanents to produce mana for `who` for the given cost.
    /// Returns a log of activations.
    fn produce_mana(&mut self, who: &str, cost: &ManaCost, _t: u8) -> Vec<String> {
        let mut log: Vec<String> = Vec::new();
        let color_specs: [(i32, char, fn(&mut ManaPool)); 6] = [
            (cost.b, 'B', |p| p.b += 1),
            (cost.u, 'U', |p| p.u += 1),
            (cost.w, 'W', |p| p.w += 1),
            (cost.r, 'R', |p| p.r += 1),
            (cost.g, 'G', |p| p.g += 1),
            (cost.c, 'C', |p| p.c += 1),
        ];

        for (need, color_char, add_color) in color_specs {
            let mut remaining = need;
            while remaining > 0 {
                // Find a battlefield permanent controlled by `who` with the right ability.
                let found = self.objects.iter()
                    .find(|(id, c)| {
                        c.controller == who && c.zone == CardZone::Battlefield &&
                        c.bf.as_ref().map_or(false, |bf| {
                            self.materialized.defs.get(*id).map(|d| d.mana_abilities()).unwrap_or(&[])
                                .iter().any(|ma| (!ma.tap_self || !bf.tapped) && ma.produces.contains(color_char))
                        })
                    })
                    .map(|(id, c)| {
                        let bf = c.bf.as_ref().unwrap();
                        let sac = self.materialized.defs.get(id).map(|d| d.mana_abilities()).unwrap_or(&[])
                            .iter()
                            .find(|ma| (!ma.tap_self || !bf.tapped) && ma.produces.contains(color_char))
                            .map(|ma| ma.sacrifice_self)
                            .unwrap_or(false);
                        (*id, c.catalog_key.clone(), sac)
                    });
                if let Some((id, name, sac)) = found {
                    if sac {
                        log.push(format!("sac {} → {}", name, color_char));
                        if let Some(card) = self.objects.get_mut(&id) {
                            card.zone = CardZone::Graveyard;
                            card.bf = None;
                        }
                    } else {
                        log.push(format!("tap {} → {}", name, color_char));
                        if let Some(bf) = self.permanent_bf_mut(id) {
                            bf.tapped = true;
                        }
                    }
                    add_color(&mut self.player_mut(who).pool);
                    self.player_mut(who).pool.total += 1;
                    remaining -= 1;
                    continue;
                }
                break;
            }
        }

        // Generic: tap any remaining untapped source.
        let mut remaining_generic = cost.generic;
        while remaining_generic > 0 {
            let found = self.objects.iter()
                .find(|(id, c)| {
                    c.controller == who && c.zone == CardZone::Battlefield &&
                    c.bf.as_ref().map_or(false, |bf| {
                        let mas = self.materialized.defs.get(*id).map(|d| d.mana_abilities()).unwrap_or(&[]);
                        !mas.is_empty() && mas.iter().any(|ma| !ma.tap_self || !bf.tapped)
                    })
                })
                .map(|(id, c)| {
                    let bf = c.bf.as_ref().unwrap();
                    let sac = self.materialized.defs.get(id).map(|d| d.mana_abilities()).unwrap_or(&[])
                        .iter()
                        .find(|ma| !ma.tap_self || !bf.tapped)
                        .map(|ma| ma.sacrifice_self)
                        .unwrap_or(false);
                    (*id, c.catalog_key.clone(), sac)
                });
            if let Some((id, name, sac)) = found {
                if sac {
                    log.push(format!("sac {} → 1", name));
                    if let Some(card) = self.objects.get_mut(&id) {
                        card.zone = CardZone::Graveyard;
                        card.bf = None;
                    }
                } else {
                    log.push(format!("tap {} → 1", name));
                    if let Some(bf) = self.permanent_bf_mut(id) {
                        bf.tapped = true;
                    }
                }
                self.player_mut(who).pool.total += 1;
                remaining_generic -= 1;
                continue;
            }
            break;
        }
        log
    }

    /// Produce mana and immediately spend it.
    fn pay_mana(&mut self, who: &str, cost: &ManaCost, t: u8) -> Vec<String> {
        let log = self.produce_mana(who, cost, t);
        self.player_mut(who).pool.spend(cost);
        log
    }

    /// True if `who` can currently produce at least one black mana.
    fn has_black_mana(&self, who: &str) -> bool {
        self.potential_mana(who).b > 0
    }

    fn life_of(&self, who: &str) -> i32 {
        self.player(who).life
    }

    fn lose_life(&mut self, who: &str, n: i32) {
        self.player_mut(who).life -= n;
    }

    fn log(&mut self, t: u8, who: &str, msg: impl Into<String>) {
        let is_player = who == "us" || who == "opp";
        let phase_str = match self.current_phase {
            Some(TurnPosition::Step(s))  => format!("{:?}", s),
            Some(TurnPosition::Phase(p)) => format!("{:?}", p),
            None                         => String::new(),
        };
        let ctx = if is_player && self.current_ap != ObjId::UNSET {
            format!("|{}/{}", self.who_str(self.current_ap), phase_str)
        } else {
            String::new()
        };
        self.log.push(format!("T{} [{}{}] {}", t, who, ctx, msg.into()));
    }

    /// Log each mana activation returned by pay_mana/produce_mana.
    fn log_mana_activations(&mut self, t: u8, who: &str, activations: Vec<String>) {
        for entry in activations {
            self.log(t, who, format!("→ {}", entry));
        }
    }

    pub(crate) fn stack_item_owner(&self, id: ObjId) -> ObjId {
        if let Some(card) = self.objects.get(&id) {
            return self.player_id(&card.owner);
        }
        if let Some(ab) = self.abilities.get(&id) {
            return ab.owner;
        }
        ObjId::UNSET
    }

    pub(crate) fn stack_item_display_name(&self, id: ObjId) -> &str {
        if let Some(card) = self.objects.get(&id) {
            return card.catalog_key.as_str();
        }
        if let Some(ab) = self.abilities.get(&id) {
            return ab.source_name.as_str();
        }
        ""
    }

    pub(crate) fn stack_item_is_counterable(&self, id: ObjId) -> bool {
        self.objects.contains_key(&id) && self.objects[&id].zone == CardZone::Stack
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

// PlayerState Display is handled via SimState::fmt_player_zones which has access to state.objects.

impl SimState {
    /// Write hand/graveyard/exile zones for `who` to the formatter — one line per zone.
    fn fmt_player_zones(&self, f: &mut std::fmt::Formatter<'_>, who: &str) -> std::fmt::Result {
        let mut visible: Vec<&str> = self.hand_of(who)
            .filter(|c| matches!(c.zone, CardZone::Hand { known: true }))
            .map(|c| c.catalog_key.as_str())
            .collect();
        visible.sort();
        let hidden = self.hand_of(who)
            .filter(|c| matches!(c.zone, CardZone::Hand { known: false }))
            .count();
        if visible.len() + hidden > 0 {
            let mut parts = Self::collapse_counts(visible.iter().map(|s| s.to_string()).collect());
            if hidden > 0 { parts.push(format!("({} hidden)", hidden)); }
            writeln!(f, "  Hand      : {}", parts.join(", "))?;
        }

        let gy: Vec<String> = self.graveyard_order.iter()
            .filter_map(|id| self.objects.get(id))
            .filter(|c| c.owner == who)
            .map(|c| c.catalog_key.clone())
            .collect();
        if !gy.is_empty() {
            writeln!(f, "  Graveyard : {}", Self::collapse_counts(gy).join(", "))?;
        }

        let mut exile: Vec<String> = self.exile_of(who)
            .map(|c| if matches!(c.zone, CardZone::Exile { on_adventure: true }) {
                format!("{} (adv)", c.catalog_key)
            } else {
                c.catalog_key.clone()
            })
            .collect();
        if !exile.is_empty() {
            exile.sort();
            writeln!(f, "  Exile     : {}", Self::collapse_counts(exile).join(", "))?;
        }

        Ok(())
    }

    /// Collapse a list of display strings into `"Name ×N"` entries, preserving first-seen order.
    fn collapse_counts(items: Vec<String>) -> Vec<String> {
        let mut seen: Vec<(String, usize)> = Vec::new();
        for item in items {
            if let Some(entry) = seen.iter_mut().find(|(s, _)| *s == item) {
                entry.1 += 1;
            } else {
                seen.push((item, 1));
            }
        }
        seen.into_iter().map(|(s, n)| if n > 1 { format!("{} ×{}", s, n) } else { s }).collect()
    }

    /// Write permanents for `who` — lands on one line, non-lands on another.
    fn fmt_permanents(&self, f: &mut std::fmt::Formatter<'_>, who: &str) -> std::fmt::Result {
        let fmt_perm = |card: &&GameObject| -> Option<String> {
            let bf = card.bf.as_ref()?;
            let mut tags: Vec<String> = Vec::new();
            if bf.counters != 0 { tags.push(format!("{:+}", bf.counters)); }
            if bf.loyalty > 0   { tags.push(format!("loy:{}", bf.loyalty)); }
            if bf.tapped         { tags.push("tapped".into()); }
            let suffix = if tags.is_empty() { String::new() } else { format!(" [{}]", tags.join(", ")) };
            Some(format!("{}{}", card.catalog_key, suffix))
        };

        let mut lands: Vec<&GameObject> = self.permanents_of(who)
            .filter(|c| c.bf.is_some() && !self.materialized.defs.get(&c.id).map(|d| d.mana_abilities()).unwrap_or(&[]).is_empty())
            .collect();
        let tapped_first = |a: &&GameObject, b: &&GameObject| {
            let a_tap = a.bf.as_ref().map_or(false, |bf| bf.tapped);
            let b_tap = b.bf.as_ref().map_or(false, |bf| bf.tapped);
            b_tap.cmp(&a_tap).then(a.catalog_key.cmp(&b.catalog_key))
        };
        lands.sort_by(tapped_first);

        let mut others: Vec<&GameObject> = self.permanents_of(who)
            .filter(|c| c.bf.is_none() || self.materialized.defs.get(&c.id).map(|d| d.mana_abilities()).unwrap_or(&[]).is_empty())
            .collect();
        others.sort_by(tapped_first);

        if !lands.is_empty() {
            let items = Self::collapse_counts(lands.iter().filter_map(fmt_perm).collect());
            writeln!(f, "  Lands     : {}", items.join(", "))?;
        }
        if !others.is_empty() {
            let items = Self::collapse_counts(others.iter().filter_map(fmt_perm).collect());
            writeln!(f, "  Permanents: {}", items.join(", "))?;
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
        self.fmt_permanents(f, "us")?;
        self.fmt_player_zones(f, "us")?;
        writeln!(f)?;

        let opp_label = format!("OPPONENT: {}", self.opp.deck_name);
        writeln!(f, "{}", sec(&opp_label))?;
        writeln!(f)?;
        writeln!(f, "  Life       : {}", self.opp.life)?;
        self.fmt_permanents(f, "opp")?;
        self.fmt_player_zones(f, "opp")?;

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

// ── Turn simulation ───────────────────────────────────────────────────────────


/// Play a specific, pre-chosen land from hand (moves it to Battlefield).
/// Fetches stay in play to be cracked later in the ability pass.
fn sim_play_land(
    state: &mut SimState,
    t: u8,
    who: &str,
    card_id: ObjId,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) {
    if !state.player(who).land_drop_available { return; }
    let land_name = match state.objects.get(&card_id) {
        Some(c) if matches!(c.zone, CardZone::Hand { .. }) => c.catalog_key.clone(),
        _ => return,
    };
    state.log(t, who, format!("Play {} [hand: {}]", land_name, state.hand_size(who)));
    change_zone(card_id, ZoneId::Battlefield, state, t, who, catalog_map, rng);
}


/// Discard down to 7 at end of turn.
fn sim_discard_to_limit(state: &mut SimState, t: u8, who: &str) {
    let hand = state.hand_size(who);
    if hand > 7 {
        let n = hand - 7;
        // Discard n cards (move from Hand to Graveyard).
        let to_discard: Vec<ObjId> = state.hand_of(who).take(n as usize).map(|c| c.id).collect();
        for id in to_discard {
            state.set_card_zone(id, CardZone::Graveyard);
        }
        state.log(t, who, format!("Discard {} to hand limit", n));
    }
}

// ── Action system ─────────────────────────────────────────────────────────────

// resolve_who, matches_target_type, has_valid_target, ability_available,
// collect_hand_actions, choose_land_name, respond_with_counter
// are defined in strategy.rs / predicates.rs

// choose_permanent_target is defined in predicates.rs

pub(super) fn card_zone_to_id(zone: &CardZone) -> ZoneId {
    match zone {
        CardZone::Library        => ZoneId::Library,
        CardZone::Hand { .. }    => ZoneId::Hand,
        CardZone::Stack          => ZoneId::Stack,
        CardZone::Battlefield    => ZoneId::Battlefield,
        CardZone::Graveyard      => ZoneId::Graveyard,
        CardZone::Exile { .. }   => ZoneId::Exile,
    }
}

pub(super) fn card_type_str(d: &CardDef) -> &'static str {
    if d.is_creature()      { "creature" }
    else if d.is_instant()  { "instant" }
    else if d.is_sorcery()  { "sorcery" }
    else if d.is_land()     { "land" }
    else                    { "permanent" }
}

/// The central elemental event pipeline: check_replacement → do_effect → check_triggers.
/// Every elemental game operation (zone change, draw, step/phase entry, etc.) passes through here.
/// If a replacement fires, the original effect is suppressed and the replacement runs immediately.
pub(super) fn fire_event(
    event: GameEvent,
    state: &mut SimState,
    t: u8,
    actor: &str,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut dyn rand::RngCore,
) {
    state.repl_depth += 1;
    if state.repl_depth == 1 {
        state.repl_applied.clear();
    }

    // check_replacement: first active, non-applied replacement that matches
    let repl_match = {
        let mut found = None;
        for inst in &state.replacement_instances {
            if !inst.active { continue; }
            if state.repl_applied.contains(&inst.id) { continue; }
            if let Some(targets) = (inst.check)(&event, inst.source_id, &inst.controller) {
                found = Some((inst.id, targets, inst.effect.clone()));
                break;
            }
        }
        found
    };

    if let Some((repl_id, targets, effect)) = repl_match {
        state.repl_applied.insert(repl_id);
        effect.call(state, t, &targets, catalog_map, rng);
        state.repl_depth -= 1;
        return; // original effect suppressed
    }

    // do_effect: apply the state mutation for this event type
    do_effect(&event, state, catalog_map);

    // log
    log_event(&event, state, t, actor);

    // check_triggers
    let triggers = fire_triggers(&event, state);
    state.pending_triggers.extend(triggers);

    state.repl_depth -= 1;
    if state.repl_depth == 0 {
        // Each top-level fire_event is one game-state tick.
        // Advance generation so consumers can detect staleness.
        state.generation += 1;
        // Rebuild the materialized snapshot after every top-level tick so that
        // strategy, display, and combat damage always see a current, CE-adjusted view.
        // The new generation is embedded in the snapshot.
        let mat = recompute(state, catalog_map);
        state.materialized = mat;
    }
}

fn do_effect(event: &GameEvent, state: &mut SimState, _catalog_map: &HashMap<&str, &CardDef>) {
    match event {
        GameEvent::ZoneChange { id, from, to, .. } => {
            let id = *id;
            let from = *from;
            let to = *to;

            let new_zone = match to {
                ZoneId::Graveyard   => CardZone::Graveyard,
                ZoneId::Exile       => CardZone::Exile { on_adventure: false },
                ZoneId::Hand        => CardZone::Hand { known: false },
                ZoneId::Library     => CardZone::Library,
                ZoneId::Stack       => CardZone::Stack,
                ZoneId::Battlefield => CardZone::Battlefield,
            };

            if let Some(card) = state.objects.get_mut(&id) {
                // Only update if zone actually changed (idempotent guard for re-fired ETB events)
                if card.zone != new_zone {
                    if new_zone == CardZone::Graveyard { state.graveyard_order.push(id); }
                    else { state.graveyard_order.retain(|&x| x != id); }
                    card.zone = new_zone;
                    if from == ZoneId::Battlefield { card.bf = None; }
                }
                if to == ZoneId::Battlefield && card.bf.is_none() {
                    card.bf = Some(BattlefieldState {
                        entered_this_turn: true,
                        ..BattlefieldState::new()
                    });
                }
            }

        }
        GameEvent::Draw { controller, .. } => {
            let controller = controller.clone();
            let top_id = state.library_of(&controller).next().map(|c| c.id);
            if let Some(card_id) = top_id {
                state.set_card_zone(card_id, CardZone::Hand { known: false });
            }
        }
        // EnteredStep, EnteredPhase, CreatureAttacked — notification events, no state mutation
        _ => {}
    }
}

fn log_event(event: &GameEvent, state: &mut SimState, t: u8, actor: &str) {
    match event {
        GameEvent::ZoneChange { from, to, card, controller, .. } => {
            match (from, to) {
                // Stack→Graveyard is silent here: resolution logs "{name} resolves" before calling
                // change_zone, and eff_counter_target logs "→ {name} countered" before setting zone
                // directly (bypassing change_zone). Logging here would produce a spurious "countered".
                (ZoneId::Stack,       ZoneId::Graveyard)   => {}
                (ZoneId::Battlefield, ZoneId::Graveyard)   => state.log(t, actor, format!("→ {} destroyed", card)),
                (ZoneId::Hand,        ZoneId::Graveyard)   => state.log(t, actor, format!("→ {} discarded", card)),
                (_,                   ZoneId::Graveyard)   => state.log(t, actor, format!("→ {} to graveyard", card)),
                (_,                   ZoneId::Exile)       => state.log(t, actor, format!("→ {} exiled", card)),
                (ZoneId::Hand,        ZoneId::Library)     => state.log(t, actor, format!("→ {} put back", card)),
                (_,                   ZoneId::Hand)        => state.log(t, actor, format!("→ {} returned to {}'s hand", card, controller)),
                (ZoneId::Graveyard,   ZoneId::Battlefield) => state.log(t, actor, format!("→ {} returns from graveyard", card)),
                _ => {}
            }
        }
        GameEvent::Draw { controller, draw_index, is_natural } => {
            let hand = state.hand_size(controller);
            if *is_natural {
                state.log(t, controller, format!("Draw [hand: {}]", hand));
            } else {
                state.log(t, controller, format!("draw ({}) [hand: {}]", draw_index, hand));
            }
        }
        _ => {}
    }
}

/// Move a game object from its current zone to `to`.
/// Works for any zone transition. No-ops silently if the id is not found.
/// Fires the event pipeline (replacements → state mutation → triggers → log).
pub(super) fn change_zone(
    id: ObjId,
    to: ZoneId,
    state: &mut SimState,
    t: u8,
    actor: &str,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut dyn rand::RngCore,
) {
    let (name, controller, from, card_type) = {
        let card = match state.objects.get(&id) {
            Some(c) => c,
            None => return,
        };
        let from = card_zone_to_id(&card.zone);
        let ct = state.materialized.defs.get(&id)
            .map(|d| card_type_str(d))
            .unwrap_or("permanent");
        (card.catalog_key.clone(), card.controller.clone(), from, ct)
    };
    // Activate/deactivate instances BEFORE firing the event so replacement checks see the right state.
    if from == ZoneId::Battlefield { deactivate_instances(id, state); }
    if to == ZoneId::Battlefield {
        // catalog: activate_instances needs the static ability list to register triggered
        // and replacement instances. The ObjId exists but materialized isn't updated until
        // after this event resolves — bootstrapping from catalog is required here.
        let def = catalog_map.get(name.as_str()).copied();
        activate_instances(id, &controller, def, state);
    }
    fire_event(
        GameEvent::ZoneChange {
            id,
            actor: actor.to_string(),
            card: name,
            card_type: card_type.to_string(),
            from,
            to,
            controller,
        },
        state, t, actor, catalog_map, rng,
    );
}

// matches_search_filter is defined in predicates.rs

/// Draw one card for `who` through the event pipeline. Increments draws_this_turn, fires a Draw
/// event (which handles the state mutation, logging, and trigger dispatch).
fn sim_draw(state: &mut SimState, who: &str, t: u8, is_natural: bool, catalog_map: &HashMap<&str, &CardDef>, rng: &mut dyn rand::RngCore) {
    state.player_mut(who).draws_this_turn += 1;
    let draw_index = state.player(who).draws_this_turn;
    let ev = GameEvent::Draw { controller: who.to_string(), draw_index, is_natural };
    fire_event(ev, state, t, who, catalog_map, rng);
}

/// Pay the activation cost of an ability: mana, life, tap, and/or sacrifice.
/// Effects are NOT applied here — they happen when the ability resolves off the stack.
fn pay_activation_cost(
    state: &mut SimState,
    t: u8,
    who: &str,
    source_id: ObjId,
    ability: &AbilityDef,
    _catalog_map: &HashMap<&str, &CardDef>,
) {
    let source_name = state.permanent_name(source_id)
        .or_else(|| state.hand_of(who).find(|c| c.id == source_id).map(|c| c.catalog_key.clone()))
        .unwrap_or_default();
    state.log(t, who, format!("Activate {} ability", source_name));

    // Pay mana cost.
    if !ability.mana_cost.is_empty() {
        let cost = parse_mana_cost(&ability.mana_cost);
        let mana_log = state.pay_mana(who, &cost, t);
        state.log_mana_activations(t, who, mana_log);
    }

    // Pay life cost.
    if ability.life_cost > 0 {
        state.lose_life(who, ability.life_cost);
    }

    // Pay tap cost.
    if ability.tap_self && !ability.sacrifice_self {
        if let Some(bf) = state.permanent_bf_mut(source_id) {
            bf.tapped = true;
        }
    }

    // Pay sacrifice cost (in-play permanent).
    if ability.sacrifice_self && ability.zone != "hand" {
        state.set_card_zone(source_id, CardZone::Graveyard);
    }

    // Discard cost (zone="hand"): move from hand to graveyard.
    if ability.discard_self {
        // Find the hand card by name.
        let hand_card_id = state.hand_of(who)
            .find(|c| c.catalog_key == source_name)
            .map(|c| c.id);
        if let Some(hid) = hand_card_id {
            state.set_card_zone(hid, CardZone::Graveyard);
        }
    }

    // Ninjutsu cost: move ninja from hand to stack zone, and return an unblocked attacker to hand.
    if ability.ninjutsu {
        // Find the ninja in hand by name and move it to a neutral zone (it will enter BF via effect).
        let ninja_hand_id = state.hand_of(who)
            .find(|c| c.catalog_key == source_name)
            .map(|c| c.id);
        if let Some(nid) = ninja_hand_id {
            // Keep it in cards but mark as Stack (it "enters via ninjutsu" when the ability resolves).
            if let Some(card) = state.objects.get_mut(&nid) {
                card.zone = CardZone::Stack;
            }
        }
        let unblocked_attacker = state.permanents_of(who)
            .find(|c| c.bf.as_ref().map_or(false, |bf| bf.attacking && bf.unblocked))
            .map(|c| (c.id, c.catalog_key.clone(), c.bf.as_ref().and_then(|bf| bf.attack_target)));
        if let Some((atk_id, atk_name, _atk_target)) = unblocked_attacker {
            if let Some(card) = state.objects.get_mut(&atk_id) {
                card.zone = CardZone::Hand { known: false };
                card.bf = None;
            }
            state.combat_attackers.retain(|&a| a != atk_id);
            state.combat_blocks.retain(|(a, _)| *a != atk_id);
            state.log(t, who, format!("→ return {} to hand (ninjutsu)", atk_name));
        }
    }

    // Sacrifice-a-land cost (e.g. Edge of Autumn cycling).
    if ability.sacrifice_land {
        // Prefer permanents with no mana abilities to preserve mana sources.
        let land_to_sac = state.permanents_of(who)
            .find(|c| c.bf.is_some() && state.materialized.defs.get(&c.id).map(|d| d.mana_abilities().is_empty()).unwrap_or(true))
            .or_else(|| state.permanents_of(who).next())
            .map(|c| c.id);
        if let Some(sac_id) = land_to_sac {
            state.set_card_zone(sac_id, CardZone::Graveyard);
        }
    }

    // Loyalty ability: adjust planeswalker loyalty and mark activated this turn.
    if let Some(loyalty_delta) = ability.loyalty_cost {
        let new_loyalty = if let Some(bf) = state.permanent_bf_mut(source_id) {
            bf.loyalty += loyalty_delta;
            bf.pw_activated_this_turn = true;
            Some(bf.loyalty)
        } else {
            None
        };
        if let Some(new_loyalty) = new_loyalty {
            state.log(t, who, format!("→ {} loyalty {} → {}", source_name,
                if loyalty_delta >= 0 { format!("+{}", loyalty_delta) } else { loyalty_delta.to_string() },
                new_loyalty));
        }
    }
}

/// Check whether `cost` can be paid by `who` given current state.
/// `source_name` is the counterspell card name (excluded from blue pitch candidates).
fn can_pay_alternate_cost(
    cost: &AlternateCost,
    state: &SimState,
    who: &str,
    source_name: &str,
    catalog_map: &HashMap<&str, &CardDef>,
) -> bool {
    if state.hand_size(who) < cost.hand_min {
        return false;
    }
    if !cost.mana_cost.is_empty() {
        let cost_mc = parse_mana_cost(&cost.mana_cost);
        if !state.potential_mana(who).can_pay(&cost_mc) {
            return false;
        }
    }
    if cost.exile_blue_from_hand {
        let has_pitch = state.hand_of(who)
            .any(|c| c.catalog_key != source_name && {
                let is_blue_non_land = |d: &CardDef| !d.is_land() && d.is_blue();
                state.materialized.defs.get(&c.id).map(is_blue_non_land)
                    .unwrap_or_else(|| catalog_map.get(c.catalog_key.as_str()).map_or(false, |d| is_blue_non_land(d)))
            });
        if !has_pitch {
            return false;
        }
    }
    if cost.bounce_island {
        if !state.permanents_of(who).any(|c| c.bf.is_some() && state.materialized.defs.get(&c.id).map(|d| d.mana_abilities().iter().any(|ma| ma.produces.contains('U'))).unwrap_or(false)) {
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
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    if cost.exile_blue_from_hand {
        // Collect pitch candidates from hand.
        let pitch_ids: Vec<(ObjId, String)> = state.hand_of(who)
            .filter(|c| c.catalog_key != source_name && {
                let is_blue_non_land = |d: &CardDef| !d.is_land() && d.is_blue();
                state.materialized.defs.get(&c.id).map(is_blue_non_land)
                    .unwrap_or_else(|| catalog_map.get(c.catalog_key.as_str()).map_or(false, |d| is_blue_non_land(d)))
            })
            .map(|c| (c.id, c.catalog_key.clone()))
            .collect();
        let idx = rng.gen_range(0..pitch_ids.len());
        let (pitch_id, pitch_name) = pitch_ids[idx].clone();
        state.set_card_zone(pitch_id, CardZone::Exile { on_adventure: false });
        parts.push(format!("exile {}", pitch_name));
    }
    if cost.bounce_island {
        let bounce = state.permanents_of(who)
            .find(|c| c.bf.is_some() && state.materialized.defs.get(&c.id).map(|d| d.mana_abilities().iter().any(|ma| ma.produces.contains('U'))).unwrap_or(false))
            .map(|c| (c.id, c.catalog_key.clone()))
            .unwrap();
        let (bounce_id, land_name) = bounce;
        if let Some(card) = state.objects.get_mut(&bounce_id) {
            card.zone = CardZone::Hand { known: false };
            card.bf = None;
        }
        parts.push(format!("bounce {}", land_name));
    }
    if !cost.mana_cost.is_empty() {
        let cost_mc = parse_mana_cost(&cost.mana_cost);
        let mana_log = state.pay_mana(who, &cost_mc, t);
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
/// and return the card's ObjId (now on the stack).
///
/// Cost selection: if `preferred_cost` is `Some`, that specific alternate cost is used
/// (caller already verified it's payable, e.g. `respond_with_counter` after prob checks).
/// Otherwise the standard mana cost is tried first; if unpayable (or mana_cost is empty
/// and the card has alternate costs), the first payable alternate cost is used instead.
///
/// Permanent targets (from `CardDef.target`) are chosen randomly at cast time and
/// locked into the SpellState on the card; resolution uses the stored target directly.
/// Cast a spell identified by `card_id`, using the specified `face` (Main or Adventure).
/// Pays cost, builds effect, sets SpellState on the card object.
/// Returns `None` if the cast fails (cost unpayable, card missing).
fn cast_spell(
    state: &mut SimState,
    t: u8,
    who: &str,
    card_id: ObjId,
    face: SpellFace,
    preferred_cost: Option<&AlternateCost>,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> Option<ObjId> {
    let name = state.objects.get(&card_id)?.catalog_key.clone();
    // Prefer the post-CE materialized def (current in normal game flow where recompute
    // runs before every priority window). Fall back to catalog for tests that call
    // cast_spell directly without a preceding recompute.
    let def = state.materialized.defs.get(&card_id)
        .cloned()
        .or_else(|| catalog_map.get(name.as_str()).map(|&d| d.clone()))?;

    if face == SpellFace::Back {
        let adv = def.adventure()?.clone();
        let is_sorcery = adv.is_sorcery();
        if is_sorcery && !state.stack.is_empty() {
            eprintln!("[priority] BUG: split-back sorcery {} on non-empty stack, treating as Pass", adv.name);
            return None;
        }
        let cost = parse_mana_cost(adv.mana_cost());
        let mana_log = state.pay_mana(who, &cost, t);
        state.log_mana_activations(t, who, mana_log);
        let (adv_spec, adv_eff) = build_spell_effect(&adv, who);
        let adv_targets = choose_spell_target(&adv_spec, who, state, rng)
            .into_iter().collect::<Vec<_>>();
        state.log(t, who, format!("Cast {} ({}, {}) [hand: {}]", adv.name, adv.mana_cost(), name, state.hand_size(who)));
        if let Some(card) = state.objects.get_mut(&card_id) {
            card.zone = CardZone::Stack;
            card.spell = Some(SpellState {
                effect: Some(adv_eff),
                chosen_targets: adv_targets,
                is_back_face: true,
            });
        }
        return Some(card_id);
    }

    // Main face: pay main cost (with delve and alternate costs).
    let mut cost = parse_mana_cost(def.mana_cost());

    // Delve: reduce generic cost by exiling cards from the caster's graveyard.
    let to_exile_ids: Vec<(ObjId, String)> = if def.delve() && cost.generic > 0 {
        let gy: Vec<(ObjId, String)> = state.graveyard_of(who).map(|c| (c.id, c.catalog_key.clone())).collect();
        let mut cards = Vec::new();
        for (id, card_name) in &gy {
            if cards.len() as i32 >= cost.generic { break; }
            cards.push((*id, card_name.clone()));
        }
        cost.generic -= cards.len() as i32;
        cards
    } else {
        Vec::new()
    };

    // Empty mana_cost means the card has no castable mana cost (alt-cost-only, or truly uncostable).
    // Use mana_cost = "0" in the catalog for genuinely free spells (Lotus Petal, LED).
    let has_alt_costs = !def.alternate_costs().is_empty();
    let mana_is_usable = !def.mana_cost().is_empty() && state.potential_mana(who).can_pay(&cost);

    // Select cost.
    let alt_cost: Option<AlternateCost> = if let Some(pc) = preferred_cost {
        Some(pc.clone())
    } else if !mana_is_usable {
        def.alternate_costs()
            .iter()
            .find(|c| can_pay_alternate_cost(c, state, who, &name, catalog_map))
            .cloned()
    } else if has_alt_costs {
        None
    } else {
        None
    };

    if alt_cost.is_none() && !mana_is_usable {
        return None;
    }

    // Move to Stack zone.
    if let Some(card) = state.objects.get_mut(&card_id) {
        card.zone = CardZone::Stack;
    }

    // Pay cost and build a log label.
    let cast_label = if let Some(ref cost) = alt_cost {
        let parts = apply_alt_cost_components(cost, state, t, who, &name, catalog_map, rng);
        parts.join(", ")
    } else {
        let mana_log = state.pay_mana(who, &cost, t);
        state.log_mana_activations(t, who, mana_log);
        def.mana_cost().to_string()
    };

    // Exile delve cards from graveyard (cost payment).
    let to_exile_names: Vec<String> = to_exile_ids.iter().map(|(_, n)| n.clone()).collect();
    for (exile_id, _) in &to_exile_ids {
        change_zone(*exile_id, ZoneId::Exile, state, t, who, catalog_map, rng);
    }

    let delve_label = if to_exile_names.is_empty() {
        String::new()
    } else {
        format!(", delve: {}", to_exile_names.join(", "))
    };
    state.log(t, who, format!("Cast {} ({}{}) [hand: {}]", name, cast_label, delve_label, state.hand_size(who)));

    let (spell_target_spec, spell_eff) = build_spell_effect(&def, who);
    let spell_chosen_targets = choose_spell_target(&spell_target_spec, who, state, rng)
        .into_iter().collect::<Vec<_>>();

    if let Some(card) = state.objects.get_mut(&card_id) {
        card.spell = Some(SpellState {
            effect: Some(spell_eff),
            chosen_targets: spell_chosen_targets,
            is_back_face: false,
        });
    }

    Some(card_id)
}






// ── Keyword helpers ───────────────────────────────────────────────────────────

/// Return true if the permanent with `id` has the given keyword in the materialized (CE-applied) view.
/// Always reads from materialized state so CEs that grant or remove keywords are respected.
pub(super) fn creature_has_keyword(id: ObjId, kw: &str, state: &SimState) -> bool {
    state.materialized.defs.get(&id)
        .map(|d| d.has_keyword(kw))
        .unwrap_or(false)
}


/// Check and apply all State-Based Actions (rule 704). Called before every priority grant.
/// Runs in a loop until no SBA fires in a pass — the rules require repeated checking until stable.
fn check_state_based_actions(
    state: &mut SimState,
    t: u8,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut dyn rand::RngCore,
) {
    // Ensure materialized state is current before reading it for SBA checks.
    // (It may be stale if state was mutated outside fire_event, e.g. directly in tests.)
    let mat = recompute(state, catalog_map);
    state.materialized = mat;

    loop {
        let mut any = false;

        // SBA: player with life ≤ 0 loses the game (rule 704.5a).
        for who in ["us", "opp"] {
            if state.life_of(who) <= 0 {
                state.log(t, who, format!("→ loses the game (life: {})", state.life_of(who)));
                if who == "us" { state.reroll = true; }
                return; // game over — no further SBA processing
            }
        }

        // SBA: token in a zone other than the battlefield ceases to exist (rule 704.5d).
        let dead_tokens: Vec<ObjId> = state.objects.values()
            .filter(|c| c.is_token && c.zone != CardZone::Battlefield)
            .map(|c| c.id)
            .collect();
        for id in dead_tokens {
            state.objects.remove(&id);
            any = true;
        }

        // SBA: creature with toughness ≤ 0 goes to graveyard (rule 704.5f).
        // SBA: creature with lethal damage goes to graveyard (rule 704.5g).
        for who in ["us", "opp"] {
            let dying: Vec<ObjId> = state.permanents_of(who)
                .filter_map(|card| {
                    let bf = card.bf.as_ref()?;
                    if !state.materialized.defs.get(&card.id).map_or(false, |d| d.is_creature()) { return None; }
                    let tgh = state.materialized.defs.get(&card.id)
                        .and_then(|d| d.as_creature())
                        .map(|c| c.toughness())
                        .unwrap_or(1);
                    if tgh <= 0 || bf.damage >= tgh { Some(card.id) } else { None }
                })
                .collect();
            for id in dying {
                change_zone(id, ZoneId::Graveyard, state, t, who, catalog_map, rng);
                any = true;
            }
        }

        // SBA: planeswalker with loyalty ≤ 0 goes to graveyard (rule 704.5i).
        for who in ["us", "opp"] {
            let dying: Vec<ObjId> = state.permanents_of(who)
                .filter_map(|card| {
                    let bf = card.bf.as_ref()?;
                    if !state.materialized.defs.get(&card.id).map_or(false, |d| matches!(d.kind, CardKind::Planeswalker(_))) { return None; }
                    if bf.loyalty <= 0 { Some(card.id) } else { None }
                })
                .collect();
            for id in dying {
                change_zone(id, ZoneId::Graveyard, state, t, who, catalog_map, rng);
                any = true;
            }
        }

        // SBA: legend rule — if a player controls two or more legendary permanents with the
        // same name, that player chooses one to keep; the rest go to graveyard (rule 704.5j).
        for who in ["us", "opp"] {
            // Collect (name, id) for all legendary permanents controlled by `who`.
            let mut seen: HashMap<String, ObjId> = HashMap::new();
            let mut extras: Vec<ObjId> = Vec::new();
            let legendaries: Vec<(String, ObjId)> = state.permanents_of(who)
                .filter(|card| {
                    state.materialized.defs.get(&card.id)
                        .map_or(false, |d| d.legendary())
                })
                .map(|card| (card.catalog_key.clone(), card.id))
                .collect();
            for (name, id) in legendaries {
                if let Some(_existing) = seen.get(&name) {
                    extras.push(id); // keep the first one, sacrifice the later one
                } else {
                    seen.insert(name, id);
                }
            }
            for id in extras {
                change_zone(id, ZoneId::Graveyard, state, t, who, catalog_map, rng);
                any = true;
            }
        }

        if !any { break; }
    }
}

fn opp_of(who: &str) -> &'static str {
    if who == "us" { "opp" } else { "us" }
}

fn do_amass_orc(controller: &str, n: i32, state: &mut SimState, t: u8) {
    let army_id: Option<ObjId> = state.permanents_of(controller)
        .find(|c| c.catalog_key == "Orc Army")
        .map(|c| c.id);
    if let Some(army_id) = army_id {
        if let Some(bf) = state.permanent_bf_mut(army_id) {
            bf.counters += n;
        }
        let c = state.permanent_bf(army_id).map_or(0, |bf| bf.counters);
        state.log(t, controller, format!("Orc Army grows to {c}/{c}"));
    } else {
        let new_id = state.alloc_id();
        state.objects.insert(new_id, GameObject {
            id: new_id,
            catalog_key: "Orc Army".to_string(),
            owner: controller.to_string(),
            controller: controller.to_string(),
            zone: CardZone::Battlefield,
            is_token: true,
            spell: None,
            bf: Some(BattlefieldState {
                counters: n,
                ..BattlefieldState::new()
            }),
        });
        state.log(t, controller, format!("Orc Army token created {n}/{n}"));
    }
}

fn do_create_clue(controller: &str, state: &mut SimState, t: u8) {
    let new_id = state.alloc_id();
    state.objects.insert(new_id, GameObject {
        id: new_id,
        catalog_key: "Clue Token".to_string(),
        owner: controller.to_string(),
        controller: controller.to_string(),
        zone: CardZone::Battlefield,
        is_token: true,
        spell: None,
        bf: Some(BattlefieldState::new()),
    });
    state.log(t, controller, "Clue Token created");
}

fn do_flip_tamiyo(source_id: ObjId, controller: &str, state: &mut SimState, t: u8) {
    // Read the back-face starting loyalty from the front-face materialized def.
    // The front face is still current in materialized at the moment the trigger resolves
    // (active_face == 0). `back` carries the printed PW data for the flipped face.
    let loyalty = state.materialized.defs.get(&source_id)
        .and_then(|d| d.back.as_ref())
        .and_then(|b| if let CardKind::Planeswalker(ref p) = b.kind { Some(p.loyalty) } else { None })
        .unwrap_or(2);
    // Set active_face = 1. catalog_key is intentionally NOT changed — recompute substitutes
    // the back-face kind into the materialized def whenever active_face == 1.
    if let Some(bf) = state.objects.get_mut(&source_id).and_then(|c| c.bf.as_mut()) {
        bf.loyalty = loyalty;
        bf.active_face = 1;
    }
    state.log(t, controller, format!("Tamiyo flips → Tamiyo, Seasoned Scholar [loyalty: {}]", loyalty));
}

/// Pop and resolve the top item of the stack.
///
/// If the top id is in `state.objects` it is a spell: runs its effect and moves the card to
/// graveyard (instant/sorcery) or exile-on-adventure, or leaves zone management to
/// `eff_enter_permanent` (permanent spells). If the id is in `state.abilities` it is an
/// activated or triggered ability: runs its effect and removes the entry.
fn resolve_top_of_stack(
    state: &mut SimState,
    t: u8,
    _ap: &str,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) {
    let id = state.stack.pop().unwrap();
    if state.objects.contains_key(&id) {
        // It's a spell (card on the stack)
        let spell = state.objects[&id].spell.clone().unwrap_or_else(|| SpellState {
            effect: None,
            chosen_targets: vec![],
            is_back_face: false,
        });
        let owner_str = state.objects[&id].owner.clone();
        let name = state.objects[&id].catalog_key.clone();

        // Back face of a split card whose back has subtype "adventure" → exile to on_adventure.
        let is_adventure = spell.is_back_face
            && catalog_map.get(name.as_str())
                .and_then(|d| d.back.as_ref())
                .map_or(false, |b| b.has_subtype("adventure"));

        if is_adventure {
            if let Some(ref eff) = spell.effect {
                let rng_dyn: &mut dyn rand::RngCore = rng;
                eff.call(state, t, &spell.chosen_targets, catalog_map, rng_dyn);
            }
            let back_name = catalog_map.get(name.as_str())
                .and_then(|d| d.back.as_ref())
                .map(|b| b.name.as_str())
                .unwrap_or(name.as_str())
                .to_string();
            if let Some(card_obj) = state.objects.get_mut(&id) {
                card_obj.zone = CardZone::Exile { on_adventure: true };
                card_obj.spell = None;
            }
            state.log(t, &owner_str, format!("{} resolves → {} on adventure in exile", back_name, name));
        } else if let Some(ref eff) = spell.effect {
            let is_perm = state.materialized.defs.get(&id)
                .map(|d| matches!(d.kind, CardKind::Creature(_) | CardKind::Artifact(_)
                    | CardKind::Planeswalker(_) | CardKind::Enchantment))
                .unwrap_or(false);
            if !is_perm {
                if let Some(card_obj) = state.objects.get_mut(&id) {
                    card_obj.spell = None;
                }
                state.log(t, &owner_str, format!("{} resolves", name));
                change_zone(id, ZoneId::Graveyard, state, t, &owner_str, catalog_map, rng);
            }
            let rng_dyn: &mut dyn rand::RngCore = rng;
            eff.call(state, t, &spell.chosen_targets, catalog_map, rng_dyn);
            if is_perm {
                if let Some(card_obj) = state.objects.get_mut(&id) {
                    card_obj.spell = None;
                }
            }
        } else {
            if let Some(card_obj) = state.objects.get_mut(&id) {
                card_obj.spell = None;
            }
            state.log(t, &owner_str, format!("{} resolves", name));
            change_zone(id, ZoneId::Graveyard, state, t, &owner_str, catalog_map, rng);
        }
    } else if let Some(ability) = state.abilities.remove(&id) {
        let rng_dyn: &mut dyn rand::RngCore = rng;
        ability.effect.call(state, t, &ability.chosen_targets, catalog_map, rng_dyn);
    }
}

fn handle_priority_round(
    state: &mut SimState,
    t: u8,
    ap: &str,
    dd_turn: u8,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) {
    let nap = if ap == "us" { "opp" } else { "us" };
    let mut priority_holder = ap.to_string();
    let mut last_passer: Option<String> = None;
    let mut last_action: PriorityAction = PriorityAction::Pass;

    loop {
        let queued = std::mem::take(&mut state.pending_triggers);
        push_triggers(queued, state);
        check_state_based_actions(state, t, catalog_map, rng);

        let who = priority_holder.clone();
        let action = decide_action(
            state, t, ap, &who, dd_turn, &last_action, catalog_map, rng,
        );
        last_action = action.clone();

        match action {
            PriorityAction::LandDrop(card_id) => {
                sim_play_land(state, t, &who, card_id, catalog_map, rng);
                state.player_mut(&who).land_drop_available = false;
                last_passer = None;
            }
            PriorityAction::ActivateAbility(source_id, ref ability) => {
                if ability.loyalty_cost.is_some() && !state.stack.is_empty() {
                    last_passer = Some(who.clone());
                    priority_holder = if who == ap { nap.to_string() } else { ap.to_string() };
                    continue;
                }
                let source_name_for_stack = state.permanent_name(source_id)
                    .or_else(|| state.objects.get(&source_id).map(|c| c.catalog_key.clone()))
                    .unwrap_or_default();
                let (ability_effect, ability_targets): (Option<Effect>, Vec<Target>) = if ability.ninjutsu {
                    let attack_target = state.permanents_of(&who)
                        .find(|p| p.bf.as_ref().map_or(false, |bf| bf.attacking && bf.unblocked))
                        .and_then(|p| p.bf.as_ref().and_then(|bf| bf.attack_target));
                    let who_str = who.clone();
                    let ninja_effect = Effect(std::sync::Arc::new(move |state, t, _targets, _catalog_map, _rng| {
                        let ninja_name = state.objects.get(&source_id)
                            .map(|c| c.catalog_key.clone())
                            .unwrap_or_default();
                        if ninja_name.is_empty() { return; }
                        let new_id = state.alloc_id();
                        state.objects.insert(new_id, GameObject {
                            id: new_id,
                            catalog_key: ninja_name.clone(),
                            owner: who_str.clone(),
                            controller: who_str.clone(),
                            zone: CardZone::Battlefield,
                            is_token: false,
                            spell: None,
                            bf: Some(BattlefieldState {
                                tapped: true,
                                entered_this_turn: true,
                                attacking: true,
                                unblocked: true,
                                attack_target,
                                ..BattlefieldState::new()
                            }),
                        });
                        state.combat_attackers.push(new_id);
                        state.log(t, &who_str, format!("{} enters play tapped and attacking (ninjutsu)", ninja_name));
                    }));
                    (Some(ninja_effect), vec![])
                } else {
                    let eff = build_ability_effect(ability, &who, source_id);
                    let targets = if ability.target.is_some() {
                        choose_permanent_target(ability.target.as_deref().unwrap_or(""), &who, state, catalog_map, rng)
                            .map(|id| vec![Target::Object(id)])
                            .unwrap_or_default()
                    } else {
                        vec![]
                    };
                    (Some(eff), targets)
                };
                pay_activation_cost(state, t, &who, source_id, ability, catalog_map);
                let ab_id = state.alloc_id();
                let ab_owner = state.player_id(&who);
                let ab = StackAbility {
                    id: ab_id,
                    source_name: source_name_for_stack,
                    owner: ab_owner,
                    effect: ability_effect.unwrap_or_else(|| Effect(std::sync::Arc::new(|_, _, _, _, _| {}))),
                    chosen_targets: ability_targets,
                };
                state.abilities.insert(ab_id, ab);
                state.stack.push(ab_id);
                let next = if who == ap { nap } else { ap };
                priority_holder = next.to_string();
                last_passer = None;
            }
            PriorityAction::CastSpell { card_id, face, ref preferred_cost } => {
                let name = state.objects.get(&card_id).map(|c| c.catalog_key.clone()).unwrap_or_default();
                // Sorcery-speed check: for the main face read the materialized def; for the back
                // face read the back def's kind (the back face might be instant even if the front
                // face creature isn't).
                let is_instant = match face {
                    SpellFace::Main => state.materialized.defs.get(&card_id)
                        .map(|d| d.is_instant()).unwrap_or(false),
                    SpellFace::Back => state.materialized.defs.get(&card_id)
                        .and_then(|d| d.back.as_ref())
                        .map(|b| b.is_instant())
                        .unwrap_or(false),
                };
                if !is_instant && !state.stack.is_empty() {
                    eprintln!("[priority] BUG: sorcery-speed {} on non-empty stack, treating as Pass", name);
                    debug_assert!(false, "BUG: sorcery-speed cast of {} on non-empty stack", name);
                    last_passer = Some(who.clone());
                    priority_holder = if who == ap { nap.to_string() } else { ap.to_string() };
                } else if let Some(cid) = cast_spell(state, t, &who, card_id, face, preferred_cost.as_ref(), catalog_map, rng) {
                    if name == "Doomsday" && who == "us" { state.us.dd_cast = true; }
                    state.player_mut(&who).spells_cast_this_turn += 1;
                    state.stack.push(cid);
                    let next = if who == ap { nap } else { ap };
                    priority_holder = next.to_string();
                    last_passer = None;
                } else {
                    let pool = &state.player(&who).pool;
                    eprintln!("[priority] BUG: cast_spell failed for {} by {} (pool B={} U={} tot={}, hand={})",
                        name, who, pool.b, pool.u, pool.total, state.hand_size(&who));
                    debug_assert!(false, "BUG: cast_spell failed — decision function returned unaffordable/unavailable spell");
                    last_passer = Some(who.clone());
                    priority_holder = if who == ap { nap.to_string() } else { ap.to_string() };
                }
            }
            PriorityAction::Pass => {
                let other = if who == ap { nap } else { ap };
                if last_passer.as_deref() == Some(other) {
                    if state.stack.is_empty() {
                        break;
                    } else {
                        resolve_top_of_stack(state, t, ap, catalog_map, rng);
                        priority_holder = ap.to_string();
                        last_passer = None;
                        last_action = PriorityAction::Pass;
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
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) {
    // Ensure materialized state is current at the start of every step.
    // Strategy calls (declare_attackers, declare_blockers) and combat damage run against
    // this snapshot; fire_event also rebuilds it after each tick.
    let mat = recompute(state, catalog_map);
    state.materialized = mat;

    state.current_phase = Some(TurnPosition::Step(step.kind));
    match step.kind {
        StepKind::Untap => {
            let perm_ids: Vec<ObjId> = state.permanents_of(ap).map(|c| c.id).collect();
            for id in perm_ids {
                if let Some(bf) = state.permanent_bf_mut(id) {
                    bf.tapped = false;
                    bf.entered_this_turn = false;
                    bf.pw_activated_this_turn = false;
                }
            }
            state.player_mut(ap).land_drop_available = true;
            state.player_mut(ap).spells_cast_this_turn = 0;
            state.player_mut(ap).draws_this_turn = 0;
            // Expire "until your next turn" trigger and continuous instances for the active player.
            state.trigger_instances.retain(|ti| {
                !(ti.expiry == Some(ContinuousExpiry::StartOfControllerNextTurn) && ti.controller == ap)
            });
            state.continuous_instances.retain(|ci| {
                !(ci.expiry == ContinuousExpiry::StartOfControllerNextTurn && ci.controller == ap)
            });
        }
        StepKind::Draw => {
            let this_player_on_play = if ap == "us" { on_play } else { !on_play };
            let skip = this_player_on_play && t == 1;
            if skip {
                state.log(t, ap, "No draw (on the play)");
            } else {
                sim_draw(state, ap, t, true, catalog_map, rng);
            }
        }
        StepKind::Cleanup => {
            sim_discard_to_limit(state, t, ap);
            let cleanup_ids: Vec<ObjId> = state.permanents_of(ap).map(|c| c.id).collect();
            for id in cleanup_ids {
                if let Some(bf) = state.permanent_bf_mut(id) {
                    bf.damage = 0;
                }
            }
            // Expire EndOfTurn continuous and trigger instances.
            state.continuous_instances.retain(|ci| ci.expiry != ContinuousExpiry::EndOfTurn);
            state.trigger_instances.retain(|ti| ti.expiry != Some(ContinuousExpiry::EndOfTurn));
        }
        StepKind::DeclareAttackers => {
            // Strategy decides who attacks and what each attacker targets.
            let decisions = declare_attackers(ap, state, catalog_map, rng);
            // Apply: mark each attacker on the battlefield.
            for &(atk_id, target) in &decisions {
                if let Some(bf) = state.permanent_bf_mut(atk_id) {
                    bf.attacking = true;
                    bf.tapped = true;
                    bf.attack_target = target;
                }
            }
            let attackers: Vec<ObjId> = decisions.iter().map(|&(id, _)| id).collect();
            if !attackers.is_empty() {
                let atk_descs: Vec<String> = attackers.iter().filter_map(|&atk_id| {
                    let p = state.objects.get(&atk_id)?;
                    let target_name = p.bf.as_ref()?.attack_target
                        .and_then(|id| state.permanent_name(id))
                        .unwrap_or_else(|| "player".to_string());
                    Some(format!("{} → {}", p.catalog_key, target_name))
                }).collect();
                state.log(t, ap, format!("Declare attackers: {}", atk_descs.join(", ")));
            }
            state.combat_attackers = attackers.clone();
            // Fire triggers after attackers are marked.
            for atk_id in attackers {
                fire_event(GameEvent::CreatureAttacked {
                    attacker_id: atk_id,
                    attacker_controller: ap.to_string(),
                }, state, t, ap, catalog_map, rng);
            }
            fire_event(GameEvent::EnteredStep {
                step: StepKind::DeclareAttackers,
                active_player: ap.to_string(),
            }, state, t, ap, catalog_map, rng);
        }
        StepKind::DeclareBlockers => {
            let nap = opp_of(ap);
            // Strategy decides which blockers to assign.
            let blocks = declare_blockers(ap, state, catalog_map);
            for &(atk_id, blk_id) in &blocks {
                let atk_name = state.objects.get(&atk_id).map(|p| p.catalog_key.as_str()).unwrap_or("");
                let blk_name = state.objects.get(&blk_id).map(|p| p.catalog_key.clone()).unwrap_or_default();
                state.log(t, nap, format!("{} blocks {}", blk_name, atk_name));
            }
            state.combat_blocks = blocks;
            // Mark unblocked attackers so ninjutsu can target them.
            let blocked_atk_ids: std::collections::HashSet<ObjId> =
                state.combat_blocks.iter().map(|(a, _)| *a).collect();
            for &atk_id in &state.combat_attackers.clone() {
                if !blocked_atk_ids.contains(&atk_id) {
                    if let Some(bf) = state.permanent_bf_mut(atk_id) {
                        bf.unblocked = true;
                    }
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
                    let atk_pow = state.materialized.defs.get(&atk_id)
                        .and_then(|d| d.as_creature())
                        .map(|c| c.power())
                        .unwrap_or(1);
                    let blk_pow = state.materialized.defs.get(&blk_id)
                        .and_then(|d| d.as_creature())
                        .map(|c| c.power())
                        .unwrap_or(1);
                    if let Some(bf) = state.permanent_bf_mut(atk_id) {
                        bf.damage += blk_pow;
                    }
                    if let Some(bf) = state.permanent_bf_mut(blk_id) {
                        bf.damage += atk_pow;
                    }
                }

                let mut pw_damage: HashMap<ObjId, i32> = HashMap::new();
                for &atk_id in &attackers {
                    if !blocked_atk_ids.contains(&atk_id) {
                        let atk_pow = state.materialized.defs.get(&atk_id)
                            .and_then(|d| d.as_creature())
                            .map(|c| c.power())
                            .unwrap_or(1);
                        let attack_target = state.objects.get(&atk_id)
                            .and_then(|p| p.bf.as_ref())
                            .and_then(|bf| bf.attack_target);
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
                    let new_loyalty = if let Some(bf) = state.permanent_bf_mut(pw_id) {
                        bf.loyalty -= dmg;
                        Some(bf.loyalty)
                    } else {
                        None
                    };
                    if let Some(new_loyalty) = new_loyalty {
                        let pw_name = state.permanent_name(pw_id).unwrap_or_default();
                        state.log(t, ap, format!("Combat: {} damage to {} (loyalty: {})", dmg, pw_name, new_loyalty));
                    }
                }

            }
        }
        StepKind::EndCombat => {
            state.combat_attackers.clear();
            state.combat_blocks.clear();
            let all_ids: Vec<ObjId> = state.objects.values()
                .filter(|c| c.zone == CardZone::Battlefield)
                .map(|c| c.id)
                .collect();
            for id in all_ids {
                if let Some(bf) = state.permanent_bf_mut(id) {
                    bf.attacking = false;
                    bf.unblocked = false;
                }
            }
        }
        StepKind::Upkeep | StepKind::BeginCombat | StepKind::End => {
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
        fire_event(step_ev, state, t, ap, catalog_map, rng);
    }

    if step.prio {
        handle_priority_round(state, t, ap, dd_turn, catalog_map, rng);
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
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) {
    for step in &phase.steps {
        do_step(state, t, ap, step, dd_turn, on_play, catalog_map, rng);
        if state.done() {
            return;
        }
    }
    if phase.is_main_phase() {
        state.current_phase = Some(TurnPosition::Phase(phase.kind));
        let phase_ev = GameEvent::EnteredPhase { phase: phase.kind };
        fire_event(phase_ev, state, t, ap, catalog_map, rng);
        handle_priority_round(state, t, ap, dd_turn, catalog_map, rng);
        // Mana pool drains at the end of the main phase.
        state.us.pool.drain();
        state.opp.pool.drain();
        if state.done() { return; }
    }
}

/// Simulate one full turn for the active player `ap`.
fn do_turn(
    state: &mut SimState,
    t: u8,
    ap: &str,
    dd_turn: u8,
    on_play: bool,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) {
    state.current_ap = state.player_id(ap);
    do_phase(state, t, ap, &beginning_phase(), dd_turn, on_play, catalog_map, rng);
    if state.done() { return; }

    do_phase(state, t, ap, &main_phase(), dd_turn, on_play, catalog_map, rng);
    if state.done() { return; }

    do_phase(state, t, ap, &combat_phase(), dd_turn, on_play, catalog_map, rng);
    if state.done() { return; }

    do_phase(state, t, ap, &post_combat_main_phase(), dd_turn, on_play, catalog_map, rng);
    if state.done() { return; }

    do_phase(state, t, ap, &end_phase(), dd_turn, on_play, catalog_map, rng);
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

    // Populate state.objects with Library-zone objects for each player's mainboard.
    // catalog: game setup — ObjIds are assigned here for the first time; materialized
    // does not exist yet. Catalog is the only source of card definitions at this stage.
    for (name, qty, board) in all_cards {
        if board != "main" { continue; }
        if catalog_map.get(name.as_str()).is_none() { continue; }
        for _ in 0..*qty {
            let id = state.alloc_id();
            state.objects.insert(id, GameObject::new(id, name.clone(), "us"));
            if let Some(def) = catalog_map.get(name.as_str()) {
                preregister_instances(def, id, "us", &mut state);
            }
        }
    }
    for (name, qty, board) in opp_cards {
        if board != "main" { continue; }
        if catalog_map.get(name.as_str()).is_none() { continue; }
        for _ in 0..*qty {
            let id = state.alloc_id();
            state.objects.insert(id, GameObject::new(id, name.clone(), "opp"));
            if let Some(def) = catalog_map.get(name.as_str()) {
                preregister_instances(def, id, "opp", &mut state);
            }
        }
    }

    // Deal opening hands: move `7 - mulligans` cards from Library to Hand.
    for _ in 0..(7u8.saturating_sub(our_mulligans)) {
        sim_draw(&mut state, "us", 0, false, &catalog_map, rng);
    }
    for _ in 0..(7u8.saturating_sub(opp_mulligans)) {
        sim_draw(&mut state, "opp", 0, false, &catalog_map, rng);
    }

    let us_hand = state.hand_size("us");
    let opp_hand = state.hand_size("opp");
    state.log(
        0,
        "—",
        format!(
            "Turn {} — {} ({}) | us: {} cards (-{} mulligans), opp: {} cards (-{} mulligans)",
            turn,
            opponent,
            if on_play { "play" } else { "draw" },
            us_hand,
            our_mulligans,
            opp_hand,
            opp_mulligans
        ),
    );

    // ── Turn loop ────────────────────────────────────────────────────────────

    for t in 1..=turn {
        if !on_play {
            do_turn(&mut state, t, "opp", turn, on_play, &catalog_map, rng);
            if state.done() { break; }
        }
        {
            do_turn(&mut state, t, "us", turn, on_play, &catalog_map, rng);
            if state.done() { break; }
        }
        if on_play && t < turn {
            do_turn(&mut state, t, "opp", turn, on_play, &catalog_map, rng);
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

fn generate_scenario(
    deck_name: &str,
    opp_display: &str,
    config: &PilegenConfig,
    all_cards: &[(String, i32, String)],
    opp_cards: &[(String, i32, String)],
) -> SimState {
    let mut rng = rand::thread_rng();
    loop {
        if let Some(state) =
            simulate_game(deck_name, opp_display, config, all_cards, opp_cards, &mut rng)
        {
            // All cards are already in their correct zones in state.objects.
            // Hand cards were moved to Hand zone by sim_draw during opening hand deal.
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
/// - Spells need a target (including stack targets), abilities, or effects in `build_spell_effect`.
fn card_has_implementation(def: &CardDef) -> bool {
    if def.is_land() { return true; }
    if !def.abilities().is_empty() { return true; }
    if def.target().is_some() { return true; }
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

