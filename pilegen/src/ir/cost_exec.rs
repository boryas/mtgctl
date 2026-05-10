//! Cost-IR executor.
//!
//! Two operations: `build_schema` walks a cost `Action` and produces the flat
//! list of `Decision`s the player must answer; `pay` validates the strategy's
//! answers (a `BindEnv`) against the schema and runs the cost via the
//! ordinary `executor::execute`. There is no second interpreter.
//!
//! The CR 601.2g mana sub-loop is not implemented here. `Action::PayMana`
//! returning `ExecResult::ManaShortage` is the protocol — the cost driver
//! (caller of `pay`) yields control to the strategy on shortage so it can
//! activate mana abilities (each itself a PlayableAction), then resumes.
//! `pay` does not do that loop on its own to keep this module decoupled
//! from the `playable.rs` layer that lives one phase ahead.

use crate::ir::action::Action;
use crate::ir::context::Ctx;
use crate::ir::cost::{CostSchema, Decision, DecisionKind, NumberKind, PayError};
use crate::ir::executor::{execute_mut, BindEnv, ExecResult};
use crate::ir::expr::{Expr, Filter, Value};
use crate::{ObjId, PlayerId, SimState};

/// Walk `cost` and produce the flat `CostSchema` of decisions the strategy
/// must answer. Returns `None` when the cost is structurally unpayable —
/// e.g. a `Sacrifice` whose filter matches 0 candidates and `count > 0`.
///
/// Mana shortage is NOT a build_schema concern — `Action::PayMana` reads the
/// pool at execution time. `build_schema` only fails for object-shortage and
/// Choose-with-no-payable-branches.
pub(crate) fn build_schema(
    cost: &Action,
    state: &SimState,
    who: PlayerId,
    source: ObjId,
) -> Option<CostSchema> {
    let mut schema = CostSchema::empty();
    let mut idx = SchemaCounter::default();
    walk(cost, state, who, source, &mut schema, &mut idx)?;
    Some(schema)
}

/// Run the cost. Validates `env` answers every `Decision` in `schema`, then
/// hands the cost tree to `executor::execute`. The IR cost executor does not
/// import or call `pay_single_cost` — this is enforced by the design and is
/// the whole point of having a separate IR executor.
///
/// On `ExecResult::ManaShortage`, returns `PayError::ManaShortage(remaining)`
/// so the cost driver can yield to the strategy. On `Ok`, returns the
/// `CostsPaidCtx` consumed-by-effects record (currently empty until later
/// phases populate it).
pub(crate) fn pay(
    cost: &Action,
    schema: &CostSchema,
    env: &BindEnv,
    state: &mut SimState,
    _t: u8,
    who: PlayerId,
    source: ObjId,
) -> Result<crate::CostsPaidCtx, PayError> {
    validate(schema, env)?;
    let mut env = env.clone();
    env.source = Some(source);
    env.controller = Some(who);
    match execute_mut(cost, state, &mut env) {
        ExecResult::Ok => Ok(build_costs_paid_ctx(schema, &env)),
        ExecResult::ManaShortage(rem) => Err(PayError::ManaShortage(rem)),
        ExecResult::Unimplemented(s) => panic!("cost_exec::pay: unimplemented action: {}", s),
    }
}

/// Read each `Objects` decision's binding back out of the post-execution
/// `BindEnv` to populate `CostsPaidCtx.objects_moved`. This is the IR
/// counterpart to `pay_single_cost`'s `ctx.objects_moved.push(id)` calls.
/// Order of insertion matches schema-decision order, which matches the
/// order in which the cost tree consumed them.
fn build_costs_paid_ctx(schema: &CostSchema, env: &BindEnv) -> crate::CostsPaidCtx {
    use crate::ir::expr::Value;
    let mut ctx = crate::CostsPaidCtx::default();
    for d in &schema.decisions {
        if !matches!(d.kind, crate::ir::cost::DecisionKind::Objects { .. }) {
            continue;
        }
        match env.bindings.get(d.binding) {
            Some(Value::Obj(id)) => ctx.objects_moved.push(*id),
            Some(Value::ObjSet(ids)) => ctx.objects_moved.extend(ids.iter().copied()),
            _ => {}
        }
    }
    ctx
}

// ── internals ───────────────────────────────────────────────────────────────

#[derive(Default)]
struct SchemaCounter(u32);

impl SchemaCounter {
    fn next_binding(&mut self, prefix: &'static str) -> &'static str {
        let n = self.0;
        self.0 += 1;
        // Leak a short identifier so the binding can live as &'static str. The
        // schema/cost lifecycle is short — one announcement per spell — so the
        // leak is bounded by play activity in practice. Returning &'static
        // matches what `BindEnv.bindings` expects without a wider lifetime
        // refactor.
        Box::leak(format!("${}__{}", prefix, n).into_boxed_str())
    }
}

fn walk(
    cost: &Action,
    state: &SimState,
    who: PlayerId,
    source: ObjId,
    schema: &mut CostSchema,
    idx: &mut SchemaCounter,
) -> Option<()> {
    match cost {
        Action::Noop => Some(()),

        Action::Sequence(actions) => {
            for a in actions {
                walk(a, state, who, source, schema, idx)?;
            }
            Some(())
        }

        Action::Tap { target } => {
            // Tap is a single-object operation. If the Expr resolves to a
            // fixed object (Source or a literal Var), no decision is needed.
            // If it resolves to a filter-set with multiple candidates, the
            // strategy must pick — but for Phase 1 we only use Tap with a
            // singleton expression in cost trees (Tap source, primarily).
            if expr_singleton_or_source(target) {
                Some(())
            } else {
                // Fall back to "no candidates means unpayable; one means auto;
                // many means decision". For now panic — no cost-tree card
                // emits multi-target Tap, and growing this case can wait.
                panic!("cost_exec: multi-target Tap in cost tree not yet supported")
            }
        }

        Action::PayLife { who: _, amount } => {
            // Constant amount = no decision; the executor reads `amount` directly.
            // Bail upfront if the cost would reduce life to zero or below.
            // (CR 119.4 strictly only forbids paying *more* life than you have,
            // but the legacy executor's strategic safeguard refuses to commit
            // suicide — `life > n` rather than `life >= n`. We match it here
            // for migration parity; loosening the check is a separate decision
            // that affects strategy behavior across the whole engine.)
            if let Some(n) = expr_const_u32(amount) {
                if n >= state.player(who).life as u32 {
                    return None;
                }
                return Some(());
            }
            // Non-constant (e.g. `Expr::Ctx(Ctx::Var("$x"))`) = X-cost; emit an
            // XLife Number decision under the variable's own name so the
            // executor's `eval_expr` lookup finds the strategy's binding.
            let max = state.player(who).life.saturating_sub(1) as u32;
            let binding = match amount {
                Expr::Ctx(Ctx::Var(name)) => *name,
                _ => idx.next_binding("xlife"),
            };
            schema.push(Decision {
                binding,
                kind: DecisionKind::Number { kind: NumberKind::XLife, max },
            });
            Some(())
        }

        Action::PayMana(_) => {
            // No announcement decision — pool is consulted at execution time.
            Some(())
        }

        Action::LoyaltyAdjust(_) => {
            // Single-object effect against env.source — no decision.
            Some(())
        }

        Action::Replicate(_mc) => {
            schema.push(Decision {
                binding: idx.next_binding("replicate"),
                kind: DecisionKind::Number { kind: NumberKind::Replicate, max: u32::MAX },
            });
            Some(())
        }

        Action::Sacrifice { who: _, filter, count, bind_as: _ } => {
            let n = expr_const_u32(count)?;
            if n == 0 {
                return Some(());
            }
            let candidates: Vec<ObjId> = state
                .permanents_of(who)
                .filter(|c| c.bf.is_some())
                .map(|c| c.id)
                .filter(|&id| filter_matches_for_schema(filter, id, source, state))
                .collect();
            if (candidates.len() as u32) < n {
                return None;
            }
            schema.push(Decision {
                binding: idx.next_binding("sac"),
                kind: DecisionKind::Objects { candidates, count: n },
            });
            Some(())
        }

        Action::MoveByChoice { who: _, from, to: _, verb: _, filter, count, bind_as } => {
            // Generalised player-pick-from-zone primitive. `from` zone
            // determines the candidate pool (permanents-of for Battlefield,
            // hand-of for Hand, etc.). `bind_as` (when Some) names the
            // schema decision so the executor's BindEnv lookup finds the
            // strategy's chosen ObjIds.
            let n = expr_const_u32(count)?;
            if n == 0 {
                return Some(());
            }
            let candidates: Vec<ObjId> = candidates_in_zone(state, who, *from)
                .filter(|&id| filter_matches_for_schema(filter, id, source, state))
                .collect();
            if (candidates.len() as u32) < n {
                return None;
            }
            let binding = bind_as.unwrap_or_else(|| idx.next_binding("pick"));
            schema.push(Decision {
                binding,
                kind: DecisionKind::Objects { candidates, count: n },
            });
            Some(())
        }

        Action::Discard { who: dwho, count, at_random: _, filter } => {
            let actor = match dwho {
                crate::ir::action::Who::You => who,
                _ => who, // best-effort for Phase 1
            };
            let n = expr_const_u32(count)?;
            if n == 0 {
                return Some(());
            }
            let candidates: Vec<ObjId> = state
                .hand_of(actor)
                .map(|c| c.id)
                .filter(|&id| match filter {
                    Some(f) => filter_matches_for_schema(f, id, source, state),
                    None => true,
                })
                .collect();
            if (candidates.len() as u32) < n {
                return None;
            }
            schema.push(Decision {
                binding: idx.next_binding("discard"),
                kind: DecisionKind::Objects { candidates, count: n },
            });
            Some(())
        }

        Action::Exile { target, bind_as: _ } => {
            // Exile in a cost tree is typically against a hand-card filter
            // resolved through env (e.g. Force of Will pitch). For Phase 1 we
            // only consume Exile when its target Expr is a singleton/Source.
            if expr_singleton_or_source(target) {
                Some(())
            } else {
                // Multi-candidate exile from cost tree: emit an Objects
                // decision over hand cards matching the underlying filter.
                // Phase 1 has no migrated card that needs this; defer.
                panic!("cost_exec: multi-target Exile in cost tree not yet supported")
            }
        }

        Action::Return { what, to: _, bind_as: _ } => {
            if expr_singleton_or_source(what) {
                Some(())
            } else {
                panic!("cost_exec: multi-target Return in cost tree not yet supported")
            }
        }

        Action::Choose { who: _, prompt: _, options } => {
            // Build sub-schemas for each option; only options whose sub-schema
            // exists (i.e. is payable) are listed in `payable`. Phase 1
            // restricts Choose options to leaf actions — no nested decisions.
            // When that limit bites a real card (Force of Will branches do
            // qualify), we'll lift it.
            let mut labels: Vec<&'static str> = Vec::with_capacity(options.len());
            let mut payable: Vec<usize> = Vec::with_capacity(options.len());
            for (i, opt) in options.iter().enumerate() {
                labels.push(opt.label);
                let mut sub = CostSchema::empty();
                let mut sub_idx = SchemaCounter::default();
                if walk(&opt.action, state, who, source, &mut sub, &mut sub_idx).is_some()
                    && sub.decisions.is_empty()
                {
                    payable.push(i);
                }
            }
            if payable.is_empty() {
                return None;
            }
            schema.push(Decision {
                binding: idx.next_binding("branch"),
                kind: DecisionKind::Branch { labels, payable },
            });
            Some(())
        }

        Action::IfThen { cond: _, then, else_ } => {
            // Conservative: walk both branches. The executor will pick one at
            // run time; the strategy answers any decisions encountered along
            // the way. Phase 1 has no card with cost-tree IfThen, so this is
            // forward-compatible plumbing rather than load-bearing logic.
            walk(then, state, who, source, schema, idx)?;
            if let Some(e) = else_ {
                walk(e, state, who, source, schema, idx)?;
            }
            Some(())
        }

        // Effect-only actions inside a cost tree are unusual but legal —
        // payment can compose any structural mutation. They emit no decision
        // and run at execute-time like everything else.
        Action::Draw { .. }
        | Action::DealDamage { .. }
        | Action::GainLife { .. }
        | Action::PutCounters { .. }
        | Action::RemoveCounters { .. }
        | Action::Destroy { .. }
        | Action::Untap { .. }
        | Action::Move { .. }
        | Action::AddMana { .. }
        | Action::Mill { .. }
        | Action::Reveal { .. }
        | Action::Scry { .. }
        | Action::Surveil { .. }
        | Action::Look { .. }
        | Action::Counter { .. }
        | Action::OfferCast { .. }
        | Action::CopySpell { .. }
        | Action::ApplyCE { .. }
        | Action::ScheduleDelayedTrigger { .. }
        | Action::GrantCEToNextSpellCast { .. }
        | Action::CreateToken { .. }
        | Action::PutOnLibrary { .. }
        | Action::Search { .. }
        | Action::ForEach { .. }
        | Action::MayDo { .. } => Some(()),
    }
}

/// True when `e` resolves to a single object reference under the current
/// bind env — either `Ctx::Source`, `Ctx::Var(_)`, or `Ctx::It`. Filter-set
/// expressions return false; multi-target Tap/Exile/Return inside cost trees
/// is unsupported in Phase 1.
fn expr_singleton_or_source(e: &Expr) -> bool {
    matches!(e, Expr::Ctx(Ctx::Source) | Expr::Ctx(Ctx::Var(_)) | Expr::Ctx(Ctx::It))
}

/// Extract a constant non-negative `u32` from an `Expr`. Returns `None` for
/// runtime-evaluated expressions; callers fall back to an alternate strategy
/// (e.g. `PayLife` with a non-constant `amount` becomes an X-decision).
fn expr_const_u32(e: &Expr) -> Option<u32> {
    if let Expr::Num(n) = e {
        if *n >= 0 {
            return Some(*n as u32);
        }
    }
    None
}

/// Filter evaluation for schema-building. The IR `Filter` interpreter lives
/// in `executor.rs` and takes a `BindEnv`; for schema-building we synthesise
/// a minimal env (controller + source) since the candidate id is the subject.
fn filter_matches_for_schema(filter: &Filter, candidate: ObjId, source: ObjId, state: &SimState) -> bool {
    let env = BindEnv::default()
        .with_source(source)
        .with_controller(owner_of(candidate, state).unwrap_or(PlayerId::Us));
    crate::ir::executor::matches(filter, candidate, state, &env)
}

fn owner_of(id: ObjId, state: &SimState) -> Option<PlayerId> {
    state.objects.get(&id).map(|o| o.owner)
}

/// Iterate object ids in `who`'s view of `zone` — the candidate pool for
/// `Action::MoveByChoice`. Battlefield uses `permanents_of` (objects with
/// `bf` set); other zones use the per-player iterators.
fn candidates_in_zone<'a>(
    state: &'a SimState,
    who: PlayerId,
    zone: crate::ir::expr::ZoneKindSel,
) -> Box<dyn Iterator<Item = ObjId> + 'a> {
    use crate::ir::expr::ZoneKindSel;
    match zone {
        ZoneKindSel::Battlefield => Box::new(
            state.permanents_of(who).filter(|c| c.bf.is_some()).map(|c| c.id),
        ),
        ZoneKindSel::Hand => Box::new(state.hand_of(who).map(|c| c.id)),
        ZoneKindSel::Graveyard => Box::new(state.graveyard_of(who).map(|c| c.id)),
        ZoneKindSel::Exile => Box::new(state.exile_of(who).map(|c| c.id)),
        ZoneKindSel::Library => Box::new(state.library_of(who).map(|c| c.id)),
        ZoneKindSel::Stack | ZoneKindSel::Command => Box::new(std::iter::empty()),
    }
}

fn validate(schema: &CostSchema, env: &BindEnv) -> Result<(), PayError> {
    for d in &schema.decisions {
        let v = env.bindings.get(d.binding).ok_or(PayError::MissingBinding(d.binding))?;
        match (&d.kind, v) {
            (DecisionKind::Objects { candidates, count }, Value::ObjSet(ids)) => {
                if ids.len() as u32 != *count {
                    return Err(PayError::WrongBindingShape(d.binding));
                }
                for id in ids {
                    if !candidates.contains(id) {
                        return Err(PayError::BindingNotInCandidates {
                            binding: d.binding,
                            provided: *id,
                        });
                    }
                }
            }
            (DecisionKind::Objects { candidates, count }, Value::Obj(id)) if *count == 1 => {
                if !candidates.contains(id) {
                    return Err(PayError::BindingNotInCandidates {
                        binding: d.binding,
                        provided: *id,
                    });
                }
            }
            (DecisionKind::Branch { payable, .. }, Value::Num(n)) => {
                let i = *n as usize;
                if !payable.contains(&i) {
                    return Err(PayError::WrongBindingShape(d.binding));
                }
            }
            (DecisionKind::Number { max, .. }, Value::Num(n)) => {
                if *n < 0 || (*n as u32) > *max {
                    return Err(PayError::NumberOutOfRange {
                        binding: d.binding,
                        provided: *n as u32,
                        max: *max,
                    });
                }
            }
            _ => return Err(PayError::WrongBindingShape(d.binding)),
        }
    }
    Ok(())
}

// ── helpers used by tests ───────────────────────────────────────────────────

/// True when the candidate is in the player's hand. Helper for tests that
/// build hand-targeting Filter candidate sets without going through the full
/// `Filter` interpreter.
#[cfg(test)]
pub(crate) fn in_hand(id: ObjId, state: &SimState) -> bool {
    state
        .objects
        .get(&id)
        .map(|o| matches!(o.zone, crate::CardZone::Hand { .. }))
        .unwrap_or(false)
}
