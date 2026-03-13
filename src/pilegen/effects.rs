use std::sync::Arc;
use std::collections::HashMap;
use super::*;

/// Actor-relative player reference used in effect primitives.
/// `Actor` = the spell's controller; `Opp` = their opponent.
#[derive(Clone, Copy)]
pub(crate) enum Who { Actor, Opp }

impl Who {
    pub(crate) fn resolve<'a>(&self, actor: &'a str) -> &'a str {
        match self { Who::Actor => actor, Who::Opp => opp_of(actor) }
    }
}

/// A composable game effect. Wraps a closure that mutates SimState.
/// Built from primitives (eff_draw, eff_destroy_target, etc.) and chained with `.then()`.
pub(crate) struct Effect(pub(crate) Arc<dyn Fn(&mut SimState, u8, &[Target], &HashMap<&str, &CardDef>, &mut dyn rand::RngCore) + Send + Sync>);

impl Clone for Effect {
    fn clone(&self) -> Self { Effect(Arc::clone(&self.0)) }
}

impl Effect {
    pub(crate) fn call(
        &self,
        state: &mut SimState,
        t: u8,
        targets: &[Target],
        catalog: &HashMap<&str, &CardDef>,
        rng: &mut dyn rand::RngCore,
    ) {
        (self.0)(state, t, targets, catalog, rng);
    }

    /// Chain two effects: `self` runs first, then `next`.
    pub(crate) fn then(self, next: Effect) -> Effect {
        let a = self.0;
        let b = next.0;
        Effect(Arc::new(move |state, t, targets, catalog, rng| {
            a(state, t, targets, catalog, rng);
            b(state, t, targets, catalog, rng);
        }))
    }
}

// ── Effect primitives ─────────────────────────────────────────────────────────

/// Draw `n` cards for `who`.
pub(crate) fn eff_draw(who: impl Into<String>, n: usize) -> Effect {
    let who = who.into();
    Effect(Arc::new(move |state, t, _targets, _catalog, _rng| {
        for _ in 0..n {
            state.sim_draw(&who, t, false);
        }
    }))
}

/// Put `n` cards back from `who`'s hand (Brainstorm put-back).
pub(crate) fn eff_put_back(who: impl Into<String>, n: usize) -> Effect {
    let who = who.into();
    Effect(Arc::new(move |state, _t, _targets, _catalog, _rng| {
        let actual = (n as i32).min(state.player(&who).hand.hidden);
        state.player_mut(&who).hand.hidden -= actual;
    }))
}

/// `who` loses `n` life, with a log line.
pub(crate) fn eff_life_loss(who: impl Into<String>, n: i32) -> Effect {
    let who = who.into();
    Effect(Arc::new(move |state, t, _targets, _catalog, _rng| {
        state.lose_life(&who, n);
        let life = state.life_of(&who);
        state.log(t, &who, format!("→ lose {} life (now {})", n, life));
    }))
}

/// Add mana per `spec` (e.g. `"BBB"`) to `who`'s pool.
pub(crate) fn eff_mana(who: impl Into<String>, spec: impl Into<String>) -> Effect {
    let who = who.into();
    let spec = spec.into();
    Effect(Arc::new(move |state, t, _targets, _catalog, _rng| {
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
    Effect(Arc::new(move |state, t, targets, _catalog, _rng| {
        if let Some(Target::Object(id)) = targets.first() {
            apply_effect_to("destroy", *id, state, t, &caster);
        }
    }))
}

/// Bounce the permanent in `targets[0]` to its controller's hand.
pub(crate) fn eff_bounce_target(caster: impl Into<String>) -> Effect {
    let caster = caster.into();
    Effect(Arc::new(move |state, t, targets, _catalog, _rng| {
        if let Some(Target::Object(id)) = targets.first() {
            let id = *id;
            let controller = state.permanent_controller(id).map(|s| s.to_string());
            let name = state.permanent_name(id);
            if let (Some(controller), Some(name)) = (controller, name) {
                if let Some(idx) = state.player(&controller).permanents.iter().position(|p| p.id == id) {
                    state.player_mut(&controller).permanents.remove(idx);
                    state.player_mut(&controller).hand.hidden += 1;
                    state.log(t, &caster, format!("→ {} returned to {}'s hand", name, controller));
                }
            }
        }
    }))
}

/// Set `state.success = true` (Doomsday resolved).
pub(crate) fn eff_doomsday() -> Effect {
    Effect(Arc::new(|state, _t, _targets, _catalog, _rng| {
        state.success = true;
    }))
}

/// Discard `n` random cards from `target`'s hand. `nonland=true` skips lands.
pub(crate) fn eff_discard(caster: impl Into<String>, target: Who, n: usize, nonland: bool) -> Effect {
    let caster = caster.into();
    Effect(Arc::new(move |state, t, _targets, _catalog, rng| {
        use rand::Rng;
        let target_who = target.resolve(&caster).to_string();
        let mut lib = std::mem::take(&mut state.player_mut(&target_who).library);
        let mut discarded: Vec<String> = Vec::new();
        for _ in 0..n {
            if state.player(&target_who).hand.hidden <= 0 { break; }
            let candidates: Vec<usize> = lib.iter().enumerate()
                .filter(|(_, (_, _, d))| !nonland || !d.is_land())
                .map(|(i, _)| i)
                .collect();
            if candidates.is_empty() { break; }
            let idx = candidates[rng.gen_range(0..candidates.len())];
            let (_id, card, _) = lib.remove(idx);
            state.player_mut(&target_who).hand.hidden -= 1;
            state.player_mut(&target_who).graveyard.visible.push(card.clone());
            discarded.push(card);
        }
        state.player_mut(&target_who).library = lib;
        if !discarded.is_empty() {
            state.log(t, &caster, format!("→ {} discards: {}", target_who, discarded.join(", ")));
        }
    }))
}

/// Put `card_name` onto the battlefield as a permanent for `owner`. Fires ETB triggers.
pub(crate) fn eff_enter_permanent(
    owner: impl Into<String>,
    card_name: impl Into<String>,
    annotation: Option<String>,
) -> Effect {
    let owner = owner.into();
    let card_name = card_name.into();
    Effect(Arc::new(move |state, t, _targets, catalog, rng| {
        use rand::Rng;
        let (counters, ann) = match annotation.as_deref() {
            Some(s) if s.starts_with('+') => (s[1..].parse::<i32>().unwrap_or(0), None),
            _ => {
                let ann = if annotation.is_some() {
                    annotation.clone()
                } else if let Some(d) = catalog.get(card_name.as_str()) {
                    if !d.annotation_options().is_empty() {
                        Some(d.annotation_options()[rng.gen_range(0..d.annotation_options().len())].clone())
                    } else { None }
                } else { None };
                (0, ann)
            }
        };
        let pw_loyalty = catalog.get(card_name.as_str())
            .and_then(|d| if let CardKind::Planeswalker(ref p) = d.kind { Some(p.loyalty) } else { None })
            .unwrap_or(0);
        let mana_abs = catalog.get(card_name.as_str())
            .map_or_else(Vec::new, |d| d.mana_abilities().to_vec());
        let new_id = state.alloc_id();
        state.cards.insert(new_id, CardObject::new(new_id, card_name.clone(), &owner));
        state.player_mut(&owner).permanents.push(SimPermanent {
            id: new_id,
            name: card_name.clone(),
            annotation: ann,
            counters,
            tapped: false,
            damage: 0,
            entered_this_turn: true,
            mana_abilities: mana_abs,
            attacking: false,
            unblocked: false,
            loyalty: pw_loyalty,
            pw_activated_this_turn: false,
            attack_target: None,
            power_mod: 0,
            toughness_mod: 0,
        });
        let etb_ev = GameEvent::ZoneChange {
            card: card_name.clone(),
            card_type: "creature".to_string(),
            from: ZoneId::Stack,
            to: ZoneId::Battlefield,
            controller: owner.clone(),
        };
        state.queue_triggers(&etb_ev);
        state.log(t, &owner, format!("{} enters play", card_name));
    }))
}

/// Counter the spell in `targets[0]` (a stack ObjId). Removes it from `state.stack` and
/// puts it in the owner's graveyard. If the target is no longer on the stack, fizzles.
pub(crate) fn eff_counter_target(caster: impl Into<String>) -> Effect {
    let caster = caster.into();
    Effect(Arc::new(move |state, t, targets, _catalog, _rng| {
        let Some(Target::Object(target_id)) = targets.first() else { return; };
        let target_id = *target_id;
        let pos = state.stack.iter().position(|s| s.id() == target_id);
        if let Some(pos) = pos {
            let target = state.stack.remove(pos);
            let target_owner = state.who_str(target.owner()).to_string();
            let target_name = target.display_name().to_string();
            state.player_mut(&target_owner).graveyard.visible.push(target_name.clone());
            state.log(t, &caster, format!("→ {} countered", target_name));
        } else {
            state.log(t, &caster, "→ fizzled (target already resolved)".to_string());
        }
    }))
}

/// Reanimate a random card of `type_filter` from `target`'s graveyard.
pub(crate) fn eff_reanimate(actor: impl Into<String>, target: Who, type_filter: impl Into<String>) -> Effect {
    let actor = actor.into();
    let type_filter = type_filter.into();
    Effect(Arc::new(move |state, t, _targets, catalog, rng| {
        use rand::Rng;
        let target_who = target.resolve(&actor).to_string();
        let candidates: Vec<String> = state.player(&target_who).graveyard.visible.iter()
            .filter(|n| catalog.get(n.as_str())
                .map(|d| matches_target_type(&type_filter, &d.kind, false, Some(*d)))
                .unwrap_or(false))
            .cloned()
            .collect();
        if candidates.is_empty() { return; }
        let chosen = candidates[rng.gen_range(0..candidates.len())].clone();
        state.player_mut(&target_who).graveyard.visible.retain(|n| n != &chosen);
        let mana_abs = catalog.get(chosen.as_str()).map_or_else(Vec::new, |d| d.mana_abilities().to_vec());
        let new_id = state.alloc_id();
        state.cards.insert(new_id, CardObject::new(new_id, chosen.clone(), &target_who));
        state.player_mut(&target_who).permanents.push(SimPermanent {
            id: new_id,
            name: chosen.clone(),
            mana_abilities: mana_abs,
            ..SimPermanent::new(&chosen)
        });
        state.log(t, &actor, format!("→ {} returns from graveyard", chosen));
    }))
}
