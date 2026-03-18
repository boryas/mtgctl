use rand::Rng;
use super::*;


fn pick_on_board_action(
    state: &mut SimState,
    ap: PlayerId,
    t: u8,
    dd_turn: u8,
    rng: &mut impl Rng,
) -> Option<PriorityAction> {
    let mut candidates: Vec<PriorityAction> = Vec::new();

    // 75% roll per land (permanent that is_land) with an available ability.
    let land_ids: Vec<ObjId> = state.permanents_of(ap)
        .filter(|p| !p.bf.as_ref().map_or(false, |bf| bf.tapped))
        .filter(|p| state.def_of(p.id)
            .map_or(false, |def| def.is_land() && def.abilities().iter()
                .any(|ab| ability_available(ab, state, ap, true))))
        .map(|p| p.id)
        .collect();
    for land_id in land_ids {
        if let Some(def) = state.def_of(land_id) {
            if let Some(ab) = def.abilities().iter()
                .find(|ab| ability_available(ab, state, ap, true))
                .cloned()
            {
                if rng.gen_bool(0.75) {
                    let targets = legal_targets(&ab.target_spec, ap, state);
                    let chosen = pick_target(&targets, state).into_iter().collect();
                    candidates.push(PriorityAction::ActivateAbility(land_id, ab, chosen));
                }
            }
        }
    }

    // 75% roll per non-land permanent with an available non-loyalty ability.
    // Lands are already handled above; including them here would double-queue fetch abilities.
    let perm_ids: Vec<(ObjId, bool)> = state.permanents_of(ap)
        .filter(|p| {
            let tapped = p.bf.as_ref().map_or(false, |bf| bf.tapped);
            state.def_of(p.id)
                .map_or(false, |def| !def.is_land() && def.abilities().iter()
                    .any(|ab| ab.loyalty_cost.is_none() && ability_available(ab, state, ap, !tapped)))
        })
        .map(|p| (p.id, p.bf.as_ref().map_or(false, |bf| bf.tapped)))
        .collect();
    for (perm_id, tapped) in perm_ids {
        if let Some(def) = state.def_of(perm_id) {
            if let Some(ab) = def.abilities().iter()
                .find(|ab| ab.loyalty_cost.is_none() && ability_available(ab, state, ap, !tapped))
                .cloned()
            {
                if rng.gen_bool(0.75) {
                    let targets = legal_targets(&ab.target_spec, ap, state);
                    let chosen = pick_target(&targets, state).into_iter().collect();
                    candidates.push(PriorityAction::ActivateAbility(perm_id, ab, chosen));
                }
            }
        }
    }

    // Postcombat main phase: activate any un-activated planeswalkers (sorcery speed, empty stack).
    let is_postcombat_main = matches!(state.current_phase, Some(TurnPosition::Phase(PhaseKind::PostCombatMain)));
    if is_postcombat_main && state.stack.is_empty() {
        let pw_data: Vec<(ObjId, i32)> = state.permanents_of(ap)
            .filter(|p| {
                let bf = p.bf.as_ref();
                !bf.map_or(true, |bf| bf.pw_activated_this_turn)
                    && state.def_of(p.id)
                        .map_or(false, |d| matches!(d.kind, CardKind::Planeswalker(_)))
            })
            .map(|p| (p.id, p.bf.as_ref().map_or(0, |bf| bf.loyalty)))
            .collect();
        for (pw_id, loyalty) in pw_data {
            if let Some(def) = state.def_of(pw_id) {
                if let Some(ab) = def.abilities().iter()
                    .filter(|ab| {
                        let Some(cost) = ab.loyalty_cost else { return false; };
                        !(cost < 0 && loyalty < -cost)
                    })
                    .next()
                    .cloned()
                {
                    let targets = legal_targets(&ab.target_spec, ap, state);
                    let chosen = pick_target(&targets, state).into_iter().collect();
                    candidates.push(PriorityAction::ActivateAbility(pw_id, ab, chosen));
                }
            }
        }
    }

    // Fateful turn override: force-include fetch lands that can search for a black source,
    // if we have no black mana. (These bypass the 75% roll.)
    if ap == PlayerId::Us && t == dd_turn && !state.has_black_mana(PlayerId::Us) {
        let fetch_ids: Vec<ObjId> = state.permanents_of(PlayerId::Us)
            .filter(|p| !p.bf.as_ref().map_or(false, |bf| bf.tapped))
            .filter(|p| state.def_of(p.id).map_or(false, |def|
                def.abilities().iter().any(|ab| ab.sacrifice_self && ab.life_cost > 0)
            ))
            .map(|p| p.id)
            .collect();
        if !fetch_ids.is_empty() {
            for &fid in &fetch_ids {
                // Force-include if not already a candidate (bypasses the 75% roll).
                if !candidates.iter().any(|a| matches!(a, PriorityAction::ActivateAbility(id, _, _) if *id == fid)) {
                    if let Some(def) = state.def_of(fid) {
                        if let Some(ab) = def.abilities().iter()
                            .find(|ab| ability_available(ab, state, PlayerId::Us, true))
                            .cloned()
                        {
                            let targets = legal_targets(&ab.target_spec, PlayerId::Us, state);
                            let chosen = pick_target(&targets, state).into_iter().collect();
                            candidates.push(PriorityAction::ActivateAbility(fid, ab, chosen));
                        }
                    }
                }
            }
        } else {
            // No fetch available — ensure the land drop fires.
            state.us.must_land_drop = true;
        }
    }

    // Adventure creatures in exile: 75% roll to cast the creature face.
    let on_adventure: Vec<ObjId> = state.on_adventure_of(ap).map(|c| c.id).collect();
    for card_id in on_adventure {
        if let Some(def) = state.def_of(card_id) {
            let cost = parse_mana_cost(def.mana_cost());
            if !state.potential_mana(ap).can_pay(&cost) { continue; }
            if rng.gen_bool(0.75) {
                let targets = legal_targets(def.target_spec(), ap, state);
                let chosen = pick_target(&targets, state).into_iter().collect();
                candidates.push(PriorityAction::CastSpell { card_id, face: SpellFace::Main, preferred_cost: None, chosen_targets: chosen });
            }
        }
    }

    if candidates.is_empty() { None } else { Some(candidates.remove(rng.gen_range(0..candidates.len()))) }
}

/// True if `name` is a spell the NAP considers worth spending a free counterspell (FoW / Daze) on.
/// Cantrips and mana rituals are not worth pitching; permanents and combo pieces are.
fn worth_countering(id: ObjId, name: &str, state: &SimState) -> bool {
    if let Some(def) = state.def_of(id) {
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
    who: PlayerId,
    last_action: &PriorityAction,
    rng: &mut impl Rng,
) -> PriorityAction {
    let other_acted = matches!(last_action, PriorityAction::CastSpell { .. } | PriorityAction::ActivateAbility(..));
    if other_acted {
        for idx in (0..state.stack.len()).rev() {
            let item_id = state.stack[idx];
            let item_owner = state.stack_item_owner(item_id);
            let item_is_counterable = state.stack_item_is_counterable(item_id);
            let item_name = state.stack_item_display_name(item_id).to_string();
            if item_owner != state.player_id(who) && item_is_counterable {
                if !worth_countering(item_id, &item_name, state) {
                    eprintln!("[decision] {}: NAP ignores {} (not worth countering)", who, item_name);
                    break;
                }
                if let Some(action) = respond_with_counter(state, idx, who, rng, true) {
                    if let PriorityAction::CastSpell { card_id, .. } = action {
                        let spell_name = state.objects.get(&card_id).map_or("?", |c| c.catalog_key.as_str());
                        eprintln!("[decision] {}: NAP counter {} targeting {}", who, spell_name, item_name);
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
    who: PlayerId,
    rng: &mut impl Rng,
) -> Option<PriorityAction> {
    if who != PlayerId::Us || state.stack.is_empty() {
        return None;
    }
    let top_idx = state.stack.len() - 1;
    let top_id = state.stack[top_idx];
    let top_is_counterable = state.stack_item_is_counterable(top_id);
    let top_owner = state.stack_item_owner(top_id);
    let top_chosen = state.objects.get(&top_id)
        .and_then(|c| c.spell.as_ref())
        .map(|s| s.chosen_targets.clone())
        .unwrap_or_default();
    let us_id = state.us.id;
    let dd_countered = top_is_counterable
        && top_owner != us_id
        && top_chosen.first()
            .copied()
            .and_then(|id| state.stack.iter().find(|&&s| s == id).map(|_| id))
            .is_some_and(|id| {
                state.objects.get(&id)
                    .map(|c| c.catalog_key == "Doomsday" && state.player_id(c.owner) == us_id)
                    .unwrap_or(false)
            });
    if !dd_countered {
        return None;
    }
    Some(
        if let Some(action) = respond_with_counter(state, top_idx, PlayerId::Us, rng, false) {
            action
        } else {
            state.log(t, PlayerId::Us, "⚠ Doomsday countered — could not protect");
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
    who: PlayerId,
    dd_turn: u8,
    rng: &mut impl Rng,
) -> PriorityAction {
    // Land drop (sorcery speed: requires empty stack).
    if state.stack.is_empty() && state.player(who).land_drop_available {
        let fateful = who == PlayerId::Us && t == dd_turn;
        // On the fateful turn, skip the land drop if Doomsday is already castable — playing
        // a land might spend our last card in hand and leave us unable to cast Doomsday.
        let dd_already_castable = fateful && !state.us.dd_cast
            && state.potential_mana(PlayerId::Us).can_pay(&ManaCost { b: 3, ..Default::default() })
            && state.hand_of(PlayerId::Us).any(|c| c.catalog_key == "Doomsday");
        if !dd_already_castable {
            let force = state.player(who).must_land_drop;
            let land_count = state.hand_of(who)
                .filter(|c| state.def_of(c.id).map_or(false, |d| d.is_land()))
                .count();
            if land_count > 0 {
                // T1=100%, T2=90%, T3=80%, T4+=70%; forced to 100% when must_land_drop is set.
                let prob = if force { 1.0 } else { match t { 1 => 1.0, 2 => 0.9, 3 => 0.80, _ => 0.70 } };
                if rng.gen::<f64>() < prob {
                    if let Some(id) = choose_land(state, who, fateful, rng) {
                        state.player_mut(who).must_land_drop = false;
                        return PriorityAction::LandDrop(id);
                    }
                }
            }
            state.player_mut(who).must_land_drop = false;
            state.player_mut(who).land_drop_available = false;
        }
    }

    // On-board actions: computed fresh from current state.
    if let Some(action) = pick_on_board_action(state, who, t, dd_turn, rng) {
        return action;
    }

    // Hand actions: only on empty stack.
    if !state.stack.is_empty() {
        return PriorityAction::Pass;
    }

    let actions = collect_hand_actions(state, who);
    if actions.is_empty() {
        let pool = &state.player(who).pool;
        let hand = state.hand_size(who);
        eprintln!("[decision] {}: no castable spells (pool B={} U={} tot={}, hand={})",
            who, pool.b, pool.u, pool.total, hand);
        if who == PlayerId::Us && t == dd_turn && !state.us.dd_cast {
            let dd_in_hand = state.hand_of(PlayerId::Us).filter(|c| c.catalog_key == "Doomsday").count();
            eprintln!("[decision] fateful turn: Doomsday not cast — hand={}, dd_in_hand={}, potential B={} tot={}",
                hand, dd_in_hand, pool.b, pool.total);
        }
        return PriorityAction::Pass;
    }

    let spell_name = |a: &PriorityAction| -> Option<String> {
        if let PriorityAction::CastSpell { card_id, .. } = a {
            state.objects.get(card_id).map(|c| c.catalog_key.clone())
        } else { None }
    };

    // Fateful turn prioritization: Doomsday > Dark Ritual > anything else.
    let fateful = who == PlayerId::Us && t == dd_turn && !state.us.dd_cast;
    let action = if fateful {
        let priority = ["Doomsday", "Dark Ritual"];
        priority.iter()
            .find_map(|&p| actions.iter().find(|a| spell_name(a).as_deref() == Some(p)))
            .or_else(|| if actions.is_empty() { None } else { Some(&actions[rng.gen_range(0..actions.len())]) })
            .cloned()
            .unwrap_or(PriorityAction::Pass)
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

    if let PriorityAction::CastSpell { card_id, .. } = &action {
        let name = state.objects.get(card_id).map_or("?".to_string(), |c| c.catalog_key.clone());
        let options = actions.iter().filter_map(|a| spell_name(a)).collect::<Vec<_>>().join(", ");
        eprintln!("[decision] {}: proactive cast {} (options: {})", who, name, options);
    }
    action
}

/// Decide what action the player `who` takes when they hold priority.
/// `ap` is the active player (whose turn it is). Phase context is read from
/// `state.current_phase` (set by `do_turn`/`do_step`/`do_phase` before each priority window).
pub(super) fn decide_action(
    state: &mut SimState,
    t: u8,
    ap: PlayerId,
    who: PlayerId,
    dd_turn: u8,
    last_action: &PriorityAction,
    rng: &mut impl Rng,
) -> PriorityAction {
    if who != ap {
        if state.stack.is_empty() { return PriorityAction::Pass; }
        return nap_action(state, who, last_action, rng);
    }
    // Ninjutsu: AP can activate during DeclareBlockers / CombatDamage / EndCombat.
    let in_ninjutsu_step = matches!(state.current_phase,
        Some(TurnPosition::Step(StepKind::DeclareBlockers))
        | Some(TurnPosition::Step(StepKind::CombatDamage))
        | Some(TurnPosition::Step(StepKind::EndCombat)));
    if in_ninjutsu_step {
        if let Some(action) = try_ninjutsu(state, who, rng) {
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
    if let Some(action) = ap_react(state, t, who, rng) {
        return action;
    }
    ap_proactive(state, t, who, dd_turn, rng)
}

// ── Combat strategy ───────────────────────────────────────────────────────────

/// Decide which creatures attack and what each one targets.
///
/// Returns `(attacker_id, attack_target)` pairs. `attack_target` is `None` to attack the
/// opponent player, or `Some(pw_id)` to attack a planeswalker. Attackers are chosen if their
/// toughness exceeds the total power of NAP creatures that could block them.
pub(super) fn declare_attackers(
    ap: PlayerId,
    state: &SimState,
    rng: &mut impl Rng,
) -> Vec<(ObjId, Option<ObjId>)> {
    let nap = ap.opp();
    // Compute NAP blocker stats (ObjId, power) for flying/non-flying checks.
    let nap_blockers: Vec<(ObjId, i32)> = state.permanents_of(nap)
        .filter(|p| !p.bf.as_ref().map_or(false, |bf| bf.tapped))
        .filter_map(|p| {
            let def = state.def_of(p.id)?;
            if !def.is_creature() { return None; }
            let pow = state.def_of(p.id)
                .and_then(|d| d.as_creature())
                .map(|c| c.power())
                .unwrap_or(1);
            Some((p.id, pow))
        })
        .collect();
    let nap_pw_ids: Vec<ObjId> = state.permanents_of(nap)
        .filter(|p| state.def_of(p.id)
            .map_or(false, |d| matches!(d.kind, CardKind::Planeswalker(_))))
        .map(|p| p.id)
        .collect();
    state.permanents_of(ap)
        .filter(|p| p.bf.as_ref().map_or(false, |bf| !bf.tapped && !bf.entered_this_turn))
        .filter_map(|p| {
            let def = state.def_of(p.id)?;
            if !def.is_creature() { return None; }
            let atk_flies = creature_has_keyword(p.id, "flying", state);
            // Sum power of NAP creatures that can block this attacker.
            let blocking_power: i32 = nap_blockers.iter()
                .filter(|(blk_id, _)| !atk_flies || creature_has_keyword(*blk_id, "flying", state))
                .map(|(_, pow)| *pow)
                .sum();
            let tgh = state.def_of(p.id)
                .and_then(|d| d.as_creature())
                .map(|c| c.toughness())
                .unwrap_or(1);
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
    ap: PlayerId,
    state: &SimState,
) -> Vec<(ObjId, ObjId)> {
    let nap = ap.opp();
    let mut used_blockers: std::collections::HashSet<ObjId> = Default::default();
    let mut blocks: Vec<(ObjId, ObjId)> = Vec::new();
    for &atk_id in &state.combat_attackers {
        let (atk_pow, atk_tgh) = match state.objects.get(&atk_id)
            .and_then(|p| p.bf.as_ref().map(|_| ()))
        {
            Some(()) => {
                let pow = state.def_of(atk_id)
                    .and_then(|d| d.as_creature())
                    .map(|c| c.power())
                    .unwrap_or(1);
                let tgh = state.def_of(atk_id)
                    .and_then(|d| d.as_creature())
                    .map(|c| c.toughness())
                    .unwrap_or(1);
                (pow, tgh)
            }
            None => continue,
        };
        let atk_flies = creature_has_keyword(atk_id, "flying", state);
        let blocker = state.permanents_of(nap)
            .filter(|p| !p.bf.as_ref().map_or(false, |bf| bf.tapped) && !used_blockers.contains(&p.id))
            .find_map(|p| {
                if !state.def_of(p.id).map(|d| d.is_creature()).unwrap_or(false) { return None; }
                // Flying attackers can only be blocked by flying creatures.
                if atk_flies && !creature_has_keyword(p.id, "flying", state) { return None; }
                let blk_pow = state.def_of(p.id)
                    .and_then(|d| d.as_creature())
                    .map(|c| c.power())
                    .unwrap_or(1);
                let blk_tgh = state.def_of(p.id)
                    .and_then(|d| d.as_creature())
                    .map(|c| c.toughness())
                    .unwrap_or(1);
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

// ── Hand and board action enumeration ─────────────────────────────────────────

/// Check whether an ability can be activated (cost payable + valid target exists).
/// `source_untapped` must be true when the source is an untapped land/permanent.
fn ability_available(
    ability: &AbilityDef,
    state: &SimState,
    who: PlayerId,
    source_untapped: bool,
) -> bool {
    if ability.tap_self && !source_untapped {
        return false;
    }
    if !ability.mana_cost.is_empty() {
        let cost = parse_mana_cost(&ability.mana_cost);
        if !state.potential_mana(who).can_pay(&cost) {
            return false;
        }
    }
    if !ability.target_spec.is_none() {
        if !has_valid_target(&ability.target_spec, state, who) {
            return false;
        }
    }
    true
}

/// True if the player can currently afford to cast `name` via any available cost.
///
/// Tries the standard mana cost first; falls back to alternate costs (e.g. delve, pitch).
fn spell_is_affordable(
    name: &str,
    def: &CardDef,
    state: &SimState,
    who: PlayerId,
) -> bool {
    let mut cost = parse_mana_cost(def.mana_cost());
    if def.delve() && cost.generic > 0 {
        let gy_len = state.graveyard_of(who).count() as i32;
        cost.generic = (cost.generic - gy_len).max(0);
    }
    let mana_is_usable = !def.mana_cost().is_empty() && state.potential_mana(who).can_pay(&cost);
    if mana_is_usable { return true; }
    def.alternate_costs().iter().any(|c| can_pay_alternate_cost(c, state, who, name))
}

fn hand_ability_affordable(ability: &AbilityDef, state: &SimState, who: PlayerId) -> bool {
    let player = state.player(who);
    if !ability.mana_cost.is_empty() {
        if !state.potential_mana(who).can_pay(&parse_mana_cost(&ability.mana_cost)) { return false; }
    }
    if ability.life_cost > 0 && player.life <= ability.life_cost { return false; }
    if ability.sacrifice_land && !state.permanents_of(who).any(|c| {
        c.bf.is_some() && !state.def_of(c.id).map(|d| d.mana_abilities()).unwrap_or(&[]).is_empty()
    }) { return false; }
    true
}

fn collect_hand_actions(
    state: &SimState,
    who: PlayerId,
) -> Vec<PriorityAction> {
    if state.hand_size(who) <= 0 {
        return Vec::new();
    }
    let hand_cards: Vec<(ObjId, String)> = state.hand_of(who)
        .map(|c| (c.id, c.catalog_key.clone()))
        .collect();

    let mut seen_names: std::collections::HashSet<String> = Default::default();
    let mut actions: Vec<PriorityAction> = Vec::new();

    for (card_id, name) in &hand_cards {
        let Some(def) = state.def_of(*card_id) else { continue; };
        if def.is_land() { continue; }
        if !card_has_implementation(def) { continue; }
        if def.legendary() && state.permanents_of(who).any(|c| c.catalog_key == name.as_str()) { continue; }
        if !def.target_spec().is_none() && !has_valid_target(def.target_spec(), state, who) { continue; }
        if !spell_is_affordable(name, def, state, who) { continue; }
        if seen_names.insert(name.clone()) {
            let targets = legal_targets(def.target_spec(), who, state);
            let chosen = pick_target(&targets, state).into_iter().collect();
            actions.push(PriorityAction::CastSpell { card_id: *card_id, face: SpellFace::Main, preferred_cost: None, chosen_targets: chosen });
        }

        // In-hand abilities (cycling, channel, etc.)
        for ab in def.abilities().iter().filter(|ab| ab.zone == "hand") {
            if hand_ability_affordable(ab, state, who) {
                let targets = legal_targets(&ab.target_spec, who, state);
                let chosen = pick_target(&targets, state).into_iter().collect();
                actions.push(PriorityAction::ActivateAbility(*card_id, ab.clone(), chosen));
            }
        }

        // Adventure spell face.
        if let Some(face) = def.adventure() {
            if !face.mana_cost().is_empty() {
                let cost = parse_mana_cost(face.mana_cost());
                if !state.potential_mana(who).can_pay(&cost) { continue; }
            }
            if !face.target_spec().is_none() && !has_valid_target(face.target_spec(), state, who) { continue; }
            let adv_targets = legal_targets(face.target_spec(), who, state);
            let adv_chosen = pick_target(&adv_targets, state).into_iter().collect();
            actions.push(PriorityAction::CastSpell { card_id: *card_id, face: SpellFace::Back, preferred_cost: None, chosen_targets: adv_chosen });
        }
    }

    actions
}

/// Select which land to play on a given turn. Returns the card's `ObjId`, or `None`.
///
/// On the fateful turn, requires a black-producing land if no black source is in play.
fn choose_land(
    state: &SimState,
    who: PlayerId,
    fateful: bool,
    rng: &mut impl Rng,
) -> Option<ObjId> {
    if state.hand_size(who) <= 0 {
        return None;
    }
    let need_black = fateful && !state.has_black_mana(who);
    let candidates: Vec<ObjId> = state.hand_of(who)
        .filter_map(|c| {
            let def = state.def_of(c.id)?;
            let land = def.as_land()?;
            if need_black && !land.mana_abilities.iter().any(|ma| ma.produces.contains('B')) { return None; }
            Some(c.id)
        })
        .collect();
    if candidates.is_empty() { return None; }
    Some(candidates[rng.gen_range(0..candidates.len())])
}

// ── Counterspell decision ──────────────────────────────────────────────────────

fn p_card_in_hand(library_size: usize, hand_size: i32, copies: usize) -> f64 {
    let t = library_size;
    let h = (hand_size.max(0) as usize).min(t);
    let n = copies;
    if n == 0 || h == 0 { return 0.0; }
    if n >= t { return 1.0; }
    // P(0 in hand) = ∏ᵢ₌₀ʰ⁻¹ (T-N-i)/(T-i)
    let mut p_none: f64 = 1.0;
    for i in 0..h {
        let num = t.saturating_sub(n + i);
        if num == 0 { return 1.0; }
        p_none *= num as f64 / (t - i) as f64;
    }
    (1.0 - p_none).max(0.0)
}

/// Try to respond to `stack[target_idx]` by casting a counterspell.
///
/// When `probabilistic = true`:
///   - Per counterspell: roll P(card in hand) via hypergeometric, then a strategic 50% choice
///     (overridden by `cost.prob` if set on the first cost option).
///   - For `exile_blue_from_hand` costs: also roll P(have a blue pitch card in hand).
/// When `probabilistic = false` the attempt is deterministic (used when protecting Doomsday).
///
/// On success, returns a `CastSpell` intent targeting `stack[target_idx]`.
/// No resources are spent; the caller (`handle_priority_round`) commits the action.
fn respond_with_counter(
    state: &SimState,
    target_idx: usize,
    responding_who: PlayerId,
    rng: &mut impl Rng,
    probabilistic: bool,
) -> Option<PriorityAction> {
    let target_id = state.stack[target_idx];
    let target_owner = if state.stack_item_owner(target_id) == state.us.id { PlayerId::Us } else { PlayerId::Opp };
    let target_has_untapped_lands = state.permanents_of(target_owner).any(|c| {
        c.bf.as_ref().map_or(false, |bf| !bf.tapped) && !state.def_of(c.id).map(|d| d.mana_abilities()).unwrap_or(&[]).is_empty()
    });

    // Collect (ObjId, name) for each unique counterspell type in hand that can target the stack spell.
    let mut seen = std::collections::HashSet::new();
    let counterspells: Vec<(ObjId, String)> = state.hand_of(responding_who)
        .filter_map(|c| {
            let def = state.def_of(c.id)?;
            let stack_pred = if let TargetSpec::ObjectInZone { zone: ZoneId::Stack, ref filter, .. } = def.target_spec() {
                filter.clone()
            } else {
                return None;
            };
            if !state.def_of(target_id).map(|d| stack_pred(d)).unwrap_or(true) { return None; }
            if def.alternate_costs().is_empty() { return None; }
            if c.catalog_key == "Daze" && target_has_untapped_lands { return None; }
            seen.insert(c.catalog_key.clone()).then(|| (c.id, c.catalog_key.clone()))
        })
        .collect();

    if counterspells.is_empty() {
        return None;
    }

    let hand_size = state.hand_size(responding_who);
    let lib_size = state.library_size(responding_who) + hand_size as usize;

    for (cs_id, cs_name) in &counterspells {
        let def = match state.def_of(*cs_id) {
            Some(d) => d,
            None => continue,
        };
        if probabilistic {
            let copies = state.hand_of(responding_who).filter(|c| c.catalog_key == *cs_name).count();
            let p_have = p_card_in_hand(lib_size, hand_size, copies);
            if !rng.gen_bool(p_have.max(f64::MIN_POSITIVE)) { continue; }

            let costs = def.alternate_costs();
            let strategic = costs.first().and_then(|c| c.prob).unwrap_or(0.5);
            if !rng.gen_bool(strategic) { continue; }
        }

        let costs = def.alternate_costs().to_vec();
        for cost in &costs {
            if probabilistic && cost.exile_blue_from_hand {
                let n_blue = state.hand_of(responding_who)
                    .filter(|c| c.id != *cs_id
                        && state.def_of(c.id).map_or(false, |d| !d.is_land() && d.is_blue()))
                    .count();
                let p_have_blue = p_card_in_hand(lib_size, hand_size, n_blue);
                if !rng.gen_bool(p_have_blue.max(f64::MIN_POSITIVE)) { continue; }
            }
            if can_pay_alternate_cost(cost, state, responding_who, cs_name) {
                return Some(PriorityAction::CastSpell {
                    card_id: *cs_id,
                    face: SpellFace::Main,
                    preferred_cost: Some(cost.clone()),
                    chosen_targets: vec![target_id],
                });
            }
        }
    }
    None
}

// ── Combat strategy ───────────────────────────────────────────────────────────

/// Try to perform ninjutsu during a combat priority window (DeclareBlockers / CombatDamage / EndCombat).
///
/// Requires: unblocked attacker, ninjutsu card in hand, and enough mana.
/// Returns an `ActivateAbility` action or `None` if conditions aren't met.
pub(super) fn try_ninjutsu(
    state: &SimState,
    who: PlayerId,
    rng: &mut impl Rng,
) -> Option<PriorityAction> {
    if state.hand_size(who) <= 0 { return None; }
    let has_unblocked = state.permanents_of(who)
        .any(|c| c.bf.as_ref().map_or(false, |bf| bf.attacking && bf.unblocked));
    if !has_unblocked { return None; }
    let ninjas: Vec<(ObjId, String)> = state.hand_of(who)
        .filter(|c| state.def_of(c.id).map_or(false, |d| d.ninjutsu().is_some()))
        .map(|c| (c.id, c.catalog_key.clone()))
        .collect();
    if ninjas.is_empty() { return None; }
    // 35% roll: simulates probability of wanting to use it.
    if !rng.gen_bool(0.35) { return None; }
    let idx = rng.gen_range(0..ninjas.len());
    let (ninja_id, _) = &ninjas[idx];
    let ninja_def = state.def_of(*ninja_id)?;
    let ninjutsu_cost = parse_mana_cost(&ninja_def.ninjutsu()?.mana_cost);
    if !state.potential_mana(who).can_pay(&ninjutsu_cost) { return None; }
    Some(PriorityAction::ActivateAbility(*ninja_id, ninja_def.ninjutsu()?.as_ability_def(), vec![]))
}
