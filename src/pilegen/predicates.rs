use super::*;

// ── CardPredicate ─────────────────────────────────────────────────────────────

/// A composable predicate over a `CardDef`. Used to express targeting filters
/// without string dispatch.
pub(crate) type CardPredicate = std::sync::Arc<dyn Fn(&CardDef) -> bool + Send + Sync>;

/// Always returns true.
pub(crate) fn pred_any() -> CardPredicate {
    std::sync::Arc::new(|_| true)
}

/// Always returns false.
pub(crate) fn pred_none() -> CardPredicate {
    std::sync::Arc::new(|_| false)
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

/// A concrete, resolved reference to a game object that can be targeted.
/// `id` fields default to `ObjId::UNSET` for now; they'll be filled in as objects get IDs.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Target {
    Player(ObjId),
    Object(ObjId),
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
    ObjectInZone { controller: Who, zone: ZoneId, filter: CardPredicate },
    /// Any one of several sub-specs is a legal target (e.g. "any target" = creature | planeswalker | player).
    Union(Vec<TargetSpec>),
}

/// Build a `CardPredicate` from a permanent filter string (e.g. `"creature"`, `"nonbasic_land"`).
fn permanent_pred_from_str(filter: &str) -> CardPredicate {
    match filter {
        "any"             => pred_any(),
        "land"            => pred_type_eq(CardType::Land),
        "nonbasic_land"   => pred_and(pred_type_eq(CardType::Land), pred_not(pred_has_supertype(Supertype::Basic))),
        "creature"        => pred_type_eq(CardType::Creature),
        "planeswalker"    => pred_type_eq(CardType::Planeswalker),
        "artifact"        => pred_type_eq(CardType::Artifact),
        "nonland"         => pred_not(pred_type_eq(CardType::Land)),
        "permanent_nonland" => pred_not(pred_type_eq(CardType::Land)),
        "creature_mv_lt4" => pred_and(pred_type_eq(CardType::Creature), pred_mana_value_le(3)),
        "creature_nonblack" => pred_and(pred_type_eq(CardType::Creature), pred_not(pred_has_color(Color::Black))),
        _                 => pred_none(),
    }
}

/// Build a `CardPredicate` from a stack entry filter string (e.g. `"any"`, `"instant_or_sorcery"`).
pub(crate) fn stack_pred_from_str(filter: &str) -> CardPredicate {
    match filter {
        "any"                => pred_any(),
        "noncreature"        => pred_not(pred_type_eq(CardType::Creature)),
        "nonland"            => pred_not(pred_type_eq(CardType::Land)),
        "instant_or_sorcery" => pred_or(pred_type_eq(CardType::Instant), pred_type_eq(CardType::Sorcery)),
        _                    => pred_none(),
    }
}

/// Build a `CardPredicate` for graveyard/hand zone filter strings.
pub(crate) fn zone_pred_from_str(filter: &str) -> CardPredicate {
    match filter {
        "" | "any" => pred_any(),
        other      => permanent_pred_from_str(other),
    }
}

/// Parse a TOML target string into a `TargetSpec`.
pub(crate) fn target_spec_from_str(target: Option<&str>) -> TargetSpec {
    let Some(s) = target else { return TargetSpec::None; };
    if let Some(filter) = s.strip_prefix("stack:") {
        return TargetSpec::ObjectInZone {
            controller: Who::Opp,
            zone: ZoneId::Stack,
            filter: stack_pred_from_str(filter),
        };
    }
    if let Some(rest) = s.strip_prefix("self:gy:") {
        return TargetSpec::ObjectInZone {
            controller: Who::Actor,
            zone: ZoneId::Graveyard,
            filter: zone_pred_from_str(rest),
        };
    }
    if let Some(filter) = s.strip_prefix("opp:") {
        return TargetSpec::ObjectInZone {
            controller: Who::Opp,
            zone: ZoneId::Battlefield,
            filter: permanent_pred_from_str(filter),
        };
    }
    if s == "any_target" {
        // "Any target" = creature permanent | planeswalker permanent | player.
        return TargetSpec::Union(vec![
            TargetSpec::ObjectInZone {
                controller: Who::Opp,
                zone: ZoneId::Battlefield,
                filter: pred_type_eq(CardType::Creature),
            },
            TargetSpec::ObjectInZone {
                controller: Who::Opp,
                zone: ZoneId::Battlefield,
                filter: pred_type_eq(CardType::Planeswalker),
            },
            TargetSpec::Player(Who::Opp),
        ]);
    }
    TargetSpec::None
}

/// Pick one target from a list of legal targets using the standard heuristic:
/// prefer a killable creature (tgh - damage <= 1), then planeswalker or player over
/// non-killable creatures, then fall back to the first available target.
pub(crate) fn pick_target(targets: &[Target], state: &SimState) -> Option<Target> {
    if targets.is_empty() { return None; }
    // Prefer a killable creature
    if let Some(&Target::Object(id)) = targets.iter().find(|t| {
        let Target::Object(id) = t else { return false; };
        let is_creature = state.def_of(*id)
            .or_else(|| state.objects.get(id).and_then(|o| state.catalog.get(o.catalog_key.as_str())))
            .map(|d| d.is_creature()).unwrap_or(false);
        if !is_creature { return false; }
        let tgh = state.def_of(*id)
            .or_else(|| state.objects.get(id).and_then(|o| state.catalog.get(o.catalog_key.as_str())))
            .and_then(|d| d.as_creature()).map(|c| c.toughness()).unwrap_or(1);
        let dmg = state.permanent_bf(*id).map(|bf| bf.damage).unwrap_or(0);
        tgh > 0 && tgh - dmg <= 1
    }) {
        return Some(Target::Object(id));
    }
    // Skip non-killable creatures — prefer planeswalker or player over them
    if let Some(t) = targets.iter().find(|t| {
        !matches!(t, Target::Object(id) if
            state.def_of(*id)
                .or_else(|| state.objects.get(id).and_then(|o| state.catalog.get(o.catalog_key.as_str())))
                .map(|d| d.is_creature()).unwrap_or(false))
    }) {
        return Some(t.clone());
    }
    // Fallback: first target
    Some(targets[0].clone())
}

/// Enumerate all legal targets for `spec` given the current game state.
/// No heuristic — returns every valid option. Caller picks.
pub(crate) fn legal_targets(spec: &TargetSpec, controller: &str, state: &SimState) -> Vec<Target> {
    match spec {
        TargetSpec::None => vec![],
        TargetSpec::Player(who) => vec![Target::Player(state.player_id(who.resolve(controller)))],
        TargetSpec::ObjectInZone { controller: who, zone, filter } => {
            let target_who = who.resolve(controller);
            objects_in_zone(zone, target_who, state)
                .filter(|&id| {
                    if *zone == ZoneId::Stack {
                        let actor_id = state.player_id(controller);
                        if state.stack_item_owner(id) == actor_id
                            || !state.stack_item_is_counterable(id) { return false; }
                    }
                    state.def_of(id)
                        .or_else(|| state.objects.get(&id)
                            .and_then(|o| state.catalog.get(o.catalog_key.as_str())))
                        .map(|d| filter(d))
                        .unwrap_or(false)
                })
                .map(Target::Object)
                .collect()
        }
        TargetSpec::Union(specs) => {
            // Collect all legal targets from all sub-specs, deduplicating by value.
            let mut seen = std::collections::HashSet::new();
            let mut result = Vec::new();
            for sub in specs {
                for t in legal_targets(sub, controller, state) {
                    if seen.insert(format!("{t:?}")) {
                        result.push(t);
                    }
                }
            }
            result
        }
    }
}

/// Return true if at least one valid target exists for `target_str`.
/// For `"stack:<filter>"` targets, checks the current stack for opposing non-ability spells.
/// For permanent/zone targets, checks the battlefield or zone.
pub(crate) fn has_valid_target(
    target_str: &str,
    state: &SimState,
    actor: &str,
) -> bool {
    has_valid_target_spec(&target_spec_from_str(Some(target_str)), state, actor)
}

fn has_valid_target_spec(
    spec: &TargetSpec,
    state: &SimState,
    actor: &str,
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
                    state.def_of(id).map(|d| filter(d)).unwrap_or(false)
                })
        }
        TargetSpec::Union(specs) => specs.iter().any(|s| has_valid_target_spec(s, state, actor)),
    }
}


/// Build a `CardPredicate` from a library search filter token.
///
/// Token syntax (all parsed at load time — no runtime string dispatch):
/// - `"land"` → type == Land
/// - `"land-island"` / `"land-swamp"` etc. → type == Land AND has that basic land subtype
/// - `"land-island|swamp"` → type == Land AND (island OR swamp)
/// - `"sorcery"` → type == Sorcery
/// - `"instant"` → type == Instant
/// - `"creature"` → type == Creature
/// - `"creature-green"` → type == Creature AND color contains Green
/// - `"artifact"` → type == Artifact
/// - `"artifact-cost01"` → type == Artifact AND no colored pips AND mana value ≤ 1
pub(crate) fn search_filter_pred(filter: &str) -> CardPredicate {
    // Simple type tokens
    match filter {
        "land"            => return pred_type_eq(CardType::Land),
        "sorcery"         => return pred_type_eq(CardType::Sorcery),
        "instant"         => return pred_type_eq(CardType::Instant),
        "creature"        => return pred_type_eq(CardType::Creature),
        "artifact"        => return pred_type_eq(CardType::Artifact),
        "artifact-cost01" => return pred_and(
            pred_type_eq(CardType::Artifact),
            pred_and(pred_no_colored_pips(), pred_mana_value_le(1)),
        ),
        _ => {}
    }
    // "land-<subtype>" and "land-<subtype>|<subtype>" patterns
    if let Some(types_str) = filter.strip_prefix("land-") {
        let subtypes: Vec<&str> = types_str.split('|').collect();
        let mut pred: CardPredicate = pred_none();
        for subtype in subtypes {
            let p = match subtype {
                "island"   => pred_land_subtype("island"),
                "swamp"    => pred_land_subtype("swamp"),
                "plains"   => pred_land_subtype("plains"),
                "mountain" => pred_land_subtype("mountain"),
                "forest"   => pred_land_subtype("forest"),
                _          => pred_none(),
            };
            pred = pred_or(pred, p);
        }
        return pred_and(pred_type_eq(CardType::Land), pred);
    }
    // "creature-<color>" patterns
    if let Some(color_str) = filter.strip_prefix("creature-") {
        let color_pred = match color_str {
            "white" => pred_has_color(Color::White),
            "blue"  => pred_has_color(Color::Blue),
            "black" => pred_has_color(Color::Black),
            "red"   => pred_has_color(Color::Red),
            "green" => pred_has_color(Color::Green),
            _       => pred_none(),
        };
        return pred_and(pred_type_eq(CardType::Creature), color_pred);
    }
    pred_none()
}

/// Iterate over ObjIds in the given zone controlled (or owned) by `who`.
fn objects_in_zone<'a>(
    zone: &ZoneId,
    who: &'a str,
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
    let who = who.to_string();
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

