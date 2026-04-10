use rand::{Rng, SeedableRng};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

mod catalog;
pub(crate) use catalog::*;

mod card_defs;
pub use card_defs::build_catalog;

mod effects;
pub(crate) use effects::*;

mod predicates;
pub(crate) use predicates::*;

mod strategy;
use strategy::{Strategy, DoomsdayStrategy, GenericOppStrategy};
#[cfg(test)] use strategy::try_ninjutsu;

#[cfg(test)]
mod tests;

// ── WASM entry point ─────────────────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg(target_arch = "wasm32")]
fn cards(list: &[(&str, i32)]) -> Vec<(String, i32, String)> {
    list.iter().map(|(n, q)| (n.to_string(), *q, "main".to_string())).collect()
}

#[cfg(target_arch = "wasm32")]
fn dd_deck() -> Vec<(String, i32, String)> {
    // tempo-doomsday-wasteland-1.4
    cards(&[
        ("Underground Sea", 3), ("Polluted Delta", 4), ("Flooded Strand", 1),
        ("Misty Rainforest", 1), ("Scalding Tarn", 1), ("Marsh Flats", 1),
        ("Island", 1), ("Swamp", 1), ("Undercity Sewers", 2), ("Wasteland", 3),
        ("Cavern of Souls", 1),
        ("Lotus Petal", 2), ("Lion's Eye Diamond", 1),
        ("Dark Ritual", 4), ("Doomsday", 4), ("Brainstorm", 4),
        ("Ponder", 4), ("Consider", 1), ("Edge of Autumn", 1),
        ("Force of Will", 4), ("Daze", 3), ("Thoughtseize", 2),
        ("Street Wraith", 1), ("Thassa's Oracle", 1), ("Unearth", 1),
        ("Tamiyo, Inquisitive Student", 4), ("Orcish Bowmasters", 2),
        ("Murktide Regent", 2),
    ])
}

#[cfg(target_arch = "wasm32")]
fn izzet_delver_deck() -> Vec<(String, i32, String)> {
    // izzet-delver-ethanr-mar26
    cards(&[
        ("Volcanic Island", 4), ("Scalding Tarn", 2), ("Flooded Strand", 2),
        ("Misty Rainforest", 2), ("Polluted Delta", 3), ("Wasteland", 4),
        ("Island", 1), ("Thundering Falls", 1),
        ("Delver of Secrets", 3), ("Dragon's Rage Channeler", 4),
        ("Murktide Regent", 2), ("Brazen Borrower", 1),
        ("Cori-Steel Cutter", 3), ("Mishra's Bauble", 4),
        ("Lightning Bolt", 4), ("Unholy Heat", 1),
        ("Force of Will", 4), ("Force of Negation", 1), ("Daze", 4),
        ("Brainstorm", 4), ("Ponder", 4), ("Preordain", 2),
    ])
}

#[cfg(target_arch = "wasm32")]
fn ub_tempo_deck() -> Vec<(String, i32, String)> {
    // dimir-tempo-1.0
    cards(&[
        ("Underground Sea", 4), ("Polluted Delta", 4), ("Flooded Strand", 2),
        ("Misty Rainforest", 1), ("Scalding Tarn", 1), ("Bloodstained Mire", 1),
        ("Island", 1), ("Swamp", 1), ("Wasteland", 4), ("Undercity Sewers", 1),
        ("Tamiyo, Inquisitive Student", 4), ("Orcish Bowmasters", 4),
        ("Murktide Regent", 3), ("Barrowgoyf", 2), ("Brazen Borrower", 1),
        ("Kaito, Bane of Nightmares", 2),
        ("Brainstorm", 4), ("Ponder", 4), ("Force of Will", 4), ("Daze", 3),
        ("Fatal Push", 4), ("Snuff Out", 1), ("Thoughtseize", 4),
    ])
}

/// Returns JSON list of available matchup names.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn list_matchups() -> String {
    serde_json::to_string(&["Izzet Delver", "UB Tempo"]).unwrap()
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn run_scenario(matchup: &str) -> String {
    let catalog = build_catalog();
    let dd_cards = dd_deck();

    let (opp_name, opp_cards) = match matchup {
        "UB Tempo" => ("UB Tempo", ub_tempo_deck()),
        _ => ("Izzet Delver", izzet_delver_deck()),
    };

    let state = generate_scenario("doomsday", opp_name, &catalog, &dd_cards, &opp_cards);
    serde_json::to_string(&state.to_result()).unwrap()
}

// ── Game state ────────────────────────────────────────────────────────────────

// ── Stable object identity ────────────────────────────────────────────────────

/// Opaque game object identifier. Every player, card, token, and stack ability
/// gets one at construction time and keeps it through all zone changes.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub(crate) struct ObjId(u64);

/// Type of a counter placed on a game object.
/// Counters persist across zone changes (stored on `GameObject`, not `BattlefieldState`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum CounterType {
    /// Placed on cards exiled by Dauthi Voidwalker's replacement effect.
    Void,
    /// Placed on Engineered Explosives (and similar) via sunburst on entry.
    Charge,
}


impl ObjId {
    const UNSET: ObjId = ObjId(0);

}

/// Typed player identifier. Replaces the "us"/"opp" string convention.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum PlayerId { Us, Opp }

impl PlayerId {
    pub(crate) fn opp(self) -> PlayerId {
        match self { PlayerId::Us => PlayerId::Opp, PlayerId::Opp => PlayerId::Us }
    }
}

impl std::fmt::Display for PlayerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { PlayerId::Us => write!(f, "us"), PlayerId::Opp => write!(f, "opp") }
    }
}

/// Zone a card currently occupies.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum CardZone {
    Library,
    Hand { known: bool },   // known = identity visible to opponent
    Stack,
    Battlefield,
    Graveyard,
    Exile { on_adventure: bool },
}

/// Context recording which objects moved during cost payment.
/// Carried on stack items so that resolution effects can inspect what was paid.
#[derive(Clone, Default)]
pub(crate) struct CostsPaidCtx {
    /// ObjIds of all objects moved as cost (exiled, discarded, sacrificed, returned).
    pub(crate) objects_moved: Vec<ObjId>,
    /// For each `ReturnFromBattlefield` payment, the `attack_target` the returned
    /// permanent had at the time it left the battlefield (in payment order).
    pub(crate) returned_attack_targets: Vec<Option<ObjId>>,
    /// Number of times a Replicate cost was paid for the spell (CR 702.58).
    /// Set during cost payment; used by cast_spell to push copies to the stack.
    pub(crate) replicate_count: u32,
    /// Strategy-chosen X value paid as `XLife` additional cost (0 if no X cost).
    pub(crate) chosen_x: u32,
    /// Chosen mode for modal spells (CR 700.2a). Set at cast time.
    pub(crate) chosen_mode: usize,
    /// Index of the alternate cost used from `def.alternate_costs()`, if any.
    /// `None` = hardcast (mana cost paid). Used by evoke triggers to detect
    /// whether the evoke cost path was taken (CR 702.74).
    pub(crate) alt_cost_index: Option<usize>,
}

/// Spell-on-stack state for a card while it's on the stack.
/// Populated at cast time; cleared when the spell resolves or is countered.
#[derive(Clone)]
struct SpellState {
    effect: Option<Effect>,
    chosen_targets: Vec<ObjId>,
    /// True when the back face of a split card was cast (e.g. an adventure instant).
    is_back_face: bool,
    /// Objects moved during cost payment (for effects that depend on what was paid).
    costs_paid_ctx: CostsPaidCtx,
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
    /// Choice made as this permanent entered the battlefield (e.g. color for Painter's Servant,
    /// creature type for Cavern of Souls, card name for Disruptor Flute). Written by ETB
    /// replacement closures that call `resolve_choice`; cleared automatically on LTB when
    /// `bf` is dropped. Most cards also capture the choice value in their CE closure (the CE IS
    /// the primary storage); `etb_choice` is the side-channel for abilities that need to inspect
    /// "what was named" without holding a captured copy.
    pub(crate) etb_choice: Option<ChoiceResult>,
    /// Equipment: the creature this Equipment is attached to (CR 301.5).
    pub(crate) attached_to: Option<ObjId>,
}

impl BattlefieldState {
    fn new() -> Self {
        BattlefieldState {
            tapped: false, damage: 0, entered_this_turn: true, counters: 0,
            power_mod: 0, toughness_mod: 0, loyalty: 0, pw_activated_this_turn: false,
            attacking: false, unblocked: false, attack_target: None,
            active_face: 0, etb_choice: None, attached_to: None,
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
    owner: PlayerId,
    controller: PlayerId,
    zone: CardZone,
    is_token: bool,
    bf: Option<BattlefieldState>,      // Some only when zone == Battlefield
    spell: Option<SpellState>,         // Some only when zone == Stack (spell on stack)
    /// Inlined post-CE materialized snapshot. Rebuilt by `recompute` after each state-mutating tick.
    materialized: Option<CardDef>,
    /// Zone-independent counters (e.g. void counters from Dauthi Voidwalker).
    /// Persists across zone changes.
    pub(crate) counters: HashMap<CounterType, u32>,
    /// CI timestamp assigned when this object enters the battlefield.
    /// Used by `recompute` to give static-ability CIs stable timestamps across recompute cycles
    /// (CR 613.6: simultaneous effects from the same source share a timestamp).
    pub(crate) ci_timestamp: u32,
}

impl GameObject {
    fn new(id: ObjId, catalog_key: impl Into<String>, owner: PlayerId) -> Self {
        GameObject {
            id, catalog_key: catalog_key.into(), controller: owner, owner,
            zone: CardZone::Library, is_token: false, bf: None, spell: None,
            materialized: None, counters: HashMap::new(), ci_timestamp: 0,
        }
    }
}


/// An activated or triggered ability on the stack.
#[derive(Clone)]
pub(crate) struct StackAbility {
    /// The stable ObjId for this ability (also the key in `SimState::abilities`).
    #[allow(dead_code)]
    pub(crate) id: ObjId,
    pub(crate) source_name: String,
    pub(crate) owner: ObjId,         // player id
    pub(crate) effect: Effect,
    pub(crate) chosen_targets: Vec<ObjId>,
    /// Objects moved during cost payment (for effects that depend on what was paid).
    #[allow(dead_code)]
    pub(crate) costs_paid_ctx: CostsPaidCtx,
    /// True iff this is a triggered ability (vs. an activated ability).
    /// Used by `TargetSpec::AbilityOnStack` with `AbilityType::Triggered` to match triggered abilities.
    /// All stack items — including activated and triggered abilities — are legal targets
    /// for counters that specify them; "can't be countered" is a resolution rule, not a
    /// targeting restriction (CR 608.2b).
    pub(crate) is_triggered: bool,
    /// False iff this ability can't be countered (CR 608.2b). Checked at resolution;
    /// the ability is still a legal target for counter effects.
    pub(crate) counterable: bool,
    /// If `Some`, the engine enumerates choices at resolution and asks strategy to pick one.
    /// The chosen ObjId is prepended to `chosen_targets` before calling `effect`.
    pub(crate) choice_spec: Option<ChoiceSpec>,
}

// ── Trigger system ────────────────────────────────────────────────────────────

/// Zones a card or permanent can occupy.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum ZoneId {
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
pub(crate) enum GameEvent {
    /// A card moved from one zone to another (ETB, GY→Exile, etc.).
    /// Does NOT include drawing — use `Draw` for that.
    ZoneChange {
        id: ObjId,
        actor: PlayerId,
        from: ZoneId,
        to: ZoneId,
        controller: PlayerId,
    },
    /// A player draws a card. `draw_index` is which draw this is this turn (1-based).
    /// `is_natural` is true only for the draw-step draw.
    Draw {
        controller: PlayerId,
        draw_index: u8,
        is_natural: bool,
    },
    /// Fired after step-specific actions complete and before priority begins.
    /// Only fires for named steps that have a priority round (not Untap or Cleanup).
    EnteredStep {
        step: StepKind,
        active_player: PlayerId,
    },
    /// Fired at the start of a phase-level priority window (main phases, which have no named steps).
    EnteredPhase {
        phase: PhaseKind,
    },
    /// Mana was added to a player's pool. Fires through the event pipeline so
    /// replacement effects (e.g. Damping Sphere) can intercept.
    ManaProduced { who: PlayerId, spec: String },
    /// A creature was declared as an attacker.
    CreatureAttacked {
        attacker_id: ObjId,
        attacker_controller: PlayerId,
    },
    /// Fired just before cost payment and state mutation in `cast_spell`. Prohibition gate.
    /// If suppressed by a "can't cast" prohibition, `cast_spell` returns `None`.
    SpellBeingCast {
        caster: PlayerId,
        card_id: ObjId,
        mana_value: i32,
        is_noncreature: bool,
    },
    /// Fired after all costs are paid and the spell object is on the stack.
    /// Used by triggers that react to casting (e.g. Lavinia's counter-free-spells trigger).
    SpellCast {
        caster: PlayerId,
        card_id: ObjId,
        mana_spent: bool,
    },
    /// Fired inside `counter_one` before the counterable check, for spell objects only.
    /// Prohibition gate: "can't be countered" effects suppress this event (CR 614.17).
    /// `caster` is the controller of the spell being countered.
    SpellBeingCountered {
        caster: PlayerId,
        card_id: ObjId,
    },
    /// Fired in `sim_play_land` after the zone change. Distinguishes the once-per-turn
    /// land play from hand vs. lands entering via fetch, reanimate, etc.
    LandPlayed { id: ObjId, controller: PlayerId },
    // Future variants: DamageDealt, SpellResolved, AbilityActivated,
    //                  CounterChanged, LifeChanged, TokenCreated.
}

/// Data stored with a triggered ability waiting to be pushed onto the stack.
/// The effect closure captures all context (targets, source data) at trigger-push time.
#[derive(Clone)]
pub(crate) struct TriggerContext {
    /// Display name of the source — used for stack item naming and logging.
    pub(crate) source_name: String,
    /// Player who controls that permanent.
    pub(crate) controller: PlayerId,
    /// Legal targets this trigger may choose from. Resolved when pushed to the stack.
    pub(crate) target_spec: TargetSpec,
    /// The effect to apply when this trigger resolves. Receives the chosen targets.
    pub(crate) effect: Effect,
}

// ── Triggers and replacement effects ─────────────────────────────────────────

/// Signature for a per-card trigger check function.
/// Inspects the event and game state; if a trigger fires, appends a `TriggerContext` to `pending`.
pub(crate) type TriggerCheckFn =
    std::sync::Arc<dyn Fn(&GameEvent, ObjId, PlayerId, &SimState, &mut Vec<TriggerContext>) + Send + Sync>;

/// Signature for a per-card replacement check function.
/// Returns Some(targets) if this replacement applies to the event; None otherwise.
/// `source_id` is passed so self-ETB checks work without string dispatch.
pub(crate) type ReplacementCheckFn = std::sync::Arc<dyn Fn(&GameEvent, ObjId, PlayerId, &SimState) -> Option<Vec<ObjId>> + Send + Sync>;

/// Signature for a "can't happen" prohibition check (CR 614.17).
/// Returns true if the event is prohibited. Takes `&SimState` so checks can inspect card types,
/// controller, etc. Prohibition checks run before replacement effects (CR 614.17 — can't effects
/// aren't replacements and take precedence over permissive effects).
pub(crate) type ProhibitionCheckFn =
    std::sync::Arc<dyn Fn(&GameEvent, ObjId, PlayerId, &SimState) -> bool + Send + Sync>;

/// Predicate controlling when a card-bound trigger is armed.
/// Receives (source_id, &SimState) and returns true if the trigger should fire.
pub(crate) type TriggerPredicate =
    std::sync::Arc<dyn Fn(ObjId, &SimState) -> bool + Send + Sync>;

/// Default trigger predicate: source is on the battlefield.
/// Also correct for ETB-self triggers — fire_triggers runs at Stage 5, after
/// do_effect has already moved the card to the battlefield.
pub(crate) fn tp_on_battlefield() -> TriggerPredicate {
    Arc::new(|src, state| {
        state.objects.get(&src).map_or(false, |o| matches!(o.zone, CardZone::Battlefield))
    })
}

/// Trigger predicate: source is on the stack (e.g. Storm).
pub(crate) fn tp_on_stack() -> TriggerPredicate {
    Arc::new(|src, state| {
        state.objects.get(&src).map_or(false, |o| matches!(o.zone, CardZone::Stack))
    })
}

/// Trigger predicate: always active regardless of zone.
/// Used for intrinsic entry replacements (CR 614.1c/d: "As this permanent enters...")
/// where the check fn itself guards on (id == source_id && to == Battlefield).
pub(crate) fn tp_always() -> TriggerPredicate {
    Arc::new(|_, _| true)
}

/// Predicate for LatentSpellMod: given (spell ObjId, caster, &SimState), returns
/// true if the spell qualifies for the latent modification.
pub(crate) type SpellPredicate =
    Arc<dyn Fn(ObjId, PlayerId, &SimState) -> bool + Send + Sync>;

/// A latent continuous effect that modifies the next qualifying spell cast
/// (CR 611.2f). Pushed by an ability resolution; consumed during 601.2a when
/// a qualifying spell is announced.
pub(crate) struct LatentSpellMod {
    pub(crate) controller: PlayerId,
    /// Does this spell qualify? (e.g. "the next instant or sorcery spell")
    pub(crate) predicate: SpellPredicate,
    /// Given the qualifying spell's ObjId and controller, produce a CI to apply.
    pub(crate) make_ci: Arc<dyn Fn(ObjId, PlayerId) -> ContinuousInstance + Send + Sync>,
    /// Fallback expiry if no qualifying spell is cast.
    pub(crate) expiry: Expiry,
}

/// A trigger definition on a CardDef.  Pairs the check function with a predicate
/// that determines when the trigger is armed based on the source's game state.
#[derive(Clone)]
pub(crate) struct TriggerDef {
    pub(crate) check: TriggerCheckFn,
    /// In which state is this trigger armed?
    /// Default (tp_on_battlefield): source is on the battlefield.
    /// Storm: tp_on_stack (source is on the stack, fires on self-cast).
    pub(crate) active_when: TriggerPredicate,
}

/// Ephemeral trigger instance created at runtime by ability effects (e.g. Sneak Attack
/// end-step sacrifice, Tamiyo +2 watcher). Card-bound triggers are derived from catalog
/// at fire time via `fire_triggers`.
pub(crate) struct TriggerInstance {
    pub(crate) source_id: ObjId,
    pub(crate) controller: PlayerId,
    pub(crate) check: TriggerCheckFn,
    /// None for permanent (card-based) triggers; Some for floating triggers created by abilities.
    pub(crate) expiry: Option<Expiry>,
}

/// Ephemeral replacement instance created at runtime by ability effects (e.g. Force of Negation
/// "exile instead of graveyard"). Card-bound replacements are derived from catalog at fire time.
pub(crate) struct ReplacementInstance {
    pub(crate) source_id: ObjId,
    pub(crate) controller: PlayerId,
    pub(crate) check: ReplacementCheckFn,
    pub(crate) effect: Effect,
}

// ── Continuous effects (new model) ───────────────────────────────────────────

/// The five colors of Magic.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Color { White, Blue, Black, Red, Green }

/// A typed choice that an effect needs to make at resolution/ETB time.
/// Passed to `SimState.resolve_choice`; the installed closure returns a `ChoiceResult`.
/// This covers decisions that are not targets (`TargetSpec`) and not object selections
/// (`ChoiceSpec`) — specifically, choices over abstract typed values.
#[derive(Clone)]
pub(crate) enum ChoiceRequest {
    /// Choose one of the five colors (e.g. Painter's Servant ETB).
    Color,
    /// Choose a creature type by name (e.g. Cavern of Souls ETB).
    CreatureType,
    /// Choose a card name (e.g. Disruptor Flute, Pithing Needle, Meddling Mage).
    CardName,
    /// Choose one of N modes for a modal spell (e.g. Sheoldred's Edict "Choose one —").
    /// The payload is the number of available modes. Strategy returns `ChoiceResult::Mode(i)`
    /// where `i < N`. Default: mode 0.
    Mode(usize),
    /// Offered when a Ward trigger resolves: should the targeting player pay the ward cost?
    /// Returns `ChoiceResult::Bool(true)` to pay (spell proceeds), `false` to decline (spell countered).
    WardPayment { cost: Vec<CostComponent> },
    /// "You may put one of these onto the battlefield" (CR 101.4, e.g. Show and Tell).
    /// Returns `ChoiceResult::OptionalObject(Some(id))` to place, or `None` to decline.
    MayPutOnBattlefield { candidates: Vec<ObjId> },
    /// "You may attach this Equipment to it" (CR 701.3).
    /// Returns `ChoiceResult::Bool(true)` to attach, `false` to decline.
    MayAttach,
}

/// The value returned by `SimState.resolve_choice` for a given `ChoiceRequest`.
#[derive(Clone)]
pub(crate) enum ChoiceResult {
    Color(Color),
    CreatureType(String),
    CardName(String),
    Mode(usize),
    /// Returned for `ChoiceRequest::WardPayment`: true = pay, false = decline.
    Bool(bool),
    /// Returned for `ChoiceRequest::MayPutOnBattlefield`: chosen object or decline.
    OptionalObject(Option<ObjId>),
}

/// Card supertypes (Legendary, Basic, Snow, World, Ongoing).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Supertype { Legendary, Basic, Snow }

/// The seven layers in which continuous effects are applied (MTG rule 613).
/// Ordering is derived: effects in earlier layers apply before later ones.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[allow(dead_code)] // L1–L5 are defined for completeness; only L6–L7 are currently used
pub(crate) enum ContinuousLayer {
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
pub(crate) type ContinuousModFn =
    std::sync::Arc<dyn Fn(&mut CardDef, &SimState) + Send + Sync>;

/// Predicate that decides whether a continuous effect applies to a given object.
/// Receives (target_id, target_controller, state).
pub(crate) type ContinuousFilterFn =
    std::sync::Arc<dyn Fn(ObjId, PlayerId, &SimState) -> bool + Send + Sync>;

/// What characteristics a CE reads from targets (for CR 613.7 dependency analysis).
/// If CE_A reads a category that CE_B writes, A depends on B within the same layer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)]
pub(crate) enum CeReads {
    LandTypes,
    Supertypes,
    Abilities,
    Color,
    PowerToughness,
    CardTypes,
}

/// What characteristics a CE writes (modifies) on targets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)]
pub(crate) enum CeWrites {
    LandTypes,
    Supertypes,
    Abilities,
    Color,
    PowerToughness,
    CardTypes,
}

/// When a `ContinuousInstance` expires and should be removed.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Expiry {
    /// Removed during the Cleanup step of the current turn.
    EndOfTurn,
    /// Removed at the start of the controlling player's next Untap step.
    StartOfControllerNextTurn,
    /// Tied to a permanent being on the battlefield; removed when it leaves play.
    /// Used for ephemeral CEs created by abilities (e.g. Sneak Attack haste grant).
    WhileSourceOnBattlefield,
    /// Fires once, then self-removes. Used for delayed triggers (e.g. Sneak Attack
    /// "sacrifice at the beginning of the next end step").
    OneShot,
}

/// A single registered continuous-effect instance.
/// Created when a spell or ability that grants a CE resolves.
/// Removed when `expiry` is met.
pub(crate) struct ContinuousInstance {
    /// Object that generated this effect (for expiry tracking and logging).
    pub(crate) source_id: ObjId,
    /// Controller of the source at the time the effect was created.
    pub(crate) controller: PlayerId,
    /// Which layer this modifier applies in (determines application order).
    pub(crate) layer: ContinuousLayer,
    /// CR 613.7: what this CE reads from targets to determine applicability/behavior.
    /// Used to compute dependency edges within a layer.
    pub(crate) reads: Vec<CeReads>,
    /// CR 613.7: what this CE writes (modifies) on targets.
    pub(crate) writes: Vec<CeWrites>,
    /// CR 613.6: registration sequence — tiebreaker after dependency ordering.
    pub(crate) timestamp: u32,
    /// Determines which game objects this CE affects.
    pub(crate) filter: ContinuousFilterFn,
    /// Mutates the target object's cloned `CardDef`.
    pub(crate) modifier: ContinuousModFn,
    /// When this instance should be removed.
    pub(crate) expiry: Expiry,
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

/// CR 613.7: A depends on B if B writes a category that A reads.
fn ce_categories_match(r: CeReads, w: CeWrites) -> bool {
    matches!(
        (r, w),
        (CeReads::LandTypes, CeWrites::LandTypes)
            | (CeReads::Supertypes, CeWrites::Supertypes)
            | (CeReads::Abilities, CeWrites::Abilities)
            | (CeReads::Color, CeWrites::Color)
            | (CeReads::PowerToughness, CeWrites::PowerToughness)
            | (CeReads::CardTypes, CeWrites::CardTypes)
    )
}

/// Topological sort of CIs within a single layer using Kahn's algorithm.
/// Ties broken by timestamp (CR 613.6). Cycles fall back to timestamp order.
/// Topological sort within a single layer.
/// `static_cis` and `ephemeral_cis` form a combined index space:
/// indices 0..static_cis.len() refer to static_cis, the rest to ephemeral_cis.
fn topo_sort_layer(
    layer_slice: &[usize],
    static_cis: &[ContinuousInstance],
    ephemeral_cis: &[ContinuousInstance],
    out: &mut Vec<usize>,
) {
    use std::collections::BinaryHeap;
    use std::cmp::Reverse;

    let sc = static_cis.len();
    let get = |idx: usize| -> &ContinuousInstance {
        if idx < sc { &static_cis[idx] } else { &ephemeral_cis[idx - sc] }
    };

    let n = layer_slice.len();
    // Build dependency edges: in_degree[i] = count of CIs that i depends on.
    let mut in_degree = vec![0usize; n];
    let mut dependents: Vec<Vec<usize>> = vec![vec![]; n];
    for i in 0..n {
        for j in 0..n {
            if i == j { continue; }
            let ci_i = get(layer_slice[i]);
            let ci_j = get(layer_slice[j]);
            if ci_i.reads.iter().any(|r| ci_j.writes.iter().any(|w| ce_categories_match(*r, *w))) {
                in_degree[i] += 1;
                dependents[j].push(i);
            }
        }
    }
    // Min-heap keyed by timestamp — independent CIs with lowest timestamp first.
    let mut ready: BinaryHeap<Reverse<(u32, usize)>> = BinaryHeap::new();
    for i in 0..n {
        if in_degree[i] == 0 {
            ready.push(Reverse((get(layer_slice[i]).timestamp, i)));
        }
    }
    let start = out.len();
    while let Some(Reverse((_, idx))) = ready.pop() {
        out.push(layer_slice[idx]);
        for &dep in &dependents[idx] {
            in_degree[dep] -= 1;
            if in_degree[dep] == 0 {
                ready.push(Reverse((get(layer_slice[dep]).timestamp, dep)));
            }
        }
    }
    // Cycle fallback: append remaining in timestamp order.
    if out.len() - start < n {
        for i in 0..n {
            if !out[start..].contains(&layer_slice[i]) {
                out.push(layer_slice[i]);
            }
        }
    }
}

/// Rebuild each game object's inlined `materialized` field by applying all active
/// `ContinuousInstance`s to clones of the object's `CardDef` from the catalog.
///
/// All zones are covered: CEs such as Painter's Servant and Mycosynth Lattice can modify
/// per-card characteristics in every zone (hand, library, GY, exile, stack, battlefield).
/// Objects with no entry in the catalog (e.g. naked stack abilities) are silently skipped
/// by the `catalog.get()` guard below.
///
/// Called after every `fire_event` at recursion depth 0 (each "tick"). Strategy and display
/// code read `state.def_of(id)` which returns the inlined snapshot; they never access raw
/// `CardDef` fields directly.
///
/// Application order: CIs sorted by layer, then topologically by reads/writes
/// dependency within each layer, with timestamp as tiebreaker (CR 613.6/613.7).
/// CIs are applied **one at a time across all objects** (CE-by-CE, not object-by-object).
/// After each CI is applied, dependent CIs check whether their source still has the
/// generating ability via the in-progress materialized state (CR 613.7 dependency).
pub(crate) fn recompute(state: &mut SimState) {
    let ids: Vec<ObjId> = state.objects.keys().copied().collect();

    // Phase 1: initialize each object's materialized def from catalog base.
    for &id in &ids {
        let Some(catalog_key) = state.objects.get(&id).map(|o| o.catalog_key.clone()) else { continue };
        let Some(base) = state.catalog.get(&catalog_key) else { continue };
        let mut def = base.clone();

        // DFC back-face substitution.
        {
            let obj = state.objects.get(&id).unwrap();
            if obj.bf.as_ref().map_or(false, |bf| bf.active_face == 1) {
                if let Some(ref back) = def.back.take() {
                    def.name = back.name.clone();
                    def.kind = back.kind.clone();
                }
            }
        }

        // Fold game-accumulated state (counters, temporary P/T mods).
        {
            let obj = state.objects.get(&id).unwrap();
            fold_game_state_into_def(&mut def, obj);
        }

        // Zone-based castable default: cards in hand are castable, others are not
        // (CEs may override — e.g. Dauthi sets castable=true on exiled cards).
        {
            let obj = state.objects.get(&id).unwrap();
            def.castable = matches!(obj.zone, CardZone::Hand { .. });
        }

        state.objects.get_mut(&id).unwrap().materialized = Some(def);
    }

    // Phase 1b: Generate static-ability CIs from catalog for all BF permanents.
    // These are produced fresh each recompute cycle with stable timestamps from
    // GameObject.ci_timestamp (assigned at ETB). They are NOT stored in
    // continuous_instances — only ephemeral CIs live there.
    let mut static_cis: Vec<ContinuousInstance> = Vec::new();
    for (&id, obj) in &state.objects {
        if !matches!(obj.zone, CardZone::Battlefield) { continue; }
        let Some(card_def) = state.catalog.get(&obj.catalog_key) else { continue };
        for factory in &card_def.static_ability_defs {
            let mut ci = factory(id, obj.controller);
            ci.timestamp = obj.ci_timestamp;
            static_cis.push(ci);
        }
    }

    // Build combined CI list: static-ability CIs + ephemeral CIs from state.
    // We index into this combined list for sorting and application.
    let static_count = static_cis.len();
    let total = static_count + state.continuous_instances.len();

    // Helper: access CI by combined index (0..static_count → static_cis, rest → ephemeral).
    let get_ci = |idx: usize| -> &ContinuousInstance {
        if idx < static_count { &static_cis[idx] }
        else { &state.continuous_instances[idx - static_count] }
    };

    // Phase 2: within each layer, compute dependency DAG and topological sort (CR 613.7).
    // A depends on B if B.writes overlaps A.reads. Ties broken by timestamp (CR 613.6).
    let mut ci_order: Vec<usize> = (0..total).collect();
    ci_order.sort_by_key(|&i| (get_ci(i).layer, get_ci(i).timestamp));
    let mut final_order: Vec<usize> = Vec::with_capacity(ci_order.len());
    let mut layer_start = 0;
    while layer_start < ci_order.len() {
        let current_layer = get_ci(ci_order[layer_start]).layer;
        let mut layer_end = layer_start;
        while layer_end < ci_order.len()
            && get_ci(ci_order[layer_end]).layer == current_layer
        {
            layer_end += 1;
        }
        let layer_slice = &ci_order[layer_start..layer_end];
        if layer_slice.len() <= 1 {
            final_order.extend_from_slice(layer_slice);
        } else {
            topo_sort_layer(layer_slice, &static_cis, &state.continuous_instances, &mut final_order);
        }
        layer_start = layer_end;
    }
    let ci_order = final_order;

    // Phase 3: apply CIs one at a time across all objects.
    for ci_idx in ci_order {
        let ci = get_ci(ci_idx);

        // CR 613.7: for static-ability CIs (idx < static_count), check whether the
        // source's in-progress materialized state still has static abilities.
        // If an earlier CI (e.g. Blood Moon) stripped them, this CI is suppressed.
        if ci_idx < static_count {
            let src = ci.source_id;
            let base_has_statics = state.objects.get(&src)
                .and_then(|o| state.catalog.get(&o.catalog_key))
                .map(|d| !d.static_ability_defs.is_empty())
                .unwrap_or(false);
            if base_has_statics {
                let suppressed = state.objects.get(&src)
                    .and_then(|o| o.materialized.as_ref())
                    .map(|d| d.static_ability_defs.is_empty())
                    .unwrap_or(false);
                if suppressed { continue; }
            }
        }

        let modifier = std::sync::Arc::clone(&ci.modifier);
        let filter = std::sync::Arc::clone(&ci.filter);

        for &id in &ids {
            let controller = match state.objects.get(&id) {
                Some(o) => o.controller,
                None => continue,
            };
            if !filter(id, controller, state) { continue; }
            // Extract → modify → reinsert to avoid borrow conflict with &SimState.
            let mut def = match state.objects.get_mut(&id).and_then(|o| o.materialized.take()) {
                Some(d) => d,
                None => continue,
            };
            (modifier)(&mut def, state);
            state.objects.get_mut(&id).unwrap().materialized = Some(def);
        }
    }
}

// ── Turn structure ────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum PhaseKind {
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
pub(crate) enum StepKind {
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

// ── Priority action types (CR 601.2 state machine) ──────────────────────────

/// Engine-provided legal action. Strategy picks one via `choose_action`.
#[derive(Clone)]
enum LegalAction {
    Pass,
    LandDrop(ObjId),
    /// Normal cast from hand, adventure back-face, or free cast from exile.
    CastSpell { card_id: ObjId, face: SpellFace },
    /// Activate a non-mana ability on a permanent.
    ActivateAbility { source_id: ObjId, ability_index: usize },
    /// Activate a mana ability with non-default timing (e.g. LED: instant-only).
    /// These are excluded from the CR 601.2g mana sub-loop but available during priority.
    ActivateManaAbility { source_id: ObjId, ability_index: usize },
}

/// Options presented to strategy at the Announce step (CR 601.2b).
struct AnnounceOptions {
    available_modes: Vec<usize>,
    available_alt_costs: Vec<AlternateCost>,
    has_x_cost: bool,
}

/// Strategy's choices at the Announce step.
struct AnnounceChoice {
    chosen_mode: usize,
    /// Index into `AnnounceOptions.available_alt_costs` (= `def.alternate_costs()`).
    /// `None` = pay mana cost normally.
    alt_cost_index: Option<usize>,
    chosen_x: u32,
}

/// Accumulated state during the cast sub-machine (CR 601.2a-i).
#[allow(dead_code)]
struct CastContext {
    card_id: ObjId,
    face: SpellFace,
    caster: PlayerId,
    chosen_mode: usize,
    alt_cost: Option<AlternateCost>,
    chosen_x: u32,
    chosen_targets: Vec<ObjId>,
    total_cost: Option<TotalCost>,
    costs_paid_ctx: CostsPaidCtx,
}

/// Computed total cost after modifications (CR 601.2f).
/// Phase 4 will use this for strategy-driven mana activation.
#[allow(dead_code)]
struct TotalCost {
    mana: ManaCost,
    additional: Vec<CostComponent>,
}

/// A mana ability the strategy can activate during ActivateMana.
#[derive(Clone)]
pub(crate) struct ManaAbilityOption {
    source_id: ObjId,
    ability_index: usize,
    produces: Vec<Color>,
    produces_count: usize,
}

/// Strategy's decision to activate a mana ability.
#[derive(Clone)]
pub(crate) struct ManaActivation {
    source_id: ObjId,
    ability_index: usize,
    /// Which color to produce (None = colorless/any, for generic mana needs).
    color_choice: Option<Color>,
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

// ── Mana ability primitives ───────────────────────────────────────────────────

/// Enumerate all mana abilities that `who` can currently activate.
/// Checks zone, tapped state, activatable flag, and condition predicate.
pub(crate) fn enumerate_mana_abilities(state: &SimState, who: PlayerId) -> Vec<ManaAbilityOption> {
    let mut options = Vec::new();
    // Battlefield permanents.
    for card in state.permanents_of(who) {
        let mas = state.def_of(card.id).map(|d| d.mana_abilities()).unwrap_or(&[]);
        let bf = match &card.bf { Some(bf) => bf, None => continue };
        for (idx, ma) in mas.iter().enumerate() {
            if !ma.activatable { continue; }
            if ma.timing != ActivationTiming::Default { continue; } // non-default timing excluded from mana sub-loop
            if !matches!(ma.source_zone, SourceZone::Battlefield) { continue; }
            if ma_requires_tap(ma) && bf.tapped { continue; }
            if ma.condition.as_ref().map_or(false, |cond| !cond(card.id, state)) { continue; }
            options.push(ManaAbilityOption {
                source_id: card.id,
                ability_index: idx,
                produces: ma.produces.clone(),
                produces_count: ma.produces_count,
            });
        }
    }
    // Hand-zone mana abilities (e.g. Simian Spirit Guide).
    for card in state.hand_of(who) {
        let mas = state.catalog.get(&card.catalog_key).map(|d| d.mana_abilities()).unwrap_or(&[]);
        for (idx, ma) in mas.iter().enumerate() {
            if !ma.activatable { continue; }
            if !matches!(ma.source_zone, SourceZone::Hand) { continue; }
            options.push(ManaAbilityOption {
                source_id: card.id,
                ability_index: idx,
                produces: ma.produces.clone(),
                produces_count: ma.produces_count,
            });
        }
    }
    options
}

/// Compute a tap plan for paying `cost` without mutating state.
/// Returns ManaActivations in order: specific colors first (B, U, W, R, G), then generic.
pub(crate) fn auto_tap_plan(state: &SimState, who: PlayerId, cost: &ManaCost) -> Vec<ManaActivation> {
    let mut plan = Vec::new();
    let mut used: HashSet<ObjId> = HashSet::new();

    // Helper: find a battlefield source producing `color` (or any if None).
    let find_bf = |state: &SimState, used: &HashSet<ObjId>, color: Option<Color>| -> Option<(ObjId, usize, usize)> {
        state.objects.iter().find_map(|(id, c)| {
            if used.contains(id) { return None; }
            if c.controller != who || c.zone != CardZone::Battlefield { return None; }
            let bf = c.bf.as_ref()?;
            let mas = state.def_of(*id).map(|d| d.mana_abilities()).unwrap_or(&[]);
            let (idx, ma) = mas.iter().enumerate().find(|(_, ma)| {
                ma.activatable
                    && matches!(ma.source_zone, SourceZone::Battlefield)
                    && (!ma_requires_tap(ma) || !bf.tapped)
                    && ma.condition.as_ref().map_or(true, |cond| cond(*id, state))
                    && color.map_or(true, |c| ma.produces.contains(&c))
            })?;
            Some((*id, idx, ma.produces_count))
        })
    };

    let find_hand = |state: &SimState, used: &HashSet<ObjId>, color: Option<Color>| -> Option<(ObjId, usize)> {
        state.hand_of(who).find_map(|c| {
            if used.contains(&c.id) { return None; }
            let mas = state.catalog.get(&c.catalog_key).map(|d| d.mana_abilities()).unwrap_or(&[]);
            let (idx, _) = mas.iter().enumerate().find(|(_, ma)| {
                ma.activatable
                    && matches!(ma.source_zone, SourceZone::Hand)
                    && color.map_or(true, |col| ma.produces.contains(&col))
            })?;
            Some((c.id, idx))
        })
    };

    // Specific colors first.
    for &(need, color) in &[
        (cost.b, Color::Black), (cost.u, Color::Blue), (cost.w, Color::White),
        (cost.r, Color::Red), (cost.g, Color::Green),
    ] {
        let mut remaining = need;
        while remaining > 0 {
            if let Some((id, idx, _)) = find_bf(state, &used, Some(color)) {
                plan.push(ManaActivation { source_id: id, ability_index: idx, color_choice: Some(color) });
                used.insert(id);
                remaining -= 1;
            } else if let Some((id, idx)) = find_hand(state, &used, Some(color)) {
                plan.push(ManaActivation { source_id: id, ability_index: idx, color_choice: Some(color) });
                used.insert(id);
                remaining -= 1;
            } else {
                break;
            }
        }
    }

    // Generic mana.
    let mut remaining_generic = cost.generic;
    while remaining_generic > 0 {
        if let Some((id, idx, count)) = find_bf(state, &used, None) {
            plan.push(ManaActivation { source_id: id, ability_index: idx, color_choice: None });
            used.insert(id);
            remaining_generic -= count as i32;
        } else if let Some((id, idx)) = find_hand(state, &used, None) {
            plan.push(ManaActivation { source_id: id, ability_index: idx, color_choice: None });
            used.insert(id);
            remaining_generic -= 1;
        } else {
            break;
        }
    }

    plan
}

/// Execute a single mana ability activation: pay its costs (tap/sac/exile) and run make_effect.
/// Returns a log entry describing what happened.
fn execute_mana_activation(
    state: &mut SimState,
    t: u8,
    who: PlayerId,
    act: &ManaActivation,
) -> String {
    // Look up the mana ability — try materialized def first, fall back to catalog.
    let ma = state.def_of(act.source_id)
        .and_then(|d| d.mana_abilities().get(act.ability_index).cloned())
        .or_else(|| {
            let key = &state.objects.get(&act.source_id)?.catalog_key;
            state.catalog.get(key.as_str())?.mana_abilities().get(act.ability_index).cloned()
        });
    let Some(ma) = ma else { return String::new(); };
    let name = state.objects.get(&act.source_id)
        .map(|c| c.catalog_key.clone()).unwrap_or_default();

    let is_hand = state.objects.get(&act.source_id)
        .map_or(false, |c| matches!(c.zone, CardZone::Hand { .. }));

    // Pay costs: hand-zone sources are exiled; battlefield sources are tapped/sacrificed.
    let action_label = if is_hand {
        state.set_card_zone(act.source_id, CardZone::Exile { on_adventure: false });
        "exile"
    } else if ma_requires_sac(&ma) {
        if let Some(card) = state.objects.get_mut(&act.source_id) {
            card.zone = CardZone::Graveyard;
            card.bf = None;
        }
        "sac"
    } else {
        if let Some(bf) = state.permanent_bf_mut(act.source_id) {
            bf.tapped = true;
        }
        "tap"
    };

    let color_label = act.color_choice
        .map(|c| match c {
            Color::White => "W", Color::Blue => "U", Color::Black => "B",
            Color::Red => "R", Color::Green => "G",
        }.to_string())
        .unwrap_or_else(|| ma.produces_count.to_string());

    ma.make_effect.clone()(who, act.color_choice).call(state, t, &[]);

    format!("{} {} → {}", action_label, name, color_label)
}

/// Strategy-driven mana ability loop (CR 601.2g).
///
/// Repeatedly enumerates available mana abilities and asks strategy to pick one.
/// Stops when strategy returns None or no abilities remain.
/// Returns a log of activations for display.
fn run_mana_loop(
    state: &mut SimState,
    t: u8,
    who: PlayerId,
    mana_cost: &ManaCost,
    strategy: &mut dyn Strategy,
) -> Vec<String> {
    let mut log = Vec::new();
    loop {
        let available = enumerate_mana_abilities(state, who);
        if available.is_empty() { break; }

        let activation = strategy.choose_mana_ability(state, who, &available, mana_cost);
        let Some(act) = activation else { break; };

        let entry = execute_mana_activation(state, t, who, &act);
        if !entry.is_empty() {
            log.push(entry);
        }
    }
    log
}

// ── Mana potential accumulation ───────────────────────────────────────────────

/// Accumulate one source's potential contribution into the pool.
/// A source (land or permanent) contributes at most 1 to `total` because a single
/// tap or sacrifice produces one mana. The per-color fields reflect which colors
/// that source *can* produce (union across all available abilities).
fn ma_requires_tap(ma: &ManaAbility) -> bool {
    ma.costs.iter().any(|c| matches!(c, CostComponent::TapSelf))
}

fn ma_requires_sac(ma: &ManaAbility) -> bool {
    ma.costs.iter().any(|c| matches!(c, CostComponent::SacSelf))
}

/// Find a hand card with a hand-zone mana ability that produces the requested color
/// (or any mana if `color` is None for generic). Returns (id, name, make_effect).
fn find_hand_mana_source(
    state: &SimState,
    who: PlayerId,
    color: Option<Color>,
) -> Option<(ObjId, String, ManaEffectFactory)> {
    let hand_ids: Vec<_> = state.hand_of(who).map(|c| (c.id, c.catalog_key.clone())).collect();
    for (id, key) in hand_ids {
        let mas = state.catalog.get(&key).map(|d| d.mana_abilities()).unwrap_or(&[]);
        for ma in mas {
            if !matches!(ma.source_zone, SourceZone::Hand) { continue; }
            let color_ok = match color {
                Some(c) => ma.produces.contains(&c),
                None => true, // any mana for generic
            };
            if color_ok {
                return Some((id, key, std::sync::Arc::clone(&ma.make_effect)));
            }
        }
    }
    None
}

fn accumulate_source_potential(abilities: &[ManaAbility], tapped: bool, p: &mut ManaPool) {
    let avail: Vec<_> = abilities.iter()
        .filter(|ma| !ma_requires_tap(ma) || !tapped)
        .collect();
    if avail.is_empty() { return; }
    let count = avail.iter().map(|ma| ma.produces_count).max().unwrap_or(1);
    p.total += count as i32;
    // Track which colors this source *can* produce (union across available abilities).
    let mut produced = [false; 5]; // W U B R G
    for ma in &avail {
        for color in &ma.produces {
            match color {
                Color::White => produced[0] = true,
                Color::Blue  => produced[1] = true,
                Color::Black => produced[2] = true,
                Color::Red   => produced[3] = true,
                Color::Green => produced[4] = true,
            }
        }
    }
    let [w, u, b, r, g] = produced.map(|x| x as i32);
    p.w += w; p.u += u; p.b += b; p.r += r; p.g += g;
}

// ── Simulation types ──────────────────────────────────────────────────────────


struct PlayerState {
    id: ObjId,
    deck_name: String,
    life: i32,
    /// Number of lands played this turn; reset to 0 each Untap step. Engine enforces the one-per-turn rule.
    lands_played_this_turn: u8,
    /// Number of non-land spells cast this turn; reset each Untap. Used for multi-spell probability.
    spells_cast_this_turn: u8,
    /// Mana produced but not yet spent; drains at end of each step/phase.
    pool: ManaPool,
    /// Number of cards drawn this turn; reset each Untap. Used for Bowmasters / Tamiyo triggers.
    draws_this_turn: u8,
}

impl PlayerState {
    fn new(deck: &str) -> Self {
        PlayerState {
            id: ObjId::UNSET,
            life: 20,
            deck_name: deck.to_string(),
            lands_played_this_turn: 0,
            spells_cast_this_turn: 0,
            pool: ManaPool::default(),
            draws_this_turn: 0,
        }
    }

}

pub struct SimState {
    turn: u8,
    /// The turn number currently being simulated. Set at the start of each do_turn call.
    pub(crate) current_turn: u8,
    on_play: bool,
    us: PlayerState,
    opp: PlayerState,
    log: Vec<String>,
    /// Set when the game ends by normal rules (a player's life reaches 0, etc.). Holds the winner.
    winner: Option<PlayerId>,
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
    /// Costs paid to cast the spell currently resolving (set by `resolve_top_of_stack`,
    /// cleared after resolution). Read by ETB replacement effects that need cast context.
    pub(crate) resolving_costs_ctx: CostsPaidCtx,
    /// Spell/ability stack. Items are resolved last-in-first-out. Populated by
    /// handle_priority_round; empty between priority rounds.
    pub(crate) stack: Vec<ObjId>,
    /// Activated and triggered abilities on the stack, keyed by their allocated ObjId.
    pub(crate) abilities: HashMap<ObjId, StackAbility>,
    /// All cards in all zones, keyed by stable ObjId. Added as part of staged object model migration.
    objects: HashMap<ObjId, GameObject>,
    /// ID allocator — starts at 1; 0 is reserved as ObjId::UNSET.
    next_id: u64,
    /// Order in which cards entered each player's graveyard (oldest first). Used for display.
    graveyard_order: Vec<ObjId>,
    /// All trigger instances for card objects in the simulation (pre-registered at init).
    /// `active` is false until the card enters the battlefield.
    pub(crate) trigger_instances: Vec<TriggerInstance>,
    /// All replacement instances for card objects in the simulation (pre-registered at init).
    /// `active` is false until the card enters the battlefield.
    pub(crate) replacement_instances: Vec<ReplacementInstance>,
    /// All prohibition instances (CR 614.17 "can't" effects). Checked before replacements.
    /// Replacements already applied in the current fire_event call chain (prevents loops).
    repl_applied: HashSet<(ObjId, usize)>,
    /// Recursion depth for fire_event (used to clear repl_applied at the top level).
    repl_depth: u32,
    /// All active continuous-effect instances. Checked at `recompute` time; expired entries
    /// are removed at Cleanup / start-of-turn as appropriate.
    pub(crate) continuous_instances: Vec<ContinuousInstance>,
    /// Latent spell mods (CR 611.2f): consumed during 601.2a when a qualifying spell is cast.
    /// Entries expire at EndOfTurn cleanup if not consumed.
    pub(crate) latent_spell_mods: Vec<LatentSpellMod>,
    /// Monotonic counter for assigning timestamps to CIs (CR 613.6).
    pub(crate) ci_timestamp_counter: u32,
    /// Owned card catalog — populated once at sim init, never mutated.
    /// All runtime card-definition reads go through `state.def_of(id)` (live objects)
    /// or `state.catalog` (bootstrap / non-battlefield lookups).
    pub(crate) catalog: HashMap<String, CardDef>,
    /// RNG source for all in-simulation randomness (random discard, fetch, etc.).
    /// Effects access this directly via `state.rng`. Strategy functions receive their
    /// own rng parameter so their randomness remains independently injectable for tests.
    pub(crate) rng: Box<dyn rand::RngCore + Send>,
    /// Strategy callback for effects that need a typed, non-object choice at resolution/ETB
    /// time (e.g. "choose a color", "name a creature type"). Effects clone this Arc out before
    /// calling it with `&*state` to avoid a double-borrow of `state`.
    /// Default: Blue / "Wizard" / "" — suitable for the Doomsday vs. Painter heuristic.
    /// Override in tests to force specific choices.
    pub(crate) resolve_choice:
        std::sync::Arc<dyn Fn(ObjId, &ChoiceRequest, &SimState) -> ChoiceResult + Send + Sync>,
    /// Strategy callback for surveil: given the ObjId of the card being surveiled,
    /// return `true` to put it in the graveyard, `false` to keep it on top.
    /// Effects clone this Arc out before calling it with `&*state` (same double-borrow idiom).
    /// Default: coin flip. Override in tests to force deterministic outcomes.
    pub(crate) surveil_choice:
        std::sync::Arc<dyn Fn(ObjId, &SimState) -> bool + Send + Sync>,
    /// Strategy callback for forced sacrifice: given the player who must sacrifice and the
    /// list of candidate ObjIds matching the effect's filter, returns the chosen id.
    /// Used by `eff_sacrifice` (e.g. Sheoldred's Edict, Liliana of the Veil).
    /// Effects clone this Arc before calling with `&*state` (same double-borrow idiom).
    /// Default: first candidate. Override in tests or per-strategy for smarter selection.
    pub(crate) sacrifice_choice:
        std::sync::Arc<dyn Fn(PlayerId, &[ObjId], &SimState) -> Option<ObjId> + Send + Sync>,
}

impl SimState {
    /// Return the post-CE materialized `CardDef` for the object with the given id, if any.
    /// Returns `None` for naked stack abilities (no catalog entry) or unknown ids.
    pub(crate) fn def_of(&self, id: ObjId) -> Option<&CardDef> {
        self.objects.get(&id)?.materialized.as_ref()
    }
}

impl SimState {
    fn new(us: PlayerState, opp: PlayerState) -> Self {
        let mut s = SimState {
            turn: 0,
            current_turn: 0,
            on_play: true,
            us,
            opp,
            log: Vec::new(),
            winner: None,
            success: false,
            current_ap: ObjId::UNSET,
            current_phase: None,
            combat_attackers: Vec::new(),
            combat_blocks: Vec::new(),
            pending_triggers: Vec::new(),
            resolving_costs_ctx: CostsPaidCtx::default(),
            stack: Vec::new(),
            abilities: HashMap::new(),
            objects: HashMap::new(),
            next_id: 0,
            graveyard_order: Vec::new(),
            trigger_instances: Vec::new(),
            replacement_instances: Vec::new(),
            repl_applied: HashSet::new(),
            repl_depth: 0,
            continuous_instances: Vec::new(),
            latent_spell_mods: Vec::new(),
            ci_timestamp_counter: 0,
            catalog: HashMap::new(),
            rng: Box::new(rand::rngs::StdRng::from_entropy()),
            resolve_choice: std::sync::Arc::new(|_, req, _| match req {
                ChoiceRequest::Color           => ChoiceResult::Color(Color::Blue),
                ChoiceRequest::CreatureType    => ChoiceResult::CreatureType("Wizard".to_string()),
                ChoiceRequest::CardName        => ChoiceResult::CardName(String::new()),
                ChoiceRequest::Mode(_)         => ChoiceResult::Mode(0),
                ChoiceRequest::WardPayment {..} => ChoiceResult::Bool(true),
                ChoiceRequest::MayPutOnBattlefield {..} => ChoiceResult::OptionalObject(None),
                ChoiceRequest::MayAttach => ChoiceResult::Bool(true),
            }),
            surveil_choice: std::sync::Arc::new(|_, _| rand::thread_rng().gen_bool(0.5)),
            sacrifice_choice: std::sync::Arc::new(|_, candidates, _| candidates.first().copied()),
        };
        s.us.id = s.alloc_id();
        s.opp.id = s.alloc_id();
        s
    }

    pub(crate) fn next_ci_timestamp(&mut self) -> u32 {
        let t = self.ci_timestamp_counter;
        self.ci_timestamp_counter += 1;
        t
    }

    fn alloc_id(&mut self) -> ObjId {
        self.next_id += 1;
        ObjId(self.next_id)
    }

    fn permanents_of(&self, who: PlayerId) -> impl Iterator<Item = &GameObject> {
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

    fn hand_of(&self, who: PlayerId) -> impl Iterator<Item = &GameObject> {
        self.objects.values().filter(move |c| c.owner == who && matches!(c.zone, CardZone::Hand { .. }))
    }

    fn graveyard_of(&self, who: PlayerId) -> impl Iterator<Item = &GameObject> {
        self.objects.values().filter(move |c| c.owner == who && c.zone == CardZone::Graveyard)
    }

    fn exile_of(&self, who: PlayerId) -> impl Iterator<Item = &GameObject> {
        self.objects.values().filter(move |c| c.owner == who && matches!(c.zone, CardZone::Exile { .. }))
    }

    /// Cards owned by `who` that are currently in exile with adventure status.
    fn on_adventure_of(&self, who: PlayerId) -> impl Iterator<Item = &GameObject> {
        self.objects.values().filter(move |c| c.owner == who && c.zone == (CardZone::Exile { on_adventure: true }))
    }

    fn library_of(&self, who: PlayerId) -> impl Iterator<Item = &GameObject> {
        self.objects.values().filter(move |c| c.owner == who && c.zone == CardZone::Library)
    }

    fn hand_size(&self, who: PlayerId) -> i32 {
        self.hand_of(who).count() as i32
    }

    fn library_size(&self, who: PlayerId) -> usize {
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



    /// True when the simulation should stop (game ended or objective reached).
    fn done(&self) -> bool {
        self.winner.is_some() || self.success
    }

    fn player(&self, who: PlayerId) -> &PlayerState {
        match who { PlayerId::Us => &self.us, PlayerId::Opp => &self.opp }
    }

    fn player_mut(&mut self, who: PlayerId) -> &mut PlayerState {
        match who { PlayerId::Us => &mut self.us, PlayerId::Opp => &mut self.opp }
    }

    /// Resolve a PlayerId to its stable ObjId.
    fn player_id(&self, who: PlayerId) -> ObjId {
        match who { PlayerId::Us => self.us.id, PlayerId::Opp => self.opp.id }
    }

    /// Resolve a player ObjId back to a PlayerId.
    fn who_pid(&self, id: ObjId) -> PlayerId {
        if id == self.us.id { PlayerId::Us } else { PlayerId::Opp }
    }

    /// Resolve a player ObjId back to the display string ("us"/"opp"). For logging only.
    fn who_str(&self, id: ObjId) -> &'static str {
        if id == self.us.id { "us" } else { "opp" }
    }

    /// Return the name of the permanent with the given id.
    fn permanent_name(&self, id: ObjId) -> Option<String> {
        self.objects.get(&id)
            .filter(|c| c.zone == CardZone::Battlefield)
            .map(|c| c.catalog_key.clone())
    }

    /// Mana accessible right now for `who`: pool + what untapped permanents can still produce.
    fn potential_mana(&self, who: PlayerId) -> ManaPool {
        let mut p = self.player(who).pool.clone();
        for card in self.permanents_of(who) {
            if let Some(bf) = &card.bf {
                let card_id = card.id;
                let mas = self.def_of(card_id).map(|d| d.mana_abilities()).unwrap_or(&[]);
                let bf_mas: Vec<_> = mas.iter()
                    .filter(|ma| matches!(ma.source_zone, SourceZone::Battlefield))
                    .filter(|ma| ma.condition.as_ref().map_or(true, |cond| cond(card_id, self)))
                    .cloned().collect();
                accumulate_source_potential(&bf_mas, bf.tapped, &mut p);
            }
        }
        // Hand-zone mana abilities (e.g. Simian Spirit Guide).
        for card in self.hand_of(who) {
            let mas = self.catalog.get(&card.catalog_key)
                .map(|d| d.mana_abilities()).unwrap_or(&[]);
            let hand_mas: Vec<_> = mas.iter()
                .filter(|ma| matches!(ma.source_zone, SourceZone::Hand))
                .cloned().collect();
            accumulate_source_potential(&hand_mas, false, &mut p);
        }
        p
    }

    /// Tap/sac permanents to produce mana for `who` for the given cost.
    /// Returns a log of activations.
    fn produce_mana(&mut self, who: PlayerId, cost: &ManaCost, _t: u8) -> Vec<String> {
        let mut log: Vec<String> = Vec::new();
        let color_specs: [(i32, Color); 5] = [
            (cost.b, Color::Black),
            (cost.u, Color::Blue),
            (cost.w, Color::White),
            (cost.r, Color::Red),
            (cost.g, Color::Green),
        ];

        for (need, color) in color_specs {
            let mut remaining = need;
            while remaining > 0 {
                // Find a battlefield permanent controlled by `who` with the right ability.
                let found = self.objects.iter()
                    .find(|(id, c)| {
                        c.controller == who && c.zone == CardZone::Battlefield &&
                        c.bf.as_ref().map_or(false, |bf| {
                            self.def_of(**id).map(|d| d.mana_abilities()).unwrap_or(&[])
                                .iter().any(|ma| (!ma_requires_tap(ma) || !bf.tapped) && ma.produces.contains(&color)
                                    && ma.condition.as_ref().map_or(true, |cond| cond(**id, self)))
                        })
                    })
                    .map(|(id, c)| {
                        let bf = c.bf.as_ref().unwrap();
                        let sac = self.def_of(*id).map(|d| d.mana_abilities()).unwrap_or(&[])
                            .iter()
                            .find(|ma| (!ma_requires_tap(ma) || !bf.tapped) && ma.produces.contains(&color)
                                && ma.condition.as_ref().map_or(true, |cond| cond(*id, self)))
                            .map(|ma| ma_requires_sac(ma))
                            .unwrap_or(false);
                        (*id, c.catalog_key.clone(), sac)
                    });
                if let Some((id, name, sac)) = found {
                    let color_ch = match color { Color::White=>'W', Color::Blue=>'U', Color::Black=>'B', Color::Red=>'R', Color::Green=>'G' };
                    let make = self.def_of(id)
                        .and_then(|d| d.mana_abilities().iter()
                            .find(|ma| {
                                let tapped = self.objects.get(&id)
                                    .and_then(|o| o.bf.as_ref()).map_or(false, |bf| bf.tapped);
                                (!ma_requires_tap(ma) || !tapped) && ma.produces.contains(&color)
                                    && ma.condition.as_ref().map_or(true, |cond| cond(id, self))
                            })
                            .map(|ma| std::sync::Arc::clone(&ma.make_effect)));
                    if sac {
                        log.push(format!("sac {} → {}", name, color_ch));
                        if let Some(card) = self.objects.get_mut(&id) {
                            card.zone = CardZone::Graveyard;
                            card.bf = None;
                        }
                    } else {
                        log.push(format!("tap {} → {}", name, color_ch));
                        if let Some(bf) = self.permanent_bf_mut(id) {
                            bf.tapped = true;
                        }
                    }
                    if let Some(f) = make { f(who, Some(color)).call(self, _t, &[]); }
                    remaining -= 1;
                    continue;
                }
                // Fallback: hand-zone mana source (e.g. Simian Spirit Guide).
                if let Some((id, name, make)) = find_hand_mana_source(self, who, Some(color)) {
                    let color_ch = match color { Color::White=>'W', Color::Blue=>'U', Color::Black=>'B', Color::Red=>'R', Color::Green=>'G' };
                    log.push(format!("exile {} → {}", name, color_ch));
                    self.set_card_zone(id, CardZone::Exile { on_adventure: false });
                    make(who, Some(color)).call(self, _t, &[]);
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
                        let mas = self.def_of(**id).map(|d| d.mana_abilities()).unwrap_or(&[]);
                        !mas.is_empty() && mas.iter().any(|ma| (!ma_requires_tap(ma) || !bf.tapped)
                            && ma.condition.as_ref().map_or(true, |cond| cond(**id, self)))
                    })
                })
                .map(|(id, c)| {
                    let bf = c.bf.as_ref().unwrap();
                    let sac = self.def_of(*id).map(|d| d.mana_abilities()).unwrap_or(&[])
                        .iter()
                        .find(|ma| (!ma_requires_tap(ma) || !bf.tapped)
                            && ma.condition.as_ref().map_or(true, |cond| cond(*id, self)))
                        .map(|ma| ma_requires_sac(ma))
                        .unwrap_or(false);
                    (*id, c.catalog_key.clone(), sac)
                });
            if let Some((id, name, sac)) = found {
                let (make, count) = {
                    let tapped = self.objects.get(&id)
                        .and_then(|o| o.bf.as_ref()).map_or(false, |bf| bf.tapped);
                    self.def_of(id)
                        .and_then(|d| d.mana_abilities().iter()
                            .find(|ma| (!ma_requires_tap(ma) || !tapped)
                                && ma.condition.as_ref().map_or(true, |cond| cond(id, self)))
                            .map(|ma| (std::sync::Arc::clone(&ma.make_effect), ma.produces_count)))
                        .unwrap_or_else(|| (std::sync::Arc::new(|_, _| Effect(std::sync::Arc::new(|_,_,_| {}))), 1))
                };
                if sac {
                    log.push(format!("sac {} → {}", name, count));
                    if let Some(card) = self.objects.get_mut(&id) {
                        card.zone = CardZone::Graveyard;
                        card.bf = None;
                    }
                } else {
                    log.push(format!("tap {} → {}", name, count));
                    if let Some(bf) = self.permanent_bf_mut(id) {
                        bf.tapped = true;
                    }
                }
                make(who, None).call(self, _t, &[]);
                remaining_generic -= count as i32;
                continue;
            }
            // Fallback: hand-zone mana source for generic mana.
            if let Some((id, name, make)) = find_hand_mana_source(self, who, None) {
                log.push(format!("exile {} → 1", name));
                self.set_card_zone(id, CardZone::Exile { on_adventure: false });
                make(who, None).call(self, _t, &[]);
                remaining_generic -= 1;
                continue;
            }
            break;
        }
        log
    }

    /// Produce mana and immediately spend it.
    fn pay_mana(&mut self, who: PlayerId, cost: &ManaCost, t: u8) -> Vec<String> {
        let log = self.produce_mana(who, cost, t);
        self.player_mut(who).pool.spend(cost);
        log
    }

    /// True if `who` can currently produce at least one black mana.
    fn has_black_mana(&self, who: PlayerId) -> bool {
        self.potential_mana(who).b > 0
    }

    fn life_of(&self, who: PlayerId) -> i32 {
        self.player(who).life
    }

    fn lose_life(&mut self, who: PlayerId, n: i32) {
        self.player_mut(who).life -= n;
    }

    fn gain_life(&mut self, who: PlayerId, n: i32) {
        self.player_mut(who).life += n;
    }

    fn log(&mut self, t: u8, who: PlayerId, msg: impl Into<String>) {
        let phase_str = match self.current_phase {
            Some(TurnPosition::Step(s))  => format!("{:?}", s),
            Some(TurnPosition::Phase(p)) => format!("{:?}", p),
            None                         => String::new(),
        };
        let ctx = if self.current_ap != ObjId::UNSET {
            format!("|{}/{}", self.who_str(self.current_ap), phase_str)
        } else {
            String::new()
        };
        self.log.push(format!("T{} [{}{}] {}", t, who, ctx, msg.into()));
    }

    /// Log each mana activation returned by pay_mana/produce_mana.
    fn log_mana_activations(&mut self, t: u8, who: PlayerId, activations: Vec<String>) {
        for entry in activations {
            self.log(t, who, format!("→ {}", entry));
        }
    }

    pub(crate) fn stack_item_owner(&self, id: ObjId) -> ObjId {
        if let Some(card) = self.objects.get(&id) {
            return self.player_id(card.owner);
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

    /// True iff `id` is a stack item (spell or ability) that a counter could target.
    /// All spells and all triggered/activated abilities on the stack are legal targets
    /// for appropriate counters — "can't be countered" is enforced at resolution, not targeting.
    pub(crate) fn stack_item_is_counterable(&self, id: ObjId) -> bool {
        (self.objects.contains_key(&id) && self.objects[&id].zone == CardZone::Stack)
            || self.abilities.contains_key(&id)
    }

    /// Iterate over all triggered/activated abilities currently on the stack.
    pub(crate) fn abilities_on_stack(&self) -> impl Iterator<Item = (ObjId, &StackAbility)> {
        self.abilities.iter().map(|(&id, ab)| (id, ab))
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
    fn fmt_player_zones(&self, f: &mut std::fmt::Formatter<'_>, who: PlayerId) -> std::fmt::Result {
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
    fn fmt_permanents(&self, f: &mut std::fmt::Formatter<'_>, who: PlayerId) -> std::fmt::Result {
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
            .filter(|c| c.bf.is_some() && !self.def_of(c.id).map(|d| d.mana_abilities()).unwrap_or(&[]).is_empty())
            .collect();
        let tapped_first = |a: &&GameObject, b: &&GameObject| {
            let a_tap = a.bf.as_ref().map_or(false, |bf| bf.tapped);
            let b_tap = b.bf.as_ref().map_or(false, |bf| bf.tapped);
            b_tap.cmp(&a_tap).then(a.catalog_key.cmp(&b.catalog_key))
        };
        lands.sort_by(tapped_first);

        let mut others: Vec<&GameObject> = self.permanents_of(who)
            .filter(|c| c.bf.is_none() || self.def_of(c.id).map(|d| d.mana_abilities()).unwrap_or(&[]).is_empty())
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
        self.fmt_permanents(f, PlayerId::Us)?;
        self.fmt_player_zones(f, PlayerId::Us)?;
        writeln!(f)?;

        let opp_label = format!("OPPONENT: {}", self.opp.deck_name);
        writeln!(f, "{}", sec(&opp_label))?;
        writeln!(f)?;
        writeln!(f, "  Life       : {}", self.opp.life)?;
        self.fmt_permanents(f, PlayerId::Opp)?;
        self.fmt_player_zones(f, PlayerId::Opp)?;

        Ok(())
    }
}
// ── Structured output ────────────────────────────────────────────────────────

#[derive(serde::Serialize)]
pub struct ScenarioResult {
    pub turn: u8,
    pub stage: String,
    pub on_play: bool,
    pub us: PlayerResult,
    pub opp: PlayerResult,
    pub log: Vec<String>,
}

#[derive(serde::Serialize)]
pub struct PlayerResult {
    pub deck_name: String,
    pub life: i32,
    pub lands: Vec<PermanentResult>,
    pub permanents: Vec<PermanentResult>,
    pub hand: Vec<CardResult>,
    pub hand_hidden: usize,
    pub graveyard: Vec<String>,
    pub exile: Vec<String>,
}

#[derive(serde::Serialize)]
pub struct PermanentResult {
    pub name: String,
    pub tapped: bool,
    pub counters: i32,
    pub loyalty: i32,
}

#[derive(serde::Serialize)]
pub struct CardResult {
    pub name: String,
}

impl SimState {
    pub fn to_result(&self) -> ScenarioResult {
        ScenarioResult {
            turn: self.turn,
            stage: stage_label(self.turn).to_string(),
            on_play: self.on_play,
            us: self.player_result(PlayerId::Us),
            opp: self.player_result(PlayerId::Opp),
            log: self.log.clone(),
        }
    }

    fn player_result(&self, who: PlayerId) -> PlayerResult {
        let is_land = |c: &&GameObject| -> bool {
            c.bf.is_some()
                && !self.def_of(c.id).map(|d| d.mana_abilities()).unwrap_or(&[]).is_empty()
        };

        let to_perm = |c: &GameObject| -> PermanentResult {
            let bf = c.bf.as_ref().unwrap();
            PermanentResult {
                name: c.catalog_key.clone(),
                tapped: bf.tapped,
                counters: bf.counters,
                loyalty: bf.loyalty,
            }
        };

        let lands: Vec<PermanentResult> = self.permanents_of(who)
            .filter(is_land)
            .map(|c| to_perm(c))
            .collect();

        let permanents: Vec<PermanentResult> = self.permanents_of(who)
            .filter(|c| !is_land(c))
            .map(|c| to_perm(c))
            .collect();

        let hand: Vec<CardResult> = self.hand_of(who)
            .filter(|c| matches!(c.zone, CardZone::Hand { known: true }))
            .map(|c| CardResult { name: c.catalog_key.clone() })
            .collect();

        let hand_hidden = self.hand_of(who)
            .filter(|c| matches!(c.zone, CardZone::Hand { known: false }))
            .count();

        let graveyard: Vec<String> = self.graveyard_order.iter()
            .filter_map(|id| self.objects.get(id))
            .filter(|c| c.owner == who)
            .map(|c| c.catalog_key.clone())
            .collect();

        let exile: Vec<String> = self.exile_of(who)
            .map(|c| if matches!(c.zone, CardZone::Exile { on_adventure: true }) {
                format!("{} (adv)", c.catalog_key)
            } else {
                c.catalog_key.clone()
            })
            .collect();

        PlayerResult {
            deck_name: self.player(who).deck_name.clone(),
            life: self.player(who).life,
            lands,
            permanents,
            hand,
            hand_hidden,
            graveyard,
            exile,
        }
    }
}

// ── Turn simulation ───────────────────────────────────────────────────────────


/// Play a specific, pre-chosen land from hand (moves it to Battlefield).
/// Fetches stay in play to be cracked later in the ability pass.
fn sim_play_land(
    state: &mut SimState,
    t: u8,
    who: PlayerId,
    card_id: ObjId,
) {
    if state.player(who).lands_played_this_turn >= 1 { return; }
    let land_name = match state.objects.get(&card_id) {
        Some(c) if matches!(c.zone, CardZone::Hand { .. }) => c.catalog_key.clone(),
        _ => return,
    };
    state.log(t, who, format!("Play {} [hand: {}]", land_name, state.hand_size(who)));
    change_zone(card_id, ZoneId::Battlefield, state, t, who);
    fire_event(GameEvent::LandPlayed { id: card_id, controller: who }, state, t, who);
}


/// Discard down to 7 at end of turn.
fn sim_discard_to_limit(state: &mut SimState, t: u8, who: PlayerId) {
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
// collect_legal_actions, choose_land_name
// are defined in strategy.rs / predicates.rs

// choose_permanent_target is defined in predicates.rs

pub(crate) fn card_zone_to_id(zone: &CardZone) -> ZoneId {
    match zone {
        CardZone::Library        => ZoneId::Library,
        CardZone::Hand { .. }    => ZoneId::Hand,
        CardZone::Stack          => ZoneId::Stack,
        CardZone::Battlefield    => ZoneId::Battlefield,
        CardZone::Graveyard      => ZoneId::Graveyard,
        CardZone::Exile { .. }   => ZoneId::Exile,
    }
}


/// The central elemental event pipeline.
///
/// Stage order per the Comprehensive Rules:
///   1. Prohibition check (CR 614.17 — "can't" effects; if any match, event is suppressed)
///   2. Replacement check (CR 614 — first applicable replacement fires instead)
///   3. Do effect (state mutation for this event type)
///   4. Log
///   5. Trigger dispatch (CR 603 — collect triggered abilities)
///   6. Recompute CE materialization (at top-level depth only)
/// Returns `true` iff the event was suppressed by a "can't" prohibition (CR 614.17).
pub(crate) fn fire_event(
    event: GameEvent,
    state: &mut SimState,
    t: u8,
    actor: PlayerId,
) -> bool {
    state.repl_depth += 1;
    if state.repl_depth == 1 {
        state.repl_applied.clear();
    }

    // Stage 1: Prohibition check (CR 614.17).
    // "Can't" effects are not replacements — they suppress the event outright and
    // take precedence over replacements. Walk all objects and check prohibition_defs
    // from catalog, filtered by active_when predicate.
    let prohibited = state.objects.iter().any(|(id, obj)| {
        state.catalog.get(&obj.catalog_key).map_or(false, |card_def| {
            card_def.prohibition_defs.iter().any(|pdef| {
                (pdef.active_when)(*id, state) && (pdef.check)(&event, *id, obj.controller, state)
            })
        })
    });
    if prohibited {
        state.log(t, actor, "→ event prohibited (\"can't\" effect)".to_string());
        state.repl_depth -= 1;
        return true;
    }

    // Stage 2: Replacement check.
    // Part A: Card-bound replacements — walk all objects, check replacement_defs from catalog.
    // Part B: Ephemeral replacements — check replacement_instances (runtime-created, e.g. FoN exile).
    // First active, non-applied replacement that matches wins (CR 614.5 loop prevention).
    let repl_match = {
        let mut found = None;
        // Part A: card-bound replacements from catalog.
        for (id, obj) in &state.objects {
            let card_def = match state.catalog.get(&obj.catalog_key) {
                Some(d) => d,
                None => continue,
            };
            for (def_idx, rdef) in card_def.replacement_defs.iter().enumerate() {
                let key = (*id, def_idx);
                if !(rdef.active_when)(*id, state) { continue; }
                if state.repl_applied.contains(&key) { continue; }
                if let Some(targets) = (rdef.check)(&event, *id, obj.controller, state) {
                    let effect = (rdef.make_effect)(*id, obj.controller);
                    found = Some((key, targets, effect));
                    break;
                }
            }
            if found.is_some() { break; }
        }
        // Part B: ephemeral replacement instances (runtime-created by abilities).
        if found.is_none() {
            for (idx, inst) in state.replacement_instances.iter().enumerate() {
                let key = (inst.source_id, idx);
                if state.repl_applied.contains(&key) { continue; }
                if let Some(targets) = (inst.check)(&event, inst.source_id, inst.controller, state) {
                    found = Some((key, targets, inst.effect.clone()));
                    break;
                }
            }
        }
        found
    };

    if let Some((repl_key, targets, effect)) = repl_match {
        state.repl_applied.insert(repl_key);
        effect.call(state, t, &targets);
        state.repl_depth -= 1;
        return false; // original effect suppressed by replacement (not a prohibition)
    }

    // Stage 3: Apply state mutation.
    do_effect(&event, state);

    // Stage 4: Log.
    log_event(&event, state, t, actor);

    // Stage 5: Trigger dispatch.
    let (triggers, one_shot_fired) = fire_triggers(&event, state);
    state.pending_triggers.extend(triggers);
    // Remove OneShot trigger instances that just fired (reverse order to keep indices valid).
    for &i in one_shot_fired.iter().rev() {
        state.trigger_instances.remove(i);
    }


    state.repl_depth -= 1;
    if state.repl_depth == 0 {
        // Rebuild the inlined materialized snapshot after every top-level tick so that
        // strategy, display, and combat damage always see a current, CE-adjusted view.
        recompute(state);
    }
    false
}

fn do_effect(event: &GameEvent, state: &mut SimState) {
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

            // Detach any equipment that was attached to the departing permanent (CR 301.5c).
            if from == ZoneId::Battlefield {
                for obj in state.objects.values_mut() {
                    if let Some(ref mut bf) = obj.bf {
                        if bf.attached_to == Some(id) {
                            bf.attached_to = None;
                        }
                    }
                }
            }

        }
        GameEvent::Draw { controller, .. } => {
            let controller = *controller;
            let top_id = state.library_of(controller).next().map(|c| c.id);
            if let Some(card_id) = top_id {
                state.set_card_zone(card_id, CardZone::Hand { known: false });
            }
        }
        GameEvent::ManaProduced { who, ref spec } => {
            let mc = parse_mana_cost(spec);
            let pool = &mut state.player_mut(*who).pool;
            pool.w += mc.w; pool.u += mc.u; pool.b += mc.b;
            pool.r += mc.r; pool.g += mc.g; pool.c += mc.c;
            pool.total += mc.mana_value();
        }
        // EnteredStep, EnteredPhase, CreatureAttacked — notification events, no state mutation
        _ => {}
    }
}

fn log_event(event: &GameEvent, state: &mut SimState, t: u8, actor: PlayerId) {
    match event {
        GameEvent::ZoneChange { id, from, to, controller, .. } => {
            let card = state.objects.get(id).map(|o| o.catalog_key.as_str()).unwrap_or("?");
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
            let controller = *controller;
            let hand = state.hand_size(controller);
            if *is_natural {
                state.log(t, controller, format!("Draw [hand: {}]", hand));
            } else {
                state.log(t, controller, format!("draw ({}) [hand: {}]", draw_index, hand));
            }
        }
        GameEvent::ManaProduced { who, ref spec } => {
            state.log(t, *who, format!("→ add {} to pool", spec));
        }
        _ => {}
    }
}

/// Move a game object from its current zone to `to`.
/// Works for any zone transition. No-ops silently if the id is not found.
/// Fires the event pipeline (replacements → state mutation → triggers → log).
pub(crate) fn change_zone(
    id: ObjId,
    to: ZoneId,
    state: &mut SimState,
    t: u8,
    actor: PlayerId,
) {
    let (catalog_key, controller, from) = {
        let card = match state.objects.get(&id) {
            Some(c) => c,
            None => return,
        };
        (card.catalog_key.clone(), card.controller, card_zone_to_id(&card.zone))
    };
    // LTB: remove ephemeral CIs tied to this source.
    if from == ZoneId::Battlefield {
        state.continuous_instances.retain(|ci| {
            !(ci.source_id == id && ci.expiry == Expiry::WhileSourceOnBattlefield)
        });
    }
    // ETB: assign stable CI timestamp for static-ability CIs generated by recompute.
    if to == ZoneId::Battlefield {
        let ts = state.next_ci_timestamp();
        if let Some(obj) = state.objects.get_mut(&id) {
            obj.ci_timestamp = ts;
        }
    }
    // Prohibitions are derived at fire time via active_when predicates.
    fire_event(
        GameEvent::ZoneChange { id, actor, from, to, controller },
        state, t, actor,
    );
}

// matches_search_filter is defined in predicates.rs

/// Draw one card for `who` through the event pipeline. Increments draws_this_turn, fires a Draw
/// event (which handles the state mutation, logging, and trigger dispatch).
fn sim_draw(state: &mut SimState, who: PlayerId, t: u8, is_natural: bool) {
    state.player_mut(who).draws_this_turn += 1;
    let draw_index = state.player(who).draws_this_turn;
    let ev = GameEvent::Draw { controller: who, draw_index, is_natural };
    fire_event(ev, state, t, who);
}

// ── Unified cost check / pay ──────────────────────────────────────────────────

/// Returns true iff every component of `costs` can be paid by `who`.
/// `source_id` is used to resolve `SacSelf` and `DiscardSelf`.
fn can_pay_single_cost(
    cost: &CostComponent,
    state: &SimState,
    who: PlayerId,
    source_id: ObjId,
    source_untapped: bool,
    chosen_x: u32,
) -> bool {
    match cost {
        CostComponent::Mana(mc) => state.potential_mana(who).can_pay(mc),
        CostComponent::TapSelf => source_untapped,
        CostComponent::SacSelf => state.permanent_bf(source_id).is_some(),
        CostComponent::DiscardSelf => state.hand_of(who).any(|c| c.id == source_id),
        CostComponent::ExileSelf => state.hand_of(who).any(|c| c.id == source_id),
        CostComponent::DiscardHand => true, // discarding 0 cards is valid
        CostComponent::Life(n) => state.player(who).life > *n,
        CostComponent::XLife => state.player(who).life >= chosen_x as i32,
        CostComponent::XMana => state.potential_mana(who).total >= chosen_x as i32,
        CostComponent::SacPermanent(pred) => {
            state.permanents_of(who).any(|c| c.bf.is_some() && pred(c.id, state))
        }
        CostComponent::DiscardCard(pred) | CostComponent::ExileFromHand(pred) => {
            state.hand_of(who).any(|c| c.id != source_id && pred(c.id, state))
        }
        CostComponent::ReturnFromBattlefield(pred) => {
            state.permanents_of(who).any(|c| c.bf.is_some() && pred(c.id, state))
        }
        CostComponent::TapPermanent(pred) => {
            state.permanents_of(who).any(|c| {
                c.bf.as_ref().map_or(false, |bf| !bf.tapped) && pred(c.id, state)
            })
        }
        CostComponent::LoyaltyAdjust(n) => {
            state.permanent_bf(source_id).map_or(false, |bf| {
                !bf.pw_activated_this_turn && (*n >= 0 || bf.loyalty + n > 0)
            })
        }
        CostComponent::CostAnd(parts) => {
            can_pay_costs(parts, state, who, source_id, source_untapped, chosen_x)
        }
        CostComponent::CostOr(parts) => {
            parts.iter().any(|branch| can_pay_single_cost(branch, state, who, source_id, source_untapped, chosen_x))
        }
        CostComponent::Replicate(_) => true, // optional; 0 payments always valid
    }
}

fn can_pay_costs(
    costs: &[CostComponent],
    state: &SimState,
    who: PlayerId,
    source_id: ObjId,
    source_untapped: bool,
    chosen_x: u32,
) -> bool {
    costs.iter().all(|cost| can_pay_single_cost(cost, state, who, source_id, source_untapped, chosen_x))
}

/// Executes a single cost component, mutating state.
/// Caller must have checked `can_pay_costs` first.
fn pay_single_cost(
    cost: &CostComponent,
    state: &mut SimState,
    t: u8,
    who: PlayerId,
    source_id: ObjId,
    ctx: &mut CostsPaidCtx,
    chosen_x: u32,
) {
    match cost {
        CostComponent::Mana(mc) => {
            let mana_log = state.pay_mana(who, mc, t);
            state.log_mana_activations(t, who, mana_log);
        }
        CostComponent::TapSelf => {
            if let Some(bf) = state.permanent_bf_mut(source_id) {
                bf.tapped = true;
            }
        }
        CostComponent::SacSelf => {
            state.set_card_zone(source_id, CardZone::Graveyard);
        }
        CostComponent::DiscardSelf => {
            state.set_card_zone(source_id, CardZone::Graveyard);
        }
        CostComponent::ExileSelf => {
            state.set_card_zone(source_id, CardZone::Exile { on_adventure: false });
        }
        CostComponent::DiscardHand => {
            let hand_ids: Vec<ObjId> = state.hand_of(who).map(|c| c.id).collect();
            for id in hand_ids {
                state.set_card_zone(id, CardZone::Graveyard);
                ctx.objects_moved.push(id);
            }
        }
        CostComponent::Life(n) => {
            state.lose_life(who, *n);
        }
        CostComponent::SacPermanent(pred) => {
            // Prefer permanents without mana abilities; fall back to any match.
            let target = state.permanents_of(who)
                .filter(|c| c.bf.is_some() && pred(c.id, state))
                .min_by_key(|c| {
                    let has_mana = state.def_of(c.id)
                        .map_or(false, |d| !d.mana_abilities().is_empty());
                    has_mana as u8
                })
                .map(|c| c.id);
            if let Some(id) = target {
                state.set_card_zone(id, CardZone::Graveyard);
                ctx.objects_moved.push(id);
            }
        }
        CostComponent::DiscardCard(pred) => {
            let target = state.hand_of(who)
                .find(|c| c.id != source_id && pred(c.id, state))
                .map(|c| c.id);
            if let Some(id) = target {
                state.set_card_zone(id, CardZone::Graveyard);
                ctx.objects_moved.push(id);
            }
        }
        CostComponent::ExileFromHand(pred) => {
            let target = state.hand_of(who)
                .find(|c| c.id != source_id && pred(c.id, state))
                .map(|c| c.id);
            if let Some(id) = target {
                state.set_card_zone(id, CardZone::Exile { on_adventure: false });
                ctx.objects_moved.push(id);
            }
        }
        CostComponent::ReturnFromBattlefield(pred) => {
            let target = state.permanents_of(who)
                .find(|c| c.bf.is_some() && pred(c.id, state))
                .map(|c| (c.id, c.catalog_key.clone(), c.bf.as_ref().and_then(|bf| bf.attack_target)));
            if let Some((id, name, attack_target)) = target {
                if let Some(card) = state.objects.get_mut(&id) {
                    card.zone = CardZone::Hand { known: false };
                    card.bf = None;
                }
                state.combat_attackers.retain(|&a| a != id);
                state.combat_blocks.retain(|(a, _)| *a != id);
                state.log(t, who, format!("→ return {} to hand (cost)", name));
                ctx.objects_moved.push(id);
                ctx.returned_attack_targets.push(attack_target);
            }
        }
        CostComponent::TapPermanent(pred) => {
            let target = state.permanents_of(who)
                .find(|c| c.bf.as_ref().map_or(false, |bf| !bf.tapped) && pred(c.id, state))
                .map(|c| c.id);
            if let Some(id) = target {
                if let Some(bf) = state.permanent_bf_mut(id) {
                    bf.tapped = true;
                }
            }
        }
        CostComponent::LoyaltyAdjust(n) => {
            if let Some(bf) = state.permanent_bf_mut(source_id) {
                bf.loyalty += n;
                bf.pw_activated_this_turn = true;
            }
        }
        CostComponent::CostAnd(parts) => {
            for part in parts {
                pay_single_cost(part, state, t, who, source_id, ctx, chosen_x);
            }
        }
        CostComponent::CostOr(parts) => {
            // Strategy picks the first payable branch (greedy).
            let source_untapped = state.permanent_bf(source_id).map_or(true, |bf| !bf.tapped);
            if let Some(branch) = parts.iter().find(|branch| {
                can_pay_single_cost(branch, state, who, source_id, source_untapped, chosen_x)
            }) {
                pay_single_cost(branch, state, t, who, source_id, ctx, chosen_x);
            }
        }
        CostComponent::XLife => {
            state.player_mut(who).life -= chosen_x as i32;
            ctx.chosen_x = chosen_x;
        }
        CostComponent::XMana => {
            let mc = ManaCost { generic: chosen_x as i32, ..ManaCost::default() };
            let mana_log = state.pay_mana(who, &mc, t);
            state.log_mana_activations(t, who, mana_log);
            ctx.chosen_x = chosen_x;
        }
        CostComponent::Replicate(_) => {
            // No-op: replicate payment and copy-pushing are handled in cast_spell
            // after the spell has been placed on the stack (we need stack context).
        }
    }
}

/// Executes every component of `costs`, mutating state.
/// Caller must have checked `can_pay_costs` first.
/// For `SacPermanent` and `ReturnFromBattlefield`, prefers permanents without mana
/// abilities to preserve mana sources where possible.
/// Returns a `CostsPaidCtx` recording which objects moved during payment.
fn pay_costs(
    costs: &[CostComponent],
    state: &mut SimState,
    t: u8,
    who: PlayerId,
    source_id: ObjId,
    chosen_x: u32,
) -> CostsPaidCtx {
    let mut ctx = CostsPaidCtx::default();
    for cost in costs {
        pay_single_cost(cost, state, t, who, source_id, &mut ctx, chosen_x);
    }
    ctx
}

/// Log the ability activation and pay its costs via the unified `pay_costs` function.
/// Returns a `CostsPaidCtx` with the objects moved during payment.
/// For hand-sourced abilities (ninjutsu, etc.) the source card is moved to Stack zone.
fn pay_ability_cost(
    state: &mut SimState,
    t: u8,
    who: PlayerId,
    source_id: ObjId,
    ability: &AbilityDef,
    is_hand_source: bool,
) -> CostsPaidCtx {
    let source_name = state.permanent_name(source_id)
        .or_else(|| state.hand_of(who).find(|c| c.id == source_id).map(|c| c.catalog_key.clone()))
        .unwrap_or_default();
    state.log(t, who, format!("Activate {} ability", source_name));

    // Hand-sourced abilities (ninjutsu): move source from hand to Stack zone before paying costs.
    if is_hand_source {
        if let Some(card) = state.objects.get_mut(&source_id) {
            card.zone = CardZone::Stack;
        }
    }

    let ctx = pay_costs(&ability.costs, state, t, who, source_id, 0);

    // Log loyalty adjustment.
    if let Some(n) = ability.loyalty_delta() {
        if let Some(new_loyalty) = state.permanent_bf(source_id).map(|bf| bf.loyalty) {
            state.log(t, who, format!("→ {} loyalty {} → {}", source_name,
                if n >= 0 { format!("+{}", n) } else { n.to_string() },
                new_loyalty));
        }
    }

    ctx
}

/// Build a human-readable description of a cost list for log messages.
fn describe_costs(costs: &[CostComponent]) -> Vec<String> {
    costs.iter().map(|c| match c {
        CostComponent::Mana(mc) => mc.display(),
        CostComponent::TapSelf => "tap".to_string(),
        CostComponent::SacSelf => "sac self".to_string(),
        CostComponent::DiscardSelf => "discard self".to_string(),
        CostComponent::ExileSelf => "exile self".to_string(),
        CostComponent::DiscardHand => "discard hand".to_string(),
        CostComponent::Life(n) => format!("-{} life", n),
        CostComponent::SacPermanent(_) => "sac permanent".to_string(),
        CostComponent::DiscardCard(_) => "discard card".to_string(),
        CostComponent::ExileFromHand(_) => "exile blue".to_string(),
        CostComponent::ReturnFromBattlefield(_) => "bounce land".to_string(),
        CostComponent::TapPermanent(_) => "tap permanent".to_string(),
        CostComponent::LoyaltyAdjust(n) => format!("loyalty {}", n),
        CostComponent::CostAnd(parts) => describe_costs(parts).join(", "),
        CostComponent::CostOr(parts) => {
            let branches: Vec<String> = parts.iter()
                .map(|b| describe_costs(std::slice::from_ref(b)).join(", "))
                .collect();
            format!("({})", branches.join(" OR "))
        }
        CostComponent::Replicate(mc) => format!("replicate {}", mc.display()),
        CostComponent::XLife => "pay X life".to_string(),
        CostComponent::XMana => "pay X mana".to_string(),
    }).collect()
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
/// CR 611.2f: consume the first matching latent spell mod for this caster.
/// Called during 601.2a after the spell is on the stack. If a match is found,
/// the mod's factory produces a CI that is pushed to continuous_instances and
/// the LatentSpellMod is removed (consumed).
fn consume_latent_spell_mod(state: &mut SimState, caster: PlayerId, spell_id: ObjId) {
    let pos = state.latent_spell_mods.iter().position(|lsm| {
        lsm.controller == caster && (lsm.predicate)(spell_id, caster, state)
    });
    if let Some(i) = pos {
        let lsm = state.latent_spell_mods.remove(i);
        let ci = (lsm.make_ci)(spell_id, caster);
        state.continuous_instances.push(ci);
    }
}

/// Cast a spell identified by `card_id`, using the specified `face` (Main or Adventure).
/// Pays cost, builds effect, sets SpellState on the card object.
/// Returns `None` if the cast fails (cost unpayable, card missing).
fn cast_spell(
    state: &mut SimState,
    t: u8,
    who: PlayerId,
    card_id: ObjId,
    face: SpellFace,
    preferred_cost: Option<&AlternateCost>,
    announced_alt_index: Option<usize>,
    chosen_targets: &[ObjId],
    chosen_x: u32,
    chosen_mode: usize,
) -> Option<ObjId> {
    let name = state.objects.get(&card_id)?.catalog_key.clone();
    // Prefer the post-CE materialized def (current in normal game flow where recompute
    // runs before every priority window). Fall back to state.catalog for tests that call
    // cast_spell directly without a preceding recompute.
    let def = state.def_of(card_id)
        .cloned()
        .or_else(|| state.catalog.get(name.as_str()).cloned())?;

    if face == SpellFace::Back {
        let adv = def.adventure()?.clone();
        let is_sorcery = adv.is_sorcery();
        if is_sorcery && !state.stack.is_empty() {
            eprintln!("[priority] BUG: split-back sorcery {} on non-empty stack, treating as Pass", adv.name);
            return None;
        }
        // Casting legality is pre-checked via CardDef.castable (set by CEs in recompute).
        // Strategy only offers castable cards, so no prohibition gate needed here.
        let cost = parse_mana_cost(adv.mana_cost());
        let mana_log = state.pay_mana(who, &cost, t);
        state.log_mana_activations(t, who, mana_log);
        let (_adv_spec, adv_eff) = build_spell_effect(&adv, who, card_id, 0, 0);
        let adv_targets = chosen_targets.to_vec();
        state.log(t, who, format!("Cast {} ({}, {}) [hand: {}]", adv.name, adv.mana_cost(), name, state.hand_size(who)));
        if let Some(card) = state.objects.get_mut(&card_id) {
            card.zone = CardZone::Stack;
            card.spell = Some(SpellState {
                effect: Some(adv_eff),
                chosen_targets: adv_targets,
                is_back_face: true,
                costs_paid_ctx: CostsPaidCtx::default(),
            });
        }
        consume_latent_spell_mod(state, who, card_id);
        let back_mana_spent = mana_value(adv.mana_cost()) > 0;
        fire_event(GameEvent::SpellCast { caster: who, card_id, mana_spent: back_mana_spent }, state, t, who);
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

    // Select cost. Track the index into def.alternate_costs() for evoke/similar triggers.
    let (alt_cost, alt_cost_idx): (Option<AlternateCost>, Option<usize>) = if let Some(pc) = preferred_cost {
        // Strategy announced an alt cost — use the announced index.
        (Some(pc.clone()), announced_alt_index)
    } else if !mana_is_usable {
        let found = def.alternate_costs()
            .iter()
            .enumerate()
            .find(|(_, c)| {
                state.hand_size(who) >= c.hand_min
                    && can_pay_costs(&c.costs, state, who, card_id, false, 0)
                    && c.condition.as_ref().map_or(true, |f| f(who, state))
            });
        match found {
            Some((i, c)) => (Some(c.clone()), Some(i)),
            None => (None, None),
        }
    } else {
        (None, None)
    };

    if alt_cost.is_none() && !mana_is_usable {
        return None;
    }
    if !can_pay_costs(&def.additional_costs, state, who, card_id, false, chosen_x) {
        return None;
    }

    // Casting legality is pre-checked via CardDef.castable (set by CEs in recompute).
    // Strategy only offers castable cards, so no prohibition gate needed here.

    // Move to Stack zone.
    if let Some(card) = state.objects.get_mut(&card_id) {
        card.zone = CardZone::Stack;
    }

    // Pay cost and build a log label.
    let (cast_label, mut costs_ctx) = if let Some(ref cost) = alt_cost {
        let ctx = pay_costs(&cost.costs, state, t, who, card_id, 0);
        (describe_costs(&cost.costs).join(", "), ctx)
    } else {
        let mana_log = state.pay_mana(who, &cost, t);
        state.log_mana_activations(t, who, mana_log);
        (def.mana_cost().to_string(), CostsPaidCtx::default())
    };

    // Pay additional costs (CR 118.9d: apply regardless of which cost path was taken).
    // `chosen_x` is passed so XLife additional costs pay the strategy-chosen amount.
    if !def.additional_costs.is_empty() {
        let add_ctx = pay_costs(&def.additional_costs, state, t, who, card_id, chosen_x);
        costs_ctx.objects_moved.extend(add_ctx.objects_moved);
        costs_ctx.returned_attack_targets.extend(add_ctx.returned_attack_targets);
        costs_ctx.chosen_x = add_ctx.chosen_x;
    }
    costs_ctx.chosen_mode = chosen_mode;
    costs_ctx.alt_cost_index = alt_cost_idx;

    // Exile delve cards from graveyard (cost payment); record in costs_ctx.
    let to_exile_names: Vec<String> = to_exile_ids.iter().map(|(_, n)| n.clone()).collect();
    for (exile_id, _) in &to_exile_ids {
        change_zone(*exile_id, ZoneId::Exile, state, t, who);
        costs_ctx.objects_moved.push(*exile_id);
    }

    let delve_label = if to_exile_names.is_empty() {
        String::new()
    } else {
        format!(", delve: {}", to_exile_names.join(", "))
    };
    state.log(t, who, format!("Cast {} ({}{}) [hand: {}]", name, cast_label, delve_label, state.hand_size(who)));

    let (_spell_target_spec, spell_eff) = build_spell_effect(&def, who, card_id, chosen_x, chosen_mode);
    let spell_chosen_targets = chosen_targets.to_vec();

    if let Some(card) = state.objects.get_mut(&card_id) {
        card.spell = Some(SpellState {
            effect: Some(spell_eff),
            chosen_targets: spell_chosen_targets,
            is_back_face: false,
            costs_paid_ctx: costs_ctx,
        });
    }

    // Replicate (CR 702.58): for each time the replicate cost was paid, push a copy.
    let rep_cost = def.additional_costs.iter().find_map(|c| {
        if let CostComponent::Replicate(mc) = c { Some(mc.clone()) } else { None }
    });
    if let Some(rep_mc) = rep_cost {
        // Count other valid targets for copies (different from the original target).
        let original_targets = chosen_targets.to_vec();
        let extra_targets: Vec<ObjId> = legal_targets(def.target_spec(), who, card_id, state)
            .into_iter()
            .filter(|id| !original_targets.contains(id))
            .collect();
        let mut rep_count = 0u32;
        for &tgt in &extra_targets {
            if !state.potential_mana(who).can_pay(&rep_mc) { break; }
            let mana_log = state.pay_mana(who, &rep_mc, t);
            state.log_mana_activations(t, who, mana_log);
            let (_, copy_eff) = build_spell_effect(&def, who, card_id, chosen_x, chosen_mode);
            let copy_id = state.alloc_id();
            state.abilities.insert(copy_id, StackAbility {
                id: copy_id,
                source_name: format!("{} (replicate)", name),
                owner: state.player_id(who),
                effect: copy_eff,
                chosen_targets: vec![tgt],
                costs_paid_ctx: CostsPaidCtx::default(),
                is_triggered: false,
                counterable: true,
                choice_spec: None,
            });
            state.stack.push(copy_id);
            rep_count += 1;
            let tgt_name = state.stack_item_display_name(tgt).to_string();
            state.log(t, who, format!("Replicate → {} (targeting {})", name, tgt_name));
        }
        if rep_count > 0 {
            if let Some(card_obj) = state.objects.get_mut(&card_id) {
                if let Some(spell) = card_obj.spell.as_mut() {
                    spell.costs_paid_ctx.replicate_count = rep_count;
                }
            }
        }
    }

    // Latent spell mods (CR 611.2f): consume the first matching mod for this caster.
    consume_latent_spell_mod(state, who, card_id);

    // SpellCast fires after all costs paid and spell is on the stack.
    let mana_spent = match &alt_cost {
        None     => mana_value(def.mana_cost()) > 0,
        Some(ac) => ac.costs.iter().any(|c| matches!(c, CostComponent::Mana(_))),
    };
    fire_event(GameEvent::SpellCast { caster: who, card_id, mana_spent }, state, t, who);


    Some(card_id)
}






// ── Keyword helpers ───────────────────────────────────────────────────────────

/// Return true if the permanent with `id` has the given keyword in the materialized (CE-applied) view.
/// Always reads from materialized state so CEs that grant or remove keywords are respected.
pub(crate) fn creature_has_keyword(id: ObjId, kw: Keyword, state: &SimState) -> bool {
    state.def_of(id)
        .map(|d| d.has_keyword(kw))
        .unwrap_or(false)
}


/// Check and apply all State-Based Actions (rule 704). Called before every priority grant.
/// Runs in a loop until no SBA fires in a pass — the rules require repeated checking until stable.
fn check_state_based_actions(
    state: &mut SimState,
    t: u8,
) {
    // Ensure materialized state is current before reading it for SBA checks.
    // (It may be stale if state was mutated outside fire_event, e.g. directly in tests.)
    recompute(state);

    loop {
        let mut any = false;

        // SBA: player with life ≤ 0 loses the game (rule 704.5a).
        for who in [PlayerId::Us, PlayerId::Opp] {
            if state.life_of(who) <= 0 {
                state.log(t, who, format!("→ loses the game (life: {})", state.life_of(who)));
                state.winner = Some(who.opp());
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
        // SBA: creature with toughness ≤ 0 ceases to exist (CR 704.5f) — not "destroyed",
        // so indestructible does not apply; use change_zone directly.
        // SBA: creature with lethal damage is destroyed (CR 704.5g) — indestructible applies;
        // use destroy_one so indestructibility checks there will fire when added.
        for who in [PlayerId::Us, PlayerId::Opp] {
            let mut zero_tgh: Vec<ObjId> = Vec::new();
            let mut lethal_dmg: Vec<ObjId> = Vec::new();
            for card in state.permanents_of(who).collect::<Vec<_>>() {
                let Some(bf) = card.bf.as_ref() else { continue };
                if !state.def_of(card.id).map_or(false, |d| d.is_creature()) { continue; }
                let tgh = state.def_of(card.id)
                    .and_then(|d| d.as_creature())
                    .map(|c| c.toughness())
                    .unwrap_or(1);
                if tgh <= 0 { zero_tgh.push(card.id); }
                else if bf.damage >= tgh { lethal_dmg.push(card.id); }
            }
            for id in zero_tgh {
                change_zone(id, ZoneId::Graveyard, state, t, who);
                any = true;
            }
            for id in lethal_dmg {
                destroy_one(id, state, t, who);
                any = true;
            }
        }

        // SBA: planeswalker with loyalty ≤ 0 goes to graveyard (rule 704.5i).
        for who in [PlayerId::Us, PlayerId::Opp] {
            let dying: Vec<ObjId> = state.permanents_of(who)
                .filter_map(|card| {
                    let bf = card.bf.as_ref()?;
                    if !state.def_of(card.id).map_or(false, |d| matches!(d.kind, CardKind::Planeswalker(_))) { return None; }
                    if bf.loyalty <= 0 { Some(card.id) } else { None }
                })
                .collect();
            for id in dying {
                change_zone(id, ZoneId::Graveyard, state, t, who);
                any = true;
            }
        }

        // SBA: legend rule — if a player controls two or more legendary permanents with the
        // same name, that player chooses one to keep; the rest go to graveyard (rule 704.5j).
        for who in [PlayerId::Us, PlayerId::Opp] {
            // Collect (name, id) for all legendary permanents controlled by `who`.
            let mut seen: HashMap<String, ObjId> = HashMap::new();
            let mut extras: Vec<ObjId> = Vec::new();
            let legendaries: Vec<(String, ObjId)> = state.permanents_of(who)
                .filter(|card| {
                    state.def_of(card.id)
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
                change_zone(id, ZoneId::Graveyard, state, t, who);
                any = true;
            }
        }

        if !any { break; }
    }
}

pub(crate) fn do_amass(token_key: &str, controller: PlayerId, n: i32, state: &mut SimState, t: u8) {
    let army_id: Option<ObjId> = state.permanents_of(controller)
        .find(|c| c.catalog_key == token_key)
        .map(|c| c.id);
    if let Some(army_id) = army_id {
        if let Some(bf) = state.permanent_bf_mut(army_id) {
            bf.counters += n;
        }
        let c = state.permanent_bf(army_id).map_or(0, |bf| bf.counters);
        state.log(t, controller, format!("{token_key} grows to {c}/{c}"));
    } else {
        let def = state.catalog.get(token_key).cloned();
        let new_id = state.alloc_id();
        state.objects.insert(new_id, GameObject {
            id: new_id,
            catalog_key: token_key.to_string(),
            owner: controller,
            controller,
            zone: CardZone::Battlefield,
            is_token: true,
            spell: None,
            bf: Some(BattlefieldState {
                counters: n,
                ..BattlefieldState::new()
            }),
            materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });
        let ts = state.next_ci_timestamp();
        if let Some(obj) = state.objects.get_mut(&new_id) { obj.ci_timestamp = ts; }
        state.log(t, controller, format!("{token_key} token created {n}/{n}"));
    }
}

pub(crate) fn do_create_token(token_key: &str, controller: PlayerId, state: &mut SimState, t: u8) -> ObjId {
    let def = state.catalog.get(token_key).cloned();
    let new_id = state.alloc_id();
    state.objects.insert(new_id, GameObject {
        id: new_id,
        catalog_key: token_key.to_string(),
        owner: controller,
        controller,
        zone: CardZone::Battlefield,
        is_token: true,
        spell: None,
        bf: Some(BattlefieldState::new()),
        materialized: None,
        counters: HashMap::new(), ci_timestamp: 0,
    });
    {
        let ts = state.next_ci_timestamp();
        if let Some(obj) = state.objects.get_mut(&new_id) { obj.ci_timestamp = ts; }
    }
    state.log(t, controller, format!("{token_key} created"));
    new_id
}

fn do_flip_tamiyo(source_id: ObjId, controller: PlayerId, state: &mut SimState, t: u8) {
    // Read the back-face starting loyalty from the front-face materialized def.
    // The front face is still current in materialized at the moment the trigger resolves
    // (active_face == 0). `back` carries the printed PW data for the flipped face.
    let loyalty = state.def_of(source_id)
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
    _ap: PlayerId,
    strategies: &mut HashMap<PlayerId, Box<dyn Strategy>>,
) {
    let id = state.stack.pop().unwrap();
    if state.objects.contains_key(&id) {
        // It's a spell (card on the stack)
        let spell = state.objects[&id].spell.clone().unwrap_or_else(|| SpellState {
            effect: None,
            chosen_targets: vec![],
            is_back_face: false,
            costs_paid_ctx: CostsPaidCtx::default(),
        });
        let owner = state.objects[&id].owner;
        let name = state.objects[&id].catalog_key.clone();

        // Back face of a split card whose back has subtype "adventure" → exile to on_adventure.
        let is_adventure = spell.is_back_face
            && state.catalog.get(name.as_str())
                .and_then(|d| d.back.as_ref())
                .map_or(false, |b| b.has_subtype("adventure"));

        if is_adventure {
            if let Some(ref eff) = spell.effect {
                eff.call(state, t, &spell.chosen_targets);
            }
            let back_name = state.catalog.get(name.as_str())
                .and_then(|d| d.back.as_ref())
                .map(|b| b.name.as_str())
                .unwrap_or(name.as_str())
                .to_string();
            if let Some(card_obj) = state.objects.get_mut(&id) {
                card_obj.zone = CardZone::Exile { on_adventure: true };
                card_obj.spell = None;
            }
            state.log(t, owner, format!("{} resolves → {} on adventure in exile", back_name, name));
        } else if let Some(ref eff) = spell.effect {
            let is_perm = state.def_of(id)
                .map(|d| matches!(d.kind, CardKind::Creature(_) | CardKind::Artifact(_)
                    | CardKind::Planeswalker(_) | CardKind::Enchantment(_)))
                .unwrap_or(false);
            if !is_perm {
                if let Some(card_obj) = state.objects.get_mut(&id) {
                    card_obj.spell = None;
                }
                state.log(t, owner, format!("{} resolves", name));
                change_zone(id, ZoneId::Graveyard, state, t, owner);
            } else {
                // Stash costs-paid ctx so ETB replacement effects (e.g. Murktide) can read it.
                state.resolving_costs_ctx = spell.costs_paid_ctx.clone();
            }
            eff.call(state, t, &spell.chosen_targets);
            if is_perm {
                if let Some(card_obj) = state.objects.get_mut(&id) {
                    card_obj.spell = None;
                }
                state.resolving_costs_ctx = CostsPaidCtx::default();
            }
        } else {
            if let Some(card_obj) = state.objects.get_mut(&id) {
                card_obj.spell = None;
            }
            state.log(t, owner, format!("{} resolves", name));
            change_zone(id, ZoneId::Graveyard, state, t, owner);
        }
    } else if let Some(ability) = state.abilities.remove(&id) {
        // If the ability has a ChoiceSpec, enumerate valid choices and ask strategy to pick one.
        let mut effect_targets = ability.chosen_targets.clone();
        if let Some(ref spec) = ability.choice_spec {
            let controller = state.who_pid(ability.owner);
            let choices = enumerate_choices(spec, controller, state);
            if let Some(strategy) = strategies.get_mut(&controller) {
                if let Some(chosen) = strategy.choose_for_effect(id, &choices, state) {
                    effect_targets.insert(0, chosen);
                }
            }
        }
        // Make costs_paid_ctx visible to the effect closure (e.g. ninjutsu reads attack_target).
        state.resolving_costs_ctx = ability.costs_paid_ctx;
        ability.effect.call(state, t, &effect_targets);
        state.resolving_costs_ctx = CostsPaidCtx::default();
    }
}

/// Cast sub-machine (CR 601.2a-i).
///
/// Drives through Announce → Targets → LegalCheck → ComputeCost → ActivateMana → PayCosts → Complete.
/// Strategy callbacks drive each decision point.
fn run_cast_submachine(
    state: &mut SimState,
    t: u8,
    who: PlayerId,
    card_id: ObjId,
    face: SpellFace,
    strategy: &mut dyn Strategy,
) -> Option<ObjId> {
    // ── Announce (CR 601.2b) ────────────────────────────────────────────
    let def = state.def_of(card_id)
        .cloned()
        .or_else(|| {
            let key = &state.objects.get(&card_id)?.catalog_key;
            state.catalog.get(key.as_str()).cloned()
        });
    let def = def?;

    let options = AnnounceOptions {
        available_modes: def.spell_modes()
            .map(|m| (0..m.len()).collect())
            .unwrap_or_else(|| vec![0]),
        available_alt_costs: def.alternate_costs().to_vec(),
        has_x_cost: def.additional_costs.iter()
            .any(|c| matches!(c, CostComponent::XLife | CostComponent::XMana)),
    };
    let ann = strategy.announce(state, card_id, &options);
    let chosen_mode = ann.chosen_mode;
    let announced_alt_index = ann.alt_cost_index;
    let chosen_x = ann.chosen_x;

    // ── Targets (CR 601.2c) ─────────────────────────────────────────────
    let target_spec = if face == SpellFace::Back {
        def.back.as_ref()
            .map(|b| b.target_spec().clone())
            .unwrap_or(TargetSpec::None)
    } else {
        def.target_spec_for_mode(chosen_mode).clone()
    };
    let legal = legal_targets(&target_spec, who, card_id, state);
    let chosen_targets = if legal.is_empty() {
        vec![]
    } else {
        strategy.choose_targets(state, card_id, &legal, &target_spec)
    };

    // ── LegalCheck (CR 601.2e) ──────────────────────────────────────────
    // Sorcery-speed check done by caller before entering sub-machine.

    let preferred_cost = announced_alt_index
        .and_then(|i| def.alternate_costs().get(i).cloned());

    // ── ComputeCost + ActivateMana (CR 601.2f-g) ────────────────────────
    let mana_cost = if let Some(ref alt) = preferred_cost {
        alt.costs.iter().find_map(|c| {
            if let CostComponent::Mana(mc) = c { Some(mc.clone()) } else { None }
        }).unwrap_or_default()
    } else if face == SpellFace::Back {
        let def = state.def_of(card_id)
            .or_else(|| {
                let key = &state.objects.get(&card_id)?.catalog_key;
                state.catalog.get(key.as_str())
            });
        let back_cost = def.and_then(|d| d.adventure())
            .map(|a| a.mana_cost())
            .unwrap_or("");
        parse_mana_cost(back_cost)
    } else {
        let def = state.def_of(card_id)
            .or_else(|| {
                let key = &state.objects.get(&card_id)?.catalog_key;
                state.catalog.get(key.as_str())
            });
        let mut mc = parse_mana_cost(def.map(|d| d.mana_cost()).unwrap_or(""));
        mc.generic += def.map(|d| d.casting_cost_modifier).unwrap_or(0);
        mc
    };
    let mana_log = run_mana_loop(state, t, who, &mana_cost, strategy);
    state.log_mana_activations(t, who, mana_log);

    // ── PayCosts + Complete (CR 601.2h-i) ───────────────────────────────
    // cast_spell handles remaining payment (pool already filled by mana loop),
    // zone move, effect building, and event firing.
    cast_spell(state, t, who, card_id, face, preferred_cost.as_ref(),
               announced_alt_index, &chosen_targets, chosen_x, chosen_mode)
}

/// Activate sub-machine (CR 602.2b).
///
/// Pays ability costs, builds effect, and pushes the ability onto the stack.
/// Strategy callback drives target selection.
fn run_activate_submachine(
    state: &mut SimState,
    t: u8,
    who: PlayerId,
    source_id: ObjId,
    ability: &AbilityDef,
    strategy: &mut dyn Strategy,
) -> ObjId {
    // ── Targets ─────────────────────────────────────────────────────────
    let chosen_targets = {
        let legal = legal_targets(&ability.target_spec, who, source_id, state);
        if legal.is_empty() {
            vec![]
        } else {
            strategy.choose_targets(state, source_id, &legal, &ability.target_spec)
        }
    };

    let source_name_for_stack = state.permanent_name(source_id)
        .or_else(|| state.objects.get(&source_id).map(|c| c.catalog_key.clone()))
        .unwrap_or_default();
    let is_hand_source = matches!(ability.source_zone, SourceZone::Hand);

    // ── Pay costs ───────────────────────────────────────────────────────
    let ctx = pay_ability_cost(state, t, who, source_id, ability, is_hand_source);

    // ── Build effect ────────────────────────────────────────────────────
    let eff = build_ability_effect(ability, who, source_id);

    // ── Push to stack ───────────────────────────────────────────────────
    let ab_id = state.alloc_id();
    let ab_owner = state.player_id(who);
    let ab = StackAbility {
        id: ab_id,
        source_name: source_name_for_stack,
        owner: ab_owner,
        effect: eff,
        chosen_targets,
        costs_paid_ctx: ctx,
        is_triggered: false,
        counterable: true,
        choice_spec: ability.choice_spec.clone(),
    };
    state.abilities.insert(ab_id, ab);
    state.stack.push(ab_id);

    ab_id
}

fn handle_priority_round(
    state: &mut SimState,
    t: u8,
    ap: PlayerId,
    strategies: &mut HashMap<PlayerId, Box<dyn Strategy>>,
) {
    let nap = ap.opp();
    let mut priority_holder = ap;
    let mut last_passer: Option<PlayerId> = None;

    loop {
        let queued = std::mem::take(&mut state.pending_triggers);
        push_triggers(queued, state, strategies);
        check_state_based_actions(state, t);

        let who = priority_holder;
        let strategy = strategies.get_mut(&who).unwrap();
        let legal = strategy::collect_legal_actions(state, who);
        let chosen = strategy.choose_action(state, ap, &legal);

        match chosen {
            LegalAction::Pass => {
                let other = if who == ap { nap } else { ap };
                if last_passer == Some(other) {
                    if state.stack.is_empty() {
                        break;
                    } else {
                        resolve_top_of_stack(state, t, ap, strategies);
                        priority_holder = ap;
                        last_passer = None;
                    }
                } else {
                    last_passer = Some(who);
                    priority_holder = other;
                }
            }
            LegalAction::LandDrop(card_id) => {
                sim_play_land(state, t, who, card_id);
                state.player_mut(who).lands_played_this_turn += 1;
                last_passer = None;
            }
            LegalAction::CastSpell { card_id, face } => {
                let name = state.objects.get(&card_id).map(|c| c.catalog_key.clone()).unwrap_or_default();
                let is_instant = match face {
                    SpellFace::Main => state.def_of(card_id)
                        .map(|d| d.is_instant()).unwrap_or(false),
                    SpellFace::Back => state.def_of(card_id)
                        .and_then(|d| d.back.as_ref())
                        .map(|b| b.is_instant())
                        .unwrap_or(false),
                };
                if !is_instant && !state.stack.is_empty() {
                    eprintln!("[priority] BUG: sorcery-speed {} on non-empty stack, treating as Pass", name);
                    debug_assert!(false, "BUG: sorcery-speed cast of {} on non-empty stack", name);
                    last_passer = Some(who);
                    priority_holder = if who == ap { nap } else { ap };
                } else {
                    let strategy = strategies.get_mut(&who).unwrap();
                    if let Some(cid) = run_cast_submachine(state, t, who, card_id, face,
                                                           strategy.as_mut()) {
                        state.player_mut(who).spells_cast_this_turn += 1;
                        state.stack.push(cid);
                        priority_holder = if who == ap { nap } else { ap };
                        last_passer = None;
                    } else {
                        let pool = &state.player(who).pool;
                        eprintln!("[priority] BUG: cast failed for {} by {} (pool B={} U={} tot={}, hand={})",
                            name, who, pool.b, pool.u, pool.total, state.hand_size(who));
                        debug_assert!(false, "BUG: cast failed");
                        last_passer = Some(who);
                        priority_holder = if who == ap { nap } else { ap };
                    }
                }
            }
            LegalAction::ActivateAbility { source_id, ability_index } => {
                let ab = state.def_of(source_id)
                    .and_then(|d| d.abilities().get(ability_index).cloned())
                    .unwrap_or_default();
                let strategy = strategies.get_mut(&who).unwrap();
                run_activate_submachine(state, t, who, source_id, &ab,
                                       strategy.as_mut());
                priority_holder = if who == ap { nap } else { ap };
                last_passer = None;
            }
            LegalAction::ActivateManaAbility { source_id, ability_index } => {
                let ma = state.def_of(source_id)
                    .and_then(|d| d.mana_abilities().get(ability_index).cloned());
                if let Some(ma) = ma {
                    let name = state.objects.get(&source_id)
                        .map(|c| c.catalog_key.clone()).unwrap_or_default();
                    // Pay all costs via the general cost payment path.
                    let _ctx = pay_costs(&ma.costs, state, t, who, source_id, 0);
                    // Produce mana (pick first available color).
                    let color = ma.produces.first().copied();
                    ma.make_effect.clone()(who, color).call(state, t, &[]);
                    state.log(t, who, format!("→ activate {} (mana)", name));
                    priority_holder = if who == ap { nap } else { ap };
                    last_passer = None;
                } else {
                    last_passer = Some(who);
                    priority_holder = if who == ap { nap } else { ap };
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
    ap: PlayerId,
    step: &Step,
    on_play: bool,
    strategies: &mut HashMap<PlayerId, Box<dyn Strategy>>,
) {
    // Ensure materialized state is current at the start of every step.
    // Strategy calls (declare_attackers, declare_blockers) and combat damage run against
    // this snapshot; fire_event also rebuilds it after each tick.
    recompute(state);

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
            state.player_mut(ap).lands_played_this_turn = 0;
            state.player_mut(ap).spells_cast_this_turn = 0;
            state.player_mut(ap).draws_this_turn = 0;
            // Expire "until your next turn" trigger and continuous instances for the active player.
            state.trigger_instances.retain(|ti| {
                !(ti.expiry == Some(Expiry::StartOfControllerNextTurn) && ti.controller == ap)
            });
            state.continuous_instances.retain(|ci| {
                !(ci.expiry == Expiry::StartOfControllerNextTurn && ci.controller == ap)
            });
        }
        StepKind::Draw => {
            let this_player_on_play = if ap == PlayerId::Us { on_play } else { !on_play };
            let skip = this_player_on_play && t == 1;
            if skip {
                state.log(t, ap, "No draw (on the play)");
            } else {
                sim_draw(state, ap, t, true);
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
            state.continuous_instances.retain(|ci| ci.expiry != Expiry::EndOfTurn);
            state.trigger_instances.retain(|ti| ti.expiry != Some(Expiry::EndOfTurn));
            // Expire unconsumed latent spell mods with EndOfTurn expiry.
            state.latent_spell_mods.retain(|lsm| lsm.expiry != Expiry::EndOfTurn);
        }
        StepKind::DeclareAttackers => {
            // Strategy decides who attacks and what each attacker targets.
            let decisions = strategies.get_mut(&ap).unwrap().declare_attackers(state);
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
                    attacker_controller: ap,
                }, state, t, ap);
            }
            fire_event(GameEvent::EnteredStep {
                step: StepKind::DeclareAttackers,
                active_player: ap,
            }, state, t, ap);
        }
        StepKind::DeclareBlockers => {
            let nap = ap.opp();
            // Strategy decides which blockers to assign.
            let blocks = strategies.get_mut(&ap.opp()).unwrap().declare_blockers(state);
            // Engine validation: drop illegal blocks (protection, etc.) as a safety net.
            let blocks: Vec<(ObjId, ObjId)> = blocks.into_iter()
                .filter(|&(atk_id, blk_id)| !is_protected_from(atk_id, blk_id, state))
                .collect();
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
                let nap = ap.opp();
                let attackers   = state.combat_attackers.clone();
                let block_pairs = state.combat_blocks.clone();
                let blocked_atk_ids: std::collections::HashSet<ObjId> = block_pairs.iter()
                    .map(|(a, _)| *a).collect();

                let mut player_damage = 0i32;

                for &(atk_id, blk_id) in &block_pairs {
                    let atk_pow = state.def_of(atk_id)
                        .and_then(|d| d.as_creature())
                        .map(|c| c.power())
                        .unwrap_or(1);
                    let blk_pow = state.def_of(blk_id)
                        .and_then(|d| d.as_creature())
                        .map(|c| c.power())
                        .unwrap_or(1);
                    // CR 702.16b: protection prevents damage from sources with the quality.
                    if !is_protected_from(atk_id, blk_id, state) {
                        if let Some(bf) = state.permanent_bf_mut(atk_id) {
                            bf.damage += blk_pow;
                        }
                    }
                    if !is_protected_from(blk_id, atk_id, state) {
                        if let Some(bf) = state.permanent_bf_mut(blk_id) {
                            bf.damage += atk_pow;
                        }
                    }
                }

                let mut pw_damage: HashMap<ObjId, i32> = HashMap::new();
                for &atk_id in &attackers {
                    if !blocked_atk_ids.contains(&atk_id) {
                        let atk_pow = state.def_of(atk_id)
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
            active_player: ap,
        };
        fire_event(step_ev, state, t, ap);
    }

    if step.prio {
        handle_priority_round(state, t, ap, strategies);
    }
    // Mana pool drains at the end of every step.
    state.us.pool.drain();
    state.opp.pool.drain();
}


/// Execute a full phase: run each step, then optionally run a phase-level priority round.
fn do_phase(
    state: &mut SimState,
    t: u8,
    ap: PlayerId,
    phase: &Phase,
    on_play: bool,
    strategies: &mut HashMap<PlayerId, Box<dyn Strategy>>,
) {
    for step in &phase.steps {
        do_step(state, t, ap, step, on_play, strategies);
        if state.done() {
            return;
        }
    }
    if phase.is_main_phase() {
        state.current_phase = Some(TurnPosition::Phase(phase.kind));
        let phase_ev = GameEvent::EnteredPhase { phase: phase.kind };
        fire_event(phase_ev, state, t, ap);
        handle_priority_round(state, t, ap, strategies);
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
    ap: PlayerId,
    on_play: bool,
    strategies: &mut HashMap<PlayerId, Box<dyn Strategy>>,
) {
    state.current_turn = t;
    state.current_ap = state.player_id(ap);
    do_phase(state, t, ap, &beginning_phase(), on_play, strategies);
    if state.done() { return; }

    do_phase(state, t, ap, &main_phase(), on_play, strategies);
    if state.done() { return; }

    do_phase(state, t, ap, &combat_phase(), on_play, strategies);
    if state.done() { return; }

    do_phase(state, t, ap, &post_combat_main_phase(), on_play, strategies);
    if state.done() { return; }

    do_phase(state, t, ap, &end_phase(), on_play, strategies);
}


/// Simulate the full game up to the Doomsday turn.
/// Returns `None` if Doomsday was countered and could not be protected — caller should retry.
pub fn simulate_game(
    deck_name: &str,
    opponent: &str,
    catalog: &HashMap<String, CardDef>,
    all_cards: &[(String, i32, String)],
    opp_cards: &[(String, i32, String)],
    rng: &mut impl Rng,
) -> Option<SimState> {
    let turn = gen_turn(rng);
    let on_play = rng.gen_bool(0.5);
    let us = PlayerState::new(deck_name);
    let opp = PlayerState::new(opponent);
    let mut state = SimState::new(us, opp);
    state.catalog = catalog.clone();
    state.on_play = on_play;
    state.turn = turn;

    // Populate state.objects with Library-zone objects for each player's mainboard.
    // catalog: game setup — ObjIds are assigned here for the first time; materialized
    // does not exist yet. Catalog is the only source of card definitions at this stage.
    for (name, qty, board) in all_cards {
        if board != "main" { continue; }
        if state.catalog.get(name.as_str()).is_none() { continue; }
        for _ in 0..*qty {
            let id = state.alloc_id();
            state.objects.insert(id, GameObject::new(id, name.clone(), PlayerId::Us));
            if let Some(def) = state.catalog.get(name.as_str()) {
                let def = def.clone();

            }
        }
    }
    for (name, qty, board) in opp_cards {
        if board != "main" { continue; }
        if state.catalog.get(name.as_str()).is_none() { continue; }
        for _ in 0..*qty {
            let id = state.alloc_id();
            state.objects.insert(id, GameObject::new(id, name.clone(), PlayerId::Opp));
            if let Some(def) = state.catalog.get(name.as_str()) {
                let def = def.clone();

            }
        }
    }

    // ── Strategies and opening hands ─────────────────────────────────────────

    let mut strategies: HashMap<PlayerId, Box<dyn Strategy>> = HashMap::from([
        (PlayerId::Us,  Box::new(DoomsdayStrategy::new(turn)) as Box<dyn Strategy>),
        (PlayerId::Opp, Box::new(GenericOppStrategy::new())   as Box<dyn Strategy>),
    ]);

    // Deal opening hands with mulligan decisions.
    // The library is a HashMap so iteration order is already effectively random;
    // moving cards back and redrawing is sufficient — no explicit shuffle needed.
    let mut mulligans = [0u32; 2];
    for (i, who) in [PlayerId::Us, PlayerId::Opp].into_iter().enumerate() {
        for _ in 0..7 { sim_draw(&mut state, who, 0, false); }
        loop {
            let taken = mulligans[i];
            if !strategies.get_mut(&who).unwrap().take_mulligan(&state, taken) { break; }
            mulligans[i] += 1;
            // Return hand to library.
            let hand_ids: Vec<ObjId> = state.hand_of(who).map(|c| c.id).collect();
            for id in hand_ids { state.objects.get_mut(&id).unwrap().zone = CardZone::Library; }
            state.player_mut(who).draws_this_turn = 0;
            // Draw new hand.
            let new_size = 7u32.saturating_sub(mulligans[i]) as usize;
            for _ in 0..new_size { sim_draw(&mut state, who, 0, false); }
        }
        state.player_mut(who).draws_this_turn = 0;
    }

    let us_hand = state.hand_size(PlayerId::Us);
    let opp_hand = state.hand_size(PlayerId::Opp);
    state.log(
        0,
        PlayerId::Us,
        format!(
            "Turn {} — {} ({}) | us: {} cards (-{} mulligans), opp: {} cards (-{} mulligans)",
            turn,
            opponent,
            if on_play { "play" } else { "draw" },
            us_hand,
            mulligans[0],
            opp_hand,
            mulligans[1],
        ),
    );

    // ── Turn loop ────────────────────────────────────────────────────────────

    for t in 1..=turn {
        if !on_play {
            do_turn(&mut state, t, PlayerId::Opp, on_play, &mut strategies);
            if state.done() { break; }
        }
        {
            do_turn(&mut state, t, PlayerId::Us, on_play, &mut strategies);
            if state.done() { break; }
        }
        if on_play && t < turn {
            do_turn(&mut state, t, PlayerId::Opp, on_play, &mut strategies);
            if state.done() { break; }
        }
    }

    if !state.success {
        return None;
    }

    Some(state)
}


// ── Scenario generation ───────────────────────────────────────────────────────

pub fn generate_scenario(
    deck_name: &str,
    opp_display: &str,
    catalog: &HashMap<String, CardDef>,
    all_cards: &[(String, i32, String)],
    opp_cards: &[(String, i32, String)],
) -> SimState {
    let mut rng = rand::thread_rng();
    loop {
        if let Some(state) =
            simulate_game(deck_name, opp_display, catalog, all_cards, opp_cards, &mut rng)
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
    if !def.target_spec().is_none() { return true; }
    match &def.kind {
        CardKind::Creature(_) | CardKind::Artifact(_)
        | CardKind::Planeswalker(_) | CardKind::Enchantment(_) => true,
        CardKind::Instant(s) | CardKind::Sorcery(s) => s.modes.is_some(),
        CardKind::Land(_) => true,
    }
}

/// Print a warning for mainboard cards that lack a simulation implementation.
///
/// Two categories:
///   ✗ not in catalog — excluded from simulation entirely (silently dropped)
///   ~ in catalog but no actionable effects — drawn but never played/cast
pub fn warn_unimplemented_cards(
    cards: &[(String, i32, String)],
    deck_label: &str,
    catalog: &HashMap<String, CardDef>,
) {
    let mut missing_main:  Vec<(&str, i32)> = Vec::new();
    let mut no_effects_main: Vec<(&str, i32)> = Vec::new();
    let mut missing_side:  Vec<(&str, i32)> = Vec::new();
    let mut no_effects_side: Vec<(&str, i32)> = Vec::new();

    for (name, qty, board) in cards {
        let (missing, no_effects) = if board == "main" {
            (&mut missing_main, &mut no_effects_main)
        } else {
            (&mut missing_side, &mut no_effects_side)
        };
        match catalog.get(name.as_str()) {
            None => missing.push((name, *qty)),
            Some(def) if !card_has_implementation(def) => no_effects.push((name, *qty)),
            _ => {}
        }
    }

    if missing_main.is_empty() && no_effects_main.is_empty()
        && missing_side.is_empty() && no_effects_side.is_empty() { return; }

    println!("\n⚠  {} — unimplemented cards:", deck_label);
    for (name, qty) in &missing_main {
        println!("   ✗ {}×{} — not in catalog (excluded from simulation)", qty, name);
    }
    for (name, qty) in &no_effects_main {
        println!("   ~ {}×{} — no simulation effects (drawn but never cast)", qty, name);
    }
    if !missing_side.is_empty() || !no_effects_side.is_empty() {
        println!("   sideboard:");
        for (name, qty) in &missing_side {
            println!("   ✗ {}×{} — not in catalog", qty, name);
        }
        for (name, qty) in &no_effects_side {
            println!("   ~ {}×{} — no simulation effects", qty, name);
        }
    }
}
