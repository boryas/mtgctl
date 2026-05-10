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
            // Variant-aware feasibility: Legacy uses the closure-based
            // predicate; Ir uses build_schema (which returns None when
            // candidates are missing). The schema is reused below — for
            // Legacy it goes through the legacy_cost_as_ir shim; for Ir
            // it walks the action tree directly.
            let (payable, schema) = match &ability.costs {
                crate::ir::ability::CostBody::Legacy(comps) => {
                    let payable = crate::can_pay_costs(comps, state, who, card.id, source_untapped, 0);
                    let schema = legacy_cost_as_ir(comps)
                        .and_then(|action| build_schema(&action, state, who, card.id));
                    (payable, schema)
                }
                crate::ir::ability::CostBody::Ir(action) => {
                    let schema = build_schema(action, state, who, card.id);
                    (schema.is_some(), schema)
                }
            };
            if !payable {
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

// ── ir_cost_as_legacy ───────────────────────────────────────────────────────
//
// Reverse shim — was used by the legacy bridges to lower IR costs back to
// `Vec<CostComponent>` so synthesized `AbilityDef`/`ManaAbility` structs
// could feed the legacy mana sub-loop / activation pipeline. As of the
// Phase-6 push the bridges pass `CostBody` through directly (no lowering),
// so this shim's only remaining caller is the cost_phase4 test module's
// round-trip checks. Marked dead-code-tolerant; the function is kept until
// the cost_phase4 tests get retired alongside the legacy executor.
#[allow(dead_code)]
pub(crate) fn ir_cost_as_legacy(action: &Action) -> Option<Vec<CostComponent>> {
    let mut out = Vec::new();
    lower_one(action, &mut out)?;
    Some(out)
}

fn lower_one(action: &Action, out: &mut Vec<CostComponent>) -> Option<()> {
    use crate::ir::action::{MoveVerb, Who};
    use crate::ir::expr::ZoneKindSel;
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
        // `Move { what: Source, to: <zone> }` is the deterministic source-
        // moves-itself shape used by cycling/discard-self and exile-self
        // costs. Lowers to the matching CostComponent::*Self variant.
        Action::Move { what, to, to_owner: None, bind_as: None } if expr_is_source(what) => {
            use crate::ir::expr::ZoneKindSel;
            match to {
                ZoneKindSel::Graveyard => out.push(CostComponent::DiscardSelf),
                ZoneKindSel::Exile => out.push(CostComponent::ExileSelf),
                _ => return None,
            }
            Some(())
        }
        Action::PayMana(mc) => {
            out.push(CostComponent::Mana(mc.clone()));
            Some(())
        }
        // `Discard { count: HandSize(Controller), filter: None }` is the
        // canonical "discard your entire hand" shape (LED). Lowers to the
        // legacy `DiscardHand` component.
        Action::Discard { who: Who::You, count, at_random: _, filter: None }
            if expr_is_controller_hand_size(count) =>
        {
            out.push(CostComponent::DiscardHand);
            Some(())
        }
        Action::PayLife { who: _, amount: Expr::Num(n) } => {
            out.push(CostComponent::Life(*n as i32));
            Some(())
        }
        Action::LoyaltyAdjust(n) => {
            out.push(CostComponent::LoyaltyAdjust(*n));
            Some(())
        }
        // Old Sacrifice variant — pre-MoveByChoice migration. Still present
        // until Lotus Petal etc. switch over.
        Action::Sacrifice { who: Who::You, filter, count, bind_as: None }
            if filter_is_self(filter) && expr_const_one(count) =>
        {
            out.push(CostComponent::SacSelf);
            Some(())
        }
        // MoveByChoice — recognise the canonical (from, to, verb) self-shapes
        // and lower to the corresponding legacy CostComponent.
        Action::MoveByChoice { who: Who::You, from, to, verb, filter, count, bind_as: _ }
            if filter_is_self(filter) && expr_const_one(count) =>
        {
            match (from, to, verb) {
                (ZoneKindSel::Battlefield, ZoneKindSel::Graveyard, MoveVerb::Sacrifice) => {
                    out.push(CostComponent::SacSelf);
                    Some(())
                }
                (ZoneKindSel::Hand, ZoneKindSel::Exile, MoveVerb::Exile) => {
                    out.push(CostComponent::ExileSelf);
                    Some(())
                }
                (ZoneKindSel::Hand, ZoneKindSel::Graveyard, MoveVerb::Discard) => {
                    out.push(CostComponent::DiscardSelf);
                    Some(())
                }
                _ => None,
            }
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

/// True when `e` is `Expr::HandSize(Expr::Ctx(Ctx::Controller))` — the
/// canonical "size of the controller's hand" shape, used as a count in
/// "discard your hand" cost trees.
fn expr_is_controller_hand_size(e: &Expr) -> bool {
    let Expr::HandSize(inner) = e else { return false };
    matches!(inner.as_ref(), Expr::Ctx(Ctx::Controller))
}

/// Bridge-side helper: extract a legacy component vec from any `CostBody`.
/// Now unused by production code (the bridges pass `CostBody` through
/// directly); retained for the cost_phase4 round-trip tests until those
/// retire alongside the legacy executor.
#[allow(dead_code)]
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
        Action::MoveByChoice { .. } => "MoveByChoice",
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
