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
use strategy::{decide_action, collect_on_board_actions, declare_attackers, declare_blockers};
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
enum CardZone {
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
    is_adventure_face: bool,
    adventure_card_name: Option<String>,
    /// Pre-computed annotation (e.g. "+3" for Murktide). Captured in the Effect
    /// closure at cast time; stored here for inspection/debugging.
    #[allow(dead_code)]
    annotation: Option<String>,
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
    /// Active face index for double-faced cards (0 = front, 1 = back). Flip sets this to 1.
    active_face: u8,
}

impl BattlefieldState {
    fn new(mana_abilities: Vec<ManaAbility>) -> Self {
        BattlefieldState {
            tapped: false, damage: 0, entered_this_turn: true, counters: 0,
            power_mod: 0, toughness_mod: 0, loyalty: 0, pw_activated_this_turn: false,
            attacking: false, unblocked: false, attack_target: None,
            annotation: None, mana_abilities, active_face: 0,
        }
    }
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
    spell: Option<SpellState>,         // Some only when zone == Stack (spell on stack)
}

impl CardObject {
    fn new(id: ObjId, name: impl Into<String>, owner: impl Into<String>) -> Self {
        let owner = owner.into();
        CardObject {
            id, name: name.into(), controller: owner.clone(), owner,
            zone: CardZone::Library, bf: None, spell: None,
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

// ── Turn structure ────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Debug)]
enum PhaseKind {
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
    /// Activate a permanent ability. Carries source ObjId + ability def. Uses the stack, passes priority after.
    ActivateAbility(ObjId, AbilityDef),
    /// Intent to cast a spell. No resources are spent until `handle_priority_round` accepts and
    /// commits this action. The framework validates legality (sorcery-speed, etc.) there.
    ///
    /// `preferred_cost` — pre-selected alternate cost (used by `respond_with_counter`).
    CastSpell { name: String, preferred_cost: Option<AlternateCost> },
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
    /// On-board actions pre-collected at the start of each main phase (populated by
    /// `collect_on_board_actions`). `ap_proactive` pops from this list instead of scanning flags.
    pending_actions: Vec<PriorityAction>,
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
            pending_actions: Vec::new(),
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
    /// Active continuous effects (from loyalty abilities, spells, etc.).
    active_effects: Vec<ContinuousEffect>,
    /// Spell/ability stack. Items are resolved last-in-first-out. Populated by
    /// handle_priority_round; empty between priority rounds.
    pub(crate) stack: Vec<ObjId>,
    /// Activated and triggered abilities on the stack, keyed by their allocated ObjId.
    abilities: HashMap<ObjId, StackAbility>,
    /// All cards in all zones, keyed by stable ObjId. Added as part of staged object model migration.
    cards: HashMap<ObjId, CardObject>,
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
            current_phase: None,
            combat_attackers: Vec::new(),
            combat_blocks: Vec::new(),
            pending_triggers: Vec::new(),
            active_effects: Vec::new(),
            stack: Vec::new(),
            abilities: HashMap::new(),
            cards: HashMap::new(),
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

    fn permanents_of<'a>(&'a self, who: &'a str) -> impl Iterator<Item = &'a CardObject> {
        self.cards.values().filter(move |c| c.controller == who && c.zone == CardZone::Battlefield)
    }

    fn permanent_bf(&self, id: ObjId) -> Option<&BattlefieldState> {
        self.cards.get(&id)
            .filter(|c| c.zone == CardZone::Battlefield)
            .and_then(|c| c.bf.as_ref())
    }

    fn permanent_bf_mut(&mut self, id: ObjId) -> Option<&mut BattlefieldState> {
        self.cards.get_mut(&id)
            .filter(|c| c.zone == CardZone::Battlefield)
            .and_then(|c| c.bf.as_mut())
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

    /// Cards owned by `who` that are currently in exile with adventure status.
    fn on_adventure_of<'a>(&'a self, who: &'a str) -> impl Iterator<Item = &'a CardObject> {
        self.cards.values().filter(move |c| c.owner == who && c.zone == (CardZone::Exile { on_adventure: true }))
    }

    fn library_of<'a>(&'a self, who: &'a str) -> impl Iterator<Item = &'a CardObject> {
        self.cards.values().filter(move |c| c.owner == who && c.zone == CardZone::Library)
    }

    fn hand_size(&self, who: &str) -> i32 {
        self.hand_of(who).count() as i32
    }

    fn library_size(&self, who: &str) -> usize {
        self.library_of(who).count()
    }

    /// Mutate zone field only — no triggers, no logging. Use `change_zone` for that.
    fn set_card_zone(&mut self, id: ObjId, zone: CardZone) {
        if let Some(card) = self.cards.get_mut(&id) {
            card.zone = zone;
            if !matches!(zone, CardZone::Battlefield) {
                card.bf = None;
            }
        }
    }

    fn find_permanent_by_name<'a>(&'a self, name: &str, controller: &str) -> Option<&'a CardObject> {
        self.cards.values().find(|c| c.name == name && c.controller == controller && c.zone == CardZone::Battlefield)
    }

    fn queue_triggers(&mut self, event: &GameEvent) {
        let triggers = fire_triggers(event, self);
        self.pending_triggers.extend(triggers);
    }

    /// Draw one card for `who`. Increments draws_this_turn, moves a Library card to Hand, then fires a Draw event.
    fn sim_draw(&mut self, who: &str, t: u8, is_natural: bool) {
        self.player_mut(who).draws_this_turn += 1;
        let draw_index = self.player(who).draws_this_turn;
        // Move one Library card to Hand.
        let top_id = self.library_of(who).next().map(|c| c.id);
        if let Some(id) = top_id {
            self.set_card_zone(id, CardZone::Hand { known: false });
        }
        let hand = self.hand_size(who);
        let ev = GameEvent::Draw { controller: who.to_string(), draw_index, is_natural };
        self.queue_triggers(&ev);
        self.log(t, who, if is_natural { format!("Draw [hand: {}]", hand) } else { format!("draw ({}) [hand: {}]", draw_index, hand) });
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
        self.cards.get(&id)
            .filter(|c| c.zone == CardZone::Battlefield)
            .map(|c| c.controller.as_str())
    }

    /// Return the name of the permanent with the given id.
    fn permanent_name(&self, id: ObjId) -> Option<String> {
        self.cards.get(&id)
            .filter(|c| c.zone == CardZone::Battlefield)
            .map(|c| c.name.clone())
    }

    /// Mana accessible right now for `who`: pool + what untapped permanents can still produce.
    fn potential_mana(&self, who: &str) -> ManaPool {
        let mut p = self.player(who).pool.clone();
        for card in self.permanents_of(who) {
            if let Some(bf) = &card.bf {
                accumulate_source_potential(&bf.mana_abilities, bf.tapped, &mut p);
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
                let found = self.cards.iter()
                    .find(|(_, c)| {
                        c.controller == who && c.zone == CardZone::Battlefield &&
                        c.bf.as_ref().map_or(false, |bf| bf.mana_abilities.iter().any(|ma| {
                            (!ma.tap_self || !bf.tapped) && ma.produces.contains(color_char)
                        }))
                    })
                    .map(|(id, c)| {
                        let bf = c.bf.as_ref().unwrap();
                        let sac = bf.mana_abilities.iter()
                            .find(|ma| (!ma.tap_self || !bf.tapped) && ma.produces.contains(color_char))
                            .map(|ma| ma.sacrifice_self)
                            .unwrap_or(false);
                        (*id, c.name.clone(), sac)
                    });
                if let Some((id, name, sac)) = found {
                    if sac {
                        log.push(format!("sac {} → {}", name, color_char));
                        if let Some(card) = self.cards.get_mut(&id) {
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
            let found = self.cards.iter()
                .find(|(_, c)| {
                    c.controller == who && c.zone == CardZone::Battlefield &&
                    c.bf.as_ref().map_or(false, |bf| {
                        !bf.mana_abilities.is_empty() &&
                        bf.mana_abilities.iter().any(|ma| !ma.tap_self || !bf.tapped)
                    })
                })
                .map(|(id, c)| {
                    let bf = c.bf.as_ref().unwrap();
                    let sac = bf.mana_abilities.iter()
                        .find(|ma| !ma.tap_self || !bf.tapped)
                        .map(|ma| ma.sacrifice_self)
                        .unwrap_or(false);
                    (*id, c.name.clone(), sac)
                });
            if let Some((id, name, sac)) = found {
                if sac {
                    log.push(format!("sac {} → 1", name));
                    if let Some(card) = self.cards.get_mut(&id) {
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
        if let Some(card) = self.cards.get(&id) {
            return self.player_id(&card.owner);
        }
        if let Some(ab) = self.abilities.get(&id) {
            return ab.owner;
        }
        ObjId::UNSET
    }

    pub(crate) fn stack_item_display_name(&self, id: ObjId) -> &str {
        if let Some(card) = self.cards.get(&id) {
            return card.name.as_str();
        }
        if let Some(ab) = self.abilities.get(&id) {
            return ab.source_name.as_str();
        }
        ""
    }

    pub(crate) fn stack_item_is_counterable(&self, id: ObjId) -> bool {
        self.cards.contains_key(&id) && self.cards[&id].zone == CardZone::Stack
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

// PlayerState Display is handled via SimState::fmt_player_zones which has access to state.cards.

impl SimState {
    /// Write hand/graveyard/exile zones for `who` to the formatter, reading from state.cards.
    fn fmt_player_zones(&self, f: &mut std::fmt::Formatter<'_>, who: &str) -> std::fmt::Result {
        let hand_count = self.hand_size(who);
        if hand_count > 0 {
            // Collect known hand cards (visible names from Hand { known: true }).
            let visible: Vec<&str> = self.hand_of(who)
                .filter(|c| matches!(c.zone, CardZone::Hand { known: true }))
                .map(|c| c.name.as_str())
                .collect();
            let hidden = self.hand_of(who)
                .filter(|c| matches!(c.zone, CardZone::Hand { known: false }))
                .count() as i32;
            writeln!(f, "  Hand       :")?;
            for name in &visible {
                writeln!(f, "    * {}", name)?;
            }
            if hidden > 0 {
                writeln!(f, "    ({} hidden card{})", hidden, if hidden == 1 { "" } else { "s" })?;
            }
        }
        let mut gy: Vec<&str> = self.graveyard_of(who).map(|c| c.name.as_str()).collect();
        if !gy.is_empty() {
            gy.sort();
            writeln!(f, "  Graveyard  :")?;
            for name in &gy {
                writeln!(f, "    * {}", name)?;
            }
        }
        let mut exile_cards: Vec<(&str, bool)> = self.exile_of(who)
            .map(|c| (c.name.as_str(), matches!(c.zone, CardZone::Exile { on_adventure: true })))
            .collect();
        if !exile_cards.is_empty() {
            exile_cards.sort_by_key(|(n, _)| *n);
            writeln!(f, "  Exile      :")?;
            for (name, on_adv) in &exile_cards {
                if *on_adv {
                    writeln!(f, "    * {} (on adventure)", name)?;
                } else {
                    writeln!(f, "    * {}", name)?;
                }
            }
        }
        Ok(())
    }

    /// Write the permanents for `who` to the formatter, reading from state.cards.
    fn fmt_permanents(&self, f: &mut std::fmt::Formatter<'_>, who: &str) -> std::fmt::Result {
        let mut perms: Vec<&CardObject> = self.permanents_of(who).collect();
        perms.sort_by(|a, b| a.name.cmp(&b.name));
        if !perms.is_empty() {
            writeln!(f, "  Permanents :")?;
            for card in &perms {
                if let Some(bf) = &card.bf {
                    let mut tags: Vec<String> = Vec::new();
                    if let Some(ann) = &bf.annotation { tags.push(ann.clone()); }
                    if bf.counters > 0 { tags.push(format!("+{} counters", bf.counters)); }
                    if bf.loyalty > 0 { tags.push(format!("loyalty: {}", bf.loyalty)); }
                    let suffix = if tags.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", tags.join(", "))
                    };
                    writeln!(f, "    * {}{}", card.name, suffix)?;
                }
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

/// Choose a land to play from the hand. Returns the chosen land name, or `None` if no eligible
/// land exists. Weights black-producing lands 3× when the player has no black source.
/// `fateful` = Doomsday turn: skip cracked-land entries (e.g. Wasteland) to avoid mana issues.
fn choose_land_name(
    state: &SimState,
    who: &str,
    catalog_map: &HashMap<&str, &CardDef>,
    fateful: bool,
    rng: &mut impl Rng,
) -> Option<String> {
    if state.hand_size(who) <= 0 {
        return None;
    }
    // On the fateful turn, if we can't produce black mana yet, require a black source.
    let need_black = fateful && !state.has_black_mana(who);
    let candidates: Vec<String> = state.hand_of(who)
        .filter_map(|c| {
            let def = catalog_map.get(c.name.as_str())?;
            let land = def.as_land()?;
            if need_black && !land.mana_abilities.iter().any(|ma| ma.produces.contains('B')) { return None; }
            Some(c.name.clone())
        })
        .collect();
    if candidates.is_empty() { return None; }
    let idx = rng.gen_range(0..candidates.len());
    Some(candidates[idx].clone())
}

/// Play a specific, pre-chosen land from hand (moves it to Battlefield).
/// Fetches stay in play to be cracked later in the ability pass.
fn sim_play_land(
    state: &mut SimState,
    t: u8,
    who: &str,
    land_name: &str,
    catalog_map: &HashMap<&str, &CardDef>,
) {
    // Find a Hand card with this name.
    let card_id = state.hand_of(who).find(|c| c.name == land_name).map(|c| c.id);
    let Some(card_id) = card_id else { return; };

    let (tapped, mana_abilities) = {
        let def = catalog_map.get(land_name).and_then(|d| d.as_land()).expect("sim_play_land: not a land");
        (def.enters_tapped, def.mana_abilities.clone())
    };
    // Move from Hand to Battlefield.
    if let Some(card) = state.cards.get_mut(&card_id) {
        card.zone = CardZone::Battlefield;
        card.bf = Some(BattlefieldState {
            tapped,
            mana_abilities,
            ..BattlefieldState::new(vec![])
        });
    }
    state.log(t, who, format!("Play {} [hand: {}]", land_name, state.hand_size(who)));
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
        if !state.potential_mana(who).can_pay(&cost) {
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

/// True if the player can currently afford to cast `name` via any available cost.
///
/// Tries the standard mana cost first; falls back to alternate costs (e.g. delve, pitch).
fn spell_is_affordable(
    name: &str,
    def: &CardDef,
    state: &SimState,
    who: &str,
    catalog_map: &HashMap<&str, &CardDef>,
) -> bool {
    let mut cost = parse_mana_cost(def.mana_cost());
    if def.delve() && cost.generic > 0 {
        let gy_len = state.graveyard_of(who).count() as i32;
        cost.generic = (cost.generic - gy_len).max(0);
    }
    let mana_is_usable = !def.mana_cost().is_empty() && state.potential_mana(who).can_pay(&cost);
    if mana_is_usable { return true; }
    def.alternate_costs().iter().any(|c| can_pay_alternate_cost(c, state, who, name, catalog_map))
}

fn hand_ability_affordable(ability: &AbilityDef, state: &SimState, who: &str) -> bool {
    let player = state.player(who);
    if !ability.mana_cost.is_empty() {
        if !state.potential_mana(who).can_pay(&parse_mana_cost(&ability.mana_cost)) { return false; }
    }
    if ability.life_cost > 0 && player.life <= ability.life_cost { return false; }
    if ability.sacrifice_land && !state.permanents_of(who).any(|c| {
        c.bf.as_ref().map_or(false, |bf| !bf.mana_abilities.is_empty())
    }) { return false; }
    true
}

fn collect_hand_actions(
    state: &SimState,
    who: &str,
    catalog_map: &HashMap<&str, &CardDef>,
) -> Vec<PriorityAction> {
    if state.hand_size(who) <= 0 {
        return Vec::new();
    }
    let opp_who = if who == "us" { "opp" } else { "us" };

    // Collect hand card names and their ids (deduplicate by name to avoid offering same spell twice).
    let hand_cards: Vec<(ObjId, String)> = state.hand_of(who)
        .map(|c| (c.id, c.name.clone()))
        .collect();

    let mut seen_names: std::collections::HashSet<String> = Default::default();
    let mut actions: Vec<PriorityAction> = Vec::new();

    for (card_id, name) in &hand_cards {
        let Some(&def) = catalog_map.get(name.as_str()) else { continue; };
        if def.is_land() { continue; }
        if !card_has_implementation(def) { continue; }
        if def.legendary() && state.permanents_of(who).any(|c| c.name == name.as_str()) { continue; }
        if let Some(tgt) = def.target() {
            if !has_valid_target(tgt, state, who, catalog_map) { continue; }
        }
        let ok = def.requires().iter().all(|req| match req.as_str() {
            "opp_hand_nonempty" => state.hand_size(opp_who) > 0,
            "us_gy_has_creature" => state.graveyard_of(who)
                .any(|c| catalog_map.get(c.name.as_str()).map(|d| d.is_creature()).unwrap_or(false)),
            _ => true,
        });
        if !ok { continue; }
        if !spell_is_affordable(name, def, state, who, catalog_map) { continue; }
        if seen_names.insert(name.clone()) {
            actions.push(PriorityAction::CastSpell { name: name.clone(), preferred_cost: None });
        }

        // In-hand abilities (cycling, channel, etc.)
        for ab in def.abilities().iter().filter(|ab| ab.zone == "hand") {
            if hand_ability_affordable(ab, state, who) {
                actions.push(PriorityAction::ActivateAbility(*card_id, ab.clone()));
            }
        }

        // Adventure spell face.
        if let Some(face) = def.adventure() {
            if !face.mana_cost.is_empty() {
                let cost = parse_mana_cost(&face.mana_cost);
                if !state.potential_mana(who).can_pay(&cost) { continue; }
            }
            if let Some(ref tgt) = face.target {
                if !has_valid_target(tgt, state, who, catalog_map) { continue; }
            }
            actions.push(PriorityAction::CastAdventure { card_name: name.clone() });
        }
    }

    actions
}

// choose_permanent_target is defined in predicates.rs

fn card_zone_to_id(zone: &CardZone) -> ZoneId {
    match zone {
        CardZone::Library        => ZoneId::Library,
        CardZone::Hand { .. }    => ZoneId::Hand,
        CardZone::Stack          => ZoneId::Stack,
        CardZone::Battlefield    => ZoneId::Battlefield,
        CardZone::Graveyard      => ZoneId::Graveyard,
        CardZone::Exile { .. }   => ZoneId::Exile,
    }
}

fn card_type_str(d: &CardDef) -> &'static str {
    if d.is_creature()      { "creature" }
    else if d.is_instant()  { "instant" }
    else if d.is_sorcery()  { "sorcery" }
    else if d.is_land()     { "land" }
    else                    { "permanent" }
}

/// Move a game object from its current zone to `to`.
/// Works for any zone transition. No-ops silently if the id is not found.
/// Fires `GameEvent::ZoneChange` and logs semantically based on (from, to).
pub(super) fn change_zone(
    id: ObjId,
    to: ZoneId,
    state: &mut SimState,
    t: u8,
    actor: &str,
    catalog_map: &HashMap<&str, &CardDef>,
) {
    let (name, controller, from, card_type) = {
        let card = match state.cards.get(&id) {
            Some(c) => c,
            None => return,
        };
        let from = card_zone_to_id(&card.zone);
        let ct = catalog_map.get(card.name.as_str())
            .map(|d| card_type_str(d))
            .unwrap_or("permanent");
        (card.name.clone(), card.controller.clone(), from, ct)
    };

    let new_zone = match to {
        ZoneId::Graveyard    => CardZone::Graveyard,
        ZoneId::Exile        => CardZone::Exile { on_adventure: false },
        ZoneId::Hand         => CardZone::Hand { known: false },
        ZoneId::Library      => CardZone::Library,
        ZoneId::Stack        => CardZone::Stack,
        ZoneId::Battlefield  => CardZone::Battlefield,
    };
    if let Some(card) = state.cards.get_mut(&id) {
        card.zone = new_zone;
        if from == ZoneId::Battlefield { card.bf = None; }
        if to   == ZoneId::Battlefield {
            let mana_abs = catalog_map.get(card.name.as_str())
                .map_or_else(Vec::new, |d| d.mana_abilities().to_vec());
            let loyalty = catalog_map.get(card.name.as_str())
                .and_then(|d| if let CardKind::Planeswalker(ref p) = d.kind { Some(p.loyalty) } else { None })
                .unwrap_or(0);
            card.bf = Some(BattlefieldState {
                mana_abilities: mana_abs,
                loyalty,
                entered_this_turn: true,
                ..BattlefieldState::new(vec![])
            });
        }
    }

    match (from, to) {
        (ZoneId::Stack,       ZoneId::Graveyard)    => state.log(t, actor, format!("→ {} countered", name)),
        (ZoneId::Battlefield, ZoneId::Graveyard)    => state.log(t, actor, format!("→ {} destroyed", name)),
        (ZoneId::Hand,        ZoneId::Graveyard)    => state.log(t, actor, format!("→ {} discarded", name)),
        (_,                   ZoneId::Graveyard)    => state.log(t, actor, format!("→ {} to graveyard", name)),
        (_,                   ZoneId::Exile)        => state.log(t, actor, format!("→ {} exiled", name)),
        (_,                   ZoneId::Hand)         => state.log(t, actor, format!("→ {} returned to {}'s hand", name, controller)),
        (ZoneId::Graveyard,   ZoneId::Battlefield)  => state.log(t, actor, format!("→ {} returns from graveyard", name)),
        _ => {}
    }

    let ev = GameEvent::ZoneChange {
        card: name,
        card_type: card_type.to_string(),
        from,
        to,
        controller,
    };
    state.queue_triggers(&ev);
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
    _catalog_map: &HashMap<&str, &CardDef>,
) {
    let source_name = state.permanent_name(source_id)
        .or_else(|| state.hand_of(who).find(|c| c.id == source_id).map(|c| c.name.clone()))
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
            .find(|c| c.name == source_name)
            .map(|c| c.id);
        if let Some(hid) = hand_card_id {
            state.set_card_zone(hid, CardZone::Graveyard);
        }
    }

    // Ninjutsu cost: move ninja from hand to stack zone, and return an unblocked attacker to hand.
    if ability.ninjutsu {
        // Find the ninja in hand by name and move it to a neutral zone (it will enter BF via effect).
        let ninja_hand_id = state.hand_of(who)
            .find(|c| c.name == source_name)
            .map(|c| c.id);
        if let Some(nid) = ninja_hand_id {
            // Keep it in cards but mark as Stack (it "enters via ninjutsu" when the ability resolves).
            if let Some(card) = state.cards.get_mut(&nid) {
                card.zone = CardZone::Stack;
            }
        }
        let unblocked_attacker = state.permanents_of(who)
            .find(|c| c.bf.as_ref().map_or(false, |bf| bf.attacking && bf.unblocked))
            .map(|c| (c.id, c.name.clone(), c.bf.as_ref().and_then(|bf| bf.attack_target)));
        if let Some((atk_id, atk_name, _atk_target)) = unblocked_attacker {
            if let Some(card) = state.cards.get_mut(&atk_id) {
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
            .find(|c| c.bf.as_ref().map_or(false, |bf| bf.mana_abilities.is_empty()))
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
            .any(|c| c.name != source_name && catalog_map.get(c.name.as_str())
                .map_or(false, |d| !d.is_land() && d.is_blue()));
        if !has_pitch {
            return false;
        }
    }
    if cost.bounce_island {
        if !state.permanents_of(who).any(|c| c.bf.as_ref().map_or(false, |bf| bf.mana_abilities.iter().any(|ma| ma.produces.contains('U')))) {
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
            .filter(|c| c.name != source_name
                && catalog_map.get(c.name.as_str()).map_or(false, |d| !d.is_land() && d.is_blue()))
            .map(|c| (c.id, c.name.clone()))
            .collect();
        let idx = rng.gen_range(0..pitch_ids.len());
        let (pitch_id, pitch_name) = pitch_ids[idx].clone();
        state.set_card_zone(pitch_id, CardZone::Exile { on_adventure: false });
        parts.push(format!("exile {}", pitch_name));
    }
    if cost.bounce_island {
        let bounce = state.permanents_of(who)
            .find(|c| c.bf.as_ref().map_or(false, |bf| bf.mana_abilities.iter().any(|ma| ma.produces.contains('U'))))
            .map(|c| (c.id, c.name.clone()))
            .unwrap();
        let (bounce_id, land_name) = bounce;
        if let Some(card) = state.cards.get_mut(&bounce_id) {
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
///
/// Returns `None` if the spell can't be cast (cost unpayable or card not in hand).
fn cast_spell(
    state: &mut SimState,
    t: u8,
    who: &str,
    name: &str,
    preferred_cost: Option<&AlternateCost>,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> Option<ObjId> {
    let def = *catalog_map.get(name)?;
    let mut cost = parse_mana_cost(def.mana_cost());

    // Delve: reduce generic cost by exiling cards from the caster's graveyard.
    let to_exile_ids: Vec<(ObjId, String)> = if def.delve() && cost.generic > 0 {
        let gy: Vec<(ObjId, String)> = state.graveyard_of(who).map(|c| (c.id, c.name.clone())).collect();
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
        // Caller specified the exact cost to use.
        Some(pc.clone())
    } else if !mana_is_usable {
        // Can't pay mana (or mana_cost is empty / alt-cost-only): try alternate costs.
        def.alternate_costs()
            .iter()
            .find(|c| can_pay_alternate_cost(c, state, who, name, catalog_map))
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

    // Remove the spell from hand, capturing its stable ObjId.
    let card_id = state.hand_of(who).find(|c| c.name == name).map(|c| c.id)?;
    // Move to Stack zone.
    if let Some(card) = state.cards.get_mut(&card_id) {
        card.zone = CardZone::Stack;
    }

    // Pay cost and build a log label.
    let cast_label = if let Some(ref cost) = alt_cost {
        let parts = apply_alt_cost_components(cost, state, t, who, name, catalog_map, rng);
        parts.join(", ")
    } else {
        let mana_log = state.pay_mana(who, &cost, t);
        state.log_mana_activations(t, who, mana_log);
        def.mana_cost().to_string()
    };

    // Exile delve cards from graveyard (cost payment).
    let to_exile_names: Vec<String> = to_exile_ids.iter().map(|(_, n)| n.clone()).collect();
    for (exile_id, _card_name) in &to_exile_ids {
        // Exile from GY: fires ZoneChange trigger (e.g. Murktide counter).
        change_zone(*exile_id, ZoneId::Exile, state, t, who, catalog_map);
    }

    // For delve permanents: encode +1/+1 counter count as "+N" in annotation.
    // Counters come from instants/sorceries exiled via delve (e.g. Murktide Regent).
    let annotation: Option<String> = if def.delve() && def.is_creature() {
        let count = to_exile_names.iter()
            .filter(|n| catalog_map.get(n.as_str())
                .map(|d| d.as_spell().is_some())
                .unwrap_or(false))
            .count() as i32;
        if count > 0 { Some(format!("+{}", count)) } else { None }
    } else {
        None
    };

    let delve_label = if to_exile_names.is_empty() {
        String::new()
    } else {
        format!(", delve: {}", to_exile_names.join(", "))
    };
    state.log(t, who, format!("Cast {} ({}{}) [hand: {}]", name, cast_label, delve_label, state.hand_size(who)));

    let (spell_target_spec, spell_eff) = build_spell_effect(def, who, annotation.clone());
    let spell_chosen_targets = choose_spell_target(&spell_target_spec, who, state, catalog_map, rng)
        .into_iter()
        .collect::<Vec<_>>();

    if let Some(card) = state.cards.get_mut(&card_id) {
        card.spell = Some(SpellState {
            effect: Some(spell_eff.clone()),
            chosen_targets: spell_chosen_targets.clone(),
            is_adventure_face: false,
            adventure_card_name: None,
            annotation: annotation.clone(),
        });
    }

    Some(card_id)
}


/// Hypergeometric P(≥1 copy of a card is in the "in-hand" portion of the library pool).
///
/// `library_size` — total cards remaining in the pool (hand + undrawn).
/// `hand_size`    — how many of those are conceptually in hand.
/// `copies`       — how many copies of the card exist in the pool.
#[allow(dead_code)]
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
/// On success, returns a `CastSpell` intent targeting `stack[target_idx]`.
/// No resources are spent; the caller (`handle_priority_round`) commits the action.
fn respond_with_counter(
    state: &SimState,
    target_idx: usize,
    responding_who: &str,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
    probabilistic: bool,
) -> Option<PriorityAction> {
    let default_kind;
    let target_name = state.stack_item_display_name(state.stack[target_idx]).to_string();
    let target_kind: &CardKind = match catalog_map.get(target_name.as_str()) {
        Some(d) => &d.kind,
        None => { default_kind = CardKind::Sorcery(SpellData::default()); &default_kind }
    };

    let target_owner_str = state.who_str(state.stack_item_owner(state.stack[target_idx])).to_string();
    let target_has_untapped_lands = state.permanents_of(&target_owner_str).any(|c| {
        c.bf.as_ref().map_or(false, |bf| !bf.tapped && !bf.mana_abilities.is_empty())
    });

    // Find counterspells in hand.
    let mut seen = std::collections::HashSet::new();
    let counterspells: Vec<String> = state.hand_of(responding_who)
        .filter_map(|c| {
            let def = catalog_map.get(c.name.as_str())?;
            let filter = def.target().and_then(|t| t.strip_prefix("stack:"))?;
            if !stack_filter_matches(filter, target_kind) { return None; }
            if def.alternate_costs().is_empty() { return None; }
            if c.name == "Daze" && target_has_untapped_lands { return None; }
            seen.insert(c.name.clone()).then(|| c.name.clone())
        })
        .collect();

    if counterspells.is_empty() {
        return None;
    }

    let hand_size = state.hand_size(responding_who);
    let lib_size = state.library_size(responding_who) + hand_size as usize;

    for cs_name in &counterspells {
        if probabilistic {
            // Roll: is this counterspell in our hand?
            let copies = state.hand_of(responding_who).filter(|c| c.name == *cs_name).count();
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
                let n_blue = state.hand_of(responding_who)
                    .filter(|c| c.name != *cs_name
                        && catalog_map.get(c.name.as_str()).map_or(false, |d| !d.is_land() && d.is_blue()))
                    .count();
                let p_have_blue = p_card_in_hand(lib_size, hand_size, n_blue);
                if !rng.gen_bool(p_have_blue.max(f64::MIN_POSITIVE)) { continue; }
            }
            if can_pay_alternate_cost(cost, state, responding_who, cs_name, catalog_map) {
                return Some(PriorityAction::CastSpell {
                    name: cs_name.clone(),
                    preferred_cost: Some(cost.clone()),
                });
            }
        }
    }
    None
}



// ── Combat helpers ────────────────────────────────────────────────────────────

/// Return (power, toughness) for a permanent, adding any +1/+1 counters and temporary mods.
fn creature_stats(bf: &BattlefieldState, def: Option<&CardDef>) -> (i32, i32) {
    let power     = def.and_then(|d| d.as_creature()).map(|c| c.power).unwrap_or(1);
    let toughness = def.and_then(|d| d.as_creature()).map(|c| c.toughness).unwrap_or(1);
    (power + bf.counters + bf.power_mod, toughness + bf.counters + bf.toughness_mod)
}

// ── Keyword helpers ───────────────────────────────────────────────────────────

fn creature_has_keyword(name: &str, kw: &str, catalog_map: &HashMap<&str, &CardDef>) -> bool {
    catalog_map.get(name).map(|d| d.has_keyword(kw)).unwrap_or(false)
}


/// Remove creatures whose accumulated damage meets or exceeds their toughness (SBA).
fn check_lethal_damage(who: &str, state: &mut SimState, t: u8, catalog_map: &HashMap<&str, &CardDef>) {
    let dead: Vec<(ObjId, String)> = state.permanents_of(who)
        .filter_map(|card| {
            let bf = card.bf.as_ref()?;
            let def = catalog_map.get(card.name.as_str());
            let (_, tgh) = creature_stats(bf, def.copied());
            if bf.damage >= tgh && def.map_or(false, |d| d.is_creature()) {
                Some((card.id, card.name.clone()))
            } else { None }
        })
        .collect();
    for (id, _name) in dead {
        change_zone(id, ZoneId::Graveyard, state, t, who, catalog_map);
    }
}

fn opp_of(who: &str) -> &'static str {
    if who == "us" { "opp" } else { "us" }
}

fn do_amass_orc(controller: &str, n: i32, state: &mut SimState, t: u8) {
    let army_id: Option<ObjId> = state.permanents_of(controller)
        .find(|c| c.name == "Orc Army")
        .map(|c| c.id);
    if let Some(army_id) = army_id {
        if let Some(bf) = state.permanent_bf_mut(army_id) {
            bf.counters += n;
        }
        let c = state.permanent_bf(army_id).map_or(0, |bf| bf.counters);
        state.log(t, controller, format!("Orc Army grows to {c}/{c}"));
    } else {
        let new_id = state.alloc_id();
        state.cards.insert(new_id, CardObject {
            id: new_id,
            name: "Orc Army".to_string(),
            owner: controller.to_string(),
            controller: controller.to_string(),
            zone: CardZone::Battlefield,
            spell: None,
            bf: Some(BattlefieldState {
                counters: n,
                ..BattlefieldState::new(vec![])
            }),
        });
        state.log(t, controller, format!("Orc Army token created {n}/{n}"));
    }
}

fn do_create_clue(controller: &str, state: &mut SimState, t: u8) {
    let new_id = state.alloc_id();
    state.cards.insert(new_id, CardObject {
        id: new_id,
        name: "Clue Token".to_string(),
        owner: controller.to_string(),
        controller: controller.to_string(),
        zone: CardZone::Battlefield,
        spell: None,
        bf: Some(BattlefieldState::new(vec![])),
    });
    state.log(t, controller, "Clue Token created");
}

fn do_flip_tamiyo(source_id: ObjId, controller: &str, state: &mut SimState, t: u8, catalog_map: &HashMap<&str, &CardDef>) {
    let loyalty = catalog_map.get("Tamiyo, Seasoned Scholar")
        .and_then(|d| if let CardKind::Planeswalker(ref p) = d.kind { Some(p.loyalty) } else { None })
        .unwrap_or(2);
    // Mutate in place via state.cards — same ObjId, preserves damage/entered_this_turn.
    if let Some(card) = state.cards.get_mut(&source_id) {
        card.name = "Tamiyo, Seasoned Scholar".to_string();
        if let Some(bf) = card.bf.as_mut() {
            bf.loyalty = loyalty;
            bf.active_face = 1;
        }
    }
    state.log(t, controller, format!("Tamiyo flips → Tamiyo, Seasoned Scholar [loyalty: {}]", loyalty));
}

/// Pop and resolve the top item of the stack.
///
/// If the top id is in `state.cards` it is a spell: runs its effect and moves the card to
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
    if state.cards.contains_key(&id) {
        // It's a spell (card on the stack)
        let spell = state.cards[&id].spell.clone().unwrap_or_else(|| SpellState {
            effect: None,
            chosen_targets: vec![],
            is_adventure_face: false,
            adventure_card_name: None,
            annotation: None,
        });
        let owner_str = state.cards[&id].owner.clone();
        let name = state.cards[&id].name.clone();

        if spell.is_adventure_face {
            if let Some(ref eff) = spell.effect {
                let rng_dyn: &mut dyn rand::RngCore = rng;
                eff.call(state, t, &spell.chosen_targets, catalog_map, rng_dyn);
            }
            let card_name = spell.adventure_card_name.as_deref().unwrap_or(&name).to_string();
            if let Some(card_obj) = state.cards.get_mut(&id) {
                card_obj.zone = CardZone::Exile { on_adventure: true };
                card_obj.spell = None;
            }
            state.log(t, &owner_str, format!("{} resolves → {} on adventure in exile", name, card_name));
        } else if let Some(ref eff) = spell.effect {
            let is_perm = catalog_map.get(name.as_str())
                .map(|d| matches!(d.kind, CardKind::Creature(_) | CardKind::Artifact(_)
                    | CardKind::Planeswalker(_) | CardKind::Enchantment))
                .unwrap_or(false);
            if !is_perm {
                if let Some(card_obj) = state.cards.get_mut(&id) {
                    card_obj.zone = CardZone::Graveyard;
                    card_obj.spell = None;
                }
                state.log(t, &owner_str, format!("{} resolves", name));
            }
            let rng_dyn: &mut dyn rand::RngCore = rng;
            eff.call(state, t, &spell.chosen_targets, catalog_map, rng_dyn);
            if is_perm {
                if let Some(card_obj) = state.cards.get_mut(&id) {
                    card_obj.spell = None;
                }
            }
        } else {
            if let Some(card_obj) = state.cards.get_mut(&id) {
                card_obj.zone = CardZone::Graveyard;
                card_obj.spell = None;
            }
            state.log(t, &owner_str, format!("{} resolves", name));
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
        push_triggers(queued, state, catalog_map);
        check_lethal_damage("us",  state, t, catalog_map);
        check_lethal_damage("opp", state, t, catalog_map);

        let who = priority_holder.clone();
        let action = decide_action(
            state, t, ap, &who, dd_turn, &last_action, catalog_map, rng,
        );
        last_action = action.clone();

        match action {
            PriorityAction::LandDrop(ref land_name) => {
                sim_play_land(state, t, &who, land_name, catalog_map);
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
                    .or_else(|| state.cards.get(&source_id).map(|c| c.name.clone()))
                    .unwrap_or_default();
                let (ability_effect, ability_targets): (Option<Effect>, Vec<Target>) = if ability.ninjutsu {
                    let attack_target = state.permanents_of(&who)
                        .find(|p| p.bf.as_ref().map_or(false, |bf| bf.attacking && bf.unblocked))
                        .and_then(|p| p.bf.as_ref().and_then(|bf| bf.attack_target));
                    let who_str = who.clone();
                    let ninja_effect = Effect(std::sync::Arc::new(move |state, t, _targets, catalog_map, _rng| {
                        let ninja_name = state.cards.get(&source_id)
                            .map(|c| c.name.clone())
                            .unwrap_or_default();
                        if ninja_name.is_empty() { return; }
                        let mana_abs = catalog_map.get(ninja_name.as_str())
                            .map_or_else(Vec::new, |d| d.mana_abilities().to_vec());
                        let new_id = state.alloc_id();
                        state.cards.insert(new_id, CardObject {
                            id: new_id,
                            name: ninja_name.clone(),
                            owner: who_str.clone(),
                            controller: who_str.clone(),
                            zone: CardZone::Battlefield,
                            spell: None,
                            bf: Some(BattlefieldState {
                                tapped: true,
                                entered_this_turn: true,
                                mana_abilities: mana_abs,
                                attacking: true,
                                unblocked: true,
                                attack_target,
                                ..BattlefieldState::new(vec![])
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
            PriorityAction::CastAdventure { ref card_name } => {
                let face = catalog_map.get(card_name.as_str())
                    .and_then(|d| d.adventure())
                    .cloned();
                let Some(face) = face else {
                    last_passer = Some(who.clone());
                    priority_holder = if who == ap { nap.to_string() } else { ap.to_string() };
                    continue;
                };
                let is_sorcery = face.card_type == "sorcery";
                if is_sorcery && !state.stack.is_empty() {
                    eprintln!("[priority] adventure sorcery {} on non-empty stack, treating as Pass", face.name);
                    last_passer = Some(who.clone());
                    priority_holder = if who == ap { nap.to_string() } else { ap.to_string() };
                    continue;
                };
                let adv_card_id_opt = state.hand_of(&who).find(|c| c.name == *card_name).map(|c| c.id);
                let Some(adv_card_id) = adv_card_id_opt else {
                    last_passer = Some(who.clone());
                    priority_holder = if who == ap { nap.to_string() } else { ap.to_string() };
                    continue;
                };
                if let Some(card) = state.cards.get_mut(&adv_card_id) {
                    card.zone = CardZone::Stack;
                }
                if !face.mana_cost.is_empty() {
                    let cost = parse_mana_cost(&face.mana_cost);
                    let mana_log = state.pay_mana(&who, &cost, t);
                    state.log_mana_activations(t, &who, mana_log);
                }
                let (adv_spec, adv_eff) = build_adventure_effect(&face, &who);
                let adv_targets = choose_spell_target(&adv_spec, &who, state, catalog_map, rng)
                    .into_iter().collect::<Vec<_>>();
                state.log(t, &who, format!("Cast {} (adventure, {}) [hand: {}]", face.name, face.mana_cost, state.hand_size(&who)));
                if let Some(card) = state.cards.get_mut(&adv_card_id) {
                    card.spell = Some(SpellState {
                        effect: Some(adv_eff.clone()),
                        chosen_targets: adv_targets.clone(),
                        is_adventure_face: true,
                        adventure_card_name: Some(card_name.clone()),
                        annotation: None,
                    });
                }
                state.stack.push(adv_card_id);
                state.player_mut(&who).spells_cast_this_turn += 1;
                let next = if who == ap { nap } else { ap };
                priority_holder = next.to_string();
                last_passer = None;
            }
            PriorityAction::CastFromAdventure { ref card_name } => {
                if !state.on_adventure_of(&who).any(|c| c.name == *card_name) {
                    last_passer = Some(who.clone());
                    priority_holder = if who == ap { nap.to_string() } else { ap.to_string() };
                    continue;
                }
                let Some(&def) = catalog_map.get(card_name.as_str()) else {
                    last_passer = Some(who.clone());
                    priority_holder = if who == ap { nap.to_string() } else { ap.to_string() };
                    continue;
                };
                let cost = parse_mana_cost(def.mana_cost());
                if !state.potential_mana(&who).can_pay(&cost) {
                    last_passer = Some(who.clone());
                    priority_holder = if who == ap { nap.to_string() } else { ap.to_string() };
                    continue;
                }
                let card_obj_id = state.on_adventure_of(&who).find(|c| c.name == *card_name).map(|c| c.id);
                if let Some(cid) = card_obj_id {
                    if let Some(card_obj) = state.cards.get_mut(&cid) {
                        card_obj.zone = CardZone::Stack;
                    }
                }
                let mana_log = state.pay_mana(&who, &cost, t);
                state.log_mana_activations(t, &who, mana_log);
                state.log(t, &who, format!("Cast {} from adventure ({})", card_name, def.mana_cost()));
                let (from_adv_spec, from_adv_eff) = build_spell_effect(def, &who, None);
                let from_adv_targets = choose_spell_target(&from_adv_spec, &who, state, catalog_map, rng)
                    .into_iter()
                    .collect::<Vec<_>>();
                if let Some(cid) = card_obj_id {
                    if let Some(card) = state.cards.get_mut(&cid) {
                        card.spell = Some(SpellState {
                            effect: Some(from_adv_eff.clone()),
                            chosen_targets: from_adv_targets.clone(),
                            is_adventure_face: false,
                            adventure_card_name: None,
                            annotation: None,
                        });
                    }
                }
                state.stack.push(card_obj_id.unwrap_or(ObjId::UNSET));
                state.player_mut(&who).spells_cast_this_turn += 1;
                let next = if who == ap { nap } else { ap };
                priority_holder = next.to_string();
                last_passer = None;
            }
            PriorityAction::CastSpell { ref name, ref preferred_cost } => {
                let is_instant = catalog_map.get(name.as_str())
                    .map(|d| d.is_instant())
                    .unwrap_or(false);
                if !is_instant && !state.stack.is_empty() {
                    eprintln!("[priority] BUG: sorcery-speed {} on non-empty stack (stack={}), treating as Pass", name,
                        state.stack.iter().map(|&id| state.stack_item_display_name(id)).collect::<Vec<_>>().join(", "));
                    debug_assert!(false, "BUG: sorcery-speed cast of {} on non-empty stack", name);
                    last_passer = Some(who.clone());
                    let other = if who == ap { nap } else { ap };
                    priority_holder = other.to_string();
                } else {
                    if let Some(card_id) = cast_spell(state, t, &who, name,
                                                   preferred_cost.as_ref(), catalog_map, rng) {
                        if name == "Doomsday" && who == "us" { state.us.dd_cast = true; }
                        state.player_mut(&who).spells_cast_this_turn += 1;
                        state.stack.push(card_id);
                        let next = if who == ap { nap } else { ap };
                        priority_holder = next.to_string();
                        last_passer = None;
                    } else {
                        let pool = &state.player(&who).pool;
                        eprintln!("[priority] BUG: cast_spell failed for {} by {} (pool B={} U={} tot={}, hand={})",
                            name, who, pool.b, pool.u, pool.total, state.hand_size(&who));
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
            let cleanup_ids: Vec<ObjId> = state.permanents_of(ap).map(|c| c.id).collect();
            for id in cleanup_ids {
                if let Some(bf) = state.permanent_bf_mut(id) {
                    bf.damage = 0;
                }
            }
            // Expire EndOfTurn continuous effects, undoing any StatMod they applied.
            let expiring: Vec<ContinuousEffect> = state.active_effects.iter()
                .filter(|e| e.expires == EffectExpiry::EndOfTurn)
                .cloned()
                .collect();
            state.active_effects.retain(|e| e.expires != EffectExpiry::EndOfTurn);
            for effect in expiring {
                if let Some(sm) = &effect.stat_mod {
                    if let Some(bf) = state.permanent_bf_mut(sm.target_id) {
                        bf.power_mod -= sm.power_delta;
                        bf.toughness_mod -= sm.toughness_delta;
                    }
                }
            }
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
                    let p = state.cards.get(&atk_id)?;
                    let target_name = p.bf.as_ref()?.attack_target
                        .and_then(|id| state.permanent_name(id))
                        .unwrap_or_else(|| "player".to_string());
                    Some(format!("{} → {}", p.name, target_name))
                }).collect();
                state.log(t, ap, format!("Declare attackers: {}", atk_descs.join(", ")));
            }
            state.combat_attackers = attackers.clone();
            // Fire triggers after attackers are marked.
            for atk_id in attackers {
                state.queue_triggers(&GameEvent::CreatureAttacked {
                    attacker_id: atk_id,
                    attacker_controller: ap.to_string(),
                });
            }
            state.queue_triggers(&GameEvent::EnteredStep {
                step: StepKind::DeclareAttackers,
                active_player: ap.to_string(),
            });
        }
        StepKind::DeclareBlockers => {
            let nap = opp_of(ap);
            // Strategy decides which blockers to assign.
            let blocks = declare_blockers(ap, state, catalog_map);
            for &(atk_id, blk_id) in &blocks {
                let atk_name = state.cards.get(&atk_id).map(|p| p.name.as_str()).unwrap_or("");
                let blk_name = state.cards.get(&blk_id).map(|p| p.name.clone()).unwrap_or_default();
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
                    let atk_pow = state.cards.get(&atk_id)
                        .and_then(|p| p.bf.as_ref().map(|bf| creature_stats(bf, catalog_map.get(p.name.as_str()).copied()).0))
                        .unwrap_or(1);
                    let blk_pow = state.cards.get(&blk_id)
                        .and_then(|p| p.bf.as_ref().map(|bf| creature_stats(bf, catalog_map.get(p.name.as_str()).copied()).0))
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
                        let (atk_pow, attack_target) = state.cards.get(&atk_id)
                            .and_then(|p| p.bf.as_ref().map(|bf| {
                                (creature_stats(bf, catalog_map.get(p.name.as_str()).copied()).0, bf.attack_target)
                            }))
                            .unwrap_or((1, None));
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

                // SBAs: lethal damage check on both boards.
                for owner in [ap, nap] {
                    let dying: Vec<(ObjId, String)> = state.permanents_of(owner)
                        .filter_map(|p| {
                            let bf = p.bf.as_ref()?;
                            if bf.damage == 0 { return None; }
                            let def = catalog_map.get(p.name.as_str()).copied();
                            let (_, tgh) = creature_stats(bf, def);
                            if bf.damage >= tgh { Some((p.id, p.name.clone())) } else { None }
                        })
                        .collect();
                    for (id, name) in dying {
                        state.log(t, owner, format!("{} dies", name));
                        if let Some(card) = state.cards.get_mut(&id) {
                            card.zone = CardZone::Graveyard;
                            card.bf = None;
                        }
                    }
                }

                // SBA: destroy planeswalkers with loyalty ≤ 0.
                for owner in [ap, nap] {
                    let dying_pw: Vec<(ObjId, String)> = state.permanents_of(owner)
                        .filter(|p| {
                            catalog_map.get(p.name.as_str())
                                .map_or(false, |def| matches!(def.kind, CardKind::Planeswalker(_)))
                                && p.bf.as_ref().map_or(false, |bf| bf.loyalty <= 0)
                        })
                        .map(|p| (p.id, p.name.clone()))
                        .collect();
                    for (id, name) in dying_pw {
                        state.log(t, owner, format!("{} is destroyed (loyalty 0)", name));
                        if let Some(card) = state.cards.get_mut(&id) {
                            card.zone = CardZone::Graveyard;
                            card.bf = None;
                        }
                    }
                }
            }
        }
        StepKind::EndCombat => {
            state.combat_attackers.clear();
            state.combat_blocks.clear();
            let all_ids: Vec<ObjId> = state.cards.values()
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
        state.queue_triggers(&step_ev);
    }

    if step.prio {
        handle_priority_round(state, t, ap, dd_turn, catalog_map, rng);
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
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) {
    // Collect unactivated PW IDs + names + their current loyalty snapshot.
    let pw_data: Vec<(ObjId, String, i32)> = state.permanents_of(ap)
        .filter(|p| !p.bf.as_ref().map_or(false, |bf| bf.pw_activated_this_turn))
        .filter(|p| catalog_map.get(p.name.as_str())
            .map_or(false, |def| matches!(def.kind, CardKind::Planeswalker(_))))
        .map(|p| (p.id, p.name.clone(), p.bf.as_ref().map_or(0, |bf| bf.loyalty)))
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
        handle_priority_round(state, t, ap, dd_turn, catalog_map, rng);
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
        state.queue_triggers(&phase_ev);
        let on_board = collect_on_board_actions(state, ap, t, dd_turn, catalog_map, rng);
        state.player_mut(ap).pending_actions = on_board;
        handle_priority_round(state, t, ap, dd_turn, catalog_map, rng);
        // Mana pool drains at the end of the main phase.
        state.us.pool.drain();
        state.opp.pool.drain();
        if state.done() { return; }
        // Activate each AP planeswalker's loyalty ability (100% — runs after all other actions).
        activate_planeswalkers(state, t, ap, dd_turn, catalog_map, rng);
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

    // Populate state.cards with Library-zone objects for each player's mainboard.
    for (name, qty, board) in all_cards {
        if board != "main" { continue; }
        if catalog_map.get(name.as_str()).is_none() { continue; }
        for _ in 0..*qty {
            let id = state.alloc_id();
            state.cards.insert(id, CardObject::new(id, name.clone(), "us"));
        }
    }
    for (name, qty, board) in opp_cards {
        if board != "main" { continue; }
        if catalog_map.get(name.as_str()).is_none() { continue; }
        for _ in 0..*qty {
            let id = state.alloc_id();
            state.cards.insert(id, CardObject::new(id, name.clone(), "opp"));
        }
    }

    // Deal opening hands: move `7 - mulligans` cards from Library to Hand.
    for _ in 0..(7u8.saturating_sub(our_mulligans)) {
        state.sim_draw("us", 0, false);
    }
    for _ in 0..(7u8.saturating_sub(opp_mulligans)) {
        state.sim_draw("opp", 0, false);
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
            // All cards are already in their correct zones in state.cards.
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

