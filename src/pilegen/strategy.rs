use rand::Rng;
use std::collections::HashMap;
use super::*;

fn pick_on_board_action(
    state: &mut SimState,
    ap: &str,
    t: u8,
    dd_turn: u8,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> Option<PriorityAction> {
    let mut candidates: Vec<PriorityAction> = Vec::new();

    // 75% roll per land (permanent that is_land) with an available ability.
    let land_ids: Vec<(ObjId, String)> = state.permanents_of(ap)
        .filter(|p| !p.bf.as_ref().map_or(false, |bf| bf.tapped))
        .filter(|p| catalog_map.get(p.name.as_str())
            .map_or(false, |def| def.is_land() && def.abilities().iter()
                .any(|ab| ability_available(ab, state, ap, true, catalog_map))))
        .map(|p| (p.id, p.name.clone()))
        .collect();
    for (land_id, name) in land_ids {
        if let Some(def) = catalog_map.get(name.as_str()) {
            if let Some(ab) = def.abilities().iter()
                .find(|ab| ability_available(ab, state, ap, true, catalog_map))
                .cloned()
            {
                if rng.gen_bool(0.75) {
                    candidates.push(PriorityAction::ActivateAbility(land_id, ab));
                }
            }
        }
    }

    // 75% roll per non-land permanent with an available non-loyalty ability.
    // Lands are already handled above; including them here would double-queue fetch abilities.
    let perm_ids: Vec<(ObjId, String, bool)> = state.permanents_of(ap)
        .filter(|p| {
            let tapped = p.bf.as_ref().map_or(false, |bf| bf.tapped);
            catalog_map.get(p.name.as_str())
                .map_or(false, |def| !def.is_land() && def.abilities().iter()
                    .any(|ab| ab.loyalty_cost.is_none() && ability_available(ab, state, ap, !tapped, catalog_map)))
        })
        .map(|p| (p.id, p.name.clone(), p.bf.as_ref().map_or(false, |bf| bf.tapped)))
        .collect();
    for (perm_id, name, tapped) in perm_ids {
        if let Some(def) = catalog_map.get(name.as_str()) {
            if let Some(ab) = def.abilities().iter()
                .find(|ab| ab.loyalty_cost.is_none() && ability_available(ab, state, ap, !tapped, catalog_map))
                .cloned()
            {
                if rng.gen_bool(0.75) {
                    candidates.push(PriorityAction::ActivateAbility(perm_id, ab));
                }
            }
        }
    }

    // Postcombat main phase: activate any un-activated planeswalkers (sorcery speed, empty stack).
    let is_postcombat_main = matches!(state.current_phase, Some(TurnPosition::Phase(PhaseKind::PostCombatMain)));
    if is_postcombat_main && state.stack.is_empty() {
        let pw_data: Vec<(ObjId, String, i32)> = state.permanents_of(ap)
            .filter(|p| {
                let bf = p.bf.as_ref();
                !bf.map_or(true, |bf| bf.pw_activated_this_turn)
                    && catalog_map.get(p.name.as_str())
                        .map_or(false, |def| matches!(def.kind, CardKind::Planeswalker(_)))
            })
            .map(|p| (p.id, p.name.clone(), p.bf.as_ref().map_or(0, |bf| bf.loyalty)))
            .collect();
        for (pw_id, name, loyalty) in pw_data {
            if let Some(def) = catalog_map.get(name.as_str()) {
                if let Some(ab) = def.abilities().iter()
                    .filter(|ab| {
                        let Some(cost) = ab.loyalty_cost else { return false; };
                        !(cost < 0 && loyalty < -cost)
                    })
                    .next()
                    .cloned()
                {
                    candidates.push(PriorityAction::ActivateAbility(pw_id, ab));
                }
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
                // Force-include if not already a candidate (bypasses the 75% roll).
                if !candidates.iter().any(|a| matches!(a, PriorityAction::ActivateAbility(id, _) if *id == *fid)) {
                    if let Some(def) = catalog_map.get(name.as_str()) {
                        if let Some(ab) = def.abilities().iter()
                            .find(|ab| ability_available(ab, state, "us", true, catalog_map))
                            .cloned()
                        {
                            candidates.push(PriorityAction::ActivateAbility(*fid, ab));
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
    let on_adventure: Vec<(ObjId, String)> = state.on_adventure_of(ap).map(|c| (c.id, c.name.clone())).collect();
    for (card_id, card_name) in on_adventure {
        if let Some(&def) = catalog_map.get(card_name.as_str()) {
            let cost = parse_mana_cost(def.mana_cost());
            if !state.potential_mana(ap).can_pay(&cost) { continue; }
            if rng.gen_bool(0.75) {
                candidates.push(PriorityAction::CastSpell { card_id, face: SpellFace::Main, preferred_cost: None });
            }
        }
    }

    if candidates.is_empty() { None } else { Some(candidates.remove(rng.gen_range(0..candidates.len()))) }
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
    let other_acted = matches!(last_action, PriorityAction::CastSpell { .. } | PriorityAction::ActivateAbility(..));
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
                    if let PriorityAction::CastSpell { card_id, .. } = action {
                        let spell_name = state.cards.get(&card_id).map_or("?", |c| c.name.as_str());
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
                    if let Some(id) = choose_land(state, who, catalog_map, fateful, rng) {
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
    if let Some(action) = pick_on_board_action(state, who, t, dd_turn, catalog_map, rng) {
        return action;
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

    let spell_name = |a: &PriorityAction| -> Option<String> {
        if let PriorityAction::CastSpell { card_id, .. } = a {
            state.cards.get(card_id).map(|c| c.name.clone())
        } else { None }
    };

    // Fateful turn prioritization: Doomsday > Dark Ritual > anything else.
    let fateful = who == "us" && t == dd_turn && !state.us.dd_cast;
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
        let name = state.cards.get(card_id).map_or("?".to_string(), |c| c.name.clone());
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

// ── Hand and board action enumeration ─────────────────────────────────────────

/// Check whether an ability can be activated (cost payable + valid target exists).
/// `source_untapped` must be true when the source is an untapped land/permanent.
fn ability_available(
    ability: &AbilityDef,
    state: &SimState,
    who: &str,
    source_untapped: bool,
    catalog_map: &HashMap<&str, &CardDef>,
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
    if let Some(tgt) = &ability.target {
        if !has_valid_target(tgt, state, who, catalog_map) {
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
    who: &str,
    catalog_map: &HashMap<&str, &CardDef>,
) -> bool {
    let mut cost = parse_mana_cost(def.mana_cost());
    if def.delve() && cost.generic > 0 {
        let gy_len = state.graveyard_of(who).count() as i32;
        cost.generic = (cost.generic - gy_len).max(0);
    }
    let mana_is_usable = !def.mana_cost().is_empty() && state.potential_mana(who).can_pay(&cost);
    if mana_is_usable { return true; }
    def.alternate_costs().iter().any(|c| can_pay_alternate_cost(c, state, who, name, catalog_map))
}

fn hand_ability_affordable(ability: &AbilityDef, state: &SimState, who: &str) -> bool {
    let player = state.player(who);
    if !ability.mana_cost.is_empty() {
        if !state.potential_mana(who).can_pay(&parse_mana_cost(&ability.mana_cost)) { return false; }
    }
    if ability.life_cost > 0 && player.life <= ability.life_cost { return false; }
    if ability.sacrifice_land && !state.permanents_of(who).any(|c| {
        c.bf.as_ref().map_or(false, |bf| !bf.mana_abilities.is_empty())
    }) { return false; }
    true
}

fn collect_hand_actions(
    state: &SimState,
    who: &str,
    catalog_map: &HashMap<&str, &CardDef>,
) -> Vec<PriorityAction> {
    if state.hand_size(who) <= 0 {
        return Vec::new();
    }
    let opp_who = if who == "us" { "opp" } else { "us" };

    let hand_cards: Vec<(ObjId, String)> = state.hand_of(who)
        .map(|c| (c.id, c.name.clone()))
        .collect();

    let mut seen_names: std::collections::HashSet<String> = Default::default();
    let mut actions: Vec<PriorityAction> = Vec::new();

    for (card_id, name) in &hand_cards {
        let Some(&def) = catalog_map.get(name.as_str()) else { continue; };
        if def.is_land() { continue; }
        if !card_has_implementation(def) { continue; }
        if def.legendary() && state.permanents_of(who).any(|c| c.name == name.as_str()) { continue; }
        if let Some(tgt) = def.target() {
            if !has_valid_target(tgt, state, who, catalog_map) { continue; }
        }
        let ok = def.requires().iter().all(|req| match req.as_str() {
            "opp_hand_nonempty" => state.hand_size(opp_who) > 0,
            "us_gy_has_creature" => state.graveyard_of(who)
                .any(|c| catalog_map.get(c.name.as_str()).map(|d| d.is_creature()).unwrap_or(false)),
            _ => true,
        });
        if !ok { continue; }
        if !spell_is_affordable(name, def, state, who, catalog_map) { continue; }
        if seen_names.insert(name.clone()) {
            actions.push(PriorityAction::CastSpell { card_id: *card_id, face: SpellFace::Main, preferred_cost: None });
        }

        // In-hand abilities (cycling, channel, etc.)
        for ab in def.abilities().iter().filter(|ab| ab.zone == "hand") {
            if hand_ability_affordable(ab, state, who) {
                actions.push(PriorityAction::ActivateAbility(*card_id, ab.clone()));
            }
        }

        // Adventure spell face.
        if let Some(face) = def.adventure() {
            if !face.mana_cost.is_empty() {
                let cost = parse_mana_cost(&face.mana_cost);
                if !state.potential_mana(who).can_pay(&cost) { continue; }
            }
            if let Some(ref tgt) = face.target {
                if !has_valid_target(tgt, state, who, catalog_map) { continue; }
            }
            actions.push(PriorityAction::CastSpell { card_id: *card_id, face: SpellFace::Adventure, preferred_cost: None });
        }
    }

    actions
}

/// Select which land to play on a given turn. Returns the card's `ObjId`, or `None`.
///
/// On the fateful turn, requires a black-producing land if no black source is in play.
fn choose_land(
    state: &SimState,
    who: &str,
    catalog_map: &HashMap<&str, &CardDef>,
    fateful: bool,
    rng: &mut impl Rng,
) -> Option<ObjId> {
    if state.hand_size(who) <= 0 {
        return None;
    }
    let need_black = fateful && !state.has_black_mana(who);
    let candidates: Vec<ObjId> = state.hand_of(who)
        .filter_map(|c| {
            let def = catalog_map.get(c.name.as_str())?;
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
    responding_who: &str,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
    probabilistic: bool,
) -> Option<PriorityAction> {
    let default_kind;
    let target_name = state.stack_item_display_name(state.stack[target_idx]).to_string();
    let target_kind: &CardKind = match catalog_map.get(target_name.as_str()) {
        Some(d) => &d.kind,
        None => { default_kind = CardKind::Sorcery(SpellData::default()); &default_kind }
    };

    let target_owner_str = state.who_str(state.stack_item_owner(state.stack[target_idx])).to_string();
    let target_has_untapped_lands = state.permanents_of(&target_owner_str).any(|c| {
        c.bf.as_ref().map_or(false, |bf| !bf.tapped && !bf.mana_abilities.is_empty())
    });

    let mut seen = std::collections::HashSet::new();
    let counterspells: Vec<String> = state.hand_of(responding_who)
        .filter_map(|c| {
            let def = catalog_map.get(c.name.as_str())?;
            let filter = def.target().and_then(|t| t.strip_prefix("stack:"))?;
            if !stack_filter_matches(filter, target_kind) { return None; }
            if def.alternate_costs().is_empty() { return None; }
            if c.name == "Daze" && target_has_untapped_lands { return None; }
            seen.insert(c.name.clone()).then(|| c.name.clone())
        })
        .collect();

    if counterspells.is_empty() {
        return None;
    }

    let hand_size = state.hand_size(responding_who);
    let lib_size = state.library_size(responding_who) + hand_size as usize;

    for cs_name in &counterspells {
        if probabilistic {
            let copies = state.hand_of(responding_who).filter(|c| c.name == *cs_name).count();
            let p_have = p_card_in_hand(lib_size, hand_size, copies);
            if !rng.gen_bool(p_have.max(f64::MIN_POSITIVE)) { continue; }

            let costs = catalog_map[cs_name.as_str()].alternate_costs();
            let strategic = costs.first().and_then(|c| c.prob).unwrap_or(0.5);
            if !rng.gen_bool(strategic) { continue; }
        }

        let costs = catalog_map[cs_name.as_str()].alternate_costs().to_vec();
        for cost in &costs {
            if probabilistic && cost.exile_blue_from_hand {
                let n_blue = state.hand_of(responding_who)
                    .filter(|c| c.name != *cs_name
                        && catalog_map.get(c.name.as_str()).map_or(false, |d| !d.is_land() && d.is_blue()))
                    .count();
                let p_have_blue = p_card_in_hand(lib_size, hand_size, n_blue);
                if !rng.gen_bool(p_have_blue.max(f64::MIN_POSITIVE)) { continue; }
            }
            if can_pay_alternate_cost(cost, state, responding_who, cs_name, catalog_map) {
                let card_id = state.hand_of(responding_who).find(|c| c.name == *cs_name).map(|c| c.id)?;
                return Some(PriorityAction::CastSpell {
                    card_id,
                    face: SpellFace::Main,
                    preferred_cost: Some(cost.clone()),
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
