use std::collections::HashMap;
use rand::Rng;
use super::*;

/// A concrete, resolved reference to a game object that can be targeted.
/// `id` fields default to `ObjId::UNSET` for now; they'll be filled in as objects get IDs.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Target {
    Player(ObjId),
    Object(ObjId),
}

/// Declarative description of what targets a spell or ability may choose from.
/// Used both to enumerate legal choices and to re-validate at resolution.
#[derive(Clone, Debug)]
pub(crate) enum TargetSpec {
    None,
    Player,
    AnyOpponentCreature,
    AnyOpponentNonlandPermanent,
    OpponentCreatureMvLt4,
    OpponentNonblackCreature,
    CardInOwnGraveyard { card_type: Option<String> },
    /// Any player or creature — used by Orcish Bowmasters ping.
    AnyTarget,
    /// A spell currently on the stack owned by the opponent, matching `filter`.
    /// Used by counterspells: `filter` is "any", "noncreature", "nonland", or "instant_or_sorcery".
    StackEntry { filter: String },
    // Extend as new cards require it.
}

/// Return true if stack spell `kind` matches the given filter string.
pub(crate) fn stack_filter_matches(filter: &str, kind: &CardKind) -> bool {
    match filter {
        "any"                => true,
        "noncreature"        => !matches!(kind, CardKind::Creature(_)),
        "nonland"            => !matches!(kind, CardKind::Land(_)),
        "instant_or_sorcery" => matches!(kind, CardKind::Instant(_) | CardKind::Sorcery(_)),
        _                    => false,
    }
}

/// Choose a target for a trigger according to its spec and current game state.
/// Returns None if the spec is None or no legal targets exist.
pub(crate) fn choose_trigger_target(
    spec: &TargetSpec,
    controller: &str,
    state: &SimState,
    catalog_map: &HashMap<&str, &CardDef>,
) -> Option<Target> {
    let opp = opp_of(controller);
    match spec {
        TargetSpec::None => None,
        TargetSpec::Player => Some(Target::Player(state.player_id(opp))),
        TargetSpec::AnyOpponentCreature => {
            state.player(opp).permanents.iter()
                .find(|p| p.name != "Orc Army") // placeholder: any creature
                .map(|p| Target::Object(p.id))
        }
        TargetSpec::AnyOpponentNonlandPermanent => {
            state.player(opp).permanents.first()
                .map(|p| Target::Object(p.id))
        }
        TargetSpec::OpponentCreatureMvLt4 => {
            state.player(opp).permanents.iter()
                .find(|p| {
                    catalog_map.get(p.name.as_str())
                        .map(|d| d.is_creature() && mana_value(d.mana_cost()) < 4)
                        .unwrap_or(true)
                })
                .map(|p| Target::Object(p.id))
        }
        TargetSpec::OpponentNonblackCreature => {
            state.player(opp).permanents.iter()
                .find(|p| {
                    catalog_map.get(p.name.as_str())
                        .map(|d| d.is_creature() && !d.is_black())
                        .unwrap_or(true)
                })
                .map(|p| Target::Object(p.id))
        }
        TargetSpec::CardInOwnGraveyard { .. } => {
            // Graveyard cards don't have stable ObjIds yet; target selection deferred.
            None
        }
        TargetSpec::AnyTarget => {
            // Strategy: prefer a killable opponent creature (1-damage kill),
            // then default to pinging the opponent's face.
            if let Some(id) = state.player(opp).permanents.iter()
                .filter(|p| {
                    let def = catalog_map.get(p.name.as_str());
                    if !def.map(|d| d.is_creature()).unwrap_or(false) { return false; }
                    let (_, tgh) = creature_stats(p, def.copied());
                    tgh - p.damage <= 1 && tgh > 0
                })
                .map(|p| p.id)
                .next()
            {
                return Some(Target::Object(id));
            }
            Some(Target::Player(state.player_id(opp)))
        }
        TargetSpec::StackEntry { filter } => {
            let caster_id = state.player_id(controller);
            // Pick the topmost opposing non-ability spell matching the filter.
            state.stack.iter().rev()
                .find(|item| {
                    if item.owner == caster_id || item.is_ability { return false; }
                    match catalog_map.get(item.name.as_str()) {
                        Some(d) => stack_filter_matches(filter, &d.kind),
                        None    => filter == "any",
                    }
                })
                .map(|item| Target::Object(item.id))
        }
    }
}

/// Choose a target for a spell using the same TargetSpec logic as trigger target selection.
pub(crate) fn choose_spell_target(
    spec: &TargetSpec,
    caster: &str,
    state: &SimState,
    catalog_map: &HashMap<&str, &CardDef>,
) -> Option<Target> {
    choose_trigger_target(spec, caster, state, catalog_map)
}

/// Check whether `type_str` matches a permanent. `def` is the target card's definition,
/// required for MV and color checks (may be None for lands or unknown cards).
pub(crate) fn matches_target_type(
    type_str: &str,
    kind: &CardKind,
    basic: bool,
    def: Option<&CardDef>,
) -> bool {
    match type_str {
        "nonbasic_land" => matches!(kind, CardKind::Land(_)) && !basic,
        "land"          => matches!(kind, CardKind::Land(_)),
        "creature"      => matches!(kind, CardKind::Creature(_)),
        "planeswalker"  => matches!(kind, CardKind::Planeswalker(_)),
        "artifact"      => matches!(kind, CardKind::Artifact(_)),
        "any"           => true,
        "creature_mv_lt4" => {
            matches!(kind, CardKind::Creature(_))
                && def.map(|d| mana_value(d.mana_cost()) < 4).unwrap_or(true)
        }
        "creature_nonblack" => {
            matches!(kind, CardKind::Creature(_))
                && def.map(|d| !d.is_black()).unwrap_or(true)
        }
        // Non-land permanent: since all entries in `permanents` are already non-land,
        // this is true for any permanent (lands are in `lands`, not `permanents`).
        "permanent_nonland" => !matches!(kind, CardKind::Land(_)),
        _ => false,
    }
}

/// Return true if at least one valid target exists for `target_str`.
/// For `"stack:<filter>"` targets, checks the current stack for opposing non-ability spells.
/// For permanent/player targets, checks the battlefield.
pub(crate) fn has_valid_target(
    target_str: &str,
    state: &SimState,
    actor: &str,
    catalog_map: &HashMap<&str, &CardDef>,
) -> bool {
    if let Some(filter) = target_str.strip_prefix("stack:") {
        let actor_id = state.player_id(actor);
        return state.stack.iter().any(|item| {
            if item.owner == actor_id || item.is_ability { return false; }
            match catalog_map.get(item.name.as_str()) {
                Some(d) => stack_filter_matches(filter, &d.kind),
                None    => filter == "any",
            }
        });
    }
    let (who_rel, type_str) = match target_str.split_once(':') {
        Some(pair) => pair,
        None => return false,
    };
    let target_who = resolve_who(who_rel, actor);
    let player = state.player(target_who);
    player
        .lands
        .iter()
        .any(|l| matches_target_type(type_str, &CardKind::Land(LandData::default()), l.basic, None))
        || player.permanents.iter().any(|p| {
            match catalog_map.get(p.name.as_str()).copied() {
                Some(d) => matches_target_type(type_str, &d.kind, false, Some(d)),
                None    => type_str == "any",
            }
        })
}

/// Pick a random valid permanent target for `target_str` (e.g. "opp:creature_mv_lt4").
/// Returns the stable `ObjId` of the chosen permanent, or `None` if no valid target exists.
pub(crate) fn choose_permanent_target(
    target_str: &str,
    actor: &str,
    state: &SimState,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> Option<ObjId> {
    let (who_rel, type_str) = target_str.split_once(':')?;
    let target_who = resolve_who(who_rel, actor).to_string();

    let mut candidates: Vec<ObjId> = Vec::new();
    for land in &state.player(&target_who).lands {
        if matches_target_type(type_str, &CardKind::Land(LandData::default()), land.basic, None) {
            candidates.push(land.id);
        }
    }
    for perm in &state.player(&target_who).permanents {
        let def = catalog_map.get(perm.name.as_str()).copied();
        let matched = match def {
            Some(d) => matches_target_type(type_str, &d.kind, false, Some(d)),
            None    => type_str == "any",
        };
        if matched { candidates.push(perm.id); }
    }
    if candidates.is_empty() {
        return None;
    }
    let idx = rng.gen_range(0..candidates.len());
    Some(candidates.remove(idx))
}


/// Match a search filter string against a card definition.
/// Filter syntax: `"land"`, `"land-island"`, `"land-swamp"`, `"land-island|swamp"`.
pub(crate) fn matches_search_filter(filter: &str, def: &CardDef) -> bool {
    let Some(land) = def.as_land() else { return false; };
    if filter == "land" { return true; }
    if let Some(types_str) = filter.strip_prefix("land-") {
        return types_str.split('|').any(|t| match t {
            "island"   => land.land_types.island,
            "swamp"    => land.land_types.swamp,
            "plains"   => land.land_types.plains,
            "mountain" => land.land_types.mountain,
            "forest"   => land.land_types.forest,
            _          => false,
        });
    }
    false
}

/// Resolve `"<who>"` relative to the acting player.
pub(crate) fn resolve_who<'a>(who_rel: &str, actor: &'a str) -> &'a str {
    if who_rel == "opp" {
        if actor == "us" {
            "opp"
        } else {
            "us"
        }
    } else {
        actor
    }
}
