use std::collections::HashMap;
use std::sync::Arc;
use super::*;

// ── Public API ────────────────────────────────────────────────────────────────

/// Build the full card catalog used by the simulation engine.
pub(crate) fn build_catalog() -> HashMap<String, CardDef> {
    all_cards().into_iter().map(|c| (c.name.clone(), c)).collect()
}

fn all_cards() -> Vec<CardDef> {
    vec![
        // Lands — basics
        island(),
        swamp(),
        plains(),
        mountain(),
        forest(),
        wastes(),
        snow_covered_island(),
        snow_covered_swamp(),
        snow_covered_plains(),
        snow_covered_mountain(),
        snow_covered_forest(),
        snow_covered_wastes(),
        // Lands — ABU duals
        underground_sea(),
        tundra(),
        badlands(),
        taiga(),
        savannah(),
        scrubland(),
        volcanic_island(),
        bayou(),
        plateau(),
        tropical_island(),
        // Lands — MKM surveil duals (enter tapped)
        undercity_sewers(),
        meticulous_archive(),
        raucous_theater(),
        hedge_maze(),
        commercial_district(),
        lush_portico(),
        thundering_falls(),
        underground_mortuary(),
        elegant_parlor(),
        shadowy_backstreet(),
        // Lands — fetches
        polluted_delta(),
        flooded_strand(),
        misty_rainforest(),
        scalding_tarn(),
        marsh_flats(),
        bloodstained_mire(),
        windswept_heath(),
        wooded_foothills(),
        verdant_catacombs(),
        arid_mesa(),
        // Lands — other
        wasteland(),
        karakas(),
        ancient_tomb(),
        cavern_of_souls(),
        // Artifacts
        lotus_petal(),
        lions_eye_diamond(),
        ursas_saga(),
        engineered_explosives(),
        grafdiggers_cage(),
        // Spells — instants
        brainstorm(),
        consider(),
        daze(),
        force_of_negation(),
        force_of_will(),
        dark_ritual(),
        fatal_push(),
        snuff_out(),
        swords_to_plowshares(),
        bitter_triumph(),
        long_goodbye(),
        consign_to_memory(),
        surgical_extraction(),
        abrade(),
        red_elemental_blast(),
        pyroblast(),
        blue_elemental_blast(),
        hydroblast(),
        sheoldreds_edict(),
        spell_pierce(),
        flusterstorm(),
        // Spells — sorceries
        toxic_deluge(),
        doomsday(),
        stock_up(),
        ponder(),
        thoughtseize(),
        unearth(),
        hymn_to_tourach(),
        edge_of_autumn(),
        personal_tutor(),
        green_suns_zenith(),
        show_and_tell(),
        // Creatures
        thassas_oracle(),
        street_wraith(),
        barrowgoyf(),
        ingenious_infiltrator(),
        kaito_bane_of_nightmares(),
        recruiter_of_the_guard(),
        orcish_bowmasters(),
        murktide_regent(),
        dauthi_voidwalker(),
        lavinia_azorius_renegade(),
        hexing_squelcher(),
        simian_spirit_guide(),
        // DFCs / split
        tamiyo_inquisitive_student(),
        brazen_borrower(),
        // Opponent archetypes / hate cards
        painters_servant(),
        leyline_of_the_void(),
        disruptor_flute(),
        // Tokens
        orc_army_token(),
        clue_token(),
    ]
}

// ── Local helpers ─────────────────────────────────────────────────────────────

/// `CardDef` with no supertypes, normal layout, no back, no triggers/replacements/statics.
fn simple(name: &str, kind: CardKind, colors: Vec<Color>, play_weight: Option<u32>) -> CardDef {
    CardDef::new(
        name, kind, colors, play_weight,
        vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![],
    )
}

fn color_to_mana_char(c: Color) -> &'static str {
    match c {
        Color::White => "W", Color::Blue => "U", Color::Black => "B",
        Color::Red => "R", Color::Green => "G",
    }
}

/// `ManaAbility` that taps self and produces the given mana string (e.g. `"U"`, `"B"`).
fn tap_produces(s: &str) -> ManaAbility {
    let s_owned = s.to_string();
    ManaAbility {
        source_zone: SourceZone::Battlefield,
        costs: vec![CostComponent::TapSelf],
        produces: produces_colors(s),
        produces_count: 1,
        make_effect: std::sync::Arc::new(move |who, _color| eff_mana(who, s_owned.clone())),
    }
}

/// `AbilityDef` for a fetch land: sacrifice self, pay 1 life, search → Battlefield.
fn fetch_ability(pred: CardPredicate) -> AbilityDef {
    AbilityDef {
        costs: vec![CostComponent::SacSelf, CostComponent::Life(1)],
        ability_factory: Some(Arc::new(move |who, _| {
            eff_fetch_search(who, pred.clone(), ZoneId::Battlefield)
        })),
        ..Default::default()
    }
}

/// Basic land (Island, Swamp, Plains, Mountain, Forest, Wastes).
fn basic_land(name: &str, land_types: LandTypes, mana: &str) -> CardDef {
    CardDef::new(
        name, CardKind::Land(LandData {
            land_types,
            mana_abilities: vec![tap_produces(mana)],
            ..Default::default()
        }),
        vec![], Some(25), vec![Supertype::Basic], CardLayout::Normal, None,
        vec![], vec![], vec![], vec![],
    )
}

/// Basic snow land (Snow-Covered X).
fn snow_basic(name: &str, land_types: LandTypes, mana: &str) -> CardDef {
    CardDef::new(
        name, CardKind::Land(LandData {
            land_types,
            mana_abilities: vec![tap_produces(mana)],
            ..Default::default()
        }),
        vec![], Some(25), vec![Supertype::Basic, Supertype::Snow], CardLayout::Normal, None,
        vec![], vec![], vec![], vec![],
    )
}

/// Dual land that always enters tapped (surveil lands, etc.).
/// MKM-style surveil dual: always enters tapped, triggers surveil 1 on ETB.
fn surveil_dual(name: &'static str, land_types: LandTypes, c1: &str, c2: &str) -> CardDef {
    let trigger = etb_self_trigger(name, TargetSpec::None, move |_, controller| {
        eff_surveil(controller, 1)
    });
    CardDef::new(
        name,
        CardKind::Land(LandData {
            land_types,
            mana_abilities: vec![tap_produces(c1), tap_produces(c2)],
            ..Default::default()
        }),
        vec![], None, vec![], CardLayout::Normal, None,
        vec![trigger],
        vec![replacement_enters_tapped()],
        vec![],
        vec![],
    )
}

// ── Lands ─────────────────────────────────────────────────────────────────────

fn underground_sea() -> CardDef {
    simple("Underground Sea", CardKind::Land(LandData {
        land_types: LandTypes { island: true, swamp: true, ..Default::default() },
        mana_abilities: vec![tap_produces("U"), tap_produces("B")],
        ..Default::default()
    }), vec![], None)
}

fn tundra() -> CardDef {
    simple("Tundra", CardKind::Land(LandData {
        land_types: LandTypes { plains: true, island: true, ..Default::default() },
        mana_abilities: vec![tap_produces("W"), tap_produces("U")],
        ..Default::default()
    }), vec![], None)
}

fn badlands() -> CardDef {
    simple("Badlands", CardKind::Land(LandData {
        land_types: LandTypes { swamp: true, mountain: true, ..Default::default() },
        mana_abilities: vec![tap_produces("B"), tap_produces("R")],
        ..Default::default()
    }), vec![], None)
}

fn taiga() -> CardDef {
    simple("Taiga", CardKind::Land(LandData {
        land_types: LandTypes { mountain: true, forest: true, ..Default::default() },
        mana_abilities: vec![tap_produces("R"), tap_produces("G")],
        ..Default::default()
    }), vec![], None)
}

fn savannah() -> CardDef {
    simple("Savannah", CardKind::Land(LandData {
        land_types: LandTypes { forest: true, plains: true, ..Default::default() },
        mana_abilities: vec![tap_produces("G"), tap_produces("W")],
        ..Default::default()
    }), vec![], None)
}

fn scrubland() -> CardDef {
    simple("Scrubland", CardKind::Land(LandData {
        land_types: LandTypes { plains: true, swamp: true, ..Default::default() },
        mana_abilities: vec![tap_produces("W"), tap_produces("B")],
        ..Default::default()
    }), vec![], None)
}

fn volcanic_island() -> CardDef {
    simple("Volcanic Island", CardKind::Land(LandData {
        land_types: LandTypes { island: true, mountain: true, ..Default::default() },
        mana_abilities: vec![tap_produces("U"), tap_produces("R")],
        ..Default::default()
    }), vec![], None)
}

fn bayou() -> CardDef {
    simple("Bayou", CardKind::Land(LandData {
        land_types: LandTypes { swamp: true, forest: true, ..Default::default() },
        mana_abilities: vec![tap_produces("B"), tap_produces("G")],
        ..Default::default()
    }), vec![], None)
}

fn plateau() -> CardDef {
    simple("Plateau", CardKind::Land(LandData {
        land_types: LandTypes { mountain: true, plains: true, ..Default::default() },
        mana_abilities: vec![tap_produces("R"), tap_produces("W")],
        ..Default::default()
    }), vec![], None)
}

fn tropical_island() -> CardDef {
    simple("Tropical Island", CardKind::Land(LandData {
        land_types: LandTypes { forest: true, island: true, ..Default::default() },
        mana_abilities: vec![tap_produces("G"), tap_produces("U")],
        ..Default::default()
    }), vec![], None)
}

fn swamp() -> CardDef {
    CardDef::new(
        "Swamp",
        CardKind::Land(LandData {
            land_types: LandTypes { swamp: true, ..Default::default() },
            mana_abilities: vec![tap_produces("B")],
            ..Default::default()
        }),
        vec![], Some(25), vec![Supertype::Basic], CardLayout::Normal, None,
        vec![], vec![], vec![], vec![],
    )
}

fn island() -> CardDef {
    CardDef::new(
        "Island",
        CardKind::Land(LandData {
            land_types: LandTypes { island: true, ..Default::default() },
            mana_abilities: vec![tap_produces("U")],
            ..Default::default()
        }),
        vec![], Some(25), vec![Supertype::Basic], CardLayout::Normal, None,
        vec![], vec![], vec![], vec![],
    )
}

fn plains() -> CardDef {
    basic_land("Plains", LandTypes { plains: true, ..Default::default() }, "W")
}

fn mountain() -> CardDef {
    basic_land("Mountain", LandTypes { mountain: true, ..Default::default() }, "R")
}

fn forest() -> CardDef {
    basic_land("Forest", LandTypes { forest: true, ..Default::default() }, "G")
}

/// Wastes: basic land with no subtype, produces {C}.
fn wastes() -> CardDef {
    basic_land("Wastes", LandTypes::default(), "")
}

fn snow_covered_island() -> CardDef {
    snow_basic("Snow-Covered Island", LandTypes { island: true, ..Default::default() }, "U")
}

fn snow_covered_swamp() -> CardDef {
    snow_basic("Snow-Covered Swamp", LandTypes { swamp: true, ..Default::default() }, "B")
}

fn snow_covered_plains() -> CardDef {
    snow_basic("Snow-Covered Plains", LandTypes { plains: true, ..Default::default() }, "W")
}

fn snow_covered_mountain() -> CardDef {
    snow_basic("Snow-Covered Mountain", LandTypes { mountain: true, ..Default::default() }, "R")
}

fn snow_covered_forest() -> CardDef {
    snow_basic("Snow-Covered Forest", LandTypes { forest: true, ..Default::default() }, "G")
}

fn snow_covered_wastes() -> CardDef {
    CardDef::new(
        "Snow-Covered Wastes",
        CardKind::Land(LandData {
            land_types: LandTypes::default(),
            mana_abilities: vec![tap_produces("")],
            ..Default::default()
        }),
        vec![], Some(25), vec![Supertype::Basic, Supertype::Snow], CardLayout::Normal, None,
        vec![], vec![], vec![], vec![],
    )
}

/// Enters tapped. CR 614.1 (replacement effect): replaces the ETB event to set tapped=true.
// ── MKM surveil lands (always enter tapped; surveil 1 on ETB) ─────────────────

fn undercity_sewers()     -> CardDef { surveil_dual("Undercity Sewers",     LandTypes { island: true,   swamp: true,    ..Default::default() }, "U", "B") }
fn meticulous_archive()   -> CardDef { surveil_dual("Meticulous Archive",   LandTypes { plains: true,   island: true,   ..Default::default() }, "W", "U") }
fn raucous_theater()      -> CardDef { surveil_dual("Raucous Theater",      LandTypes { swamp: true,    mountain: true, ..Default::default() }, "B", "R") }
fn hedge_maze()           -> CardDef { surveil_dual("Hedge Maze",           LandTypes { mountain: true, forest: true,   ..Default::default() }, "R", "G") }
fn commercial_district()  -> CardDef { surveil_dual("Commercial District",  LandTypes { forest: true,   plains: true,   ..Default::default() }, "G", "W") }
fn lush_portico()         -> CardDef { surveil_dual("Lush Portico",         LandTypes { plains: true,   forest: true,   ..Default::default() }, "W", "G") }
fn thundering_falls()     -> CardDef { surveil_dual("Thundering Falls",     LandTypes { island: true,   mountain: true, ..Default::default() }, "U", "R") }
fn underground_mortuary() -> CardDef { surveil_dual("Underground Mortuary", LandTypes { swamp: true,    forest: true,   ..Default::default() }, "B", "G") }
fn elegant_parlor()       -> CardDef { surveil_dual("Elegant Parlor",       LandTypes { mountain: true, plains: true,   ..Default::default() }, "R", "W") }
fn shadowy_backstreet()   -> CardDef { surveil_dual("Shadowy Backstreet",   LandTypes { plains: true,   swamp: true,    ..Default::default() }, "W", "B") }

/// {T}, Sacrifice: destroy target nonbasic land. CR 701.7.
fn wasteland() -> CardDef {
    simple("Wasteland", CardKind::Land(LandData {
        abilities: vec![AbilityDef {
            costs: vec![CostComponent::TapSelf, CostComponent::SacSelf],
            target_spec: TargetSpec::ObjectInZone {
                controller: Who::Opp,
                zone: ZoneId::Battlefield,
                filter: obj_pred_from_card(pred_and(pred_type_eq(CardType::Land), pred_not(pred_has_supertype(Supertype::Basic)))),
            },
            ability_factory: Some(Arc::new(|who, _| eff_destroy_target(who))),
            ..Default::default()
        }],
        ..Default::default()
    }), vec![], None)
}

fn karakas() -> CardDef {
    let legend_creature = obj_pred_from_card(pred_and(
        pred_type_eq(CardType::Creature),
        pred_has_supertype(Supertype::Legendary),
    ));
    CardDef::new(
        "Karakas",
        CardKind::Land(LandData {
            mana_abilities: vec![tap_produces("W")],
            abilities: vec![AbilityDef {
                costs: vec![CostComponent::TapSelf],
                target_spec: TargetSpec::Union(vec![
                    TargetSpec::ObjectInZone {
                        controller: Who::Actor,
                        zone: ZoneId::Battlefield,
                        filter: legend_creature.clone(),
                    },
                    TargetSpec::ObjectInZone {
                        controller: Who::Opp,
                        zone: ZoneId::Battlefield,
                        filter: legend_creature,
                    },
                ]),
                ability_factory: Some(Arc::new(|who, _| eff_bounce_target(who))),
                ..Default::default()
            }],
            ..Default::default()
        }),
        vec![], None, vec![Supertype::Legendary], CardLayout::Normal, None,
        vec![], vec![], vec![], vec![],
    )
}

fn ancient_tomb() -> CardDef {
    simple("Ancient Tomb", CardKind::Land(LandData {
        mana_abilities: vec![ManaAbility {
            source_zone: SourceZone::Battlefield,
            costs: vec![CostComponent::TapSelf],
            produces: vec![],      // colorless
            produces_count: 2,
            make_effect: std::sync::Arc::new(|who, _| {
                eff_mana(who, "CC").then(eff_life_loss(who, 2))
            }),
        }],
        ..Default::default()
    }), vec![], None)
}

fn polluted_delta() -> CardDef {
    simple("Polluted Delta", CardKind::Land(LandData {
        abilities: vec![fetch_ability(pred_and(
            pred_type_eq(CardType::Land),
            pred_or(pred_land_subtype("island"), pred_land_subtype("swamp")),
        ))],
        ..Default::default()
    }), vec![], Some(25))
}

fn flooded_strand() -> CardDef {
    simple("Flooded Strand", CardKind::Land(LandData {
        abilities: vec![fetch_ability(pred_and(
            pred_type_eq(CardType::Land),
            pred_land_subtype("island"),
        ))],
        ..Default::default()
    }), vec![], Some(25))
}

fn misty_rainforest() -> CardDef {
    simple("Misty Rainforest", CardKind::Land(LandData {
        abilities: vec![fetch_ability(pred_and(
            pred_type_eq(CardType::Land),
            pred_land_subtype("island"),
        ))],
        ..Default::default()
    }), vec![], Some(25))
}

fn scalding_tarn() -> CardDef {
    simple("Scalding Tarn", CardKind::Land(LandData {
        abilities: vec![fetch_ability(pred_and(
            pred_type_eq(CardType::Land),
            pred_land_subtype("island"),
        ))],
        ..Default::default()
    }), vec![], Some(25))
}

fn marsh_flats() -> CardDef {
    simple("Marsh Flats", CardKind::Land(LandData {
        abilities: vec![fetch_ability(pred_and(
            pred_type_eq(CardType::Land),
            pred_land_subtype("swamp"),
        ))],
        ..Default::default()
    }), vec![], Some(25))
}

fn bloodstained_mire() -> CardDef {
    simple("Bloodstained Mire", CardKind::Land(LandData {
        abilities: vec![fetch_ability(pred_and(
            pred_type_eq(CardType::Land),
            pred_land_subtype("swamp"),
        ))],
        ..Default::default()
    }), vec![], Some(25))
}

fn windswept_heath() -> CardDef {
    simple("Windswept Heath", CardKind::Land(LandData {
        abilities: vec![fetch_ability(pred_and(
            pred_type_eq(CardType::Land),
            pred_or(pred_land_subtype("forest"), pred_land_subtype("plains")),
        ))],
        ..Default::default()
    }), vec![], Some(25))
}

fn wooded_foothills() -> CardDef {
    simple("Wooded Foothills", CardKind::Land(LandData {
        abilities: vec![fetch_ability(pred_and(
            pred_type_eq(CardType::Land),
            pred_or(pred_land_subtype("forest"), pred_land_subtype("mountain")),
        ))],
        ..Default::default()
    }), vec![], Some(25))
}

fn verdant_catacombs() -> CardDef {
    simple("Verdant Catacombs", CardKind::Land(LandData {
        abilities: vec![fetch_ability(pred_and(
            pred_type_eq(CardType::Land),
            pred_or(pred_land_subtype("forest"), pred_land_subtype("swamp")),
        ))],
        ..Default::default()
    }), vec![], Some(25))
}

fn arid_mesa() -> CardDef {
    simple("Arid Mesa", CardKind::Land(LandData {
        abilities: vec![fetch_ability(pred_and(
            pred_type_eq(CardType::Land),
            pred_or(pred_land_subtype("plains"), pred_land_subtype("mountain")),
        ))],
        ..Default::default()
    }), vec![], Some(25))
}

/// Produces generic mana only (no colored pips). CR 106.
/// Legendary land. ETB: choose a creature type (logged, used for future uncounterable modeling).
/// {T}: Add {C}.
/// {T}: Add one mana of any color (TODO: restrict to spells of the named type; mana is uncounterable).
fn cavern_of_souls() -> CardDef {
    let repl = etb_self_replacement(|source_id, id, controller, state, t| {
        let f = Arc::clone(&state.resolve_choice);
        let ChoiceResult::CreatureType(chosen_type) =
            f(source_id, &ChoiceRequest::CreatureType, state) else { return };
        if let Some(bf) = state.permanent_bf_mut(id) {
            bf.etb_choice = Some(ChoiceResult::CreatureType(chosen_type.clone()));
        }
        state.log(t, controller, format!("Cavern of Souls names \"{}\"", chosen_type));
    });
    CardDef::new(
        "Cavern of Souls",
        CardKind::Land(LandData {
            // {T}: Add {C} — colorless
            // {T}: Add one mana of any color (type restriction and uncounterable not yet modeled)
            mana_abilities: vec![
                ManaAbility {
                    source_zone: SourceZone::Battlefield,
                    costs: vec![CostComponent::TapSelf],
                    produces: vec![],
                    produces_count: 1,
                    make_effect: std::sync::Arc::new(|who, _| eff_mana(who, "C")),
                },
                ManaAbility {
                    source_zone: SourceZone::Battlefield,
                    costs: vec![CostComponent::TapSelf],
                    produces: produces_colors("WUBRG"),
                    produces_count: 1,
                    make_effect: std::sync::Arc::new(|who, color| {
                        eff_mana(who, color.map(color_to_mana_char).unwrap_or("1"))
                    }),
                },
            ],
            ..Default::default()
        }),
        vec![],
        Some(50),
        vec![Supertype::Legendary], CardLayout::Normal, None,
        vec![],
        vec![repl],
        vec![],
        vec![],
    )
}

// ── Artifacts ─────────────────────────────────────────────────────────────────

/// Sacrifice: add one mana of any color. CR 106.3.
fn lotus_petal() -> CardDef {
    simple("Lotus Petal", CardKind::Artifact(ArtifactData {
        mana_cost: "0".to_string(),
        mana_abilities: vec![ManaAbility {
            source_zone: SourceZone::Battlefield,
            costs: vec![CostComponent::SacSelf],
            produces: produces_colors("WUBRG"),
            produces_count: 1,
            make_effect: std::sync::Arc::new(|who, color| {
                eff_mana(who, color.map(color_to_mana_char).unwrap_or("1"))
            }),
        }],
        ..Default::default()
    }), vec![], Some(25))
}

/// Mana cost {0}. Produces BBB when sacrificed with a spell on the stack.
/// The sacrifice-to-activate mechanic is handled by strategy; not modeled as an ability here.
fn lions_eye_diamond() -> CardDef {
    simple("Lion's Eye Diamond", CardKind::Artifact(ArtifactData {
        mana_cost: "0".to_string(),
        ..Default::default()
    }), vec![], Some(10))
}

/// Chapter III ability: search for an artifact with no colored pips and MV ≤ 1.
/// Full chapter/saga trigger system is future work; modeled as a sacrifice-self activated ability.
fn ursas_saga() -> CardDef {
    let pred = pred_and(
        pred_type_eq(CardType::Artifact),
        pred_and(pred_no_colored_pips(), pred_mana_value_le(1)),
    );
    simple("Urza's Saga", CardKind::Artifact(ArtifactData {
        mana_cost: String::new(),
        abilities: vec![AbilityDef {
            costs: vec![CostComponent::SacSelf],
            ability_factory: Some(Arc::new(move |who, _| {
                eff_fetch_search(who, pred.clone(), ZoneId::Battlefield)
            })),
            ..Default::default()
        }],
        ..Default::default()
    }), vec![], None)
}

/// {1}. Static: creature cards in graveyards and libraries can't enter the battlefield.
/// Players can't cast spells from graveyards or libraries.
/// Modeled as a CE that sets `blocks_gy_and_lib_access = true` on the Cage itself while it's on the
/// Two static effects while on the battlefield:
///   (a) CR 614.17 prohibition: creature cards from graveyards/libraries can't enter the BF.
///       Implemented as a `ProhibitionDef` — checked in `fire_event` before replacements.
///   (b) CE sets `blocks_gy_and_lib_access = true` on Cage's own materialized def, gating
///       `play_free_cast` so spells can't be cast from graveyards or libraries either.
/// Sunburst: enters with a charge counter for each distinct color of mana spent to cast it.
/// Modeled via `CostComponent::XMana` (strategy declares the intended distinct-color count
/// as chosen_x; the engine pays that many generic mana). The ETB replacement reads
/// `resolving_costs_ctx.chosen_x` and places that many Charge counters.
/// {2}, Sacrifice: destroy each nonland permanent with MV equal to the charge count.
/// CR 702.43 sunburst, CR 701.7 destroy.
fn engineered_explosives() -> CardDef {
    let mut def = CardDef::new(
        "Engineered Explosives",
        CardKind::Artifact(ArtifactData {
            mana_cost: "0".to_string(),
            abilities: vec![AbilityDef {
                source_zone: SourceZone::Battlefield,
                costs: vec![
                    CostComponent::Mana(parse_mana_cost("2")),
                    CostComponent::SacSelf,
                ],
                ability_factory: Some(Arc::new(|who, source_id| {
                    Effect(Arc::new(move |state, t, _targets| {
                        // EE has been sacrificed; zone-independent counters persist in objects map.
                        let n = state.objects.get(&source_id)
                            .and_then(|o| o.counters.get(&CounterType::Charge))
                            .copied()
                            .unwrap_or(0) as i32;
                        let filter = obj_pred_from_card(
                            pred_and(pred_not(pred_type_eq(CardType::Land)), pred_mana_value_eq(n)),
                        );
                        state.log(t, who, format!("→ EE[{}]: destroy all nonland MV {}", n, n));
                        eff_destroy_all(who, filter).call(state, t, &[]);
                    }))
                })),
                ..Default::default()
            }],
            ..Default::default()
        }),
        vec![],
        None,
        vec![], CardLayout::Normal, None,
        vec![],
        vec![ReplacementDef {
            check: Arc::new(etb_self_check),
            make_effect: Arc::new(|_source_id, controller: PlayerId| {
                Effect(Arc::new(move |state, t, targets| {
                    let Some(&id) = targets.first() else { return; };
                    let chosen_x = state.resolving_costs_ctx.chosen_x;
                    if let Some(obj) = state.objects.get_mut(&id) {
                        *obj.counters.entry(CounterType::Charge).or_insert(0) = chosen_x;
                    }
                    let from = current_zone_id(id, state);
                    fire_event(
                        GameEvent::ZoneChange { id, actor: controller, from, to: ZoneId::Battlefield, controller },
                        state, t, controller,
                    );
                }))
            }),
        }],
        vec![],
        vec![],
    );
    def.additional_costs = vec![CostComponent::XMana];
    def
}

fn grafdiggers_cage() -> CardDef {
    CardDef::new(
        "Grafdigger's Cage",
        CardKind::Artifact(ArtifactData {
            mana_cost: "1".to_string(),
            ..Default::default()
        }),
        vec![],
        Some(40),
        vec![], CardLayout::Normal, None,
        vec![],  // no trigger_defs
        // (b): ETB replacement installs a CE that gates play_free_cast
        vec![etb_self_replacement(|source_id, _id, _controller, state, _t| {
            let controller = state.objects.get(&source_id).map_or(PlayerId::Us, |o| o.controller);
            state.continuous_instances.push(ContinuousInstance {
                source_id,
                controller,
                layer: ContinuousLayer::L3TextEffects,
                filter: Arc::new(move |cid, _| cid == source_id),
                modifier: Arc::new(|def, _| { def.blocks_gy_and_lib_access = true; }),
                expiry: ContinuousExpiry::WhileSourceOnBattlefield,
            });
        })],
        // (a): prohibition blocks ZoneChange from GY/library to BF for creature cards
        vec![ProhibitionDef {
            check: Arc::new(|event, _source_id, _controller, state| {
                if let GameEvent::ZoneChange {
                    id, from: ZoneId::Graveyard | ZoneId::Library, to: ZoneId::Battlefield, ..
                } = event {
                    let key = state.objects.get(id).map(|o| o.catalog_key.as_str()).unwrap_or("");
                    state.catalog.get(key).map_or(false, |d| d.is_creature())
                } else {
                    false
                }
            }),
        }],
        vec![],  // no static_ability_defs
    )
}

// ── Instants ──────────────────────────────────────────────────────────────────

/// Draw 3, put back 2. CR 420 (draw), CR 701.26 (library manipulation).
fn brainstorm() -> CardDef {
    simple("Brainstorm", CardKind::Instant(SpellData {
        mana_cost: "U".to_string(),
        exileable: true,
        spell_factory: Some(Arc::new(|who, _source_id, _x| {
            eff_draw(who, 3).then(eff_put_back(who, 2))
        })),
        ..Default::default()
    }), parse_colors("U", false, false), None)
}

/// Surveil 1, then draw 1. CR 701.43 (surveil — not modeled; treated as draw:1).
fn consider() -> CardDef {
    simple("Consider", CardKind::Instant(SpellData {
        mana_cost: "U".to_string(),
        exileable: true,
        spell_factory: Some(Arc::new(|who, _source_id, _x| eff_draw(who, 1))),
        ..Default::default()
    }), parse_colors("U", false, false), None)
}

/// Counter target spell. Alternate costs: bounce a blue-producing island (free),
/// or pay {1U} (20% probability). CR 701.5.
/// "Counter target spell unless its controller pays {1}."
fn daze() -> CardDef {
    simple("Daze", CardKind::Instant(SpellData {
        mana_cost: "U".to_string(),
        exileable: true,
        // blue=true so it can be pitched to Force of Will
        target_spec: TargetSpec::ObjectInZone { controller: Who::Opp, zone: ZoneId::Stack, filter: obj_pred_from_card(pred_any()) },
        alternate_costs: vec![
            AlternateCost { costs: vec![CostComponent::ReturnFromBattlefield(cost_pred_blue_producing())], hand_min: 1, ..Default::default() },
            AlternateCost { costs: vec![CostComponent::Mana(parse_mana_cost("1U"))], hand_min: 1, prob: Some(0.2), ..Default::default() },
        ],
        spell_factory: Some(Arc::new(|who, _source_id, _x| {
            eff_counter_unless_pays(who, vec![CostComponent::Mana(parse_mana_cost("1"))])
        })),
        ..Default::default()
    }), parse_colors("U", true, false), None)
}

/// Counter target noncreature spell. Pitch cost (exile a blue card) only available when it's
/// not your turn; the countered spell is exiled via a scoped replacement (CR 118.9b, 614.1a).
fn force_of_negation() -> CardDef {
    simple("Force of Negation", CardKind::Instant(SpellData {
        mana_cost: "1UU".to_string(),
        target_spec: TargetSpec::ObjectInZone {
            controller: Who::Opp,
            zone: ZoneId::Stack,
            filter: obj_pred_from_card(pred_not(pred_type_eq(CardType::Creature))),
        },
        alternate_costs: vec![
            AlternateCost {
                costs: vec![CostComponent::ExileFromHand(cost_pred_blue_nonland())],
                hand_min: 2,
                condition: Some(std::sync::Arc::new(|caster, state| {
                    state.current_ap != state.player_id(caster)
                })),
                ..Default::default()
            },
        ],
        spell_factory: Some(Arc::new(|who, source_id, _x| eff_counter_and_exile(who, source_id))),
        ..Default::default()
    }), parse_colors("1UU", true, false), None)
}

/// Counter target spell. Alternate costs: exile a blue card from hand + pay 1 life (pitch),
/// or pay {3UU} (hard cost, rare). CR 702.14 (pitch cost), CR 701.5.
fn force_of_will() -> CardDef {
    simple("Force of Will", CardKind::Instant(SpellData {
        mana_cost: "3UU".to_string(),
        target_spec: TargetSpec::ObjectInZone { controller: Who::Opp, zone: ZoneId::Stack, filter: obj_pred_from_card(pred_any()) },
        alternate_costs: vec![
            AlternateCost { costs: vec![CostComponent::ExileFromHand(cost_pred_blue_nonland()), CostComponent::Life(1)], hand_min: 2, ..Default::default() },
            AlternateCost { costs: vec![CostComponent::Mana(parse_mana_cost("3UU"))], hand_min: 1, ..Default::default() },
        ],
        spell_factory: Some(Arc::new(|who, _source_id, _x| eff_counter_target(who))),
        ..Default::default()
    }), parse_colors("3UU", true, false), None)
}

/// Add {B}{B}{B}. CR 106.3.
fn dark_ritual() -> CardDef {
    simple("Dark Ritual", CardKind::Instant(SpellData {
        mana_cost: "B".to_string(),
        spell_factory: Some(Arc::new(|who, _source_id, _x| eff_mana(who, "BBB"))),
        ..Default::default()
    }), parse_colors("B", false, false), None)
}

/// Destroy target creature with MV ≤ 3. CR 701.7.
fn fatal_push() -> CardDef {
    simple("Fatal Push", CardKind::Instant(SpellData {
        mana_cost: "B".to_string(),
        target_spec: TargetSpec::ObjectInZone {
            controller: Who::Opp,
            zone: ZoneId::Battlefield,
            filter: obj_pred_from_card(pred_and(pred_type_eq(CardType::Creature), pred_mana_value_le(3))),
        },
        spell_factory: Some(Arc::new(|who, _source_id, _x| eff_destroy_target(who))),
        ..Default::default()
    }), parse_colors("B", false, false), None)
}

/// Destroy target non-black creature. Alternate cost: pay 4 life (free spell). CR 701.7.
fn snuff_out() -> CardDef {
    simple("Snuff Out", CardKind::Instant(SpellData {
        mana_cost: "3BB".to_string(),
        target_spec: TargetSpec::ObjectInZone {
            controller: Who::Opp,
            zone: ZoneId::Battlefield,
            filter: obj_pred_from_card(pred_and(pred_type_eq(CardType::Creature), pred_not(pred_has_color(Color::Black)))),
        },
        alternate_costs: vec![
            AlternateCost { costs: vec![CostComponent::Life(4)], ..Default::default() },
        ],
        spell_factory: Some(Arc::new(|who, _source_id, _x| eff_destroy_target(who))),
        ..Default::default()
    }), parse_colors("3BB", false, true), None)
}

/// Exile target creature. Its controller gains life equal to its power. CR 701.10.
fn swords_to_plowshares() -> CardDef {
    simple("Swords to Plowshares", CardKind::Instant(SpellData {
        mana_cost: "W".to_string(),
        target_spec: TargetSpec::ObjectInZone {
            controller: Who::Opp,
            zone: ZoneId::Battlefield,
            filter: obj_pred_from_card(pred_type_eq(CardType::Creature)),
        },
        spell_factory: Some(Arc::new(|who, _source_id, _x| eff_exile_target_gain_power(who))),
        ..Default::default()
    }), parse_colors("W", true, false), None)
}

/// Destroy target creature or planeswalker.
/// Additional cost: discard a card OR pay 3 life (CR 118.9d).
fn bitter_triumph() -> CardDef {
    let mut def = simple("Bitter Triumph", CardKind::Instant(SpellData {
        mana_cost: "1B".to_string(),
        target_spec: TargetSpec::ObjectInZone {
            controller: Who::Opp,
            zone: ZoneId::Battlefield,
            filter: obj_pred_from_card(pred_or(pred_type_eq(CardType::Creature), pred_type_eq(CardType::Planeswalker))),
        },
        spell_factory: Some(Arc::new(|who, _source_id, _x| eff_destroy_target(who))),
        ..Default::default()
    }), parse_colors("1B", false, false), None);
    def.additional_costs = vec![
        CostComponent::CostOr(vec![
            CostComponent::DiscardCard(obj_pred_from_card(pred_any())),
            CostComponent::Life(3),
        ]),
    ];
    def
}

/// Destroy target creature or planeswalker with MV ≤ 3. This spell can't be countered (CR 608.2b).
fn long_goodbye() -> CardDef {
    CardDef::new(
        "Long Goodbye",
        CardKind::Instant(SpellData {
            mana_cost: "1B".to_string(),
            target_spec: TargetSpec::ObjectInZone {
                controller: Who::Opp,
                zone: ZoneId::Battlefield,
                filter: obj_pred_from_card(pred_and(
                    pred_or(pred_type_eq(CardType::Creature), pred_type_eq(CardType::Planeswalker)),
                    pred_mana_value_le(3),
                )),
            },
            spell_factory: Some(Arc::new(|who, _source_id, _x| eff_destroy_target(who))),
            ..Default::default()
        }),
        parse_colors("1B", false, false),
        None,
        vec![], CardLayout::Normal, None,
        vec![], vec![],
        // "This spell can't be countered": ProhibitionDef active while on the stack.
        vec![ProhibitionDef {
            check: Arc::new(|event, source_id, _controller, _state| {
                matches!(event, GameEvent::SpellBeingCountered { card_id, .. } if *card_id == source_id)
            }),
        }],
        vec![],
    )
}

/// Choose one — each opponent sacrifices a nontoken creature (mode 0), a creature token
/// (mode 1), or a planeswalker (mode 2) of their choice. Mode selected via `resolve_choice`;
/// the opponent's specific sacrifice goes through `sacrifice_choice`. CR 700.2, CR 701.16.
fn sheoldreds_edict() -> CardDef {
    simple("Sheoldred's Edict", CardKind::Instant(SpellData {
        mana_cost: "1B".to_string(),
        spell_factory: Some(Arc::new(|who, source_id, _x| {
            Effect(Arc::new(move |state, t, _targets| {
                let f = Arc::clone(&state.resolve_choice);
                let mode = match f(source_id, &ChoiceRequest::Mode(3), &*state) {
                    ChoiceResult::Mode(m) => m,
                    _ => 0,
                };
                let filter: ObjPredicate = match mode {
                    1 => Arc::new(|id, state: &SimState| {
                        state.objects.get(&id).map_or(false, |o| o.is_token)
                    }),
                    2 => obj_pred_from_card(pred_type_eq(CardType::Planeswalker)),
                    _ => Arc::new(|id, state: &SimState| {
                        state.objects.get(&id).map_or(false, |o| {
                            !o.is_token && state.catalog.get(o.catalog_key.as_str())
                                .map_or(false, |d| d.is_creature())
                        })
                    }),
                };
                eff_sacrifice(who, Who::Opp, filter).call(state, t, &[]);
            }))
        })),
        ..Default::default()
    }), parse_colors("1B", false, false), None)
}

/// Counter target noncreature spell unless its controller pays {2}. CR 700.2.
fn spell_pierce() -> CardDef {
    simple("Spell Pierce", CardKind::Instant(SpellData {
        mana_cost: "U".to_string(),
        exileable: true,
        target_spec: TargetSpec::ObjectInZone {
            controller: Who::Opp,
            zone: ZoneId::Stack,
            filter: obj_pred_from_card(pred_not(pred_type_eq(CardType::Creature))),
        },
        spell_factory: Some(Arc::new(|who, _source_id, _x| {
            eff_counter_unless_pays(who, vec![CostComponent::Mana(parse_mana_cost("2"))])
        })),
        ..Default::default()
    }), parse_colors("U", true, false), None)
}

/// Counter target instant or sorcery spell unless its controller pays {1}.
/// Storm (CR 702.40): when you cast this spell, copy it for each spell cast before it
/// this turn. Copies are counterable stack abilities targeting other legal targets.
fn flusterstorm() -> CardDef {
    let spell_data = SpellData {
        mana_cost: "U".to_string(),
        exileable: true,
        target_spec: TargetSpec::ObjectInZone {
            controller: Who::Opp,
            zone: ZoneId::Stack,
            filter: obj_pred_from_card(pred_or(
                pred_type_eq(CardType::Instant),
                pred_type_eq(CardType::Sorcery),
            )),
        },
        spell_factory: Some(Arc::new(|who, _source_id, _x| {
            eff_counter_unless_pays(who, vec![CostComponent::Mana(parse_mana_cost("1"))])
        })),
        ..Default::default()
    };
    // Clone spell_data fields we need for the storm trigger closure.
    let storm_target_spec = spell_data.target_spec.clone();
    let storm_factory = spell_data.spell_factory.clone().unwrap();
    CardDef::new(
        "Flusterstorm",
        CardKind::Instant(spell_data),
        parse_colors("U", true, false),
        None,
        vec![], CardLayout::Normal, None,
        vec![TriggerDef {
            // Storm trigger: fires on SpellCast where card_id == source_id (self-referential).
            check: Arc::new(move |event, source_id, controller, state, pending| {
                if let GameEvent::SpellCast { caster, card_id, .. } = event {
                    if *card_id != source_id || *caster != controller { return; }
                    // Capture storm count now — spells cast before this one.
                    // spells_cast_this_turn hasn't been incremented yet for this spell
                    // (that happens after cast_spell returns).
                    let storm_count = state.player(controller).spells_cast_this_turn as usize;
                    if storm_count == 0 { return; }
                    let tspec = storm_target_spec.clone();
                    let factory = storm_factory.clone();
                    let spell_source = source_id;
                    pending.push(TriggerContext {
                        source_name: "Flusterstorm (storm trigger)".into(),
                        controller,
                        target_spec: TargetSpec::None,
                        effect: Effect(Arc::new(move |state, t, _| {
                            // Find legal targets for the copies.
                            let all_targets = legal_targets(&tspec, controller, state);
                            // Original target from the spell on the stack.
                            let original_targets: Vec<ObjId> = state.objects.get(&spell_source)
                                .and_then(|o| o.spell.as_ref())
                                .map(|s| s.chosen_targets.clone())
                                .unwrap_or_default();
                            let extra_targets: Vec<ObjId> = all_targets.iter()
                                .filter(|id| !original_targets.contains(id))
                                .copied()
                                .collect();
                            for i in 0..storm_count {
                                let tgt = if i < extra_targets.len() {
                                    extra_targets[i]
                                } else if !original_targets.is_empty() {
                                    original_targets[0]
                                } else {
                                    break;
                                };
                                let copy_eff = factory(controller, spell_source, 0);
                                let copy_id = state.alloc_id();
                                state.abilities.insert(copy_id, StackAbility {
                                    id: copy_id,
                                    source_name: "Flusterstorm (storm copy)".into(),
                                    owner: state.player_id(controller),
                                    effect: copy_eff,
                                    chosen_targets: vec![tgt],
                                    costs_paid_ctx: CostsPaidCtx::default(),
                                    is_triggered: false,
                                    counterable: true,
                                    choice_spec: None,
                                });
                                state.stack.push(copy_id);
                                let tgt_name = state.stack_item_display_name(tgt).to_string();
                                state.log(t, controller, format!("Storm → Flusterstorm (targeting {})", tgt_name));
                            }
                        })),
                    });
                }
            }),
            always_active: true,
        }],
        vec![], vec![], vec![],
    )
}

/// Counter target triggered ability or colorless spell.
/// Replicate {1} (CR 702.58): optional additional cost paid 0+ times; each payment
/// creates a copy of the spell targeting another triggered ability or colorless spell.
fn consign_to_memory() -> CardDef {
    let mut def = simple("Consign to Memory", CardKind::Instant(SpellData {
        mana_cost: "U".to_string(),
        exileable: true,
        target_spec: TargetSpec::Union(vec![
            TargetSpec::AbilityOnStack { controller: Who::Opp, ability_type: AbilityType::Triggered },
            TargetSpec::ObjectInZone {
                controller: Who::Opp,
                zone: ZoneId::Stack,
                filter: obj_pred_from_card(pred_no_colored_pips()),
            },
        ]),
        spell_factory: Some(Arc::new(|who, _source_id, _x| eff_counter_target(who))),
        ..Default::default()
    }), parse_colors("U", false, false), None);
    def.additional_costs = vec![CostComponent::Replicate(parse_mana_cost("1"))];
    def
}

/// Exile target card in a graveyard (not basic land), then exile all cards with the
/// same name from that player's graveyard, hand, and library (CR 107.4f phyrexian mana).
/// {B/P}: pay {B} or pay 2 life.
fn surgical_extraction() -> CardDef {
    simple("Surgical Extraction", CardKind::Instant(SpellData {
        mana_cost: "B".to_string(),
        target_spec: TargetSpec::ObjectInZone {
            controller: Who::Opp,
            zone: ZoneId::Graveyard,
            filter: obj_pred_from_card(pred_not(pred_and(
                pred_type_eq(CardType::Land),
                pred_has_supertype(Supertype::Basic),
            ))),
        },
        alternate_costs: vec![
            AlternateCost { costs: vec![CostComponent::Life(2)], hand_min: 0, ..Default::default() },
        ],
        spell_factory: Some(Arc::new(|caster, _source_id, _x| {
            Effect(Arc::new(move |state, t, targets| {
                let Some(&target_id) = targets.first() else { return };
                let (name, owner) = match state.objects.get(&target_id) {
                    Some(o) => (o.catalog_key.clone(), o.owner),
                    None => return,
                };
                // Exile all cards with the same name from owner's graveyard, hand, and library.
                // Card names are used as identity per CLAUDE.md principle 2.
                let to_exile: Vec<ObjId> = state.objects.values()
                    .filter(|o| o.catalog_key == name && o.owner == owner)
                    .filter(|o| matches!(o.zone,
                        CardZone::Graveyard | CardZone::Hand { .. } | CardZone::Library
                    ))
                    .map(|o| o.id)
                    .collect();
                let count = to_exile.len();
                for id in to_exile {
                    change_zone(id, ZoneId::Exile, state, t, caster);
                }
                state.log(t, caster, format!("→ extracted {} × '{}'", count, name));
            }))
        })),
        ..Default::default()
    }), parse_colors("B", false, false), None)
}

/// Build a TargetSpec for the modal color-hate instants: either a spell on the stack
/// or a permanent on the battlefield, both filtered to the given color.
fn color_hate_target_spec(c: Color) -> TargetSpec {
    TargetSpec::Union(vec![
        TargetSpec::ObjectInZone {
            controller: Who::Opp,
            zone: ZoneId::Stack,
            filter: obj_pred_from_card(pred_has_color(c)),
        },
        TargetSpec::ObjectInZone {
            controller: Who::Opp,
            zone: ZoneId::Battlefield,
            filter: obj_pred_from_card(pred_has_color(c)),
        },
    ])
}

/// Build a TargetSpec for the "if it's [color]" variant: targets ANY spell on the stack
/// or ANY permanent on the battlefield (targeting is unrestricted; the effect is conditional).
fn any_spell_or_permanent_target() -> TargetSpec {
    TargetSpec::Union(vec![
        TargetSpec::ObjectInZone {
            controller: Who::Opp,
            zone: ZoneId::Stack,
            filter: obj_pred_from_card(pred_any()),
        },
        TargetSpec::ObjectInZone {
            controller: Who::Opp,
            zone: ZoneId::Battlefield,
            filter: obj_pred_from_card(pred_any()),
        },
    ])
}

/// Modal effect: counter if target is on the stack, destroy if on the battlefield.
/// Used by REB/BEB where the color restriction is on targeting (not the effect).
fn counter_or_destroy(who: PlayerId) -> Effect {
    Effect(Arc::new(move |state, t, targets| {
        let Some(&id) = targets.first() else { return };
        if state.objects.get(&id).map_or(false, |o| o.zone == CardZone::Stack) {
            eff_counter_target(who).call(state, t, targets);
        } else {
            eff_destroy_target(who).call(state, t, targets);
        }
    }))
}

/// Modal effect: counter/destroy only if target is the given color; otherwise fizzles.
/// Used by Pyroblast/Hydroblast where ANY spell/permanent can be targeted but the
/// effect only applies "if it's [color]" (CR 608.2b — legal target, effect doesn't apply).
fn counter_or_destroy_if_color(who: PlayerId, c: Color) -> Effect {
    Effect(Arc::new(move |state, t, targets| {
        let Some(&id) = targets.first() else { return };
        let is_color = state.def_of(id).map_or(false, |d| d.colors.contains(&c));
        if !is_color { return; }
        if state.objects.get(&id).map_or(false, |o| o.zone == CardZone::Stack) {
            eff_counter_target(who).call(state, t, targets);
        } else {
            eff_destroy_target(who).call(state, t, targets);
        }
    }))
}

/// Choose one — Counter target blue spell; or destroy target blue permanent. CR 701.5, 701.7.
/// Choose one: deal 3 damage to target creature; or destroy target artifact.
fn abrade() -> CardDef {
    simple("Abrade", CardKind::Instant(SpellData {
        mana_cost: "1R".to_string(),
        target_spec: TargetSpec::Union(vec![
            TargetSpec::ObjectInZone {
                controller: Who::Opp,
                zone: ZoneId::Battlefield,
                filter: obj_pred_from_card(pred_type_eq(CardType::Creature)),
            },
            TargetSpec::ObjectInZone {
                controller: Who::Opp,
                zone: ZoneId::Battlefield,
                filter: obj_pred_from_card(pred_type_eq(CardType::Artifact)),
            },
        ]),
        spell_factory: Some(Arc::new(|who, _source_id, _x| {
            Effect(Arc::new(move |state, t, targets| {
                if let Some(&id) = targets.first() {
                    let is_creature = state.def_of(id).map_or(false, |d| d.is_creature());
                    if is_creature {
                        eff_damage_target(who, 3).call(state, t, targets);
                    } else {
                        eff_destroy_target(who).call(state, t, targets);
                    }
                }
            }))
        })),
        ..Default::default()
    }), parse_colors("1R", false, false), None)
}

fn red_elemental_blast() -> CardDef {
    simple("Red Elemental Blast", CardKind::Instant(SpellData {
        mana_cost: "R".to_string(),
        target_spec: color_hate_target_spec(Color::Blue),
        spell_factory: Some(Arc::new(|who, _source_id, _x| counter_or_destroy(who))),
        ..Default::default()
    }), parse_colors("R", false, false), None)
}

/// Choose one — Counter target spell if it's blue; or destroy target permanent if it's blue.
/// Targets any opp spell/permanent; effect fizzles if the target is not blue. CR 701.5, 701.7.
fn pyroblast() -> CardDef {
    simple("Pyroblast", CardKind::Instant(SpellData {
        mana_cost: "R".to_string(),
        target_spec: any_spell_or_permanent_target(),
        spell_factory: Some(Arc::new(|who, _source_id, _x| counter_or_destroy_if_color(who, Color::Blue))),
        ..Default::default()
    }), parse_colors("R", false, false), None)
}

/// Choose one — Counter target red spell; or destroy target red permanent. CR 701.5, 701.7.
fn blue_elemental_blast() -> CardDef {
    simple("Blue Elemental Blast", CardKind::Instant(SpellData {
        mana_cost: "U".to_string(),
        exileable: true,
        target_spec: color_hate_target_spec(Color::Red),
        spell_factory: Some(Arc::new(|who, _source_id, _x| counter_or_destroy(who))),
        ..Default::default()
    }), parse_colors("U", false, false), None)
}

/// Choose one — Counter target spell if it's red; or destroy target red permanent.
/// Targets any opp spell/permanent; effect fizzles if the target is not red. CR 701.5, 701.7.
fn hydroblast() -> CardDef {
    simple("Hydroblast", CardKind::Instant(SpellData {
        mana_cost: "U".to_string(),
        exileable: true,
        target_spec: any_spell_or_permanent_target(),
        spell_factory: Some(Arc::new(|who, _source_id, _x| counter_or_destroy_if_color(who, Color::Red))),
        ..Default::default()
    }), parse_colors("U", false, false), None)
}

// ── Sorceries ─────────────────────────────────────────────────────────────────

/// All creatures get -X/-X until end of turn; additional cost: pay X life (CR 107.2).
/// The -X/-X is a Layer 7 ContinuousInstance; creatures with resulting toughness ≤ 0
/// die when the engine checks state-based actions before the next priority grant.
/// X is chosen by the strategy (default: 3) via `choose_x_for_spell`.
fn toxic_deluge() -> CardDef {
    let mut def = simple("Toxic Deluge", CardKind::Sorcery(SpellData {
        mana_cost: "2B".to_string(),
        spell_factory: Some(Arc::new(|caster, source_id, x| {
            let xi = x as i32;
            Effect(Arc::new(move |state, t, _targets| {
                state.continuous_instances.push(ContinuousInstance {
                    source_id,
                    controller: caster,
                    layer: ContinuousLayer::L7PowerToughness,
                    filter: std::sync::Arc::new(|_, _| true),
                    modifier: std::sync::Arc::new(move |def, _state| {
                        if let CardKind::Creature(c) = &mut def.kind {
                            c.adjust_pt(-xi, -xi);
                        }
                    }),
                    expiry: ContinuousExpiry::EndOfTurn,
                });
                state.log(t, caster, format!("→ all creatures get -{xi}/-{xi} until end of turn"));
            }))
        })),
        ..Default::default()
    }), parse_colors("2B", false, false), None);
    def.additional_costs = vec![CostComponent::XLife];
    def
}

/// Win condition: set success=true. In full rules: opponent's library and graveyard become
/// their library; controller searches for exactly five cards. CR 101.1 (shortcut).
fn doomsday() -> CardDef {
    simple("Doomsday", CardKind::Sorcery(SpellData {
        mana_cost: "BBB".to_string(),
        spell_factory: Some(Arc::new(|_who, _source_id, _x| eff_doomsday())),
        ..Default::default()
    }), parse_colors("BBB", false, false), None)
}

/// Look at top 5, put two in hand, rest on bottom in any order. Modeled as draw:2. CR 701.26.
fn stock_up() -> CardDef {
    simple("Stock Up", CardKind::Sorcery(SpellData {
        mana_cost: "2U".to_string(),
        spell_factory: Some(Arc::new(|who, _source_id, _x| eff_draw(who, 2))),
        ..Default::default()
    }), parse_colors("U", false, false), None)
}

/// Look at top 3, put one in hand, rest on bottom in any order. Modeled as draw:1. CR 701.26.
fn ponder() -> CardDef {
    simple("Ponder", CardKind::Sorcery(SpellData {
        mana_cost: "U".to_string(),
        exileable: true,
        spell_factory: Some(Arc::new(|who, _source_id, _x| eff_draw(who, 1))),
        ..Default::default()
    }), parse_colors("U", false, false), None)
}

/// Target opponent discards a nonland card; you lose 2 life. CR 701.8, CR 702.1.
fn thoughtseize() -> CardDef {
    simple("Thoughtseize", CardKind::Sorcery(SpellData {
        mana_cost: "B".to_string(),
        spell_factory: Some(Arc::new(|who, _source_id, _x| {
            eff_discard(who, Who::Opp, 1, pred_not(pred_type_eq(CardType::Land)))
                .then(eff_life_loss(who, 2))
        })),
        ..Default::default()
    }), parse_colors("B", false, false), None)
}

/// Return target creature from your graveyard to play. CR 701.14.
fn unearth() -> CardDef {
    simple("Unearth", CardKind::Sorcery(SpellData {
        mana_cost: "B".to_string(),
        target_spec: TargetSpec::ObjectInZone {
            controller: Who::Actor,
            zone: ZoneId::Graveyard,
            filter: obj_pred_from_card(pred_type_eq(CardType::Creature)),
        },
        spell_factory: Some(Arc::new(|who, _source_id, _x| eff_reanimate(who))),
        ..Default::default()
    }), parse_colors("B", false, false), None)
}

/// Target opponent discards 2 cards at random. CR 701.8.
fn hymn_to_tourach() -> CardDef {
    simple("Hymn to Tourach", CardKind::Sorcery(SpellData {
        mana_cost: "BB".to_string(),
        spell_factory: Some(Arc::new(|who, _source_id, _x| eff_discard(who, Who::Opp, 2, pred_any()))),
        ..Default::default()
    }), parse_colors("BB", false, false), None)
}

/// Cycling: discard this card, sacrifice a land you control → draw a card.
/// Modeled as a hand-zone activated ability. Cast cost {G}{W} rarely used.
fn edge_of_autumn() -> CardDef {
    simple("Edge of Autumn", CardKind::Sorcery(SpellData {
        mana_cost: "GW".to_string(),
        // Hand ability: discard self + sacrifice a land → draw 1.
        // Modeled via AbilityDef on SpellData is not standard; the TOML used `abilities`
        // at the top level. Since SpellData has no abilities field, this card has no
        // castable effects but the hand ability is registered via the cycling-like path
        // in strategy. Future work: add `abilities` to SpellData.
        ..Default::default()
    }), parse_colors("GW", false, false), None)
}

/// Search your library for a sorcery card, put it on top. CR 700.3, CR 701.19.
fn personal_tutor() -> CardDef {
    simple("Personal Tutor", CardKind::Sorcery(SpellData {
        mana_cost: "U".to_string(),
        spell_factory: Some(Arc::new(|who, _source_id, _x| {
            eff_fetch_search(who, pred_type_eq(CardType::Sorcery), ZoneId::Library)
        })),
        ..Default::default()
    }), parse_colors("U", false, false), None)
}

/// Search your library for a green creature and put it onto the battlefield.
/// X not modeled; treated as {1G} (fixed cost). CR 700.3, CR 701.19.
fn green_suns_zenith() -> CardDef {
    simple("Green Sun's Zenith", CardKind::Sorcery(SpellData {
        mana_cost: "1G".to_string(),
        spell_factory: Some(Arc::new(|who, _source_id, _x| {
            eff_fetch_search(
                who,
                pred_and(pred_type_eq(CardType::Creature), pred_has_color(Color::Green)),
                ZoneId::Battlefield,
            )
        })),
        ..Default::default()
    }), parse_colors("1G", false, false), None)
}

/// Each player may put an artifact, creature, enchantment, or land card from their
/// hand onto the battlefield. Both placements are simultaneous (CR 101.4).
fn show_and_tell() -> CardDef {
    simple("Show and Tell", CardKind::Sorcery(SpellData {
        mana_cost: "2U".to_string(),
        spell_factory: Some(Arc::new(|who, _source_id, _x| {
            eff_each_may_put(who, pred_or(
                pred_or(pred_type_eq(CardType::Artifact), pred_type_eq(CardType::Creature)),
                pred_or(pred_type_eq(CardType::Enchantment), pred_type_eq(CardType::Land)),
            ))
        })),
        ..Default::default()
    }), parse_colors("U", false, false), None)
}

// ── Creatures ─────────────────────────────────────────────────────────────────

/// ETB: look at top X cards of your library, where X is the number of cards in it;
/// if you control more blue/black permanents than opponent, you win. Modeled as win-on-ETB
/// via strategy, not via trigger here (no ETB trigger — strategy checks for Oracle).
/// CR 702.15 (devotion), CR 104.3b.
fn thassas_oracle() -> CardDef {
    let mut data = CreatureData::new("UU", 1, 3);
    data.exileable = true;
    simple("Thassa's Oracle", CardKind::Creature(data), parse_colors("UU", false, false), Some(1))
}

/// Cycling (hand ability): discard this + pay 2 life → draw 1. CR 702.28.
fn street_wraith() -> CardDef {
    let mut data = CreatureData::new("3BB", 3, 4);
    data.abilities = vec![AbilityDef {
        source_zone: SourceZone::Hand,
        costs: vec![CostComponent::DiscardSelf, CostComponent::Life(2)],
        ability_factory: Some(Arc::new(|who, _| eff_draw(who, 1))),
        ..Default::default()
    }];
    simple("Street Wraith", CardKind::Creature(data), parse_colors("3BB", false, false), Some(1))
}

/// 0/1 for {2B}. No special abilities — just a beater.
fn barrowgoyf() -> CardDef {
    let mut data = CreatureData::new("2B", 0, 1);
    data.legendary = false;
    simple("Barrowgoyf", CardKind::Creature(data), parse_colors("2B", false, true), None)
}

/// Ninjutsu {1U}: swap in with an unblocked attacker. CR 702.49.
fn ingenious_infiltrator() -> CardDef {
    let mut data = CreatureData::new("1UB", 2, 1);
    data.ninjutsu = Some(NinjutsuAbility { mana_cost: "1U".to_string() });
    simple(
        "Ingenious Infiltrator",
        CardKind::Creature(data),
        parse_colors("1UB", true, true),
        None,
    )
}

/// Legendary. Ninjutsu {1UB}. CR 702.49, CR 704.5k (legendary rule).
fn kaito_bane_of_nightmares() -> CardDef {
    let mut data = CreatureData::new("2UB", 3, 4);
    data.legendary = true;
    data.ninjutsu = Some(NinjutsuAbility { mana_cost: "1UB".to_string() });
    simple(
        "Kaito, Bane of Nightmares",
        CardKind::Creature(data),
        parse_colors("2UB", true, true),
        None,
    )
}

/// ETB: search your library for a creature with toughness ≤ 2, put it into your hand.
/// CR 700.3, CR 701.19.
fn recruiter_of_the_guard() -> CardDef {
    CardDef::new(
        "Recruiter of the Guard",
        CardKind::Creature(CreatureData::new("2W", 1, 1)),
        parse_colors("2W", false, false),
        None,
        vec![], CardLayout::Normal, None,
        vec![TriggerDef { check: Arc::new(recruiter_check), always_active: true }],
        vec![],
        vec![],
        vec![],
    )
}

/// ETB trigger + draw-trigger: deal 1 damage to any target and amass Orc 1 whenever
/// opponent draws a non-natural card. Also fires on its own ETB. CR 603.
fn orcish_bowmasters() -> CardDef {
    let mut data = CreatureData::new("1B", 1, 1);
    data.legendary = false;
    CardDef::new(
        "Orcish Bowmasters",
        CardKind::Creature(data),
        parse_colors("1B", false, true),
        None,
        vec![], CardLayout::Normal, None,
        vec![TriggerDef { check: Arc::new(bowmasters_check), always_active: false }],
        vec![],
        vec![],
        vec![],
    )
}

/// ETB replacement: enters with counters = # of instants/sorceries in controller's exile.
/// Trigger: gains +1/+1 counter when a spell is exiled from your graveyard. CR 614.1, CR 603.
fn murktide_regent() -> CardDef {
    let mut data = CreatureData::new("5UU", 3, 3);
    data.delve = true;
    CardDef::new(
        "Murktide Regent",
        CardKind::Creature(data),
        parse_colors("5UU", true, false),
        Some(25),
        vec![], CardLayout::Normal, None,
        vec![TriggerDef { check: Arc::new(murktide_check), always_active: false }],
        vec![ReplacementDef {
            check: Arc::new(murktide_etb_check),
            make_effect: Arc::new(|_source_id, controller: PlayerId| {
                Effect(Arc::new(move |state, t, targets| {
                    let Some(&id) = targets.first() else { return; };
                    // Count instants/sorceries exiled specifically as delve payment (CR 702.66b).
                    // `resolving_costs_ctx` is set by resolve_top_of_stack before the effect runs.
                    let delve_ids = state.resolving_costs_ctx.objects_moved.clone();
                    let exile_count = delve_ids.iter()
                        .filter(|&&id| {
                            state.objects.get(&id)
                                .and_then(|o| state.catalog.get(o.catalog_key.as_str()))
                                .map_or(false, |d| d.is_instant() || d.is_sorcery())
                        })
                        .count() as i32;
                    if let Some(bf) = state.permanent_bf_mut(id) {
                        bf.counters = exile_count;
                    }
                    fire_event(
                        GameEvent::ZoneChange {
                            id,
                            actor: controller,
                            from: ZoneId::Stack,
                            to: ZoneId::Battlefield,
                            controller,
                        },
                        state, t, controller,
                    );
                }))
            }),
        }],
        vec![],
        vec![],
    )
}

/// Shadow (evasion — see strategy.rs), replacement effect (opponent's GY-bound cards
/// exile with a void counter), and {T}, SacSelf activated ability (choose an exiled
/// opponent card with a void counter; grant a free-cast permission for it this turn).
/// CR 702.28 (shadow), CR 614.1a (replacement).
fn dauthi_voidwalker() -> CardDef {
    let mut data = CreatureData::new("BB", 3, 2);
    data.keywords = vec!["shadow".to_string()];
    data.abilities = vec![AbilityDef {
        source_zone: SourceZone::Battlefield,
        costs: vec![CostComponent::TapSelf, CostComponent::SacSelf],
        target_spec: TargetSpec::None,
        choice_spec: Some(ChoiceSpec {
            controller: Who::Opp,
            zone: ZoneId::Exile,
            filter: pred_has_counter(CounterType::Void),
        }),
        ability_factory: Some(Arc::new(|who, _source_id| {
            Effect(Arc::new(move |state, _t, targets| {
                if let Some(&id) = targets.first() {
                    state.free_cast_permissions.push(FreeCastPermission {
                        controller: who,
                        target_id: id,
                    });
                }
            }))
        })),
    }];

    CardDef::new(
        "Dauthi Voidwalker",
        CardKind::Creature(data),
        parse_colors("BB", false, false),
        None,
        vec![], CardLayout::Normal, None,
        vec![],
        vec![ReplacementDef {
            check: Arc::new(dv_replacement_check),
            make_effect: Arc::new(|_source_id, controller: PlayerId| {
                Effect(Arc::new(move |state, t, targets| {
                    if let Some(&id) = targets.first() {
                        change_zone(id, ZoneId::Exile, state, t, controller);
                        if let Some(obj) = state.objects.get_mut(&id) {
                            *obj.counters.entry(CounterType::Void).or_insert(0) += 1;
                        }
                    }
                }))
            }),
        }],
        vec![],
        vec![],
    )
}

/// Prohibition: each opponent can't cast noncreature spells with MV > their land count.
/// Trigger: whenever an opponent casts a spell with no mana spent, counter it.
/// CR 614.17 (prohibition), CR 603 (trigger).
fn lavinia_azorius_renegade() -> CardDef {
    let mut data = CreatureData::new("WU", 2, 2);
    data.legendary = true;
    CardDef::new(
        "Lavinia, Azorius Renegade",
        CardKind::Creature(data),
        parse_colors("WU", true, false),
        None,
        vec![], CardLayout::Normal, None,
        vec![TriggerDef { check: Arc::new(|event, _source_id, controller, _state, pending| {
            if let GameEvent::SpellCast { caster, card_id, mana_spent } = event {
                if *caster != controller && !mana_spent {
                    let spell_id = *card_id;
                    pending.push(TriggerContext {
                        source_name: "Lavinia, Azorius Renegade".into(),
                        controller,
                        target_spec: TargetSpec::None,
                        effect: Effect(Arc::new(move |state, t, _| {
                            counter_one(spell_id, state, t, controller);
                        })),
                    });
                }
            }
        }), always_active: false }],
        vec![],
        vec![ProhibitionDef {
            check: Arc::new(|event, _source_id, controller, state| {
                if let GameEvent::SpellBeingCast { caster, mana_value, is_noncreature, .. } = event {
                    if *caster == controller || !is_noncreature { return false; }
                    let land_count = state.permanents_of(*caster)
                        .filter(|o| state.catalog.get(o.catalog_key.as_str())
                            .map_or(false, |d| d.is_land()))
                        .count() as i32;
                    *mana_value > land_count
                } else { false }
            }),
        }],
        vec![],
    )
}

/// {1}{R}, 2/2 Goblin Sorcerer.
/// "This spell can't be countered." — ProhibitionDef active while on stack.
/// "Ward—Pay 2 life." — TriggerCheckFn on SpellCast, checks spell's chosen_targets.
/// "Spells you control can't be countered." — ProhibitionDef active while on battlefield.
/// "Other creatures you control have Ward—Pay 2 life." — second TriggerCheckFn (approximation;
///   see TODO below for the true CE-layer-6 implementation).
fn hexing_squelcher() -> CardDef {
    let data = CreatureData::new("1R", 2, 2);
    CardDef::new(
        "Hexing Squelcher",
        CardKind::Creature(data),
        parse_colors("R", false, false),
        None,
        vec![], CardLayout::Normal, None,
        // Ward—Pay 2 life: fires when an opponent's spell targets this permanent.
        vec![TriggerDef { check: Arc::new(|event, source_id, controller, state, pending| {
            if let GameEvent::SpellCast { caster, card_id, .. } = event {
                if *caster == controller { return; }
                let is_targeted = state.objects.get(card_id)
                    .and_then(|o| o.spell.as_ref())
                    .map_or(false, |s| s.chosen_targets.contains(&source_id));
                if is_targeted {
                    let spell_id = *card_id;
                    let targeting_caster = *caster;
                    pending.push(TriggerContext {
                        source_name: "Hexing Squelcher (Ward)".into(),
                        controller,
                        target_spec: TargetSpec::None,
                        effect: Effect(Arc::new(move |state, t, _| {
                            ward_pay_or_counter(
                                source_id,
                                &[CostComponent::Life(2)],
                                spell_id,
                                targeting_caster,
                                controller,
                                state,
                                t,
                            );
                        })),
                    });
                }
            }
        }), always_active: true }],
        vec![],
        vec![
            // "Spells you control can't be countered." (while on battlefield)
            ProhibitionDef {
                check: Arc::new(|event, _source_id, controller, _state| {
                    matches!(event, GameEvent::SpellBeingCountered { caster, .. } if *caster == controller)
                }),
            },
            // "This spell can't be countered." (while on stack)
            ProhibitionDef {
                check: Arc::new(|event, source_id, _controller, _state| {
                    matches!(event, GameEvent::SpellBeingCountered { card_id, .. } if *card_id == source_id)
                }),
            },
        ],
        // "Other creatures you control have Ward—Pay 2 life."
        // L6 CE: while Hexing Squelcher is on the battlefield, push a Ward trigger into each
        // other creature controlled by the same player via granted_trigger_defs.
        vec![Arc::new(move |source_id, controller| ContinuousInstance {
            source_id,
            controller,
            layer: ContinuousLayer::L6AbilityEffects,
            filter: Arc::new(move |id, ctr| ctr == controller && id != source_id),
            modifier: Arc::new(|def, _state| {
                if matches!(def.kind, CardKind::Creature(_)) {
                    def.granted_trigger_defs.push(Arc::new(
                        |event, source_id, controller, state, pending| {
                            if let GameEvent::SpellCast { caster, card_id, .. } = event {
                                if *caster == controller { return; }
                                let is_targeted = state.objects.get(card_id)
                                    .and_then(|o| o.spell.as_ref())
                                    .map_or(false, |s| s.chosen_targets.contains(&source_id));
                                if is_targeted {
                                    let spell_id = *card_id;
                                    let targeting_caster = *caster;
                                    pending.push(TriggerContext {
                                        source_name: "Hexing Squelcher (Ward grant)".into(),
                                        controller,
                                        target_spec: TargetSpec::None,
                                        effect: Effect(Arc::new(move |state, t, _| {
                                            ward_pay_or_counter(
                                                source_id,
                                                &[CostComponent::Life(2)],
                                                spell_id,
                                                targeting_caster,
                                                controller,
                                                state,
                                                t,
                                            );
                                        })),
                                    });
                                }
                            }
                        },
                    ));
                }
            }),
            expiry: ContinuousExpiry::WhileSourceOnBattlefield,
        })],
    )
}

// ── DFCs / split cards ────────────────────────────────────────────────────────

/// Front: 0/3 creature for {U}, generates Clue tokens when it attacks.
/// Back: Tamiyo, Seasoned Scholar — planeswalker with +2 loyalty ability.
/// Transforms after controller draws their 3rd card in a turn. CR 701.28.
fn tamiyo_inquisitive_student() -> CardDef {
    let back = CardDef::new(
        "Tamiyo, Seasoned Scholar",
        CardKind::Planeswalker(PlaneswalkerData {
            mana_cost: String::new(),
            loyalty: 2,
            abilities: vec![AbilityDef {
                costs: vec![CostComponent::LoyaltyAdjust(2)],
                ability_factory: Some(Arc::new(build_tamiyo_plus_two)),
                ..Default::default()
            }],
        }),
        parse_colors("U", false, false),
        None,
        vec![], CardLayout::Normal, None,
        vec![],
        vec![replacement_planeswalker_etb(2)],
        vec![],
        vec![],
    );

    let mut front_data = CreatureData::new("U", 0, 3);
    front_data.legendary = true;

    CardDef::new(
        "Tamiyo, Inquisitive Student",
        CardKind::Creature(front_data),
        parse_colors("U", false, false),
        None,
        vec![Supertype::Legendary], CardLayout::DoubleFaced, Some(Box::new(back)),
        vec![TriggerDef { check: Arc::new(tamiyo_check), always_active: false }],
        vec![],
        vec![],
        vec![],
    )
}

/// Artifact Creature {2}, 1/3. ETB: choose a color; all objects everywhere gain that color.
/// Layer 5 continuous effect, expires when Painter leaves the battlefield.
/// CR 613.4 (color-changing effects apply at layer 5).
fn painters_servant() -> CardDef {
    let repl = etb_self_replacement(|source_id, id, controller, state, _t| {
        let f = Arc::clone(&state.resolve_choice);
        let ChoiceResult::Color(chosen) =
            f(source_id, &ChoiceRequest::Color, state) else { return };
        if let Some(bf) = state.permanent_bf_mut(id) {
            bf.etb_choice = Some(ChoiceResult::Color(chosen));
        }
        // Register L5 CE: all objects everywhere gain chosen_color while Painter is in play.
        state.continuous_instances.push(ContinuousInstance {
            source_id,
            controller,
            layer: ContinuousLayer::L5ColorEffects,
            filter: Arc::new(|_, _| true),
            modifier: Arc::new(move |def, _| {
                if !def.colors.contains(&chosen) { def.colors.push(chosen); }
            }),
            expiry: ContinuousExpiry::WhileSourceOnBattlefield,
        });
    });
    let mut def = CardDef::new(
        "Painter's Servant",
        CardKind::Creature(CreatureData::new("2", 1, 3)),
        vec![],
        Some(40),
        vec![], CardLayout::Normal, None,
        vec![],
        vec![repl],
        vec![],
        vec![],
    );
    // Painter's Servant is an Artifact Creature; the constructor derives only one type from
    // CardKind, so we push the second type explicitly.
    def.types.push(CardType::Artifact);
    def
}

/// Enchantment for {2BB}. Replacement: any card going to any graveyard goes to exile instead.
fn leyline_of_the_void() -> CardDef {
    let replacement = ReplacementDef {
        check: Arc::new(leyline_check),
        make_effect: Arc::new(|_source_id, controller: PlayerId| {
            Effect(Arc::new(move |state, t, targets| {
                if let Some(&id) = targets.first() {
                    change_zone(id, ZoneId::Exile, state, t, controller);
                }
            }))
        }),
    };
    CardDef::new(
        "Leyline of the Void",
        CardKind::Enchantment,
        parse_colors("2BB", false, true),
        None,
        vec![], CardLayout::Normal, None,
        vec![], vec![replacement], vec![], vec![],
    )
}

/// Flash, colorless artifact for {2}.
/// As this enters, choose a card name. Spells with that name cost {3} more to cast.
/// Activated abilities of sources with that name can't be activated unless they're mana abilities.
fn disruptor_flute() -> CardDef {
    CardDef::new(
        "Disruptor Flute",
        CardKind::Artifact(ArtifactData {
            mana_cost: "2".to_string(),
            ..Default::default()  // no activated abilities
        }),
        vec![],  // colorless
        Some(40),
        vec![], CardLayout::Normal, None,
        vec![],  // no trigger_defs
        vec![etb_self_replacement(|source_id, id, _controller, state, _t| {
            let f = Arc::clone(&state.resolve_choice);
            let ChoiceResult::CardName(chosen) =
                f(source_id, &ChoiceRequest::CardName, state) else { return };
            if let Some(bf) = state.permanent_bf_mut(id) {
                bf.etb_choice = Some(ChoiceResult::CardName(chosen.clone()));
            }
            // L3TextEffects CE: cost +3 and ability suppression for matching card name.
            let controller = state.objects.get(&id).map_or(PlayerId::Us, |o| o.controller);
            state.continuous_instances.push(ContinuousInstance {
                source_id, controller,
                layer: ContinuousLayer::L3TextEffects,
                filter: Arc::new(|_, _| true),
                modifier: Arc::new(move |def, _| {
                    if def.name == chosen {
                        def.casting_cost_modifier += 3;
                        def.non_mana_abilities_suppressed = true;
                    }
                }),
                expiry: ContinuousExpiry::WhileSourceOnBattlefield,
            });
        })],
        vec![],  // no prohibition_defs
        vec![],  // no static_ability_defs
    )
}

/// Front: Brazen Borrower — 3/1 flying creature for {1UU}.
/// Back (adventure): Petty Theft — instant for {1U}, bounce a nonland permanent. CR 715.
// ── Tokens ────────────────────────────────────────────────────────────────────

/// 0/0 Orc Army creature token. Created and grown by Amass Orcs. CR 701.45.
fn orc_army_token() -> CardDef {
    simple("Orc Army", CardKind::Creature(CreatureData::new("", 0, 0)), vec![], None)
}

/// Colorless Clue artifact token. Activated ability: {2}, tap self, sacrifice self → draw one.
/// CR 701.28 (Investigate).
fn clue_token() -> CardDef {
    simple("Clue Token", CardKind::Artifact(ArtifactData {
        mana_cost: String::new(),
        abilities: vec![AbilityDef {
            costs: vec![
                CostComponent::Mana(parse_mana_cost("2")),
                CostComponent::TapSelf,
                CostComponent::SacSelf,
            ],
            ability_factory: Some(Arc::new(|who, _| eff_draw(who, 1))),
            ..Default::default()
        }],
        ..Default::default()
    }), vec![], None)
}

/// Creature — Ape Spirit, 2/2. {2}{R}.
/// "Exile this card from your hand: Add {R}." — hand-zone mana ability (CR 605.3).
fn simian_spirit_guide() -> CardDef {
    let mut data = CreatureData::new("2R", 2, 2);
    data.mana_abilities = vec![ManaAbility {
        source_zone: SourceZone::Hand,
        costs: vec![CostComponent::ExileSelf],
        produces: produces_colors("R"),
        produces_count: 1,
        make_effect: std::sync::Arc::new(|who, _color| eff_mana(who, "R")),
    }];
    simple("Simian Spirit Guide", CardKind::Creature(data), parse_colors("R", false, false), None)
}

fn brazen_borrower() -> CardDef {
    let back = simple(
        "Petty Theft",
        CardKind::Instant(SpellData {
            mana_cost: "1U".to_string(),
            target_spec: TargetSpec::ObjectInZone {
                controller: Who::Opp,
                zone: ZoneId::Battlefield,
                filter: obj_pred_from_card(pred_not(pred_type_eq(CardType::Land))),
            },
            subtypes: vec!["adventure".to_string()],
            spell_factory: Some(Arc::new(|who, _source_id, _x| eff_bounce_target(who))),
            ..Default::default()
        }),
        parse_colors("1UU", true, false),
        None,
    );

    let mut data = CreatureData::new("1UU", 3, 1);
    data.legendary = false;
    simple(
        "Brazen Borrower",
        CardKind::Creature(data),
        parse_colors("1UU", true, false),
        None,
    );

    CardDef::new(
        "Brazen Borrower",
        CardKind::Creature(CreatureData::new("1UU", 3, 1)),
        parse_colors("1UU", true, false),
        None,
        vec![], CardLayout::Split, Some(Box::new(back)),
        vec![], vec![], vec![], vec![],
    )
}
