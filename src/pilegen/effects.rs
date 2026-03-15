use std::sync::Arc;
use std::collections::HashMap;
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
    Effect(Arc::new(move |state, t, _targets, catalog, rng| {
        for _ in 0..n {
            sim_draw(state, &who, t, false, catalog, rng);
        }
    }))
}

/// Put `n` cards back from `who`'s hand (Brainstorm put-back).
/// Moves `n` hand cards back to Library zone (unknown — just sets zone).
pub(crate) fn eff_put_back(who: impl Into<String>, n: usize) -> Effect {
    let who = who.into();
    Effect(Arc::new(move |state, _t, _targets, _catalog, _rng| {
        let ids: Vec<ObjId> = state.hand_of(&who).map(|c| c.id).take(n).collect();
        for id in ids {
            if let Some(card) = state.cards.get_mut(&id) {
                card.zone = CardZone::Library;
            }
        }
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
    Effect(Arc::new(move |state, t, targets, catalog, rng| {
        if let Some(Target::Object(id)) = targets.first() {
            change_zone(*id, ZoneId::Graveyard, state, t, &caster, catalog, rng);
        }
    }))
}

/// Bounce the permanent in `targets[0]` to its controller's hand.
pub(crate) fn eff_bounce_target(caster: impl Into<String>) -> Effect {
    let caster = caster.into();
    Effect(Arc::new(move |state, t, targets, catalog, rng| {
        if let Some(Target::Object(id)) = targets.first() {
            change_zone(*id, ZoneId::Hand, state, t, &caster, catalog, rng);
        }
    }))
}

/// Set `state.success = true` (Doomsday resolved).
pub(crate) fn eff_doomsday() -> Effect {
    Effect(Arc::new(|state, _t, _targets, _catalog, _rng| {
        state.success = true;
    }))
}

/// Discard `n` random cards from `target`'s hand.
/// `filter` is a type predicate string (e.g. `"nonland"`, `"any"`, `""` = any).
pub(crate) fn eff_discard(caster: impl Into<String>, target: Who, n: usize, filter: impl Into<String>) -> Effect {
    let caster = caster.into();
    let filter = filter.into();
    Effect(Arc::new(move |state, t, _targets, catalog, rng| {
        use rand::Rng;
        let target_who = target.resolve(&caster).to_string();
        for _ in 0..n {
            let candidates: Vec<ObjId> = state.hand_of(&target_who)
                .filter(|c| filter.is_empty() || filter == "any" || catalog.get(c.name.as_str())
                    .map_or(true, |d| matches_target_type(&filter, &d.kind, false, Some(d))))
                .map(|c| c.id)
                .collect();
            if candidates.is_empty() { break; }
            let id = candidates[rng.gen_range(0..candidates.len())];
            change_zone(id, ZoneId::Graveyard, state, t, &caster, catalog, rng);
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
        // Pre-register and immediately activate instances before the event fires,
        // so ETB replacement checks (e.g. Murktide self-ETB) can intercept the event.
        if let Some(def) = catalog.get(card_name.as_str()) {
            preregister_instances(def, new_id, &owner, state);
        }
        activate_instances(new_id, state);
        state.cards.insert(new_id, CardObject {
            id: new_id,
            name: card_name.clone(),
            owner: owner.clone(),
            controller: owner.clone(),
            zone: CardZone::Battlefield,
            spell: None,
            bf: Some(BattlefieldState {
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
                active_face: 0,
            }),
        });
        fire_event(
            GameEvent::ZoneChange {
                id: new_id,
                actor: owner.clone(),
                card: card_name.clone(),
                card_type: "creature".to_string(),
                from: ZoneId::Stack,
                to: ZoneId::Battlefield,
                controller: owner.clone(),
            },
            state, t, &owner, catalog, rng,
        );
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
        let pos = state.stack.iter().position(|&id| id == target_id);
        if let Some(pos) = pos {
            state.stack.remove(pos);
            if let Some(card) = state.cards.get_mut(&target_id) {
                let name = card.name.clone();
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
    Effect(Arc::new(move |state, t, targets, catalog, rng| {
        if let Some(Target::Object(id)) = targets.first() {
            change_zone(*id, ZoneId::Battlefield, state, t, &actor, catalog, rng);
        }
    }))
}

/// Search the library for a land matching `filter` and put it into `dest` ("play" or "hand").
/// Used for fetchland abilities (e.g. `search:land-island|swamp:play`).
pub(crate) fn eff_fetch_search(
    who: impl Into<String>,
    source_id: ObjId,
    filter: impl Into<String>,
    dest: impl Into<String>,
) -> Effect {
    let who = who.into();
    let filter = filter.into();
    let dest = dest.into();
    Effect(Arc::new(move |state, t, _targets, catalog, rng| {
        use rand::Rng;
        let source_name = state.permanent_name(source_id).unwrap_or_default();
        // Collect candidates from Library zone.
        let candidates: Vec<(ObjId, String)> = state.library_of(&who)
            .filter(|c| catalog.get(c.name.as_str()).map_or(false, |d| matches_search_filter(&filter, d)))
            .map(|c| (c.id, c.name.clone()))
            .collect();
        if !candidates.is_empty() {
            // Prefer a black-producing land if available.
            let black_candidates: Vec<(ObjId, String)> = candidates.iter()
                .filter(|(_, n)| catalog.get(n.as_str()).and_then(|d| d.as_land()).map_or(false, |l| {
                    l.land_types.swamp || l.mana_abilities.iter().any(|ma| ma.produces.contains('B'))
                }))
                .cloned()
                .collect();
            let pool = if !black_candidates.is_empty() { &black_candidates } else { &candidates };
            let (chosen_id, name) = pool[rng.gen_range(0..pool.len())].clone();
            let mana_abilities = catalog.get(name.as_str())
                .and_then(|d| d.as_land())
                .map(|l| l.mana_abilities.clone())
                .unwrap_or_default();
            match dest.as_str() {
                "play" => {
                    if let Some(card) = state.cards.get_mut(&chosen_id) {
                        card.zone = CardZone::Battlefield;
                        card.bf = Some(BattlefieldState {
                            tapped: false,
                            mana_abilities,
                            ..BattlefieldState::new(vec![])
                        });
                    }
                    state.log(t, &who, format!("{} ability → {}", source_name, name));
                }
                "hand" => {
                    if let Some(card) = state.cards.get_mut(&chosen_id) {
                        card.zone = CardZone::Hand { known: true };
                    }
                    state.log(t, &who, format!("{} ability → {} (to hand)", source_name, name));
                }
                _ => {}
            }
        }
    }))
}
