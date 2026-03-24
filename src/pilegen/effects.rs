use std::sync::Arc;
use super::*;

/// Actor-relative player reference used in effect primitives.
/// `Actor` = the spell's controller; `Opp` = their opponent.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Who { Actor, Opp }

impl Who {
    pub(crate) fn resolve(&self, actor: PlayerId) -> PlayerId {
        match self { Who::Actor => actor, Who::Opp => actor.opp() }
    }
}

/// A composable game effect. Wraps a closure that mutates SimState.
/// Built from primitives (eff_draw, eff_destroy_target, etc.) and chained with `.then()`.
/// Effects that need randomness access `state.rng` directly inside their closure.
pub(crate) struct Effect(pub(crate) Arc<dyn Fn(&mut SimState, u8, &[ObjId]) + Send + Sync>);

impl Clone for Effect {
    fn clone(&self) -> Self { Effect(Arc::clone(&self.0)) }
}

impl Effect {
    pub(crate) fn call(
        &self,
        state: &mut SimState,
        t: u8,
        targets: &[ObjId],
    ) {
        (self.0)(state, t, targets);
    }

    /// Chain two effects: `self` runs first, then `next`.
    pub(crate) fn then(self, next: Effect) -> Effect {
        let a = self.0;
        let b = next.0;
        Effect(Arc::new(move |state, t, targets: &[ObjId]| {
            a(state, t, targets);
            b(state, t, targets);
        }))
    }
}

// ── Effect primitives ─────────────────────────────────────────────────────────

/// Draw `n` cards for `who`.
pub(crate) fn eff_draw(who: PlayerId, n: usize) -> Effect {
    Effect(Arc::new(move |state, t, _targets| {
        for _ in 0..n {
            sim_draw(state, who, t, false);
        }
    }))
}

/// Surveil N for `who`. For each of the top N cards of their library, calls
/// `state.surveil_choice` to decide keep-on-top or put-in-graveyard.
/// Reveal step is a no-op (hidden information is not modeled).
/// TODO: surveil N>1 for Kaito, Bane of Nightmares 0 ability (passes N cards as a batch).
pub(crate) fn eff_surveil(who: PlayerId, n: usize) -> Effect {
    Effect(Arc::new(move |state, t, _targets| {
        for _ in 0..n {
            let top = state.library_of(who).next().map(|o| o.id);
            if let Some(id) = top {
                let f = std::sync::Arc::clone(&state.surveil_choice);
                if f(id, state) {
                    change_zone(id, ZoneId::Graveyard, state, t, who);
                }
            }
        }
    }))
}

/// Put `n` cards back from `who`'s hand (Brainstorm put-back).
/// Moves `n` hand cards back to Library zone (unknown — just sets zone).
pub(crate) fn eff_put_back(who: PlayerId, n: usize) -> Effect {
    Effect(Arc::new(move |state, t, _targets| {
        let ids: Vec<ObjId> = state.hand_of(who).map(|c| c.id).take(n).collect();
        for id in ids {
            change_zone(id, ZoneId::Library, state, t, who);
        }
    }))
}

/// `who` loses `n` life, with a log line.
pub(crate) fn eff_life_loss(who: PlayerId, n: i32) -> Effect {
    Effect(Arc::new(move |state, t, _targets| {
        state.lose_life(who, n);
        let life = state.life_of(who);
        state.log(t, who, format!("→ lose {} life (now {})", n, life));
    }))
}

/// Add mana per `spec` (e.g. `"BBB"`) to `who`'s pool.
/// Fires `GameEvent::ManaProduced` so replacement effects can intercept.
pub(crate) fn eff_mana(who: PlayerId, spec: impl Into<String>) -> Effect {
    let spec = spec.into();
    Effect(Arc::new(move |state, t, _targets| {
        fire_event(GameEvent::ManaProduced { who, spec: spec.clone() }, state, t, who);
    }))
}

/// Destroy the permanent in `targets[0]`. `caster` used for logging.
/// Deal `n` damage to the permanent in `targets[0]`. SBAs handle lethal-damage destruction.
pub(crate) fn eff_damage_target(caster: PlayerId, n: i32) -> Effect {
    Effect(Arc::new(move |state, t, targets| {
        if let Some(&id) = targets.first() {
            if let Some(bf) = state.permanent_bf_mut(id) {
                bf.damage += n;
            }
            let name = state.objects.get(&id).map(|o| o.catalog_key.as_str()).unwrap_or("?");
            state.log(t, caster, format!("→ deals {} damage to {}", n, name));
        }
    }))
}

/// Force `who` to sacrifice one permanent matching `filter`, chosen via `state.sacrifice_choice`.
/// Models "sacrifice a [X] of your choice" (CR 701.16). The sacrificing player decides;
/// the effect moves the chosen permanent to the graveyard. No-ops if no match exists.
pub(crate) fn eff_sacrifice(caster: PlayerId, who: Who, filter: ObjPredicate) -> Effect {
    Effect(Arc::new(move |state, t, _targets| {
        let target_who = who.resolve(caster);
        let candidates: Vec<ObjId> = state.permanents_of(target_who)
            .filter(|o| o.bf.is_some() && filter(o.id, state))
            .map(|o| o.id)
            .collect();
        if candidates.is_empty() { return; }
        let f = Arc::clone(&state.sacrifice_choice);
        if let Some(id) = f(target_who, &candidates, &*state) {
            let name = state.objects.get(&id).map(|o| o.catalog_key.clone()).unwrap_or_default();
            state.log(t, caster, format!("→ {} sacrificed", name));
            change_zone(id, ZoneId::Graveyard, state, t, caster);
        }
    }))
}

/// Core "destroy" action for a single permanent. The future home for indestructibility checks.
/// Use this (not `change_zone`) wherever the rules say a permanent is "destroyed".
pub(super) fn destroy_one(id: ObjId, state: &mut SimState, t: u8, actor: PlayerId) {
    change_zone(id, ZoneId::Graveyard, state, t, actor);
}

pub(crate) fn eff_destroy_target(caster: PlayerId) -> Effect {
    Effect(Arc::new(move |state, t, targets| {
        if let Some(&id) = targets.first() {
            destroy_one(id, state, t, caster);
        }
    }))
}

/// Destroy every permanent on the battlefield matching `filter`. Both this and
/// `eff_destroy_target` route through `destroy_one`; indestructibility added there
/// will apply to both. Use for "destroy each" oracle text; sacrifice and 0-toughness
/// SBAs bypass indestructible and must use `change_zone` directly instead.
pub(crate) fn eff_destroy_all(caster: PlayerId, filter: ObjPredicate) -> Effect {
    Effect(Arc::new(move |state, t, _targets| {
        let to_destroy: Vec<ObjId> = state.objects.values()
            .filter(|o| o.zone == CardZone::Battlefield && filter(o.id, state))
            .map(|o| o.id)
            .collect();
        for id in to_destroy {
            destroy_one(id, state, t, caster);
        }
    }))
}

/// Exile the permanent in `targets[0]`.
#[allow(dead_code)]
pub(crate) fn eff_exile_target(caster: PlayerId) -> Effect {
    Effect(Arc::new(move |state, t, targets| {
        if let Some(&id) = targets.first() {
            change_zone(id, ZoneId::Exile, state, t, caster);
        }
    }))
}

/// Bounce the permanent in `targets[0]` to its controller's hand.
pub(crate) fn eff_bounce_target(caster: PlayerId) -> Effect {
    Effect(Arc::new(move |state, t, targets| {
        if let Some(&id) = targets.first() {
            change_zone(id, ZoneId::Hand, state, t, caster);
        }
    }))
}

/// Set `state.success = true` (Doomsday resolved).
pub(crate) fn eff_doomsday() -> Effect {
    Effect(Arc::new(|state, _t, _targets| {
        state.success = true;
    }))
}

/// Discard `n` random cards from `target`'s hand matching `filter`.
pub(crate) fn eff_discard(caster: PlayerId, target: Who, n: usize, filter: CardPredicate) -> Effect {
    let discard_pred = filter;
    Effect(Arc::new(move |state, t, _targets| {
        use rand::Rng;
        let target_who = target.resolve(caster);
        for _ in 0..n {
            let candidates: Vec<ObjId> = state.hand_of(target_who)
                .filter(|c| state.def_of(c.id).map_or(true, |d| discard_pred(d)))
                .map(|c| c.id)
                .collect();
            if candidates.is_empty() { break; }
            let id = candidates[state.rng.gen_range(0..candidates.len())];
            change_zone(id, ZoneId::Graveyard, state, t, caster);
        }
    }))
}

/// Put `card_name` onto the battlefield as a permanent for `owner`. Fires ETB triggers.
pub(crate) fn eff_enter_permanent(
    owner: PlayerId,
    card_name: impl Into<String>,
) -> Effect {
    let card_name = card_name.into();
    Effect(Arc::new(move |state, t, _targets| {
        let new_id = state.alloc_id();
        // Pre-register and immediately activate instances before the event fires,
        // so ETB replacement checks (e.g. Murktide self-ETB) can intercept the event.
        let card_def = state.catalog.get(card_name.as_str()).cloned();
        if let Some(ref def) = card_def {
            preregister_instances(def, new_id, owner, state);
        }
        activate_instances(new_id, owner, card_def.as_ref(), state);
        state.objects.insert(new_id, GameObject {
            id: new_id,
            catalog_key: card_name.clone(),
            owner,
            controller: owner,
            zone: CardZone::Battlefield,
            is_token: false,
            spell: None,
            bf: Some(BattlefieldState {
                entered_this_turn: true,
                ..BattlefieldState::new()
            }),
            materialized: None,
            counters: HashMap::new(),
        });
        fire_event(
            GameEvent::ZoneChange {
                id: new_id,
                actor: owner,
                from: ZoneId::Stack,
                to: ZoneId::Battlefield,
                controller: owner,
            },
            state, t, owner,
        );
        state.log(t, owner, format!("{} enters play", card_name));
    }))
}

/// Counter a single spell or ability by id. Called by `eff_counter_target` and
/// Lavinia-style triggers that capture the spell id at trigger time.
/// Fizzles gracefully if the id is no longer on the stack.
pub(super) fn counter_one(id: ObjId, state: &mut SimState, t: u8, actor: PlayerId) {
    let pos = state.stack.iter().position(|&sid| sid == id);
    if let Some(pos) = pos {
        // Check counterable property before removing (CR 608.2b).
        let can_counter = if state.objects.contains_key(&id) {
            state.def_of(id)
                .or_else(|| state.objects.get(&id)
                    .and_then(|o| state.catalog.get(o.catalog_key.as_str())))
                .map_or(true, |d| d.counterable())
        } else {
            state.abilities.get(&id).map_or(true, |ab| ab.counterable)
        };
        if !can_counter {
            let name = state.stack_item_display_name(id).to_string();
            state.log(t, actor, format!("→ fizzled ({} can't be countered)", name));
            return;
        }
        state.stack.remove(pos);
        if state.objects.contains_key(&id) {
            let name = state.objects[&id].catalog_key.clone();
            state.log(t, actor, format!("→ {} countered", name));
            change_zone(id, ZoneId::Graveyard, state, t, actor);
            // `change_zone` doesn't clear spell state; do it here so countered
            // spells don't carry stale SpellState in the graveyard (or exile).
            if let Some(card) = state.objects.get_mut(&id) {
                card.spell = None;
            }
        } else if let Some(ab) = state.abilities.remove(&id) {
            state.log(t, actor, format!("→ {} (triggered ability) countered", ab.source_name));
        } else {
            state.log(t, actor, "→ fizzled (target already resolved)".to_string());
        }
    } else {
        state.log(t, actor, "→ fizzled (target already resolved)".to_string());
    }
}

/// Counter the spell in `targets[0]` (a stack ObjId). Removes it from `state.stack` and
/// puts it in the owner's graveyard via `change_zone` (so replacement effects can intercept).
/// Fizzles if the target is no longer on the stack or if it can't be countered
/// (`CardDef::counterable == false` / `StackAbility::counterable == false`,
/// CR 608.2b — the spell was a legal target but the effect doesn't apply).
pub(crate) fn eff_counter_target(caster: PlayerId) -> Effect {
    Effect(Arc::new(move |state, t, targets| {
        if let Some(&id) = targets.first() {
            counter_one(id, state, t, caster);
        }
    }))
}

/// Counter target spell and exile it instead of putting it into its owner's graveyard.
/// Models cards like Force of Negation (CR 614.1a — replacement of zone-change destination).
/// Installs a scoped replacement effect on the Stack→Graveyard zone change for the specific
/// target, delegates to `eff_counter_target`, then removes the replacement.
/// The lifetime mirrors a permanent's ETB/LTB-managed replacement, but bounded by the
/// effect chain rather than the event system.
pub(crate) fn eff_counter_and_exile(caster: PlayerId, source_id: ObjId) -> Effect {
    Effect(Arc::new(move |state, t, targets| {
        let Some(&target_id) = targets.first() else { return; };
        let re_id = state.alloc_id();
        let re = ReplacementInstance {
            id: re_id,
            source_id,
            controller: caster,
            active: true,
            check: Arc::new(move |event, _, _| {
                match event {
                    GameEvent::ZoneChange { id, to, .. }
                        if *id == target_id && matches!(to, ZoneId::Graveyard) => Some(vec![]),
                    _ => None,
                }
            }),
            effect: Effect(Arc::new(move |state, t, _| {
                change_zone(target_id, ZoneId::Exile, state, t, caster);
            })),
        };
        state.replacement_instances.push(re);
        eff_counter_target(caster).call(state, t, targets);
        state.replacement_instances.retain(|r| r.source_id != source_id);
    }))
}

/// Move the card in `targets[0]` onto the Battlefield.
/// Target selection happens in the strategy layer via `choose_spell_target`.
pub(crate) fn eff_reanimate(actor: PlayerId) -> Effect {
    Effect(Arc::new(move |state, t, targets| {
        if let Some(&id) = targets.first() {
            change_zone(id, ZoneId::Battlefield, state, t, actor);
        }
    }))
}

/// Search `who`'s library for a card matching `predicate` and move it to `dest`.
/// `predicate` and `dest` are built at load time — no string dispatch at simulation time.
pub(crate) fn eff_fetch_search(
    who: PlayerId,
    predicate: CardPredicate,
    dest: ZoneId,
) -> Effect {
    Effect(Arc::new(move |state, t, _targets| {
        use rand::Rng;
        // Library cards have no materialized state; fall back to catalog for the predicate check.
        let candidates: Vec<ObjId> = state.library_of(who)
            .filter(|c| {
                state.def_of(c.id)
                    .or_else(|| state.catalog.get(c.catalog_key.as_str()))
                    .map_or(false, |d| predicate(d))
            })
            .map(|c| c.id)
            .collect();
        if !candidates.is_empty() {
            let chosen_id = candidates[state.rng.gen_range(0..candidates.len())];
            let name = state.objects.get(&chosen_id).map(|c| c.catalog_key.clone()).unwrap_or_default();
            state.log(t, who, format!("search → {}", name));
            change_zone(chosen_id, dest, state, t, who);
        }
    }))
}
