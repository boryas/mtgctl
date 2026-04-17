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

/// Evaluator-driven put-back: score hand cards and put the `n` lowest-scoring
/// on top of library. Calls `state.evaluate_card` to score each hand card.
pub(crate) fn eff_put_back(who: PlayerId, n: usize) -> Effect {
    Effect(Arc::new(move |state, t, _targets| {
        for _ in 0..n {
            let eval = Arc::clone(&state.evaluate_card);
            let scored: Vec<(ObjId, f64)> = state.hand_of(who)
                .map(|c| (c.id, eval(who, c.id, state)))
                .collect();
            if let Some(&(worst_id, _)) = scored.iter()
                .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            {
                let name = state.objects.get(&worst_id).map(|o| o.catalog_key.clone()).unwrap_or_default();
                change_zone(worst_id, ZoneId::Library, state, t, who);
                let lib = match who {
                    PlayerId::Us  => &mut state.us.library_order,
                    PlayerId::Opp => &mut state.opp.library_order,
                };
                if lib.back() == Some(&worst_id) {
                    lib.pop_back();
                    lib.push_front(worst_id);
                }
                state.log(t, who, format!("puts back {}", name));
            }
        }
        // Put-back mixes known and unknown cards, and is frequently followed by a
        // shuffle (fetchland) — reset the ENTIRE hand to unknown, not just the
        // cards that were put back (we can't tell which ones remain).
        let remaining: Vec<ObjId> = state.hand_of(who).map(|c| c.id).collect();
        for id in remaining {
            if let Some(card) = state.objects.get_mut(&id) {
                card.zone = CardZone::Hand { known: false };
            }
        }
    }))
}

/// Scry N: look at top N cards of `who`'s library. For each, if the evaluator scores
/// it above threshold (0.3), keep on top; otherwise put on bottom.
/// Cards kept on top retain their relative order; bottomed cards go to the bottom.
pub(crate) fn eff_scry(who: PlayerId, n: usize) -> Effect {
    Effect(Arc::new(move |state, _t, _targets| {
        let eval = Arc::clone(&state.evaluate_card);
        let lib = match who {
            PlayerId::Us  => &state.us.library_order,
            PlayerId::Opp => &state.opp.library_order,
        };
        let top_ids: Vec<ObjId> = lib.iter().take(n).copied().collect();
        if top_ids.is_empty() { return; }

        let mut keep_top = Vec::new();
        let mut send_bottom = Vec::new();
        for &id in &top_ids {
            let score = eval(who, id, state);
            if score >= 0.3 {
                keep_top.push(id);
            } else {
                send_bottom.push(id);
            }
        }
        // Remove the N cards from front of library, then re-insert:
        // kept cards go back to front (preserving order), bottomed cards go to back.
        let lib = match who {
            PlayerId::Us  => &mut state.us.library_order,
            PlayerId::Opp => &mut state.opp.library_order,
        };
        for _ in 0..top_ids.len().min(lib.len()) {
            lib.pop_front();
        }
        // Push kept cards back to front (in reverse to preserve order).
        for &id in keep_top.iter().rev() {
            lib.push_front(id);
        }
        // Push bottomed cards to back.
        for &id in &send_bottom {
            lib.push_back(id);
        }
        let kept = keep_top.len();
        let bottomed = send_bottom.len();
        state.log(0, who, format!("scry {} → {} top, {} bottom", n, kept, bottomed));
    }))
}

/// Order top N: sort top N cards of `who`'s library by evaluator score (best on top).
/// Used by Ponder: look at top 3 and arrange them.
pub(crate) fn eff_order(who: PlayerId, n: usize) -> Effect {
    Effect(Arc::new(move |state, _t, _targets| {
        let eval = Arc::clone(&state.evaluate_card);
        let lib = match who {
            PlayerId::Us  => &state.us.library_order,
            PlayerId::Opp => &state.opp.library_order,
        };
        let mut top: Vec<ObjId> = lib.iter().take(n).copied().collect();
        if top.len() < 2 { return; }

        // Score each card.
        let mut scored: Vec<(ObjId, f64)> = top.iter()
            .map(|&id| (id, eval(who, id, state)))
            .collect();
        // Sort best first (highest score on top = front of library).
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        top = scored.into_iter().map(|(id, _)| id).collect();

        // Remove the N cards from front, re-insert in sorted order.
        let lib = match who {
            PlayerId::Us  => &mut state.us.library_order,
            PlayerId::Opp => &mut state.opp.library_order,
        };
        for _ in 0..n.min(lib.len()) {
            lib.pop_front();
        }
        for &id in top.iter().rev() {
            lib.push_front(id);
        }
    }))
}

/// Maybe-shuffle: if the top card of `who`'s library scores below threshold (0.3),
/// shuffle the whole library. Used by Ponder after ordering top 3 — if even the best
/// arrangement is bad, shuffle for a fresh random draw instead.
pub(crate) fn eff_maybe_shuffle(who: PlayerId) -> Effect {
    Effect(Arc::new(move |state, _t, _targets| {
        let eval = Arc::clone(&state.evaluate_card);
        let top_id = match who {
            PlayerId::Us  => state.us.library_order.front().copied(),
            PlayerId::Opp => state.opp.library_order.front().copied(),
        };
        if let Some(id) = top_id {
            let score = eval(who, id, state);
            if score < 0.3 {
                state.shuffle_library(who);
                state.log(0, who, "Ponder → shuffles".to_string());
            }
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

/// Deal `n` damage to the permanent in `targets[0]`. SBAs handle lethal-damage destruction.
/// Deal `n` damage to a target — creature, planeswalker, or player (CR 120.2).
/// `source_id` identifies the damage source for protection checks (CR 702.16b).
pub(crate) fn eff_damage_target(caster: PlayerId, n: i32, source_id: ObjId) -> Effect {
    Effect(Arc::new(move |state, t, targets| {
        let Some(&id) = targets.first() else { return; };
        if is_protected_from(id, source_id, state) {
            let name = state.objects.get(&id).map(|o| o.catalog_key.as_str()).unwrap_or("?");
            state.log(t, caster, format!("→ damage to {} prevented (protection)", name));
            return;
        }
        if id == state.us.id || id == state.opp.id {
            let who = state.who_pid(id);
            state.lose_life(who, n);
            state.log(t, caster, format!("→ deals {} damage to {}", n, who));
        } else {
            if let Some(bf) = state.permanent_bf_mut(id) {
                bf.damage += n;
            }
            let name = state.objects.get(&id).map(|o| o.catalog_key.as_str()).unwrap_or("?");
            state.log(t, caster, format!("→ deals {} damage to {}", n, name));
        }
    }))
}

/// Deal `n` damage to every permanent on the battlefield matching `filter`.
/// Protection from the source prevents the damage (checked per-permanent).
pub(crate) fn eff_damage_all(caster: PlayerId, n: i32, source_id: ObjId, filter: ObjPredicate) -> Effect {
    Effect(Arc::new(move |state, t, _targets| {
        let hits: Vec<ObjId> = state.objects.values()
            .filter(|o| o.zone == CardZone::Battlefield && filter(o.id, state))
            .map(|o| o.id)
            .collect();
        for id in hits {
            if is_protected_from(id, source_id, state) { continue; }
            if let Some(bf) = state.permanent_bf_mut(id) {
                bf.damage += n;
            }
        }
        state.log(t, caster, format!("→ deals {} damage to each matching permanent", n));
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
pub(crate) fn destroy_one(id: ObjId, state: &mut SimState, t: u8, actor: PlayerId) {
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

/// Exile all targets (for "exile any number of target ..." effects).
pub(crate) fn eff_exile_all_targets(caster: PlayerId) -> Effect {
    Effect(Arc::new(move |state, t, targets| {
        for &id in targets {
            change_zone(id, ZoneId::Exile, state, t, caster);
        }
    }))
}

/// Exile target creature; its controller gains life equal to its power.
pub(crate) fn eff_exile_target_gain_power(caster: PlayerId) -> Effect {
    Effect(Arc::new(move |state, t, targets| {
        if let Some(&id) = targets.first() {
            // Read power before exiling (materialized view).
            let (controller, power) = state.def_of(id)
                .and_then(|d| d.as_creature().map(|c| c.power()))
                .map(|p| (state.objects.get(&id).map_or(caster, |o| o.controller), p))
                .unwrap_or((caster, 0));
            change_zone(id, ZoneId::Exile, state, t, caster);
            if power > 0 {
                state.gain_life(controller, power);
                state.log(t, caster, format!("→ {} gains {} life", controller, power));
            }
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

/// Doomsday resolution: halve life (rounded up), then set success.
/// Library + graveyard contents remain in place as the "pool" for pile selection;
/// the simulation stops here so the viewer can see what's available.
pub(crate) fn eff_doomsday() -> Effect {
    Effect(Arc::new(|state, _t, _targets| {
        let life = state.player(PlayerId::Us).life;
        state.life_before_dd = Some(life);
        state.player_mut(PlayerId::Us).life = life / 2;
        state.success = true;
    }))
}

/// Mark all cards in `target`'s hand as known (visible to the other player).
/// Models "Target player reveals their hand" oracle text (CR 701.16).
pub(crate) fn eff_reveal_hand(caster: PlayerId, target: Who) -> Effect {
    Effect(Arc::new(move |state, t, _targets| {
        let target_who = target.resolve(caster);
        let ids: Vec<ObjId> = state.hand_of(target_who).map(|c| c.id).collect();
        let names: Vec<String> = ids.iter()
            .filter_map(|id| state.objects.get(id))
            .map(|c| c.catalog_key.clone())
            .collect();
        for id in &ids {
            if let Some(card) = state.objects.get_mut(id) {
                card.zone = CardZone::Hand { known: true };
            }
        }
        if !names.is_empty() {
            state.log(t, caster, format!("reveals hand: {}", names.join(", ")));
        }
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
            counters: HashMap::new(), ci_timestamp: 0,
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
pub(crate) fn counter_one(id: ObjId, state: &mut SimState, t: u8, actor: PlayerId) {
    let pos = state.stack.iter().position(|&sid| sid == id);
    if let Some(pos) = pos {
        // Prohibition gate: "can't be countered" CE effects (CR 614.17).
        // Only fires for spell objects; triggered abilities on the stack are not spells.
        if state.objects.contains_key(&id) {
            let spell_caster = state.objects[&id].controller;
            if fire_event(GameEvent::SpellBeingCountered { caster: spell_caster, card_id: id }, state, t, actor) {
                return;
            }
            // Check materialized prohibition_defs (CE-granted "can't be countered").
            // Mirrors how fire_triggers reads granted_trigger_defs from materialized defs.
            let mat_prohibited = state.def_of(id).map_or(false, |d| {
                let event = GameEvent::SpellBeingCountered { caster: spell_caster, card_id: id };
                d.prohibition_defs.iter().any(|p| (p.check)(&event, id, spell_caster, state))
            });
            if mat_prohibited {
                let name = state.stack_item_display_name(id).to_string();
                state.log(t, actor, format!("→ fizzled ({} can't be countered)", name));
                return;
            }
        }
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
            let ghost = state.objects.get(&id)
                .map(|c| format!("{} (zone={:?})", c.catalog_key, c.zone))
                .unwrap_or_else(|| format!("obj#{}", id.0));
            state.log(t, actor, format!("→ fizzled (target {} not on stack)", ghost));
        }
    } else {
        let ghost = state.objects.get(&id)
            .map(|c| format!("{} (zone={:?})", c.catalog_key, c.zone))
            .unwrap_or_else(|| format!("obj#{}", id.0));
        state.log(t, actor, format!("→ fizzled (target {} not on stack)", ghost));
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
        let re = ReplacementInstance {
            source_id,
            controller: caster,
            check: Arc::new(move |event, _, _, _state| {
                match event {
                    GameEvent::ZoneChange { id, to, .. }
                        if id == &target_id && matches!(to, ZoneId::Graveyard) => Some(vec![]),
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
            // CR 701.19: shuffle library after searching.
            state.shuffle_library(who);
        }
    }))
}

/// Each player may put a card matching `filter` from their hand onto the battlefield.
/// Both choices are collected before either placement, so the placements are simultaneous
/// (CR 101.4 — "each" effects are simultaneous; no triggers fire between them).
pub(crate) fn eff_each_may_put(caster: PlayerId, filter: CardPredicate) -> Effect {
    Effect(Arc::new(move |state, t, _targets| {
        let f = std::sync::Arc::clone(&state.resolve_choice);
        let mut to_place: Vec<(ObjId, PlayerId)> = Vec::new();
        for &player in &[caster, caster.opp()] {
            let candidates: Vec<ObjId> = state.hand_of(player)
                .filter(|c| {
                    state.def_of(c.id)
                        .or_else(|| state.catalog.get(c.catalog_key.as_str()))
                        .map_or(false, |d| filter(d))
                })
                .map(|c| c.id)
                .collect();
            if candidates.is_empty() { continue; }
            let req = ChoiceRequest::MayPutOnBattlefield { candidates };
            if let ChoiceResult::OptionalObject(Some(id)) = f(ObjId(0), &req, state) {
                // Validate the chosen id is actually in the candidate set.
                if let ChoiceRequest::MayPutOnBattlefield { ref candidates } = req {
                    if candidates.contains(&id) {
                        to_place.push((id, player));
                    }
                }
            }
        }
        // Place all chosen cards simultaneously — no triggers fire between placements.
        for (id, player) in to_place {
            let name = state.objects.get(&id).map(|c| c.catalog_key.clone()).unwrap_or_default();
            state.log(t, player, format!("puts {} onto the battlefield", name));
            change_zone(id, ZoneId::Battlefield, state, t, player);
        }
    }))
}

/// Counter target spell unless its controller pays `cost` (CR 700.2).
/// Reuses `ChoiceRequest::WardPayment` for the pay-or-decline decision.
pub(crate) fn eff_counter_unless_pays(caster: PlayerId, cost: Vec<CostComponent>) -> Effect {
    Effect(Arc::new(move |state, t, targets| {
        if let Some(&spell_id) = targets.first() {
            let spell_controller = state.objects.get(&spell_id)
                .map(|o| o.controller)
                .unwrap_or(caster.opp());
            let can_pay = can_pay_costs(&cost, state, spell_controller, spell_id, false, 0);
            let will_pay = can_pay && {
                let f = std::sync::Arc::clone(&state.resolve_choice);
                matches!(f(spell_id, &ChoiceRequest::WardPayment { cost: cost.to_vec() }, state), ChoiceResult::Bool(true))
            };
            if will_pay {
                pay_costs(&cost, state, t, spell_controller, spell_id, 0);
                let name = state.objects.get(&spell_id).map(|o| o.catalog_key.clone()).unwrap_or_default();
                state.log(t, spell_controller, format!("→ pays tax for {}", name));
            } else {
                counter_one(spell_id, state, t, caster);
            }
        }
    }))
}

/// Placeholder for Atraxa, Grand Unifier's ETB: reveal top 10, for each card type
/// you may put one into your hand. Real implementation needs per-type strategy choices
/// over actual revealed cards; for now just silently move `n` library cards to hand
/// (no Draw events — does not trigger Bowmasters etc.).
///
/// TODO: replace with real reveal-top-10-by-card-type once hands are fully tracked.
pub(crate) fn eff_hand_boost(who: PlayerId, n: usize) -> Effect {
    Effect(Arc::new(move |state, t, _targets| {
        let ids: Vec<ObjId> = state.library_of(who).map(|o| o.id).take(n).collect();
        let count = ids.len();
        for id in ids {
            state.set_card_zone(id, CardZone::Hand { known: true });
        }
        state.log(t, who, format!("Atraxa ETB: {} cards to hand (placeholder)", count));
    }))
}

/// Ward pay-or-counter effect (CR 702.20).
/// Offers `targeting_caster` the chance to pay `cost`; if they decline (or can't pay),
/// `targeting_spell` is countered. Called from Ward `TriggerContext` effects.
pub(crate) fn ward_pay_or_counter(
    ward_source: ObjId,
    cost: &[CostComponent],
    targeting_spell: ObjId,
    targeting_caster: PlayerId,
    ward_holder: PlayerId,
    state: &mut SimState,
    t: u8,
) {
    let can_pay = can_pay_costs(cost, state, targeting_caster, ward_source, false, 0);
    let will_pay = can_pay && {
        let f = std::sync::Arc::clone(&state.resolve_choice);
        matches!(f(ward_source, &ChoiceRequest::WardPayment { cost: cost.to_vec() }, state), ChoiceResult::Bool(true))
    };
    if will_pay {
        pay_costs(cost, state, t, targeting_caster, ward_source, 0);
        state.log(t, targeting_caster, "→ pays ward cost".to_string());
    } else {
        state.log(t, ward_holder, "→ ward: countering spell (cost not paid)".to_string());
        counter_one(targeting_spell, state, t, ward_holder);
    }
}
