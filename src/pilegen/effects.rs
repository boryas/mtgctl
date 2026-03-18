use std::sync::Arc;
use super::*;

/// Actor-relative player reference used in effect primitives.
/// `Actor` = the spell's controller; `Opp` = their opponent.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Who { Actor, Opp }

impl Who {
    pub(crate) fn resolve<'a>(&self, actor: &'a str) -> &'a str {
        match self { Who::Actor => actor, Who::Opp => opp_of(actor) }
    }
}

/// A composable game effect. Wraps a closure that mutates SimState.
/// Built from primitives (eff_draw, eff_destroy_target, etc.) and chained with `.then()`.
pub(crate) struct Effect(pub(crate) Arc<dyn Fn(&mut SimState, u8, &[ObjId], &mut dyn rand::RngCore) + Send + Sync>);

impl Clone for Effect {
    fn clone(&self) -> Self { Effect(Arc::clone(&self.0)) }
}

impl Effect {
    pub(crate) fn call(
        &self,
        state: &mut SimState,
        t: u8,
        targets: &[ObjId],
        rng: &mut dyn rand::RngCore,
    ) {
        (self.0)(state, t, targets, rng);
    }

    /// Chain two effects: `self` runs first, then `next`.
    pub(crate) fn then(self, next: Effect) -> Effect {
        let a = self.0;
        let b = next.0;
        Effect(Arc::new(move |state, t, targets: &[ObjId], rng| {
            a(state, t, targets, rng);
            b(state, t, targets, rng);
        }))
    }
}

// ── Effect primitives ─────────────────────────────────────────────────────────

/// Draw `n` cards for `who`.
pub(crate) fn eff_draw(who: impl Into<String>, n: usize) -> Effect {
    let who = who.into();
    Effect(Arc::new(move |state, t, _targets, rng| {
        for _ in 0..n {
            sim_draw(state, &who, t, false, rng);
        }
    }))
}

/// Put `n` cards back from `who`'s hand (Brainstorm put-back).
/// Moves `n` hand cards back to Library zone (unknown — just sets zone).
pub(crate) fn eff_put_back(who: impl Into<String>, n: usize) -> Effect {
    let who = who.into();
    Effect(Arc::new(move |state, t, _targets, rng| {
        let ids: Vec<ObjId> = state.hand_of(&who).map(|c| c.id).take(n).collect();
        for id in ids {
            change_zone(id, ZoneId::Library, state, t, &who, rng);
        }
    }))
}

/// `who` loses `n` life, with a log line.
pub(crate) fn eff_life_loss(who: impl Into<String>, n: i32) -> Effect {
    let who = who.into();
    Effect(Arc::new(move |state, t, _targets, _rng| {
        state.lose_life(&who, n);
        let life = state.life_of(&who);
        state.log(t, &who, format!("→ lose {} life (now {})", n, life));
    }))
}

/// Add mana per `spec` (e.g. `"BBB"`) to `who`'s pool.
pub(crate) fn eff_mana(who: impl Into<String>, spec: impl Into<String>) -> Effect {
    let who = who.into();
    let spec = spec.into();
    Effect(Arc::new(move |state, t, _targets, _rng| {
        let mc = parse_mana_cost(&spec);
        let pool = &mut state.player_mut(&who).pool;
        pool.w += mc.w; pool.u += mc.u; pool.b += mc.b;
        pool.r += mc.r; pool.g += mc.g; pool.c += mc.c;
        pool.total += mc.mana_value();
        state.log(t, &who, format!("→ add {} to pool", spec));
    }))
}

/// Destroy the permanent in `targets[0]`. `caster` used for logging.
pub(crate) fn eff_destroy_target(caster: impl Into<String>) -> Effect {
    let caster = caster.into();
    Effect(Arc::new(move |state, t, targets, rng| {
        if let Some(&id) = targets.first() {
            change_zone(id, ZoneId::Graveyard, state, t, &caster, rng);
        }
    }))
}

/// Exile the permanent in `targets[0]`.
pub(crate) fn eff_exile_target(caster: impl Into<String>) -> Effect {
    let caster = caster.into();
    Effect(Arc::new(move |state, t, targets, rng| {
        if let Some(&id) = targets.first() {
            change_zone(id, ZoneId::Exile, state, t, &caster, rng);
        }
    }))
}

/// Bounce the permanent in `targets[0]` to its controller's hand.
pub(crate) fn eff_bounce_target(caster: impl Into<String>) -> Effect {
    let caster = caster.into();
    Effect(Arc::new(move |state, t, targets, rng| {
        if let Some(&id) = targets.first() {
            change_zone(id, ZoneId::Hand, state, t, &caster, rng);
        }
    }))
}

/// Set `state.success = true` (Doomsday resolved).
pub(crate) fn eff_doomsday() -> Effect {
    Effect(Arc::new(|state, _t, _targets, _rng| {
        state.success = true;
    }))
}

/// Discard `n` random cards from `target`'s hand.
/// `filter` is a type predicate string (e.g. `"nonland"`, `"any"`, `""` = any).
pub(crate) fn eff_discard(caster: impl Into<String>, target: Who, n: usize, filter: impl Into<String>) -> Effect {
    let caster = caster.into();
    let filter = filter.into();
    let discard_pred = zone_pred_from_str(&filter);
    Effect(Arc::new(move |state, t, _targets, rng| {
        use rand::Rng;
        let target_who = target.resolve(&caster).to_string();
        for _ in 0..n {
            let candidates: Vec<ObjId> = state.hand_of(&target_who)
                .filter(|c| state.def_of(c.id).map_or(true, |d| discard_pred(d)))
                .map(|c| c.id)
                .collect();
            if candidates.is_empty() { break; }
            let id = candidates[rng.gen_range(0..candidates.len())];
            change_zone(id, ZoneId::Graveyard, state, t, &caster, rng);
        }
    }))
}

/// Put `card_name` onto the battlefield as a permanent for `owner`. Fires ETB triggers.
pub(crate) fn eff_enter_permanent(
    owner: impl Into<String>,
    card_name: impl Into<String>,
) -> Effect {
    let owner = owner.into();
    let card_name = card_name.into();
    Effect(Arc::new(move |state, t, _targets, rng| {
        let new_id = state.alloc_id();
        // Pre-register and immediately activate instances before the event fires,
        // so ETB replacement checks (e.g. Murktide self-ETB) can intercept the event.
        let card_def = state.catalog.get(card_name.as_str()).cloned();
        if let Some(ref def) = card_def {
            preregister_instances(def, new_id, &owner, state);
        }
        activate_instances(new_id, &owner, card_def.as_ref(), state);
        state.objects.insert(new_id, GameObject {
            id: new_id,
            catalog_key: card_name.clone(),
            owner: owner.clone(),
            controller: owner.clone(),
            zone: CardZone::Battlefield,
            is_token: false,
            spell: None,
            bf: Some(BattlefieldState {
                entered_this_turn: true,
                ..BattlefieldState::new()
            }),
            materialized: None,
        });
        fire_event(
            GameEvent::ZoneChange {
                id: new_id,
                actor: owner.clone(),
                from: ZoneId::Stack,
                to: ZoneId::Battlefield,
                controller: owner.clone(),
            },
            state, t, &owner, rng,
        );
        state.log(t, &owner, format!("{} enters play", card_name));
    }))
}

/// Counter the spell in `targets[0]` (a stack ObjId). Removes it from `state.stack` and
/// puts it in the owner's graveyard. If the target is no longer on the stack, fizzles.
pub(crate) fn eff_counter_target(caster: impl Into<String>) -> Effect {
    let caster = caster.into();
    Effect(Arc::new(move |state, t, targets, _rng| {
        let Some(&target_id) = targets.first() else { return; };
        let pos = state.stack.iter().position(|&id| id == target_id);
        if let Some(pos) = pos {
            state.stack.remove(pos);
            if let Some(card) = state.objects.get_mut(&target_id) {
                let name = card.catalog_key.clone();
                card.zone = CardZone::Graveyard;
                card.spell = None;
                state.log(t, &caster, format!("→ {} countered", name));
            } else {
                state.log(t, &caster, "→ ability countered".to_string());
            }
        } else {
            state.log(t, &caster, "→ fizzled (target already resolved)".to_string());
        }
    }))
}

/// Move the card in `targets[0]` onto the Battlefield.
/// Target selection happens in the strategy layer via `choose_spell_target`.
pub(crate) fn eff_reanimate(actor: impl Into<String>) -> Effect {
    let actor = actor.into();
    Effect(Arc::new(move |state, t, targets, rng| {
        if let Some(&id) = targets.first() {
            change_zone(id, ZoneId::Battlefield, state, t, &actor, rng);
        }
    }))
}

/// Search `who`'s library for a card matching `predicate` and move it to `dest`.
/// `predicate` and `dest` are built at load time — no string dispatch at simulation time.
pub(crate) fn eff_fetch_search(
    who: impl Into<String>,
    predicate: CardPredicate,
    dest: ZoneId,
) -> Effect {
    let who = who.into();
    Effect(Arc::new(move |state, t, _targets, rng| {
        use rand::Rng;
        // Library cards have no materialized state; fall back to catalog for the predicate check.
        let candidates: Vec<ObjId> = state.library_of(&who)
            .filter(|c| {
                state.def_of(c.id)
                    .or_else(|| state.catalog.get(c.catalog_key.as_str()))
                    .map_or(false, |d| predicate(d))
            })
            .map(|c| c.id)
            .collect();
        if !candidates.is_empty() {
            let chosen_id = candidates[rng.gen_range(0..candidates.len())];
            let name = state.objects.get(&chosen_id).map(|c| c.catalog_key.clone()).unwrap_or_default();
            state.log(t, &who, format!("search → {}", name));
            change_zone(chosen_id, dest, state, t, &who, rng);
        }
    }))
}
