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

    // 75% roll per land with an available ability.
    let land_ids: Vec<(ObjId, String)> = state.player(ap).lands.iter()
        .filter(|l| !l.tapped)
        .filter(|l| catalog_map.get(l.name.as_str())
            .map_or(false, |def| def.abilities().iter()
                .any(|ab| ability_available(ab, state, ap, true, catalog_map))))
        .map(|l| (l.id, l.name.clone()))
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
    let perm_ids: Vec<(ObjId, String, bool)> = state.player(ap).permanents.iter()
        .filter(|p| catalog_map.get(p.name.as_str())
            .map_or(false, |def| def.abilities().iter()
                .any(|ab| ab.loyalty_cost.is_none() && ability_available(ab, state, ap, !p.tapped, catalog_map))))
        .map(|p| (p.id, p.name.clone(), p.tapped))
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
    let on_adventure_names: Vec<String> = state.player(ap).on_adventure.clone();
    for card_name in on_adventure_names {
        if let Some(&def) = catalog_map.get(card_name.as_str()) {
            let cost = parse_mana_cost(def.mana_cost());
            if !state.player(ap).potential_mana().can_pay(&cost) { continue; }
            if rng.gen_bool(0.75) {
                actions.push(PriorityAction::CastFromAdventure { card_name });
            }
        }
    }

    // Fateful turn override: force-include fetch lands that can search for a black source,
    // if we have no black mana. (These bypass the 75% roll.)
    if ap == "us" && t == dd_turn && !state.us.has_black_mana() {
        let can_search_black = |name: &str| catalog_map.get(name).map_or(false, |def|
            def.abilities().iter().any(|ab|
                ab.effect.starts_with("search:land-swamp")
                    || ab.effect.starts_with("search:land-island|swamp")
            )
        );
        let fetch_ids: Vec<(ObjId, String)> = state.us.lands.iter()
            .filter(|l| !l.tapped && can_search_black(&l.name))
            .map(|l| (l.id, l.name.clone()))
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
    us_lib: &mut Vec<(ObjId, String, CardDef)>,
    opp_lib: &mut Vec<(ObjId, String, CardDef)>,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> PriorityAction {
    let other_acted = matches!(last_action, PriorityAction::CastSpell { .. } | PriorityAction::ActivateAbility(..) | PriorityAction::CastAdventure { .. } | PriorityAction::CastFromAdventure { .. });
    if other_acted {
        let actor_lib: &[_] = if who == "us" { us_lib } else { opp_lib };
        for idx in (0..state.stack.len()).rev() {
            let item_owner = state.stack[idx].owner;
            let item_is_ability = state.stack[idx].is_ability;
            let item_name = state.stack[idx].name.clone();
            if item_owner != state.player_id(who) && !item_is_ability {
                if !worth_countering(&item_name, catalog_map) {
                    eprintln!("[decision] {}: NAP ignores {} (not worth countering)", who, item_name);
                    break;
                }
                if let Some(action) = respond_with_counter(state, idx, who, actor_lib, catalog_map, rng, true) {
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
    us_lib: &[(ObjId, String, CardDef)],
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> Option<PriorityAction> {
    if who != "us" || state.stack.is_empty() {
        return None;
    }
    let top_idx = state.stack.len() - 1;
    let top_is_ability = state.stack[top_idx].is_ability;
    let top_owner = state.stack[top_idx].owner;
    let top_chosen = state.stack[top_idx].chosen_targets.clone();
    let us_id = state.us.id;
    let dd_countered = !top_is_ability
        && top_owner != us_id
        && top_chosen.first()
            .and_then(|t| if let Target::Object(id) = t { Some(id) } else { None })
            .and_then(|id| state.stack.iter().find(|s| s.id == *id))
            .is_some_and(|s| s.name == "Doomsday" && s.owner == us_id);
    if !dd_countered {
        return None;
    }
    Some(
        if let Some(action) = respond_with_counter(state, top_idx, "us", us_lib, catalog_map, rng, false) {
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
    us_lib: &mut Vec<(ObjId, String, CardDef)>,
    opp_lib: &mut Vec<(ObjId, String, CardDef)>,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> PriorityAction {
    // Land drop (sorcery speed: requires empty stack).
    if state.stack.is_empty() && state.player(who).land_drop_available {
        let fateful = who == "us" && t == dd_turn;
        // On the fateful turn, skip the land drop if Doomsday is already castable — playing
        // a land might spend our last card in hand and leave us unable to cast Doomsday.
        let dd_already_castable = fateful && !state.us.dd_cast
            && state.us.potential_mana().can_pay(&ManaCost { b: 3, ..Default::default() })
            && us_lib.iter().any(|(_, n, _)| n == "Doomsday");
        if !dd_already_castable {
            let force = state.player(who).must_land_drop;
            let lib: &[(ObjId, String, CardDef)] = if who == "us" { us_lib } else { opp_lib };
            let land_count = lib.iter().filter(|(_, _, d)| d.is_land()).count();
            if land_count > 0 {
                // T1=100%, T2=90%, T3=80%, T4+=70%; forced to 100% when must_land_drop is set.
                let prob = if force { 1.0 } else { match t { 1 => 1.0, 2 => 0.9, 3 => 0.80, _ => 0.70 } };
                if rng.gen::<f64>() < prob {
                    if let Some(name) = choose_land_name(state, who, lib, fateful, rng) {
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
                            state.player(who).permanents.iter()
                                .find(|p| p.id == *source_id)
                                .map_or(false, |p| p.loyalty >= -cost)
                        } else {
                            state.player(who).permanents.iter().any(|p| p.id == *source_id)
                        };
                        let already_used = state.player(who).permanents.iter()
                            .find(|p| p.id == *source_id)
                            .map_or(true, |p| p.pw_activated_this_turn);
                        loyalty_ok && !already_used
                    }
                } else {
                    let source_untapped = state.player(who).lands.iter().any(|l| l.id == *source_id && !l.tapped)
                        || state.player(who).permanents.iter().any(|p| p.id == *source_id && (!p.tapped || ab.sacrifice_self));
                    // Also allow hand-zone abilities (ninjutsu/cycling) — check via permanent_controller
                    // (for hand-zone sources, the permanent isn't in play, so we check differently)
                    let is_hand_ability = ab.zone == "hand";
                    is_hand_ability || ability_available(ab, state, who, source_untapped, catalog_map)
                }
            }
            PriorityAction::CastFromAdventure { card_name } => {
                state.player(who).on_adventure.iter().any(|n| n == card_name)
                    && catalog_map.get(card_name.as_str())
                        .map(|def| state.player(who).potential_mana().can_pay(&parse_mana_cost(def.mana_cost())))
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

    let actor_lib: &[(ObjId, String, CardDef)] = if who == "us" { us_lib } else { opp_lib };
    let actions = collect_hand_actions(state, who, actor_lib, catalog_map);
    if actions.is_empty() {
        let pool = &state.player(who).pool;
        let hand = state.player(who).hand.hidden;
        eprintln!("[decision] {}: no castable spells (pool B={} U={} tot={}, hand={})",
            who, pool.b, pool.u, pool.total, hand);
        if who == "us" && t == dd_turn && !state.us.dd_cast {
            let dd_in_lib = actor_lib.iter().filter(|(_, n, _)| n == "Doomsday").count();
            eprintln!("[decision] fateful turn: Doomsday not cast — hand={}, dd_in_lib={}, potential B={} tot={}",
                hand, dd_in_lib, pool.b, pool.total);
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
    us_lib: &mut Vec<(ObjId, String, CardDef)>,
    opp_lib: &mut Vec<(ObjId, String, CardDef)>,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> PriorityAction {
    if who != ap {
        if state.stack.is_empty() { return PriorityAction::Pass; }
        return nap_action(state, who, last_action, us_lib, opp_lib, catalog_map, rng);
    }
    // Ninjutsu: AP can activate during DeclareBlockers / CombatDamage / EndCombat.
    let in_ninjutsu_step = matches!(state.current_phase.as_str(),
        "DeclareBlockers" | "CombatDamage" | "EndCombat");
    if in_ninjutsu_step {
        let actor_lib: &[(ObjId, String, CardDef)] = if who == "us" { us_lib } else { opp_lib };
        if let Some(action) = try_ninjutsu(state, who, actor_lib, rng) {
            return action;
        }
        return PriorityAction::Pass;
    }
    if state.current_phase != "Main" {
        return PriorityAction::Pass;
    }
    if let Some(action) = ap_react(state, t, who, us_lib, catalog_map, rng) {
        return action;
    }
    ap_proactive(state, t, who, dd_turn, us_lib, opp_lib, catalog_map, rng)
}
