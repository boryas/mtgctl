#![allow(dead_code)]
//! Castability layer (Phase 2 of the cost-IR migration).
//!
//! `enumerate_playable` returns the typed surface of "things `who` can do
//! right now": cast a hand card, activate an ability on a permanent. Each
//! `PlayableAction` carries a `CostSchema` derived from the source's IR cost
//! tree (or, for unmigrated cards, from a temporary `legacy_cost_as_ir`
//! analyser that translates `&[CostComponent]` into `Action` for the schema
//! walker only — payment for legacy cards still flows through `pay_costs`).
//!
//! This is the surface that Phase 3's `Strategy::propose_announcement` will
//! consume: one structured plan per playable action, answering every
//! announcement-time decision (targets, modes, cost bindings) in one call.
//!
//! The shim and this two-population logic both die in Phase 6 once every
//! card is on the IR cost grammar.

use crate::ir::ability::{AbilityKind, CostBody};
use crate::ir::action::{Action, ChoiceOption, Who};
use crate::ir::context::Ctx;
use crate::ir::cost::CostSchema;
use crate::ir::cost_exec::build_schema;
use crate::ir::expr::{Expr, Filter};
use crate::{
    parse_mana_cost, ActivationTiming, CostComponent, ObjId, PlayerId, SimState, SourceZone,
};

/// What kind of playable action this is. Activated abilities reference the
/// ability inside the card's `abilities()` slice; casting is just "the spell."
#[derive(Clone, Debug)]
pub(crate) enum PlayableKind {
    Cast,
    Activate { ability_index: usize },
}

/// One thing `who` can do right now. Schema is `Some` when the cost tree was
/// translatable (IR-cost cards, plus the subset of legacy patterns the shim
/// handles); `None` when the cost shape is outside the shim's scope and the
/// caller must fall back to the legacy per-decision callbacks.
#[derive(Clone)]
pub(crate) struct PlayableAction {
    pub source: ObjId,
    pub kind: PlayableKind,
    pub schema: Option<CostSchema>,
}

/// Enumerate every playable action `who` has at this moment. Pure read — no
/// mutation of `state`. The list is unordered; callers that care about
/// priority sort it themselves.
pub(crate) fn enumerate_playable(state: &SimState, who: PlayerId) -> Vec<PlayableAction> {
    let mut out = Vec::new();

    for card in state.hand_of(who) {
        let Some(def) = state.def_of(card.id) else { continue };
        if def.is_land() {
            continue;
        }
        if !def.castable {
            continue;
        }
        let cost = parse_mana_cost(def.mana_cost());
        if !state.potential_mana(who).can_pay(&cost) {
            continue;
        }
        let mana_action = Action::PayMana(cost);
        let schema = build_schema(&mana_action, state, who, card.id);
        out.push(PlayableAction {
            source: card.id,
            kind: PlayableKind::Cast,
            schema,
        });
    }

    for card in state.permanents_of(who) {
        let Some(def) = state.def_of(card.id) else { continue };
        for (idx, ability) in def.abilities().iter().enumerate() {
            if !ability.activatable {
                continue;
            }
            if !matches!(ability.source_zone, SourceZone::Battlefield) {
                continue;
            }
            if ability.timing == ActivationTiming::Sorcery && !state.stack.is_empty() {
                continue;
            }
            let source_untapped = card.bf.as_ref().map_or(false, |bf| !bf.tapped);
            if !crate::can_pay_costs(&ability.costs, state, who, card.id, source_untapped, 0) {
                continue;
            }
            let schema = legacy_cost_as_ir(&ability.costs)
                .and_then(|action| build_schema(&action, state, who, card.id));
            out.push(PlayableAction {
                source: card.id,
                kind: PlayableKind::Activate { ability_index: idx },
                schema,
            });
        }
    }

    out
}

// ── legacy_cost_as_ir ───────────────────────────────────────────────────────

/// Translate `&[CostComponent]` to `Action` for the **schema walker only**.
///
/// Returns `None` for components the shim cannot translate (object-targeted
/// selectors, CostOr branches with mixed shapes, XMana). Callers treat
/// `None` as "skip the schema; fall back to legacy callbacks." Phase 6
/// deletes this function once every card is on the IR cost grammar.
///
/// This is intentionally lossy — the shim never paid a cost in its life. It
/// only exists to feed the schema walker enough structure to surface the
/// right decisions.
pub(crate) fn legacy_cost_as_ir(comps: &[CostComponent]) -> Option<Action> {
    let mut translated = Vec::with_capacity(comps.len());
    for c in comps {
        translated.push(translate_one(c)?);
    }
    Some(match translated.len() {
        0 => Action::Noop,
        1 => translated.into_iter().next().unwrap(),
        _ => Action::Sequence(translated),
    })
}

fn translate_one(c: &CostComponent) -> Option<Action> {
    Some(match c {
        CostComponent::Mana(mc) => Action::PayMana(mc.clone()),
        CostComponent::TapSelf => Action::Tap { target: Expr::Ctx(Ctx::Source) },
        CostComponent::SacSelf => Action::Sacrifice {
            who: Who::You,
            filter: filter_is_source(),
            count: Expr::Num(1),
            bind_as: None,
        },
        CostComponent::Life(n) => Action::PayLife {
            who: Who::You,
            amount: Expr::Num(*n as i64),
        },
        CostComponent::LoyaltyAdjust(n) => Action::LoyaltyAdjust(*n),
        CostComponent::XLife => Action::PayLife {
            who: Who::You,
            amount: Expr::Ctx(Ctx::Var("$x")),
        },
        CostComponent::Replicate(mc) => Action::Replicate(mc.clone()),
        CostComponent::CostAnd(sub) => legacy_cost_as_ir(sub)?,
        CostComponent::CostOr(branches) => {
            let mut options = Vec::with_capacity(branches.len());
            for (i, b) in branches.iter().enumerate() {
                let sub = match b {
                    CostComponent::CostAnd(v) => legacy_cost_as_ir(v)?,
                    other => translate_one(other)?,
                };
                options.push(ChoiceOption {
                    label: leak_branch_label(i),
                    cost: None,
                    action: Box::new(sub),
                });
            }
            Action::Choose { who: Who::You, prompt: "alt cost", options }
        }
        // Object-targeted selectors and complex variants don't survive Phase 2's
        // shim — they want object decisions whose candidate sets depend on
        // ObjPredicate closures (incompatible with IR Filter without a per-card
        // rewrite). Phase 4 migrates each of these per-card with native IR
        // filters.
        CostComponent::SacPermanent(_)
        | CostComponent::DiscardCard(_)
        | CostComponent::ExileFromHand(_)
        | CostComponent::ReturnFromBattlefield(_)
        | CostComponent::TapPermanent(_)
        | CostComponent::DiscardSelf
        | CostComponent::ExileSelf
        | CostComponent::DiscardHand
        | CostComponent::XMana => return None,
    })
}

fn filter_is_source() -> Filter {
    Filter(Expr::Eq(
        Box::new(Expr::Ctx(Ctx::It)),
        Box::new(Expr::Ctx(Ctx::Source)),
    ))
}

// ── ir_cost_as_legacy ───────────────────────────────────────────────────────

/// Reverse shim: lower a Phase-4 IR cost `Action` back to `Vec<CostComponent>`
/// for the legacy bridges (`ir_activated_as_legacy`,
/// `ir_activated_as_mana_ability_legacy`) that still synthesize legacy
/// `AbilityDef`/`ManaAbility` structs. Phase 6 deletes both the bridges and
/// this function together.
///
/// Returns `None` for IR shapes the shim cannot lower — callers panic at
/// catalog-build time, which is the right failure mode (a card in the catalog
/// using an unmapped IR cost shape is a bug to fix at migration time, not at
/// run time).
///
/// Coverage grows as Phase 4 migrates more cards. Today: TapSelf only.
pub(crate) fn ir_cost_as_legacy(action: &Action) -> Option<Vec<CostComponent>> {
    let mut out = Vec::new();
    lower_one(action, &mut out)?;
    Some(out)
}

fn lower_one(action: &Action, out: &mut Vec<CostComponent>) -> Option<()> {
    match action {
        Action::Noop => Some(()),
        Action::Sequence(actions) => {
            for a in actions {
                lower_one(a, out)?;
            }
            Some(())
        }
        Action::Tap { target } if expr_is_source(target) => {
            out.push(CostComponent::TapSelf);
            Some(())
        }
        Action::Sacrifice { who: Who::You, filter, count, bind_as: None }
            if filter_is_self(filter) && expr_const_one(count) =>
        {
            out.push(CostComponent::SacSelf);
            Some(())
        }
        _ => None,
    }
}

fn expr_is_source(e: &Expr) -> bool {
    matches!(e, Expr::Ctx(Ctx::Source))
}

/// True when `filter` is `It == Source` — the canonical "this object only"
/// shape used by SacSelf-style costs.
fn filter_is_self(f: &Filter) -> bool {
    let Filter(expr) = f;
    let Expr::Eq(lhs, rhs) = expr else { return false };
    let l_is_it = matches!(lhs.as_ref(), Expr::Ctx(Ctx::It));
    let r_is_src = matches!(rhs.as_ref(), Expr::Ctx(Ctx::Source));
    let l_is_src = matches!(lhs.as_ref(), Expr::Ctx(Ctx::Source));
    let r_is_it = matches!(rhs.as_ref(), Expr::Ctx(Ctx::It));
    (l_is_it && r_is_src) || (l_is_src && r_is_it)
}

fn expr_const_one(e: &Expr) -> bool {
    matches!(e, Expr::Num(1))
}

/// Bridge-side helper: extract a legacy component vec from any `CostBody`.
/// Legacy variant returns its components verbatim; IR variant goes through
/// `ir_cost_as_legacy`. Panics if the IR shape isn't covered — this is a
/// catalog-time bug, fixable by extending `ir_cost_as_legacy`.
pub(crate) fn cost_body_to_legacy(cost: &CostBody) -> Vec<CostComponent> {
    match cost {
        CostBody::Legacy(v) => v.clone(),
        CostBody::Ir(a) => ir_cost_as_legacy(a).unwrap_or_else(|| {
            panic!(
                "cost_body_to_legacy: IR cost shape not covered by ir_cost_as_legacy ({})",
                action_kind_name(a),
            )
        }),
    }
}

fn action_kind_name(a: &Action) -> &'static str {
    match a {
        Action::Noop => "Noop",
        Action::Sequence(_) => "Sequence",
        Action::Tap { .. } => "Tap",
        Action::Untap { .. } => "Untap",
        Action::Destroy { .. } => "Destroy",
        Action::Exile { .. } => "Exile",
        Action::Sacrifice { .. } => "Sacrifice",
        Action::ApplyCE { .. } => "ApplyCE",
        Action::AddMana { .. } => "AddMana",
        Action::PayMana(_) => "PayMana",
        Action::PayLife { .. } => "PayLife",
        Action::LoyaltyAdjust(_) => "LoyaltyAdjust",
        Action::Replicate(_) => "Replicate",
        Action::Discard { .. } => "Discard",
        Action::Return { .. } => "Return",
        Action::IfThen { .. } => "IfThen",
        Action::MayDo { .. } => "MayDo",
        Action::ForEach { .. } => "ForEach",
        Action::Choose { .. } => "Choose",
        _ => "Action(other)",
    }
}

fn leak_branch_label(i: usize) -> &'static str {
    Box::leak(format!("alt#{}", i).into_boxed_str())
}

/// Schema-build for an activated ability's `CostBody` regardless of variant.
/// IR-cost: walk directly. Legacy: translate via the shim, then walk.
pub(crate) fn schema_for_ability_cost(
    body: &CostBody,
    state: &SimState,
    who: PlayerId,
    source: ObjId,
) -> Option<CostSchema> {
    match body {
        CostBody::Ir(a) => build_schema(a, state, who, source),
        CostBody::Legacy(comps) => {
            let action = legacy_cost_as_ir(comps)?;
            build_schema(&action, state, who, source)
        }
    }
}

/// Like `enumerate_playable` but matches `AbilityKind` cost bodies (used by IR
/// abilities living under `AbilityKind::Activated`). Phase 2's existing
/// `AbilityDef` enumeration above is the catalog/legacy path; once the
/// catalog and IR ability storage merge, this is the surviving entry point.
pub(crate) fn enumerate_ir_abilities(
    abilities: &[AbilityKind],
    state: &SimState,
    who: PlayerId,
    source: ObjId,
) -> Vec<PlayableAction> {
    let mut out = Vec::new();
    for (i, a) in abilities.iter().enumerate() {
        let AbilityKind::Activated { cost, .. } = a else { continue };
        let schema = schema_for_ability_cost(cost, state, who, source);
        out.push(PlayableAction {
            source,
            kind: PlayableKind::Activate { ability_index: i },
            schema,
        });
    }
    out
}
