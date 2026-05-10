//! Mutation sub-language. `Action` variants describe *structural* operations;
//! named MTG mechanics (flashback, cascade, pump-until-EOT) are compositions
//! built via sugar helpers, not Action variants.

use crate::ir::ce::CEMod;
use crate::ir::expr::{Expr, Filter, ZoneKindSel};
use crate::CounterType;

/// How long a `CEMod` application lasts.
#[derive(Clone)]
pub(crate) enum Expiry {
    EndOfTurn,
    EndOfCombat,
    UntilYourNextTurn,
    WhileSourcePresent,
    Permanent,
}

/// Who is performing the action / receiving the choice.
#[derive(Clone)]
pub(crate) enum Who {
    You,
    EachOpponent,
    Opponent,
    Player(Expr), // resolves to PlayerId
    Each,         // all players in APNAP order
}

/// Selector for a choose-one / choose-mode effect.
///
/// `cost` (if present) must be paid by the chooser to pick this option; the
/// executor filters out unpayable options before presenting the remaining set
/// to the strategy. This is the structural decomposition of "unless X pays Y"
/// patterns — no `CounterUnlessPays`-style named primitive exists by design.
#[derive(Clone)]
pub(crate) struct ChoiceOption {
    pub label: &'static str,
    pub cost: Option<Vec<crate::CostComponent>>,
    pub action: Box<Action>,
}

/// One-shot mutations.
#[derive(Clone)]
pub(crate) enum Action {
    // ── state movement ───────────────────────────────────────────────────
    /// Move `what` to zone `to`. The object's current zone is read from state
    /// — no `from` field is required. (`change_zone` handles all departures
    /// uniformly.)
    Move {
        what: Expr,             // object or set
        to: ZoneKindSel,
        to_owner: Option<Expr>, // default: same owner
        bind_as: Option<&'static str>,
    },
    Search {
        who: Who,
        zone: ZoneKindSel,
        filter: Filter,
        count: Expr,
        dest: ZoneKindSel,
        shuffle: bool,
        bind_as: Option<&'static str>,
    },
    Return {
        what: Expr,
        to: ZoneKindSel,
        bind_as: Option<&'static str>,
    },
    Discard {
        who: Who,
        count: Expr,
        at_random: bool,
        filter: Option<Filter>,
    },
    Mill {
        who: Who,
        count: Expr,
    },

    // ── stack / casting ──────────────────────────────────────────────────
    Counter {
        target: Expr,
    },
    /// "Offer to cast X" — subsumes cast-without-paying, flashback, cascade,
    /// madness, Snapcaster, etc. All are `OfferCast` with different
    /// `permissions` CEMods.
    OfferCast {
        what: Expr,
        permissions: Vec<CEMod>,
    },
    /// CR 706: create `n` copies of the spell referenced by `what` as stack
    /// objects. Each copy resolves with the same effect as the original.
    /// Subsumes storm (n = spells-cast-this-turn), Reverberate (n = 1),
    /// Thousand-Year Storm, fork, etc.
    ///
    /// `new_targets`: if true, the controller of each copy may pick new
    /// targets (CR 706.10f); otherwise the copy inherits the original's
    /// targets. Engine default: prefer legal targets not yet hit.
    CopySpell {
        what: Expr,
        n: Expr,
        new_targets: bool,
    },

    // ── player effects ───────────────────────────────────────────────────
    Draw {
        who: Who,
        n: Expr,
    },
    DealDamage {
        source: Expr,
        target: Expr,
        amount: Expr,
    },
    PayLife {
        who: Who,
        amount: Expr,
    },
    GainLife {
        who: Who,
        amount: Expr,
    },

    // ── counters ─────────────────────────────────────────────────────────
    PutCounters {
        on: Expr,
        kind: CounterType,
        n: Expr,
    },
    RemoveCounters {
        from: Expr,
        kind: CounterType,
        n: Expr,
    },

    // ── tap / untap ──────────────────────────────────────────────────────
    /// Tap a permanent. CR 701.20a. Universal primitive — used by direct
    /// effects ("tap target permanent"), replacement bodies that compose
    /// "enters tapped" as `Sequence([Move, Tap])`, and (eventually) cost
    /// payment. The cost-payment path still routes through
    /// `CostComponent::TapSelf` until the cost sub-language is unified.
    Tap {
        target: Expr,
    },
    /// Untap a permanent. CR 701.21a. Symmetric with `Tap`.
    Untap {
        target: Expr,
    },

    // ── destruction / targeting ──────────────────────────────────────────
    Destroy {
        target: Expr,
    },
    Exile {
        target: Expr,
        bind_as: Option<&'static str>,
    },
    Sacrifice {
        who: Who,
        filter: Filter,
        count: Expr,
        bind_as: Option<&'static str>,
    },
    /// Cost-tree primitive: return `count` permanents matching `filter` from
    /// the battlefield to their owners' hands. Mirrors `Sacrifice`'s shape.
    /// `bind_as` is the schema-binding name the strategy answers under and
    /// the executor reads from at run time — required for cost-tree usage
    /// because the IR executor consumes the binding rather than calling a
    /// callback (cf. Sacrifice, which still uses `state.sacrifice_choice`
    /// because its single migrated cost shape only ever has one candidate
    /// — the source itself).
    ReturnFromBattlefield {
        who: Who,
        filter: Filter,
        count: Expr,
        bind_as: Option<&'static str>,
    },

    // ── continuous-effect application ────────────────────────────────────
    /// Apply a bundle of CE modifications to `target` until `expiry`.
    /// Subsumes pump-until-EOT, grant-flash, gain-protection, etc.
    ApplyCE {
        target: Expr,
        mods: Vec<CEMod>,
        expiry: Expiry,
    },

    // ── control flow ─────────────────────────────────────────────────────
    Sequence(Vec<Action>),
    IfThen {
        cond: Expr,
        then: Box<Action>,
        else_: Option<Box<Action>>,
    },
    MayDo {
        who: Who,
        action: Box<Action>,
    },
    ForEach {
        over: Expr,           // set
        bind: &'static str,
        body: Box<Action>,
    },
    Choose {
        who: Who,
        prompt: &'static str,
        options: Vec<ChoiceOption>,
    },

    // ── scheduling ───────────────────────────────────────────────────────
    /// Register a delayed trigger that fires at some future event.
    ScheduleDelayedTrigger {
        fires: crate::ir::ability::TriggerSpec,
        action: Box<Action>,
    },
    /// CR 611.2f: register a latent continuous effect that applies to the
    /// next qualifying spell `who` casts. The `mods` bundle is applied as a
    /// continuous instance filtered to that spell; the LatentSpellMod itself
    /// is consumed once a matching spell is announced. `expiry` governs both
    /// the latent registration (if no qualifying spell is cast in time) and
    /// the applied CE.
    GrantCEToNextSpellCast {
        who: Who,
        predicate: Option<Filter>,
        mods: Vec<CEMod>,
        expiry: Expiry,
    },

    // ── information ──────────────────────────────────────────────────────
    Scry {
        who: Who,
        n: Expr,
    },
    Surveil {
        who: Who,
        n: Expr,
    },
    Reveal {
        who: Who,
        what: Expr,
    },
    Look {
        who: Who,
        zone: ZoneKindSel,
        n: Expr,
    },

    // ── token creation ───────────────────────────────────────────────────
    CreateToken {
        who: Who,
        spec: TokenSpec,
        n: Expr,
    },

    // ── mana production ──────────────────────────────────────────────────
    /// Add `count` mana to `who`'s mana pool. The `spec` describes the colors
    /// produced; for `AnyOneColor`, the chosen color is read from
    /// `BindEnv.chosen_color` (set by the activated-ability dispatch when the
    /// player picks a color at activation time).
    ///
    /// Fungible with destroy / draw / etc. — runs through `execute()` like any
    /// other action. The CR 605 stack-bypass distinction (mana ability vs.
    /// regular activated ability) is determined statically by inspecting the
    /// enclosing ability's body for any reachable `AddMana`, not by a separate
    /// `AbilityKind` variant.
    AddMana {
        who: Who,
        count: Expr,
        spec: ManaSpec,
    },

    // ── mana payment ─────────────────────────────────────────────────────
    /// Drain `cost` from the controller's mana pool. Symmetric with `AddMana`.
    /// CR 601.2g/h. Pool-based: this is just the *demand*. Mana abilities
    /// (the *supply*) are activated separately by the strategy as ordinary
    /// playable actions; when the pool can't yet pay, the cost driver yields
    /// control back to the strategy to activate more mana.
    PayMana(crate::ManaCost),

    // ── planeswalker loyalty ─────────────────────────────────────────────
    /// Activate-cost adjustment to the source's loyalty (CR 606.5). Sets
    /// `pw_activated_this_turn` so each planeswalker activates at most once
    /// per turn (CR 606.3c). `n` is signed: +1 for "+1: …" abilities, −X
    /// for "−X: …".
    LoyaltyAdjust(i32),

    // ── replicate ────────────────────────────────────────────────────────
    /// CR 702.58. Pay `cost` zero-or-more extra times at announcement; each
    /// extra payment creates a copy of the spell on the stack. Only valid
    /// inside a cast cost tree (not an arbitrary effect body).
    Replicate(crate::ManaCost),

    // ── library placement ────────────────────────────────────────────────
    /// Move `count` cards from zone `from` (owned by `who`) onto their
    /// library — `top` = top, `!top` = bottom. Agency: strategy picks which
    /// cards via `state.evaluate_card` (worst first for put-back semantics).
    PutOnLibrary {
        who: Who,
        count: Expr,
        from: ZoneKindSel,
        top: bool,
    },

    // ── noop ─────────────────────────────────────────────────────────────
    Noop,
}

/// What mana an `AddMana` action produces.
#[derive(Clone)]
pub(crate) enum ManaSpec {
    /// Fixed colors (e.g. `[Blue]` for Island, `[]` for a colorless source).
    /// If shorter than `count`, the remainder is padded with colorless.
    Fixed(Vec<crate::Color>),
    /// All produced mana is one color, chosen at activation. The chosen color
    /// is read from `BindEnv.chosen_color`. (Lotus Petal, Lion's Eye Diamond,
    /// Mox Opal, Birds of Paradise.)
    AnyOneColor,
}

/// Token specification — kept minimal; grows as token-generating cards land.
#[derive(Clone)]
pub(crate) struct TokenSpec {
    pub name: &'static str,
    pub types: Vec<crate::CardType>,
    pub subtypes: Vec<&'static str>,
    pub colors: Vec<crate::Color>,
    pub power: Option<i64>,
    pub toughness: Option<i64>,
    pub keywords: Vec<crate::Keyword>,
}
