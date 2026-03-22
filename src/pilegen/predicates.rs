use super::*;

// ── CardPredicate ─────────────────────────────────────────────────────────────

/// A composable predicate over a `CardDef`. Used to express targeting filters
/// without string dispatch.
pub(crate) type CardPredicate = std::sync::Arc<dyn Fn(&CardDef) -> bool + Send + Sync>;

/// Always returns true.
pub(crate) fn pred_any() -> CardPredicate {
    std::sync::Arc::new(|_| true)
}


/// True iff the card's primary type equals `t`.
pub(crate) fn pred_type_eq(t: CardType) -> CardPredicate {
    std::sync::Arc::new(move |d| d.types.contains(&t))
}

/// True iff the card has supertype `s`.
pub(crate) fn pred_has_supertype(s: Supertype) -> CardPredicate {
    std::sync::Arc::new(move |d| d.supertypes.contains(&s))
}

/// True iff the card is a land with the given land subtype (island, swamp, …).
pub(crate) fn pred_land_subtype(subtype: &'static str) -> CardPredicate {
    std::sync::Arc::new(move |d| {
        d.as_land().map_or(false, |l| match subtype {
            "island"   => l.land_types.island,
            "swamp"    => l.land_types.swamp,
            "plains"   => l.land_types.plains,
            "mountain" => l.land_types.mountain,
            "forest"   => l.land_types.forest,
            _          => false,
        })
    })
}

/// True iff the card contains the given color.
pub(crate) fn pred_has_color(c: Color) -> CardPredicate {
    std::sync::Arc::new(move |d| d.colors.contains(&c))
}

/// True iff the card's mana value is ≤ `n`.
pub(crate) fn pred_mana_value_le(n: i32) -> CardPredicate {
    std::sync::Arc::new(move |d| mana_value(d.mana_cost()) <= n)
}

/// True iff the card is a creature with toughness ≤ `n`.
pub(crate) fn pred_toughness_le(n: i32) -> CardPredicate {
    std::sync::Arc::new(move |d| d.as_creature().map_or(false, |c| c.toughness() <= n))
}

/// True iff the card's mana cost has no colored pips (generic/colorless only).
#[allow(dead_code)] // used by Urza's Saga search (search plan, not yet implemented)
pub(crate) fn pred_no_colored_pips() -> CardPredicate {
    std::sync::Arc::new(|d| d.colors.is_empty())
}

/// Logical AND of two predicates.
pub(crate) fn pred_and(a: CardPredicate, b: CardPredicate) -> CardPredicate {
    std::sync::Arc::new(move |d| a(d) && b(d))
}

/// Logical OR of two predicates.
pub(crate) fn pred_or(a: CardPredicate, b: CardPredicate) -> CardPredicate {
    std::sync::Arc::new(move |d| a(d) || b(d))
}

/// Logical NOT of a predicate.
pub(crate) fn pred_not(p: CardPredicate) -> CardPredicate {
    std::sync::Arc::new(move |d| !p(d))
}

// ── ObjPredicate ──────────────────────────────────────────────────────────────

/// A state-aware predicate over a game object: takes the candidate object id and
/// current state; can inspect both the card def and battlefield state.
/// Used for cost payment filters, choice enumeration, and `ObjectInZone` targeting.
pub(crate) type ObjPredicate = std::sync::Arc<dyn Fn(ObjId, &SimState) -> bool + Send + Sync>;

/// Lift a `CardPredicate` into an `ObjPredicate`.
/// Falls back to catalog lookup for non-battlefield objects (hand, graveyard, etc.)
/// where the materialized view is not populated.
pub(crate) fn obj_pred_from_card(p: CardPredicate) -> ObjPredicate {
    std::sync::Arc::new(move |id, state| {
        if let Some(d) = state.def_of(id) {
            p(d)
        } else if let Some(obj) = state.objects.get(&id) {
            state.catalog.get(obj.catalog_key.as_str()).map_or(false, |d| p(d))
        } else {
            false
        }
    })
}

/// True iff the object has at least one counter of the given type.
pub(crate) fn pred_has_counter(ct: CounterType) -> ObjPredicate {
    std::sync::Arc::new(move |id, state| {
        state.objects.get(&id)
            .map_or(false, |o| o.counters.get(&ct).copied().unwrap_or(0) > 0)
    })
}

/// Logical AND of two obj predicates.
#[allow(dead_code)]
pub(crate) fn cost_pred_and(a: ObjPredicate, b: ObjPredicate) -> ObjPredicate {
    std::sync::Arc::new(move |id, state| a(id, state) && b(id, state))
}

/// Logical OR of two obj predicates.
#[allow(dead_code)]
pub(crate) fn cost_pred_or(a: ObjPredicate, b: ObjPredicate) -> ObjPredicate {
    std::sync::Arc::new(move |id, state| a(id, state) || b(id, state))
}

/// Logical NOT of an obj predicate.
#[allow(dead_code)]
pub(crate) fn cost_pred_not(p: ObjPredicate) -> ObjPredicate {
    std::sync::Arc::new(move |id, state| !p(id, state))
}

/// True iff the object is a land.
#[allow(dead_code)]
pub(crate) fn cost_pred_land() -> ObjPredicate {
    obj_pred_from_card(pred_type_eq(CardType::Land))
}

/// True iff the object is blue and not a land.
pub(crate) fn cost_pred_blue_nonland() -> ObjPredicate {
    obj_pred_from_card(pred_and(pred_has_color(Color::Blue), pred_not(pred_type_eq(CardType::Land))))
}

/// True iff the object is a permanent on the battlefield that is attacking and unblocked.
pub(crate) fn cost_pred_unblocked_attacker() -> ObjPredicate {
    std::sync::Arc::new(|id, state| {
        state.permanent_bf(id).map_or(false, |bf| bf.attacking && bf.unblocked)
    })
}

/// True iff the object is a land with a mana ability that produces blue mana.
pub(crate) fn cost_pred_blue_producing() -> ObjPredicate {
    std::sync::Arc::new(|id, state| {
        state.def_of(id).map_or(false, |d| {
            d.mana_abilities().iter().any(|ma| ma.produces.contains(&Color::Blue))
        })
    })
}

/// Which kind of ability a `TargetSpec::AbilityOnStack` matches.
#[derive(Clone)]
#[allow(dead_code)]
pub(crate) enum AbilityType {
    Any,
    Triggered,
    Activated,
}

/// Declarative description of what targets a spell or ability may choose from.
/// Used both to enumerate legal choices and to re-validate at resolution.
#[derive(Clone)]
pub(crate) enum TargetSpec {
    None,
    /// A specific player (`who` resolved relative to the acting controller).
    Player(Who),
    /// Any game object in `zone` controlled by `controller` matching `filter`.
    /// Covers permanents (Battlefield), spells (Stack), and cards in graveyard/library.
    ObjectInZone { controller: Who, zone: ZoneId, filter: ObjPredicate },
    /// Any one of several sub-specs is a legal target (e.g. "any target" = creature | planeswalker | player).
    Union(Vec<TargetSpec>),
    /// An ability on the stack (Zone=Stack, StackObjectType=Ability) controlled by `controller`,
    /// optionally filtered by `ability_type` (Triggered / Activated / Any).
    /// Abilities don't have a `CardDef` so they can't be reached via `ObjectInZone`.
    AbilityOnStack { controller: Who, ability_type: AbilityType },
}

impl TargetSpec {
    /// Returns true if this spec requires no target (i.e. `TargetSpec::None`).
    pub(crate) fn is_none(&self) -> bool { matches!(self, TargetSpec::None) }
}


/// Pick one target from a list of legal targets using the standard heuristic:
/// prefer a killable creature (tgh - damage <= 1), then planeswalker or player over
/// non-killable creatures, then fall back to the first available target.
pub(crate) fn pick_target(targets: &[ObjId], state: &SimState) -> Option<ObjId> {
    if targets.is_empty() { return None; }
    // Prefer a killable creature
    if let Some(&id) = targets.iter().find(|&&id| {
        let is_creature = state.def_of(id)
            .or_else(|| state.objects.get(&id).and_then(|o| state.catalog.get(o.catalog_key.as_str())))
            .map(|d| d.is_creature()).unwrap_or(false);
        if !is_creature { return false; }
        let tgh = state.def_of(id)
            .or_else(|| state.objects.get(&id).and_then(|o| state.catalog.get(o.catalog_key.as_str())))
            .and_then(|d| d.as_creature()).map(|c| c.toughness()).unwrap_or(1);
        let dmg = state.permanent_bf(id).map(|bf| bf.damage).unwrap_or(0);
        tgh > 0 && tgh - dmg <= 1
    }) {
        return Some(id);
    }
    // Skip non-killable creatures — prefer planeswalker or player over them
    if let Some(&id) = targets.iter().find(|&&id| {
        !state.def_of(id)
            .or_else(|| state.objects.get(&id).and_then(|o| state.catalog.get(o.catalog_key.as_str())))
            .map(|d| d.is_creature()).unwrap_or(false)
    }) {
        return Some(id);
    }
    // Fallback: first target
    Some(targets[0])
}

/// Enumerate all legal targets for `spec` given the current game state.
/// No heuristic — returns every valid option. Caller picks.
pub(crate) fn legal_targets(spec: &TargetSpec, controller: PlayerId, state: &SimState) -> Vec<ObjId> {
    match spec {
        TargetSpec::None => vec![],
        TargetSpec::Player(who) => vec![state.player_id(who.resolve(controller))],
        TargetSpec::ObjectInZone { controller: who, zone, filter } => {
            let target_who = who.resolve(controller);
            objects_in_zone(zone, target_who, state)
                .filter(|&id| {
                    if *zone == ZoneId::Stack {
                        let actor_id = state.player_id(controller);
                        if state.stack_item_owner(id) == actor_id
                            || !state.stack_item_is_counterable(id) { return false; }
                    }
                    filter(id, state)
                })
                .collect()
        }
        TargetSpec::Union(specs) => {
            // Collect all legal targets from all sub-specs, deduplicating by id.
            let mut seen = std::collections::HashSet::new();
            let mut result = Vec::new();
            for sub in specs {
                for id in legal_targets(sub, controller, state) {
                    if seen.insert(id) {
                        result.push(id);
                    }
                }
            }
            result
        }
        TargetSpec::AbilityOnStack { controller: who, ability_type } => {
            let target_who = who.resolve(controller);
            let target_who_id = state.player_id(target_who);
            state.abilities_on_stack()
                .filter(|(_, ab)| {
                    ab.owner == target_who_id && match ability_type {
                        AbilityType::Any       => true,
                        AbilityType::Triggered => ab.is_triggered,
                        AbilityType::Activated => !ab.is_triggered,
                    }
                })
                .map(|(id, _)| id)
                .collect()
        }
    }
}

/// Return true if at least one valid target exists for `spec`.
/// For stack targets, checks the current stack for opposing non-ability spells.
/// For permanent/zone targets, checks the battlefield or zone.
/// Returns false for `TargetSpec::None` (no target required = always valid; caller should check `is_none()` first).
pub(crate) fn has_valid_target(
    spec: &TargetSpec,
    state: &SimState,
    actor: PlayerId,
) -> bool {
    has_valid_target_spec(spec, state, actor)
}

fn has_valid_target_spec(
    spec: &TargetSpec,
    state: &SimState,
    actor: PlayerId,
) -> bool {
    match spec {
        TargetSpec::None => false,
        TargetSpec::Player(_) => true,   // there is always an opponent
        TargetSpec::ObjectInZone { controller: who, zone, filter } => {
            let target_who = who.resolve(actor);
            objects_in_zone(zone, target_who, state)
                .any(|id| {
                    if *zone == ZoneId::Stack {
                        let actor_id = state.player_id(actor);
                        if state.stack_item_owner(id) == actor_id
                            || !state.stack_item_is_counterable(id) { return false; }
                    }
                    filter(id, state)
                })
        }
        TargetSpec::Union(specs) => specs.iter().any(|s| has_valid_target_spec(s, state, actor)),
        TargetSpec::AbilityOnStack { controller: who, ability_type } => {
            let target_who = who.resolve(actor);
            let target_who_id = state.player_id(target_who);
            state.abilities_on_stack()
                .any(|(_, ab)| {
                    ab.owner == target_who_id && match ability_type {
                        AbilityType::Any       => true,
                        AbilityType::Triggered => ab.is_triggered,
                        AbilityType::Activated => !ab.is_triggered,
                    }
                })
        }
    }
}



/// Iterate over ObjIds in the given zone controlled (or owned) by `who`.
fn objects_in_zone<'a>(
    zone: &ZoneId,
    who: PlayerId,
    state: &'a SimState,
) -> impl Iterator<Item = ObjId> + 'a {
    let zone_card = match zone {
        ZoneId::Battlefield => CardZone::Battlefield,
        ZoneId::Graveyard   => CardZone::Graveyard,
        ZoneId::Stack       => CardZone::Stack,
        ZoneId::Library     => CardZone::Library,
        ZoneId::Exile       => CardZone::Exile { on_adventure: false },
        ZoneId::Hand        => CardZone::Hand { known: false },
    };
    state.objects.values()
        .filter(move |o| {
            let zone_match = match &o.zone {
                CardZone::Hand { .. } => matches!(zone_card, CardZone::Hand { .. }),
                z => z == &zone_card,
            };
            zone_match && (o.controller == who || o.owner == who)
        })
        .map(|o| o.id)
}

