use rand::Rng;
use std::collections::HashMap;
use super::*;

pub(super) fn collect_on_board_actions(
    state: &mut SimState,
    ap: &str,
    t: u8,
    dd_turn: u8,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> Vec<PriorityAction> {
    let mut actions: Vec<PriorityAction> = Vec::new();

    // 75% roll per land (permanent that is_land) with an available ability.
    let land_ids: Vec<(ObjId, String)> = state.permanents_of(ap)
        .filter(|p| !p.bf.as_ref().map_or(false, |bf| bf.tapped))
        .filter(|p| catalog_map.get(p.name.as_str())
            .map_or(false, |def| def.is_land() && def.abilities().iter()
                .any(|ab| ability_available(ab, state, ap, true, catalog_map))))
        .map(|p| (p.id, p.name.clone()))
        .collect();
    for (land_id, name) in land_ids {
        if rng.gen_bool(0.75) {
            if let Some(def) = catalog_map.get(name.as_str()) {
                if let Some(ab) = def.abilities().iter()
                    .find(|ab| ability_available(ab, state, ap, true, catalog_map))
                    .cloned()
                {
                    actions.push(PriorityAction::ActivateAbility(land_id, ab));
                }
            }
        }
    }

    // 75% roll per permanent with an available non-loyalty ability.
    let perm_ids: Vec<(ObjId, String, bool)> = state.permanents_of(ap)
        .filter(|p| {
            let tapped = p.bf.as_ref().map_or(false, |bf| bf.tapped);
            catalog_map.get(p.name.as_str())
                .map_or(false, |def| def.abilities().iter()
                    .any(|ab| ab.loyalty_cost.is_none() && ability_available(ab, state, ap, !tapped, catalog_map)))
        })
        .map(|p| (p.id, p.name.clone(), p.bf.as_ref().map_or(false, |bf| bf.tapped)))
        .collect();
    for (perm_id, name, tapped) in perm_ids {
        if rng.gen_bool(0.75) {
            if let Some(def) = catalog_map.get(name.as_str()) {
                if let Some(ab) = def.abilities().iter()
                    .find(|ab| ab.loyalty_cost.is_none() && ability_available(ab, state, ap, !tapped, catalog_map))
                    .cloned()
                {
                    actions.push(PriorityAction::ActivateAbility(perm_id, ab));
                }
            }
        }
    }

    // Adventure creatures in exile: 75% roll to cast the creature face.
    let on_adventure_names: Vec<String> = state.on_adventure_of(ap).map(|c| c.name.clone()).collect();
    for card_name in on_adventure_names {
        if let Some(&def) = catalog_map.get(card_name.as_str()) {
            let cost = parse_mana_cost(def.mana_cost());
            if !state.potential_mana(ap).can_pay(&cost) { continue; }
            if rng.gen_bool(0.75) {
                actions.push(PriorityAction::CastFromAdventure { card_name });
            }
        }
    }

    // Fateful turn override: force-include fetch lands that can search for a black source,
    // if we have no black mana. (These bypass the 75% roll.)
    if ap == "us" && t == dd_turn && !state.has_black_mana("us") {
        let can_search_black = |name: &str| catalog_map.get(name).map_or(false, |def|
            def.abilities().iter().any(|ab|
                ab.effect.starts_with("search:land-swamp")
                    || ab.effect.starts_with("search:land-island|swamp")
            )
        );
        let fetch_ids: Vec<(ObjId, String)> = state.permanents_of("us")
            .filter(|p| !p.bf.as_ref().map_or(false, |bf| bf.tapped) && can_search_black(&p.name))
            .map(|p| (p.id, p.name.clone()))
            .collect();
        if !fetch_ids.is_empty() {
            for (fid, name) in &fetch_ids {
                // Add if not already in the list.
                if !actions.iter().any(|a| matches!(a, PriorityAction::ActivateAbility(id, _) if *id == *fid)) {
                    if let Some(def) = catalog_map.get(name.as_str()) {
                        if let Some(ab) = def.abilities().iter()
                            .find(|ab| ability_available(ab, state, "us", true, catalog_map))
                            .cloned()
                        {
                            actions.push(PriorityAction::ActivateAbility(*fid, ab));
                        }
                    }
                }
            }
        } else {
            // No fetch available — ensure the land drop fires.
            state.us.must_land_drop = true;
        }
    }

    actions
}

/// True if `name` is a spell the NAP considers worth spending a free counterspell (FoW / Daze) on.
/// Cantrips and mana rituals are not worth pitching; permanents and combo pieces are.
fn worth_countering(name: &str, catalog_map: &HashMap<&str, &CardDef>) -> bool {
    if let Some(def) = catalog_map.get(name) {
        match &def.kind {
            CardKind::Creature(_) | CardKind::Planeswalker(_)
            | CardKind::Artifact(_) | CardKind::Enchantment => return true,
            _ => {}
        }
    }
    // High-value non-permanent spells: combo kill, mass discard
    matches!(name, "Doomsday" | "Hymn to Tourach" | "Unearth")
}

/// NAP decision: if AP just acted, try to counter the top opposing spell; otherwise pass.
fn nap_action(
    state: &SimState,
    who: &str,
    last_action: &PriorityAction,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> PriorityAction {
    let other_acted = matches!(last_action, PriorityAction::CastSpell { .. } | PriorityAction::ActivateAbility(..) | PriorityAction::CastAdventure { .. } | PriorityAction::CastFromAdventure { .. });
    if other_acted {
        for idx in (0..state.stack.len()).rev() {
            let item_id = state.stack[idx];
            let item_owner = state.stack_item_owner(item_id);
            let item_is_counterable = state.stack_item_is_counterable(item_id);
            let item_name = state.stack_item_display_name(item_id).to_string();
            if item_owner != state.player_id(who) && item_is_counterable {
                if !worth_countering(&item_name, catalog_map) {
                    eprintln!("[decision] {}: NAP ignores {} (not worth countering)", who, item_name);
                    break;
                }
                if let Some(action) = respond_with_counter(state, idx, who, catalog_map, rng, true) {
                    if let PriorityAction::CastSpell { ref name, .. } = action {
                        eprintln!("[decision] {}: NAP counter {} targeting {}", who, name, item_name);
                    }
                    return action;
                }
                eprintln!("[decision] {}: NAP passes (no counter available for {})", who, item_name);
                break;
            }
        }
    }
    PriorityAction::Pass
}

/// AP reactive decision: respond to threats already on the stack.
/// Currently handles protecting our Doomsday if the opponent has countered it.
/// Returns Some(action) if we should respond, None to continue to proactive logic.
fn ap_react(
    state: &mut SimState,
    t: u8,
    who: &str,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> Option<PriorityAction> {
    if who != "us" || state.stack.is_empty() {
        return None;
    }
    let top_idx = state.stack.len() - 1;
    let top_id = state.stack[top_idx];
    let top_is_counterable = state.stack_item_is_counterable(top_id);
    let top_owner = state.stack_item_owner(top_id);
    let top_chosen = state.cards.get(&top_id)
        .and_then(|c| c.spell.as_ref())
        .map(|s| s.chosen_targets.clone())
        .unwrap_or_default();
    let us_id = state.us.id;
    let dd_countered = top_is_counterable
        && top_owner != us_id
        && top_chosen.first()
            .and_then(|t| if let Target::Object(id) = t { Some(id) } else { None })
            .and_then(|id| state.stack.iter().find(|&&s| s == *id).map(|_| *id))
            .is_some_and(|id| {
                state.cards.get(&id)
                    .map(|c| c.name == "Doomsday" && state.player_id(&c.owner) == us_id)
                    .unwrap_or(false)
            });
    if !dd_countered {
        return None;
    }
    Some(
        if let Some(action) = respond_with_counter(state, top_idx, "us", catalog_map, rng, false) {
            action
        } else {
            state.log(t, "us", "⚠ Doomsday countered — could not protect");
            state.reroll = true;
            PriorityAction::Pass
        },
    )
}

/// AP proactive decision: land drop, abilities, Doomsday setup, and general spells.
/// Only called when the AP is in the main phase.
fn ap_proactive(
    state: &mut SimState,
    t: u8,
    who: &str,
    dd_turn: u8,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> PriorityAction {
    // Land drop (sorcery speed: requires empty stack).
    if state.stack.is_empty() && state.player(who).land_drop_available {
        let fateful = who == "us" && t == dd_turn;
        // On the fateful turn, skip the land drop if Doomsday is already castable — playing
        // a land might spend our last card in hand and leave us unable to cast Doomsday.
        let dd_already_castable = fateful && !state.us.dd_cast
            && state.potential_mana("us").can_pay(&ManaCost { b: 3, ..Default::default() })
            && state.hand_of("us").any(|c| c.name == "Doomsday");
        if !dd_already_castable {
            let force = state.player(who).must_land_drop;
            let land_count = state.hand_of(who)
                .filter(|c| catalog_map.get(c.name.as_str()).map_or(false, |d| d.is_land()))
                .count();
            if land_count > 0 {
                // T1=100%, T2=90%, T3=80%, T4+=70%; forced to 100% when must_land_drop is set.
                let prob = if force { 1.0 } else { match t { 1 => 1.0, 2 => 0.9, 3 => 0.80, _ => 0.70 } };
                if rng.gen::<f64>() < prob {
                    if let Some(name) = choose_land_name(state, who, catalog_map, fateful, rng) {
                        state.player_mut(who).must_land_drop = false;
                        return PriorityAction::LandDrop(name);
                    }
                }
            }
            state.player_mut(who).must_land_drop = false;
            state.player_mut(who).land_drop_available = false;
        }
    }

    // On-board actions: pop the first pending action (pre-rolled at phase start).
    if let Some(action) = state.player(who).pending_actions.first().cloned() {
        // Verify it's still valid before committing (source might have been tapped/sacrificed).
        let still_valid = match &action {
            PriorityAction::ActivateAbility(source_id, ab) => {
                if let Some(cost) = ab.loyalty_cost {
                    if !state.stack.is_empty() { false }
                    else {
                        let loyalty_ok = if cost < 0 {
                            state.permanent_bf(*source_id)
                                .map_or(false, |bf| bf.loyalty >= -cost)
                        } else {
                            state.permanent_bf(*source_id).is_some()
                        };
                        let already_used = state.permanent_bf(*source_id)
                            .map_or(true, |bf| bf.pw_activated_this_turn);
                        loyalty_ok && !already_used
                    }
                } else {
                    let source_untapped = state.permanent_bf(*source_id)
                        .map_or(false, |bf| !bf.tapped || ab.sacrifice_self);
                    // Also allow hand-zone abilities (ninjutsu/cycling) — check via permanent_controller
                    // (for hand-zone sources, the permanent isn't in play, so we check differently)
                    let is_hand_ability = ab.zone == "hand";
                    is_hand_ability || ability_available(ab, state, who, source_untapped, catalog_map)
                }
            }
            PriorityAction::CastFromAdventure { card_name } => {
                state.on_adventure_of(who).any(|c| c.name == *card_name)
                    && catalog_map.get(card_name.as_str())
                        .map(|def| state.potential_mana(who).can_pay(&parse_mana_cost(def.mana_cost())))
                        .unwrap_or(false)
            }
            _ => false,
        };
        state.player_mut(who).pending_actions.remove(0);
        if still_valid {
            return action;
        }
        // Fall through to hand actions.
    }

    // Hand actions: only on empty stack.
    if !state.stack.is_empty() {
        return PriorityAction::Pass;
    }

    let actions = collect_hand_actions(state, who, catalog_map);
    if actions.is_empty() {
        let pool = &state.player(who).pool;
        let hand = state.hand_size(who);
        eprintln!("[decision] {}: no castable spells (pool B={} U={} tot={}, hand={})",
            who, pool.b, pool.u, pool.total, hand);
        if who == "us" && t == dd_turn && !state.us.dd_cast {
            let dd_in_hand = state.hand_of("us").filter(|c| c.name == "Doomsday").count();
            eprintln!("[decision] fateful turn: Doomsday not cast — hand={}, dd_in_hand={}, potential B={} tot={}",
                hand, dd_in_hand, pool.b, pool.total);
        }
        return PriorityAction::Pass;
    }

    // Fateful turn prioritization: Doomsday > Dark Ritual > anything else.
    let fateful = who == "us" && t == dd_turn && !state.us.dd_cast;
    let action = if fateful && actions.iter().any(|a| matches!(a, PriorityAction::CastSpell { name, .. } if name == "Doomsday")) {
        PriorityAction::CastSpell { name: "Doomsday".to_string(), preferred_cost: None }
    } else if fateful && actions.iter().any(|a| matches!(a, PriorityAction::CastSpell { name, .. } if name == "Dark Ritual")) {
        PriorityAction::CastSpell { name: "Dark Ritual".to_string(), preferred_cost: None }
    } else {
        // General casting — decaying probability for multi-spell turns.
        // 1st spell: always; 2nd: 30%; 3rd+: 10%.
        // Override to 1.0 if mana is floating in the pool: we generated it on purpose.
        let has_floating = state.player(who).pool.total > 0;
        let cast_prob = if has_floating { 1.0 } else {
            match state.player(who).spells_cast_this_turn { 0 => 1.0, 1 => 0.30, _ => 0.10 }
        };
        if rng.gen::<f64>() >= cast_prob {
            return PriorityAction::Pass;
        }
        actions[rng.gen_range(0..actions.len())].clone()
    };

    if let PriorityAction::CastSpell { ref name, .. } = action {
        eprintln!("[decision] {}: proactive cast {} (options: {})", who, name,
            actions.iter().filter_map(|a| if let PriorityAction::CastSpell { name, .. } = a { Some(name.as_str()) } else { None }).collect::<Vec<_>>().join(", "));
    }
    action
}

/// Decide what action the player `who` takes when they hold priority.
/// `ap` is the active player (whose turn it is). Phase context is read from
/// `state.current_phase` (set by `do_turn`/`do_step`/`do_phase` before each priority window).
pub(super) fn decide_action(
    state: &mut SimState,
    t: u8,
    ap: &str,
    who: &str,
    dd_turn: u8,
    last_action: &PriorityAction,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> PriorityAction {
    if who != ap {
        if state.stack.is_empty() { return PriorityAction::Pass; }
        return nap_action(state, who, last_action, catalog_map, rng);
    }
    // Ninjutsu: AP can activate during DeclareBlockers / CombatDamage / EndCombat.
    let in_ninjutsu_step = matches!(state.current_phase,
        Some(TurnPosition::Step(StepKind::DeclareBlockers))
        | Some(TurnPosition::Step(StepKind::CombatDamage))
        | Some(TurnPosition::Step(StepKind::EndCombat)));
    if in_ninjutsu_step {
        if let Some(action) = try_ninjutsu(state, who, catalog_map, rng) {
            return action;
        }
        return PriorityAction::Pass;
    }
    let in_main_phase = matches!(state.current_phase,
        Some(TurnPosition::Phase(PhaseKind::PreCombatMain))
        | Some(TurnPosition::Phase(PhaseKind::PostCombatMain)));
    if !in_main_phase {
        return PriorityAction::Pass;
    }
    if let Some(action) = ap_react(state, t, who, catalog_map, rng) {
        return action;
    }
    ap_proactive(state, t, who, dd_turn, catalog_map, rng)
}

// ── Combat strategy ───────────────────────────────────────────────────────────

/// Decide which creatures attack and what each one targets.
///
/// Returns `(attacker_id, attack_target)` pairs. `attack_target` is `None` to attack the
/// opponent player, or `Some(pw_id)` to attack a planeswalker. Attackers are chosen if their
/// toughness exceeds the total power of NAP creatures that could block them.
pub(super) fn declare_attackers(
    ap: &str,
    state: &SimState,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> Vec<(ObjId, Option<ObjId>)> {
    let nap = opp_of(ap);
    // Compute NAP blocker power per flying/non-flying attacker.
    let nap_blockers: Vec<(String, i32)> = state.permanents_of(nap)
        .filter(|p| !p.bf.as_ref().map_or(false, |bf| bf.tapped))
        .filter_map(|p| {
            let def = catalog_map.get(p.name.as_str())?;
            if !def.is_creature() { return None; }
            let bf = p.bf.as_ref().unwrap();
            Some((p.name.clone(), creature_stats(bf, Some(def)).0))
        })
        .collect();
    let nap_pw_ids: Vec<ObjId> = state.permanents_of(nap)
        .filter(|p| catalog_map.get(p.name.as_str())
            .map_or(false, |d| matches!(d.kind, CardKind::Planeswalker(_))))
        .map(|p| p.id)
        .collect();
    state.permanents_of(ap)
        .filter(|p| p.bf.as_ref().map_or(false, |bf| !bf.tapped && !bf.entered_this_turn))
        .filter_map(|p| {
            let def = catalog_map.get(p.name.as_str())?;
            if !def.is_creature() { return None; }
            let atk_flies = def.has_keyword("flying");
            // Sum power of NAP creatures that can block this attacker.
            let blocking_power: i32 = nap_blockers.iter()
                .filter(|(blk_name, _)| !atk_flies || creature_has_keyword(blk_name, "flying", catalog_map))
                .map(|(_, pow)| *pow)
                .sum();
            let (_, tgh) = creature_stats(p.bf.as_ref().unwrap(), Some(def));
            if tgh <= blocking_power { return None; }
            // Randomly attack a NAP planeswalker (50%) or the player.
            let target = if !nap_pw_ids.is_empty() && rng.gen_bool(0.5) {
                Some(nap_pw_ids[rng.gen_range(0..nap_pw_ids.len())])
            } else {
                None
            };
            Some((p.id, target))
        })
        .collect()
}

/// Decide which NAP creatures block which attackers.
///
/// Returns `(attacker_id, blocker_id)` pairs. A creature only blocks if it's a "good block":
/// it kills the attacker, or both creatures survive (no chump blocks).
pub(super) fn declare_blockers(
    ap: &str,
    state: &SimState,
    catalog_map: &HashMap<&str, &CardDef>,
) -> Vec<(ObjId, ObjId)> {
    let nap = opp_of(ap);
    let mut used_blockers: std::collections::HashSet<ObjId> = Default::default();
    let mut blocks: Vec<(ObjId, ObjId)> = Vec::new();
    for &atk_id in &state.combat_attackers {
        let (atk_name, atk_pow, atk_tgh) = match state.cards.get(&atk_id)
            .and_then(|p| p.bf.as_ref().map(|bf| (p.name.clone(), bf)))
        {
            Some((name, bf)) => {
                let def = catalog_map.get(name.as_str()).copied();
                let (pow, tgh) = creature_stats(bf, def);
                (name, pow, tgh)
            }
            None => continue,
        };
        let atk_flies = creature_has_keyword(&atk_name, "flying", catalog_map);
        let blocker = state.permanents_of(nap)
            .filter(|p| !p.bf.as_ref().map_or(false, |bf| bf.tapped) && !used_blockers.contains(&p.id))
            .find_map(|p| {
                let def = catalog_map.get(p.name.as_str()).copied();
                if !def.map(|d| d.is_creature()).unwrap_or(false) { return None; }
                // Flying attackers can only be blocked by flying creatures.
                if atk_flies && !creature_has_keyword(&p.name, "flying", catalog_map) { return None; }
                let (blk_pow, blk_tgh) = creature_stats(p.bf.as_ref().unwrap(), def);
                // Good block: kills attacker OR both survive. Not a chump.
                if blk_pow >= atk_tgh || atk_pow < blk_tgh { Some(p.id) } else { None }
            });
        if let Some(blk_id) = blocker {
            used_blockers.insert(blk_id);
            blocks.push((atk_id, blk_id));
        }
    }
    blocks
}

/// Try to perform ninjutsu during a combat priority window (DeclareBlockers / CombatDamage / EndCombat).
///
/// Requires: unblocked attacker, ninjutsu card in hand, and enough mana.
/// Returns an `ActivateAbility` action or `None` if conditions aren't met.
pub(super) fn try_ninjutsu(
    state: &SimState,
    who: &str,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> Option<PriorityAction> {
    if state.hand_size(who) <= 0 { return None; }
    let has_unblocked = state.permanents_of(who)
        .any(|c| c.bf.as_ref().map_or(false, |bf| bf.attacking && bf.unblocked));
    if !has_unblocked { return None; }
    let ninjas: Vec<(ObjId, String)> = state.hand_of(who)
        .filter(|c| catalog_map.get(c.name.as_str()).map_or(false, |d| d.ninjutsu().is_some()))
        .map(|c| (c.id, c.name.clone()))
        .collect();
    if ninjas.is_empty() { return None; }
    // 35% roll: simulates probability of wanting to use it.
    if !rng.gen_bool(0.35) { return None; }
    let idx = rng.gen_range(0..ninjas.len());
    let (ninja_id, ninja_name) = &ninjas[idx];
    let ninja_def = catalog_map.get(ninja_name.as_str())?;
    let ninjutsu_cost = parse_mana_cost(&ninja_def.ninjutsu()?.mana_cost);
    if !state.potential_mana(who).can_pay(&ninjutsu_cost) { return None; }
    Some(PriorityAction::ActivateAbility(*ninja_id, ninja_def.ninjutsu()?.as_ability_def()))
}
