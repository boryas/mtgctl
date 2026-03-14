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
    /// A specific player (`who` resolved relative to the acting controller).
    Player(Who),
    /// A permanent matching `filter` controlled by `controller`.
    Permanent { controller: Who, filter: String },
    /// A card in `zone` owned/controlled by `controller` matching `filter`.
    CardInZone { controller: Who, zone: ZoneId, filter: String },
    /// A spell on the stack owned by the opponent matching `filter`.
    StackEntry { filter: String },
    /// Any one of several sub-specs is a legal target (e.g. "any target" = creature | planeswalker | player).
    Union(Vec<TargetSpec>),
}

/// Parse a TOML target string into a `TargetSpec`.
pub(crate) fn target_spec_from_str(target: Option<&str>) -> TargetSpec {
    let Some(s) = target else { return TargetSpec::None; };
    if let Some(filter) = s.strip_prefix("stack:") {
        return TargetSpec::StackEntry { filter: filter.to_string() };
    }
    if let Some(rest) = s.strip_prefix("self:gy:") {
        return TargetSpec::CardInZone {
            controller: Who::Actor,
            zone: ZoneId::Graveyard,
            filter: rest.to_string(),
        };
    }
    if let Some(filter) = s.strip_prefix("opp:") {
        return TargetSpec::Permanent { controller: Who::Opp, filter: filter.to_string() };
    }
    if s == "any_target" {
        // "Any target" = creature permanent | planeswalker permanent | player.
        return TargetSpec::Union(vec![
            TargetSpec::Permanent { controller: Who::Opp, filter: "creature".to_string() },
            TargetSpec::Permanent { controller: Who::Opp, filter: "planeswalker".to_string() },
            TargetSpec::Player(Who::Opp),
        ]);
    }
    TargetSpec::None
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
        TargetSpec::Player(who) => Some(Target::Player(state.player_id(who.resolve(controller)))),
        TargetSpec::Permanent { controller: who, filter } => {
            let target_who = who.resolve(controller);
            state.permanents_of(target_who)
                .find(|p| {
                    catalog_map.get(p.name.as_str())
                        .map(|d| {
                            let basic = d.as_land().map_or(false, |l| l.basic);
                            matches_target_type(filter, &d.kind, basic, Some(d))
                        })
                        .unwrap_or(filter == "any")
                })
                .map(|p| Target::Object(p.id))
        }
        TargetSpec::CardInZone { controller: who, zone: ZoneId::Graveyard, filter } => {
            let target_who = who.resolve(controller);
            state.graveyard_of(target_who)
                .find(|c| {
                    if filter.is_empty() || filter == "any" { return true; }
                    catalog_map.get(c.name.as_str())
                        .map(|d| matches_target_type(filter, &d.kind, false, Some(d)))
                        .unwrap_or(false)
                })
                .map(|c| Target::Object(c.id))
        }
        TargetSpec::CardInZone { .. } => None,
        TargetSpec::Union(specs) => {
            // Strategy: prefer a killable opponent creature (1-damage kill).
            // Non-killable creatures are lower priority than planeswalker or player.
            if let Some(id) = state.permanents_of(opp)
                .filter(|p| {
                    let def = catalog_map.get(p.name.as_str());
                    if !def.map(|d| d.is_creature()).unwrap_or(false) { return false; }
                    let bf = p.bf.as_ref().unwrap();
                    let (_, tgh) = creature_stats(bf, def.copied());
                    tgh - bf.damage <= 1 && tgh > 0
                })
                .map(|p| p.id)
                .next()
            {
                return Some(Target::Object(id));
            }
            // No killable creature: try non-creature sub-specs (planeswalker, player).
            for spec in specs {
                if matches!(spec, TargetSpec::Permanent { filter, .. } if filter == "creature") { continue; }
                if let Some(t) = choose_trigger_target(spec, controller, state, catalog_map) {
                    return Some(t);
                }
            }
            None
        }
        TargetSpec::StackEntry { filter } => {
            let caster_id = state.player_id(controller);
            // Pick the topmost opposing non-ability spell matching the filter.
            state.stack.iter().rev()
                .find(|&&id| {
                    if state.stack_item_owner(id) == caster_id || !state.stack_item_is_counterable(id) { return false; }
                    match catalog_map.get(state.stack_item_display_name(id)) {
                        Some(d) => stack_filter_matches(filter, &d.kind),
                        None    => filter == "any",
                    }
                })
                .map(|&id| Target::Object(id))
        }
    }
}

/// Choose a target for a spell. Zone targets are selected randomly (strategy-layer RNG);
/// all other specs delegate to `choose_trigger_target` (deterministic).
pub(crate) fn choose_spell_target(
    spec: &TargetSpec,
    caster: &str,
    state: &SimState,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> Option<Target> {
    match spec {
        TargetSpec::CardInZone { controller: who, zone: ZoneId::Graveyard, filter } => {
            let target_who = who.resolve(caster).to_string();
            let candidates: Vec<ObjId> = state.graveyard_of(&target_who)
                .filter(|c| {
                    if filter.is_empty() || filter == "any" { return true; }
                    catalog_map.get(c.name.as_str())
                        .map(|d| matches_target_type(filter, &d.kind, false, Some(d)))
                        .unwrap_or(false)
                })
                .map(|c| c.id)
                .collect();
            if candidates.is_empty() { return None; }
            Some(Target::Object(candidates[rng.gen_range(0..candidates.len())]))
        }
        TargetSpec::Permanent { controller: who, filter } => {
            let target_who = who.resolve(caster).to_string();
            let candidates: Vec<ObjId> = state.permanents_of(&target_who)
                .filter(|p| {
                    catalog_map.get(p.name.as_str())
                        .map(|d| {
                            let basic = d.as_land().map_or(false, |l| l.basic);
                            matches_target_type(filter, &d.kind, basic, Some(d))
                        })
                        .unwrap_or(filter == "any")
                })
                .map(|p| p.id)
                .collect();
            if candidates.is_empty() { return None; }
            Some(Target::Object(candidates[rng.gen_range(0..candidates.len())]))
        }
        other => choose_trigger_target(other, caster, state, catalog_map),
    }
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
        "nonland"       => !matches!(kind, CardKind::Land(_)),
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
/// For permanent/zone targets, checks the battlefield or zone.
pub(crate) fn has_valid_target(
    target_str: &str,
    state: &SimState,
    actor: &str,
    catalog_map: &HashMap<&str, &CardDef>,
) -> bool {
    has_valid_target_spec(&target_spec_from_str(Some(target_str)), state, actor, catalog_map)
}

fn has_valid_target_spec(
    spec: &TargetSpec,
    state: &SimState,
    actor: &str,
    catalog_map: &HashMap<&str, &CardDef>,
) -> bool {
    match spec {
        TargetSpec::None => false,
        TargetSpec::Player(_) => true,   // there is always an opponent
        TargetSpec::StackEntry { filter } => {
            let actor_id = state.player_id(actor);
            state.stack.iter().any(|&id| {
                if state.stack_item_owner(id) == actor_id || !state.stack_item_is_counterable(id) { return false; }
                match catalog_map.get(state.stack_item_display_name(id)) {
                    Some(d) => stack_filter_matches(filter, &d.kind),
                    None    => filter == "any",
                }
            })
        }
        TargetSpec::Permanent { controller: who, filter } => {
            let target_who = who.resolve(actor);
            state.permanents_of(target_who).any(|p| {
                match catalog_map.get(p.name.as_str()).copied() {
                    Some(d) => {
                        let basic = d.as_land().map_or(false, |l| l.basic);
                        matches_target_type(filter, &d.kind, basic, Some(d))
                    }
                    None => filter == "any",
                }
            })
        }
        TargetSpec::CardInZone { controller: who, zone: ZoneId::Graveyard, filter } => {
            let target_who = who.resolve(actor);
            state.graveyard_of(target_who).any(|c| {
                if filter.is_empty() || filter == "any" { return true; }
                catalog_map.get(c.name.as_str())
                    .map(|d| matches_target_type(filter, &d.kind, false, Some(d)))
                    .unwrap_or(false)
            })
        }
        TargetSpec::CardInZone { .. } => false,
        TargetSpec::Union(specs) => specs.iter().any(|s| has_valid_target_spec(s, state, actor, catalog_map)),
    }
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
    for perm in state.permanents_of(&target_who) {
        let def = catalog_map.get(perm.name.as_str()).copied();
        let matched = match def {
            Some(d) => {
                let basic = d.as_land().map_or(false, |l| l.basic);
                matches_target_type(type_str, &d.kind, basic, Some(d))
            }
            None => type_str == "any",
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
