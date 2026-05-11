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
            let _source_untapped = card.bf.as_ref().map_or(false, |bf| !bf.tapped);
            let crate::ir::ability::CostBody::Ir(action) = &ability.costs;
            let schema = build_schema(action, state, who, card.id);
            if schema.is_none() {
                continue;
            }
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

fn leak_branch_label(i: usize) -> &'static str {
    Box::leak(format!("alt#{}", i).into_boxed_str())
}

/// Schema-build for an activated ability's `CostBody`.
pub(crate) fn schema_for_ability_cost(
    body: &CostBody,
    state: &SimState,
    who: PlayerId,
    source: ObjId,
) -> Option<CostSchema> {
    let CostBody::Ir(a) = body;
    build_schema(a, state, who, source)
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
