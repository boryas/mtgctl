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
        // Lands
        underground_sea(),
        swamp(),
        island(),
        undercity_sewers(),
        wasteland(),
        polluted_delta(),
        flooded_strand(),
        misty_rainforest(),
        scalding_tarn(),
        marsh_flats(),
        bloodstained_mire(),
        cavern_of_souls(),
        // Artifacts
        lotus_petal(),
        lions_eye_diamond(),
        ursas_saga(),
        // Spells — instants
        brainstorm(),
        consider(),
        daze(),
        force_of_will(),
        dark_ritual(),
        fatal_push(),
        snuff_out(),
        // Spells — sorceries
        doomsday(),
        ponder(),
        thoughtseize(),
        unearth(),
        hymn_to_tourach(),
        edge_of_autumn(),
        personal_tutor(),
        green_suns_zenith(),
        // Creatures
        thassas_oracle(),
        street_wraith(),
        barrowgoyf(),
        ingenious_infiltrator(),
        kaito_bane_of_nightmares(),
        recruiter_of_the_guard(),
        orcish_bowmasters(),
        murktide_regent(),
        // DFCs / split
        tamiyo_inquisitive_student(),
        brazen_borrower(),
        // Opponent archetypes / hate cards
        leyline_of_the_void(),
    ]
}

// ── Local helpers ─────────────────────────────────────────────────────────────

/// `CardDef` with no supertypes, normal layout, no back, no triggers/replacements/statics.
fn simple(name: &str, kind: CardKind, colors: Vec<Color>, play_weight: Option<u32>) -> CardDef {
    CardDef::new(
        name, kind, colors, play_weight,
        vec![], CardLayout::Normal, None, vec![], vec![], vec![],
    )
}

/// `ManaAbility` that taps self and produces the given mana string (e.g. `"U"`, `"B"`).
fn tap_produces(produces: impl Into<String>) -> ManaAbility {
    ManaAbility { tap_self: true, produces: produces.into(), ..Default::default() }
}

/// `AbilityDef` for a fetch land: sacrifice self, pay 1 life, search → Battlefield.
fn fetch_ability(pred: CardPredicate) -> AbilityDef {
    AbilityDef {
        sacrifice_self: true,
        life_cost: 1,
        ability_factory: Some(Arc::new(move |who, _| {
            eff_fetch_search(who, pred.clone(), ZoneId::Battlefield)
        })),
        ..Default::default()
    }
}

// ── Lands ─────────────────────────────────────────────────────────────────────

fn underground_sea() -> CardDef {
    simple("Underground Sea", CardKind::Land(LandData {
        land_types: LandTypes { island: true, swamp: true, ..Default::default() },
        mana_abilities: vec![tap_produces("U"), tap_produces("B")],
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
        vec![], vec![], vec![],
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
        vec![], vec![], vec![],
    )
}

/// Enters tapped. CR 614.1 (replacement effect): replaces the ETB event to set tapped=true.
fn undercity_sewers() -> CardDef {
    CardDef::new(
        "Undercity Sewers",
        CardKind::Land(LandData {
            land_types: LandTypes { island: true, swamp: true, ..Default::default() },
            mana_abilities: vec![tap_produces("U"), tap_produces("B")],
            ..Default::default()
        }),
        vec![], None, vec![], CardLayout::Normal, None,
        vec![],
        vec![replacement_enters_tapped()],
        vec![],
    )
}

/// {T}, Sacrifice: destroy target nonbasic land. CR 701.7.
fn wasteland() -> CardDef {
    simple("Wasteland", CardKind::Land(LandData {
        abilities: vec![AbilityDef {
            tap_self: true,
            sacrifice_self: true,
            target_spec: target_spec_from_str(Some("opp:nonbasic_land")),
            ability_factory: Some(Arc::new(|who, _| eff_destroy_target(who))),
            ..Default::default()
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

/// Produces generic mana only (no colored pips). CR 106.
fn cavern_of_souls() -> CardDef {
    simple("Cavern of Souls", CardKind::Land(LandData {
        mana_abilities: vec![ManaAbility { tap_self: true, produces: String::new(), ..Default::default() }],
        ..Default::default()
    }), vec![], Some(50))
}

// ── Artifacts ─────────────────────────────────────────────────────────────────

/// Sacrifice: add one mana of any color. CR 106.3.
fn lotus_petal() -> CardDef {
    simple("Lotus Petal", CardKind::Artifact(ArtifactData {
        mana_cost: "0".to_string(),
        mana_abilities: vec![ManaAbility { sacrifice_self: true, produces: "WUBRG".to_string(), ..Default::default() }],
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
            sacrifice_self: true,
            ability_factory: Some(Arc::new(move |who, _| {
                eff_fetch_search(who, pred.clone(), ZoneId::Battlefield)
            })),
            ..Default::default()
        }],
        ..Default::default()
    }), vec![], None)
}

// ── Instants ──────────────────────────────────────────────────────────────────

/// Draw 3, put back 2. CR 420 (draw), CR 701.26 (library manipulation).
fn brainstorm() -> CardDef {
    simple("Brainstorm", CardKind::Instant(SpellData {
        mana_cost: "U".to_string(),
        exileable: true,
        spell_factory: Some(Arc::new(|who| {
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
        spell_factory: Some(Arc::new(|who| eff_draw(who, 1))),
        ..Default::default()
    }), parse_colors("U", false, false), None)
}

/// Counter target spell. Alternate costs: bounce a blue-producing island (free),
/// or pay {1U} (20% probability). CR 701.5.
fn daze() -> CardDef {
    simple("Daze", CardKind::Instant(SpellData {
        mana_cost: "U".to_string(),
        exileable: true,
        // blue=true so it can be pitched to Force of Will
        target_spec: target_spec_from_str(Some("stack:any")),
        alternate_costs: vec![
            AlternateCost { bounce_island: true, hand_min: 1, ..Default::default() },
            AlternateCost { mana_cost: "1U".to_string(), hand_min: 1, prob: Some(0.2), ..Default::default() },
        ],
        spell_factory: Some(Arc::new(|who| eff_counter_target(who))),
        ..Default::default()
    }), parse_colors("U", true, false), None)
}

/// Counter target spell. Alternate costs: exile a blue card from hand + pay 1 life (pitch),
/// or pay {3UU} (hard cost, rare). CR 702.14 (pitch cost), CR 701.5.
fn force_of_will() -> CardDef {
    simple("Force of Will", CardKind::Instant(SpellData {
        mana_cost: "3UU".to_string(),
        target_spec: target_spec_from_str(Some("stack:any")),
        alternate_costs: vec![
            AlternateCost { exile_blue_from_hand: true, life_cost: 1, hand_min: 2, ..Default::default() },
            AlternateCost { mana_cost: "3UU".to_string(), hand_min: 1, ..Default::default() },
        ],
        spell_factory: Some(Arc::new(|who| eff_counter_target(who))),
        ..Default::default()
    }), parse_colors("3UU", true, false), None)
}

/// Add {B}{B}{B}. CR 106.3.
fn dark_ritual() -> CardDef {
    simple("Dark Ritual", CardKind::Instant(SpellData {
        mana_cost: "B".to_string(),
        spell_factory: Some(Arc::new(|who| eff_mana(who, "BBB"))),
        ..Default::default()
    }), parse_colors("B", false, false), None)
}

/// Destroy target creature with MV ≤ 3. CR 701.7.
fn fatal_push() -> CardDef {
    simple("Fatal Push", CardKind::Instant(SpellData {
        mana_cost: "B".to_string(),
        target_spec: target_spec_from_str(Some("opp:creature_mv_lt4")),
        spell_factory: Some(Arc::new(|who| eff_destroy_target(who))),
        ..Default::default()
    }), parse_colors("B", false, false), None)
}

/// Destroy target non-black creature. Alternate cost: pay 4 life (free spell). CR 701.7.
fn snuff_out() -> CardDef {
    simple("Snuff Out", CardKind::Instant(SpellData {
        mana_cost: "3BB".to_string(),
        target_spec: target_spec_from_str(Some("opp:creature_nonblack")),
        alternate_costs: vec![
            AlternateCost { life_cost: 4, ..Default::default() },
        ],
        spell_factory: Some(Arc::new(|who| eff_destroy_target(who))),
        ..Default::default()
    }), parse_colors("3BB", false, true), None)
}

// ── Sorceries ─────────────────────────────────────────────────────────────────

/// Win condition: set success=true. In full rules: opponent's library and graveyard become
/// their library; controller searches for exactly five cards. CR 101.1 (shortcut).
fn doomsday() -> CardDef {
    simple("Doomsday", CardKind::Sorcery(SpellData {
        mana_cost: "BBB".to_string(),
        spell_factory: Some(Arc::new(|_who| eff_doomsday())),
        ..Default::default()
    }), parse_colors("BBB", false, false), None)
}

/// Look at top 3, put one in hand, rest on bottom in any order. Modeled as draw:1. CR 701.26.
fn ponder() -> CardDef {
    simple("Ponder", CardKind::Sorcery(SpellData {
        mana_cost: "U".to_string(),
        exileable: true,
        spell_factory: Some(Arc::new(|who| eff_draw(who, 1))),
        ..Default::default()
    }), parse_colors("U", false, false), None)
}

/// Target opponent discards a nonland card; you lose 2 life. CR 701.8, CR 702.1.
fn thoughtseize() -> CardDef {
    simple("Thoughtseize", CardKind::Sorcery(SpellData {
        mana_cost: "B".to_string(),
        spell_factory: Some(Arc::new(|who| {
            eff_discard(who, Who::Opp, 1, "nonland")
                .then(eff_life_loss(who, 2))
        })),
        ..Default::default()
    }), parse_colors("B", false, false), None)
}

/// Return target creature from your graveyard to play. CR 701.14.
fn unearth() -> CardDef {
    simple("Unearth", CardKind::Sorcery(SpellData {
        mana_cost: "B".to_string(),
        target_spec: target_spec_from_str(Some("self:gy:creature")),
        spell_factory: Some(Arc::new(|who| eff_reanimate(who))),
        ..Default::default()
    }), parse_colors("B", false, false), None)
}

/// Target opponent discards 2 cards at random. CR 701.8.
fn hymn_to_tourach() -> CardDef {
    simple("Hymn to Tourach", CardKind::Sorcery(SpellData {
        mana_cost: "BB".to_string(),
        spell_factory: Some(Arc::new(|who| eff_discard(who, Who::Opp, 2, ""))),
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
        spell_factory: Some(Arc::new(|who| {
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
        spell_factory: Some(Arc::new(|who| {
            eff_fetch_search(
                who,
                pred_and(pred_type_eq(CardType::Creature), pred_has_color(Color::Green)),
                ZoneId::Battlefield,
            )
        })),
        ..Default::default()
    }), parse_colors("1G", false, false), None)
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
        zone: "hand".to_string(),
        discard_self: true,
        life_cost: 2,
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
        vec![Arc::new(recruiter_check)],
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
        vec![Arc::new(bowmasters_check)],
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
        vec![Arc::new(murktide_check)],
        vec![ReplacementDef {
            check: murktide_etb_check,
            make_effect: Arc::new(|source_id, controller: &str| {
                let ctl = controller.to_string();
                Effect(Arc::new(move |state, t, targets, rng| {
                    let Some(&id) = targets.first() else { return; };
                    let exile_count = state.exile_of(&ctl)
                        .filter(|c| state.def_of(c.id)
                            .map_or(false, |d| d.is_instant() || d.is_sorcery()))
                        .count() as i32;
                    if let Some(bf) = state.permanent_bf_mut(id) {
                        bf.counters = exile_count;
                    }
                    fire_event(
                        GameEvent::ZoneChange {
                            id,
                            actor: ctl.clone(),
                            from: ZoneId::Stack,
                            to: ZoneId::Battlefield,
                            controller: ctl.clone(),
                        },
                        state, t, &ctl, rng,
                    );
                }))
            }),
        }],
        vec![],
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
                loyalty_cost: Some(2),
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
    );

    let mut front_data = CreatureData::new("U", 0, 3);
    front_data.legendary = true;

    CardDef::new(
        "Tamiyo, Inquisitive Student",
        CardKind::Creature(front_data),
        parse_colors("U", false, false),
        None,
        vec![Supertype::Legendary], CardLayout::DoubleFaced, Some(Box::new(back)),
        vec![Arc::new(tamiyo_check)],
        vec![],
        vec![],
    )
}

/// Enchantment for {2BB}. Replacement: any card going to any graveyard goes to exile instead.
fn leyline_of_the_void() -> CardDef {
    let replacement = ReplacementDef {
        check: leyline_check,
        make_effect: Arc::new(|_source_id, controller: &str| {
            let ctl = controller.to_string();
            Effect(Arc::new(move |state, t, targets, rng| {
                if let Some(&id) = targets.first() {
                    change_zone(id, ZoneId::Exile, state, t, &ctl, rng);
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
        vec![], vec![replacement], vec![],
    )
}

/// Front: Brazen Borrower — 3/1 flying creature for {1UU}.
/// Back (adventure): Petty Theft — instant for {1U}, bounce a nonland permanent. CR 715.
fn brazen_borrower() -> CardDef {
    let back = simple(
        "Petty Theft",
        CardKind::Instant(SpellData {
            mana_cost: "1U".to_string(),
            target_spec: target_spec_from_str(Some("opp:permanent_nonland")),
            subtypes: vec!["adventure".to_string()],
            spell_factory: Some(Arc::new(|who| eff_bounce_target(who))),
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
        vec![], vec![], vec![],
    )
}
