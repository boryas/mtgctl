//! Ability wrappers — the five CR ability kinds as a closed enum.

use crate::ir::action::Action;
use crate::ir::ce::CEMod;
use crate::ir::expr::{Expr, Filter};

/// A card-authored ability. One card has `Vec<Ability>`.
#[derive(Clone)]
pub(crate) struct Ability {
    pub kind: AbilityKind,
    pub text: Option<&'static str>, // Oracle snippet for docs / round-trip
}

/// The five CR ability kinds. Mirrors the event-timing taxonomy.
#[derive(Clone)]
pub(crate) enum AbilityKind {
    /// "When/whenever/at [event], [effect]." Fires *after* the event.
    Triggered {
        spec: TriggerSpec,
        /// Target spec for the triggered ability. Default `TargetSpec::None`
        /// for triggers whose body references the triggering event directly
        /// (no player choice).
        target_spec: crate::TargetSpec,
        body: Action,
        /// Zone in which the source must reside for this trigger to be armed.
        /// Battlefield for permanents (default); Stack for self-triggers on
        /// spells (storm, cascade, "when you cast this spell").
        active_zone: crate::ir::expr::ZoneKindSel,
    },
    /// "As [x], …" / "If [event] would happen, instead [other]." Modifies the
    /// event as it happens. Body is `ReplacementBody`, not `Action` — a
    /// replacement is a structured transformation, not an arbitrary effect.
    ///
    /// `condition` is an optional extra predicate evaluated against the match
    /// bindings — used for "unless X" wording where the replacement only fires
    /// when some game-state condition holds (e.g. Mistrise Village: enters
    /// tapped *unless* you control a Mountain or Forest).
    Replacement {
        matches: EventPattern,
        condition: Option<Expr>,
        body: ReplacementBody,
    },
    /// "[x] can't [y]." Prevents matching events from occurring at all.
    Prohibition {
        matches: EventPattern,
    },
    /// Continuous effect: while source is active, apply these CE mods.
    Static {
        mods: Vec<CEMod>,
        /// Scope: what the CE applies to. `None` = global; else filter on
        /// candidate objects/players.
        scope: Option<Filter>,
    },
    /// "[cost]: [effect]." Mana abilities (CR 605.1a) are NOT a separate
    /// variant — they are activated abilities whose body could produce mana
    /// and whose `target_spec` is `TargetSpec::None`. The executor classifies
    /// them via `is_mana_ability` and routes through the synchronous
    /// stack-bypass path (CR 605.3b) automatically.
    Activated {
        cost: CostBody,
        /// Target spec for the ability. `TargetSpec::None` is the no-target
        /// case (Clue Token, Karn's +1, Birds of Paradise).
        target_spec: crate::TargetSpec,
        /// Resolution-time object choice ("Choose an exiled card …" — CR
        /// 701.10). Distinct from `target_spec`, which is announcement-time
        /// targeting (CR 113 / 601.2c). Chosen id is passed to the body via
        /// the `target` binding alongside announced targets.
        choice_spec: Option<crate::ChoiceSpec>,
        body: Action,
        /// Activation timing. `Default` = instant speed and allowed in the
        /// mana sub-loop (CR 601.2g) when this is a mana ability. `Instant` =
        /// instant speed but excluded from the mana sub-loop (Lion's Eye
        /// Diamond). `Sorcery` = main phase, empty stack only.
        timing: crate::ActivationTiming,
        /// "Activate only if X" restriction (Mox Opal metalcraft). `None` =
        /// always activatable (subject to other costs/timing).
        activation_condition: Option<Expr>,
        /// Zone the source must reside in to activate. Default: Battlefield.
        /// Hand for Simian Spirit Guide-style cards.
        active_zone: crate::ir::expr::ZoneKindSel,
    },
    /// Spell resolution body. Not a CR-112 ability — a spell's effect on
    /// resolution belongs to the spell itself, not to an ability it has —
    /// but represented in `AbilityKind` for uniform engine dispatch.
    ///
    /// `modes` mirrors `catalog::SpellModes`: non-modal spells have
    /// `modes.len() == 1`; modal spells (CR 700.2) have one entry per mode.
    /// The cast submachine selects the mode index; target_spec for that mode
    /// governs target selection.
    OnResolve {
        modes: Vec<IrSpellMode>,
    },
}

/// Trigger specification — what event fires this ability.
#[derive(Clone)]
pub(crate) enum TriggerSpec {
    /// Event pattern + optional extra condition (typically controller scope
    /// or object identity).
    When {
        pattern: EventPattern,
        condition: Option<Expr>,
    },
    /// Phase / step triggers ("at the beginning of your upkeep").
    AtStep {
        step: crate::StepKind,
        who: StepScope,
    },
}

/// Which player's step triggers this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StepScope {
    You,
    EachOpponent,
    EachPlayer,
    ActivePlayer,
}

/// Pattern over `GameEvent`. Matches structurally — the engine translates each
/// `GameEvent` variant into field bindings for the enclosing expression tree.
#[derive(Clone)]
pub(crate) enum EventPattern {
    /// Any event — used rarely (Orcish Bowmasters fires on any draw).
    Any,

    /// An object enters a zone.
    EntersZone {
        obj_filter: Filter,
        zone_kind: crate::ir::expr::ZoneKindSel,
    },
    /// An object leaves a zone.
    LeavesZone {
        obj_filter: Filter,
        zone_kind: crate::ir::expr::ZoneKindSel,
    },
    /// An object moves between specific zones (CR 603.6a). More precise than
    /// EntersZone/LeavesZone alone when the trigger cares about the *pair*.
    /// `actor_filter` (optional) matches the player who caused the move; used
    /// for "whenever you exile X" style triggers.
    ZoneChange {
        obj_filter: Filter,
        from: crate::ir::expr::ZoneKindSel,
        to: crate::ir::expr::ZoneKindSel,
        actor_filter: Option<Filter>,
    },
    /// A creature dies (leaves battlefield for graveyard).
    Dies {
        obj_filter: Filter,
    },
    /// A spell is cast.
    SpellCast {
        spell_filter: Filter,
    },
    /// A player draws one or more cards.
    Draw {
        who: Filter, // predicate on player
    },
    /// A player plays a land.
    LandPlayed {
        who: Filter,
        land_filter: Filter,
    },
    /// Damage dealt.
    DamageDealt {
        source_filter: Filter,
        target_filter: Filter,
        is_combat: Option<bool>,
    },
    /// Creature attacks.
    Attacks {
        attacker_filter: Filter,
    },
    /// Creature blocks.
    Blocks {
        blocker_filter: Filter,
    },

    /// Conjunction — all of these patterns must match simultaneously.
    And(Vec<EventPattern>),
}

/// How a replacement effect changes the event.
#[derive(Clone)]
pub(crate) enum ReplacementBody {
    /// Replace the event with a different action. The action sees the matched
    /// event's bindings (e.g. `Var("triggered_obj")`) via the `BindEnv`
    /// populated by `match_event_pattern`. Used for: "enters tapped" composed
    /// as `Sequence([Move(target → BF), Tap(target)])`; Leyline of the Void
    /// composed as `Sequence([Move(target → Exile), …])`; Containment Priest
    /// "exile instead of entering."
    ///
    /// CR 614.5 self-loop guard is engine-enforced — the same replacement
    /// won't re-fire on events the action body produces.
    Replace(Action),
    /// Prevent (CR 615) — damage/effect does not occur.
    Prevent,
}

/// Cost of an activated ability or alternate spell cost.
///
/// Phase 0 of the cost-IR migration: storage holds either the legacy
/// `Vec<CostComponent>` (unmigrated cards) or an `Action` tree (cards
/// ported to the IR cost grammar). The two paths run independently —
/// `cast_spell` and `pay_ability_cost` branch on this enum and dispatch
/// to either the legacy executor (`pay_costs`) or the IR cost executor.
#[derive(Clone)]
pub(crate) enum CostBody {
    Legacy(Vec<crate::CostComponent>),
    Ir(Action),
}

impl Default for CostBody {
    fn default() -> Self {
        CostBody::Legacy(Vec::new())
    }
}

impl CostBody {
    /// Empty legacy cost — convenience used by `Default for AlternateCost`.
    pub(crate) fn empty() -> Self {
        CostBody::Legacy(Vec::new())
    }

    /// Extract the legacy component vector. Panics on `Ir(_)` — callers on
    /// the legacy-only path should reach here only for cards still using
    /// `CostBody::Legacy`. Phase 1 introduces a real IR executor; until
    /// then no card emits `Ir(_)` so this never fires.
    pub(crate) fn expect_legacy(&self) -> &Vec<crate::CostComponent> {
        match self {
            CostBody::Legacy(v) => v,
            CostBody::Ir(_) => panic!("CostBody::Ir reached legacy executor — Phase 1 not implemented"),
        }
    }

    /// True if this is a legacy variant carrying no components.
    pub(crate) fn is_empty_legacy(&self) -> bool {
        matches!(self, CostBody::Legacy(v) if v.is_empty())
    }

    /// True iff this cost requires tapping the source — used by the mana
    /// affordability predictor (`accumulate_source_potential`) to skip
    /// already-tapped sources. Variant-agnostic: `Legacy` scans for
    /// `CostComponent::TapSelf`; `Ir` walks for `Action::Tap { Source }`.
    pub(crate) fn requires_tap_self(&self) -> bool {
        match self {
            CostBody::Legacy(v) => {
                v.iter().any(|c| matches!(c, crate::CostComponent::TapSelf))
            }
            CostBody::Ir(a) => action_includes_tap_source(a),
        }
    }

    /// True iff this cost requires sacrificing the source — used by the
    /// affordability predictor to mark a source as no longer available
    /// after activation. Variant-agnostic: `Legacy` scans for
    /// `CostComponent::SacSelf`; `Ir` walks for the `MoveByChoice` shape
    /// (BF→GY, verb=Sacrifice, filter=It==Source) or the legacy
    /// `Action::Sacrifice` with the same filter.
    pub(crate) fn requires_sac_self(&self) -> bool {
        match self {
            CostBody::Legacy(v) => {
                v.iter().any(|c| matches!(c, crate::CostComponent::SacSelf))
            }
            CostBody::Ir(a) => action_includes_sac_source(a),
        }
    }

    /// True iff payment of this cost involves any mana spend. Used by
    /// `cast_spell` to set `SpellCast::mana_spent` correctly across both
    /// storage variants. For `Legacy`, scans for `CostComponent::Mana(_)`;
    /// for `Ir`, walks the action tree for `Action::PayMana(_)`.
    pub(crate) fn includes_mana(&self) -> bool {
        match self {
            CostBody::Legacy(v) => {
                v.iter().any(|c| matches!(c, crate::CostComponent::Mana(_)))
            }
            CostBody::Ir(a) => action_includes_pay_mana(a),
        }
    }

    /// Extract the (first) mana cost component, if any. Used by `cast_spell`
    /// when computing the `mana_cost` to drain from the pool for the
    /// alt-cost path. Returns `None` for IR costs (alt-cost mana costs
    /// haven't migrated yet — when they do, return the `PayMana(mc)` value).
    pub(crate) fn first_mana_cost(&self) -> Option<crate::ManaCost> {
        match self {
            CostBody::Legacy(v) => v.iter().find_map(|c| match c {
                crate::CostComponent::Mana(mc) => Some(mc.clone()),
                _ => None,
            }),
            CostBody::Ir(a) => first_pay_mana(a),
        }
    }
}

fn action_includes_sac_source(a: &Action) -> bool {
    use crate::ir::action::Action::*;
    use crate::ir::action::MoveVerb;
    use crate::ir::expr::ZoneKindSel;
    let filter_self = |f: &crate::ir::expr::Filter| {
        let crate::ir::expr::Filter(expr) = f;
        let crate::ir::expr::Expr::Eq(lhs, rhs) = expr else { return false };
        let l_is_it = matches!(lhs.as_ref(), crate::ir::expr::Expr::Ctx(crate::ir::context::Ctx::It));
        let r_is_src = matches!(rhs.as_ref(), crate::ir::expr::Expr::Ctx(crate::ir::context::Ctx::Source));
        let l_is_src = matches!(lhs.as_ref(), crate::ir::expr::Expr::Ctx(crate::ir::context::Ctx::Source));
        let r_is_it = matches!(rhs.as_ref(), crate::ir::expr::Expr::Ctx(crate::ir::context::Ctx::It));
        (l_is_it && r_is_src) || (l_is_src && r_is_it)
    };
    match a {
        Sacrifice { filter, .. } if filter_self(filter) => true,
        MoveByChoice { from: ZoneKindSel::Battlefield, to: ZoneKindSel::Graveyard,
                       verb: MoveVerb::Sacrifice, filter, .. } if filter_self(filter) => true,
        Sequence(actions) => actions.iter().any(action_includes_sac_source),
        IfThen { then, else_, .. } => {
            action_includes_sac_source(then)
                || else_.as_ref().map_or(false, |e| action_includes_sac_source(e))
        }
        MayDo { action, .. } => action_includes_sac_source(action),
        ForEach { body, .. } => action_includes_sac_source(body),
        Choose { options, .. } => options.iter().any(|o| action_includes_sac_source(&o.action)),
        _ => false,
    }
}

fn action_includes_tap_source(a: &Action) -> bool {
    use crate::ir::action::Action::*;
    match a {
        Tap { target } => matches!(
            target,
            crate::ir::expr::Expr::Ctx(crate::ir::context::Ctx::Source)
        ),
        Sequence(actions) => actions.iter().any(action_includes_tap_source),
        IfThen { then, else_, .. } => {
            action_includes_tap_source(then)
                || else_.as_ref().map_or(false, |e| action_includes_tap_source(e))
        }
        MayDo { action, .. } => action_includes_tap_source(action),
        ForEach { body, .. } => action_includes_tap_source(body),
        Choose { options, .. } => options
            .iter()
            .any(|o| action_includes_tap_source(&o.action)),
        _ => false,
    }
}

fn action_includes_pay_mana(a: &Action) -> bool {
    use crate::ir::action::Action::*;
    match a {
        PayMana(_) => true,
        Sequence(actions) => actions.iter().any(action_includes_pay_mana),
        IfThen { then, else_, .. } => {
            action_includes_pay_mana(then)
                || else_.as_ref().map_or(false, |e| action_includes_pay_mana(e))
        }
        MayDo { action, .. } => action_includes_pay_mana(action),
        ForEach { body, .. } => action_includes_pay_mana(body),
        Choose { options, .. } => options
            .iter()
            .any(|o| action_includes_pay_mana(&o.action)),
        _ => false,
    }
}

fn first_pay_mana(a: &Action) -> Option<crate::ManaCost> {
    use crate::ir::action::Action::*;
    match a {
        PayMana(mc) => Some(mc.clone()),
        Sequence(actions) => actions.iter().find_map(first_pay_mana),
        IfThen { then, else_, .. } => first_pay_mana(then)
            .or_else(|| else_.as_ref().and_then(|e| first_pay_mana(e))),
        MayDo { action, .. } => first_pay_mana(action),
        ForEach { body, .. } => first_pay_mana(body),
        Choose { options, .. } => options.iter().find_map(|o| first_pay_mana(&o.action)),
        _ => None,
    }
}

/// One mode of a spell under `AbilityKind::OnResolve`.
#[derive(Clone)]
pub(crate) struct IrSpellMode {
    pub target_spec: crate::TargetSpec,
    pub body: Action,
}
