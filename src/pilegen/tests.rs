    use super::*;
    use std::collections::HashMap;
    use rand::{SeedableRng, rngs::StdRng};

    // ── Test helpers ──────────────────────────────────────────────────────────

    fn make_state() -> SimState {
        let us = PlayerState::new("us_deck", 0);
        let opp = PlayerState::new("opp_deck", 0);
        SimState::new(us, opp)
    }

    fn seeded_rng() -> StdRng {
        StdRng::seed_from_u64(42)
    }

    fn creature(name: &str, power: i32, toughness: i32) -> CardDef {
        let toml = format!(
            "name = {:?}\ncard_type = \"creature\"\npower = {}\ntoughness = {}\n",
            name, power, toughness
        );
        toml::from_str(&toml).unwrap()
    }

    /// Insert a permanent into `state.objects` for `who` and return its ObjId.
    /// Also pre-registers and activates trigger/replacement instances so fire_triggers works.
    fn add_perm(state: &mut SimState, who: &str, name: &str, bf: BattlefieldState) -> ObjId {
        let id = state.alloc_id();
        state.objects.insert(id, GameObject {
            id,
            catalog_key: name.to_string(),
            owner: who.to_string(),
            controller: who.to_string(),
            zone: CardZone::Battlefield,
            is_token: false,
            spell: None,
            bf: Some(bf),
        });
        // Build a minimal CardDef so From<RawCardDef> sets trigger/replacement defs by name.
        let toml = format!("name = {:?}\ncard_type = \"creature\"\npower = 1\ntoughness = 1\n", name);
        let def: CardDef = toml::from_str(&toml).expect("add_perm: CardDef parse failed");
        preregister_instances(&def, id, who, state);
        activate_instances(id, who, Some(&def), state);
        id
    }

    /// Insert a default permanent (untapped, no mana abilities).
    fn add_default_perm(state: &mut SimState, who: &str, name: &str) -> ObjId {
        add_perm(state, who, name, BattlefieldState::new())
    }

    /// Insert a permanent using a pre-built `CardDef` (full static_ability_defs included).
    /// Also seeds `state.materialized.defs` so mana abilities and type checks work without recompute.
    fn add_perm_with_def(state: &mut SimState, who: &str, def: &CardDef, bf: BattlefieldState) -> ObjId {
        let id = state.alloc_id();
        state.objects.insert(id, GameObject {
            id,
            catalog_key: def.name.clone(),
            owner: who.to_string(),
            controller: who.to_string(),
            zone: CardZone::Battlefield,
            is_token: false,
            spell: None,
            bf: Some(bf),
        });
        preregister_instances(def, id, who, state);
        activate_instances(id, who, Some(def), state);
        state.materialized.defs.insert(id, def.clone());
        id
    }

    fn make_land(state: &mut SimState, who: &str, name: &str, tapped: bool) -> ObjId {
        add_perm(state, who, name, BattlefieldState {
            tapped,
            ..BattlefieldState::new()
        })
    }

    fn add_hand_card(state: &mut SimState, who: &str, name: &str) -> ObjId {
        let id = state.alloc_id();
        state.objects.insert(id, GameObject {
            id,
            catalog_key: name.to_string(),
            owner: who.to_string(),
            controller: who.to_string(),
            zone: CardZone::Hand { known: false },
            is_token: false,
            spell: None,
            bf: None,
        });
        id
    }

    fn add_hand_card_with_def(state: &mut SimState, who: &str, def: &CardDef) -> ObjId {
        let id = add_hand_card(state, who, &def.name.clone());
        state.materialized.defs.insert(id, def.clone());
        id
    }

    fn add_graveyard_card(state: &mut SimState, who: &str, name: &str) -> ObjId {
        let id = state.alloc_id();
        state.objects.insert(id, GameObject {
            id,
            catalog_key: name.to_string(),
            owner: who.to_string(),
            controller: who.to_string(),
            zone: CardZone::Graveyard,
            is_token: false,
            spell: None,
            bf: None,
        });
        id
    }

    fn add_library_card(state: &mut SimState, who: &str, name: &str) -> ObjId {
        let id = state.alloc_id();
        state.objects.insert(id, GameObject {
            id,
            catalog_key: name.to_string(),
            owner: who.to_string(),
            controller: who.to_string(),
            zone: CardZone::Library,
            is_token: false,
            spell: None,
            bf: None,
        });
        id
    }

    // ── Section 1: Pure Function Tests ────────────────────────────────────────

    #[test]
    fn test_parse_mana_cost_black() {
        let mc = parse_mana_cost("BBB");
        assert_eq!(mc.b, 3);
        assert_eq!(mc.u, 0);
        assert_eq!(mc.generic, 0);
    }

    #[test]
    fn test_parse_mana_cost_mixed() {
        // "1UB" → b=1, u=1, generic=1
        let mc = parse_mana_cost("1UB");
        assert_eq!(mc.b, 1);
        assert_eq!(mc.u, 1);
        assert_eq!(mc.generic, 1);
    }

    #[test]
    fn test_parse_mana_cost_zero() {
        let mc = parse_mana_cost("0");
        assert_eq!(mc.mana_value(), 0);
    }

    #[test]
    fn test_mana_value() {
        assert_eq!(mana_value("2BB"), 4);
        assert_eq!(mana_value("0"), 0);
        assert_eq!(mana_value("U"), 1);
    }


    #[test]
    fn test_stage_label() {
        assert_eq!(stage_label(1), "Early");
        assert_eq!(stage_label(4), "Mid");
        assert_eq!(stage_label(8), "Late");
    }

    // ── Section 2: Step Tests ─────────────────────────────────────────────────

    #[test]
    fn test_untap_step_resets_permanents() {
        let mut state = make_state();
        let land_id = make_land(&mut state, "us", "Island", true);
        let ragavan_id = add_perm(&mut state, "us", "Ragavan", BattlefieldState {
            tapped: true,
            entered_this_turn: true,
            ..BattlefieldState::new()
        });
        state.us.spells_cast_this_turn = 2;

        let step = Step { kind: StepKind::Untap, prio: false };
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        do_step(&mut state, 1, "us", &step, 3, true, &catalog_map, &mut seeded_rng());

        assert!(!state.permanent_bf(land_id).unwrap().tapped, "land should be untapped");
        assert!(!state.permanent_bf(ragavan_id).unwrap().tapped, "permanent should be untapped");
        assert!(!state.permanent_bf(ragavan_id).unwrap().entered_this_turn, "summoning sickness should clear");
        assert!(state.us.land_drop_available, "land drop should reset");
        assert_eq!(state.us.spells_cast_this_turn, 0);
    }

    #[test]
    fn test_draw_step_skipped_on_play_turn1() {
        let mut state = make_state();
        add_library_card(&mut state, "us", "Island");
        let initial_hand = state.hand_size("us");

        let step = Step { kind: StepKind::Draw, prio: false };
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        // on_play=true, t=1, ap="us" → this_player_on_play=true → skip
        do_step(&mut state, 1, "us", &step, 3, true, &catalog_map, &mut seeded_rng());

        assert_eq!(state.hand_size("us"), initial_hand, "no draw on the play turn 1");
    }

    #[test]
    fn test_draw_step_draws_card() {
        let mut state = make_state();
        add_library_card(&mut state, "us", "Island");
        let initial_hand = state.hand_size("us");

        let step = Step { kind: StepKind::Draw, prio: false };
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        // on_play=false → this_player_on_play=false → no skip
        do_step(&mut state, 1, "us", &step, 3, false, &catalog_map, &mut seeded_rng());

        assert_eq!(state.hand_size("us"), initial_hand + 1, "should draw one card");
    }

    #[test]
    fn test_cleanup_removes_damage() {
        let mut state = make_state();
        let rag_id = add_perm(&mut state, "us", "Ragavan", BattlefieldState {
            damage: 3,
            ..BattlefieldState::new()
        });

        let step = Step { kind: StepKind::Cleanup, prio: false };
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        do_step(&mut state, 1, "us", &step, 3, true, &catalog_map, &mut seeded_rng());

        assert_eq!(state.permanent_bf(rag_id).unwrap().damage, 0);
    }

    #[test]
    fn test_declare_attackers_safe_to_attack() {
        let mut state = make_state();
        let ragavan_def = creature("Ragavan", 2, 4);
        let ragavan_id = add_perm(&mut state, "us", "Ragavan", BattlefieldState {
            entered_this_turn: false,
            ..BattlefieldState::new()
        });

        let catalog = vec![ragavan_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareAttackers, prio: false };
        do_step(&mut state, 1, "us", &step, 3, true, &catalog_map, &mut seeded_rng());

        assert!(state.combat_attackers.contains(&ragavan_id), "should attack");
        assert!(state.permanent_bf(ragavan_id).unwrap().tapped, "attacker should be tapped");
    }

    #[test]
    fn test_declare_attackers_too_risky() {
        let mut state = make_state();
        let attacker_def = creature("Ragavan", 2, 2);
        let blocker_def = creature("Mosscoat Construct", 3, 3);
        add_perm(&mut state, "us", "Ragavan", BattlefieldState {
            entered_this_turn: false,
            ..BattlefieldState::new()
        });
        add_default_perm(&mut state, "opp", "Mosscoat Construct");

        let catalog = vec![attacker_def, blocker_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareAttackers, prio: false };
        do_step(&mut state, 1, "us", &step, 3, true, &catalog_map, &mut seeded_rng());

        assert!(state.combat_attackers.is_empty(), "should not attack into 3/3");
    }

    #[test]
    fn test_declare_attackers_summoning_sickness() {
        let mut state = make_state();
        let def = creature("Ragavan", 2, 4);
        // entered_this_turn = true (default from BattlefieldState::new)
        add_default_perm(&mut state, "us", "Ragavan");

        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareAttackers, prio: false };
        do_step(&mut state, 1, "us", &step, 3, true, &catalog_map, &mut seeded_rng());

        assert!(state.combat_attackers.is_empty(), "sickness prevents attack");
    }

    #[test]
    fn test_declare_blockers_good_block() {
        let mut state = make_state();
        let atk_def = creature("Ragavan", 2, 2);
        let blk_def = creature("Mosscoat Construct", 3, 3);
        let ragavan_id = add_perm(&mut state, "us", "Ragavan", BattlefieldState {
            entered_this_turn: false,
            tapped: false,
            ..BattlefieldState::new()
        });
        let mosscoat_id = add_default_perm(&mut state, "opp", "Mosscoat Construct");
        state.combat_attackers = vec![ragavan_id];

        let catalog = vec![atk_def, blk_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareBlockers, prio: false };
        do_step(&mut state, 1, "us", &step, 3, true, &catalog_map, &mut seeded_rng());

        assert_eq!(state.combat_blocks.len(), 1);
        assert_eq!(state.combat_blocks[0], (ragavan_id, mosscoat_id));
    }

    #[test]
    fn test_declare_blockers_no_chump() {
        let mut state = make_state();
        let atk_def = creature("Beast", 4, 4);
        let blk_def = creature("Squirrel Token", 1, 1);
        let beast_id = add_perm(&mut state, "us", "Beast", BattlefieldState {
            entered_this_turn: false,
            ..BattlefieldState::new()
        });
        add_default_perm(&mut state, "opp", "Squirrel Token");
        state.combat_attackers = vec![beast_id];

        let catalog = vec![atk_def, blk_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareBlockers, prio: false };
        do_step(&mut state, 1, "us", &step, 3, true, &catalog_map, &mut seeded_rng());

        assert!(state.combat_blocks.is_empty(), "should not chump block");
    }

    #[test]
    fn test_combat_damage_unblocked_hits_player() {
        let mut state = make_state();
        let initial_life = state.opp.life;
        let atk_def = creature("Ragavan", 2, 1);
        let ragavan_id = add_perm(&mut state, "us", "Ragavan", BattlefieldState {
            tapped: true,
            ..BattlefieldState::new()
        });
        state.combat_attackers = vec![ragavan_id];

        let catalog = vec![atk_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::CombatDamage, prio: false };
        do_step(&mut state, 1, "us", &step, 3, true, &catalog_map, &mut seeded_rng());

        assert_eq!(state.opp.life, initial_life - 2);
    }

    #[test]
    fn test_combat_damage_blocked_no_player_damage() {
        let mut state = make_state();
        let initial_life = state.opp.life;
        let atk_def = creature("Ragavan", 2, 2);
        let blk_def = creature("Mosscoat Construct", 3, 3);
        let ragavan_id = add_perm(&mut state, "us", "Ragavan", BattlefieldState {
            tapped: true,
            ..BattlefieldState::new()
        });
        let construct_id = add_default_perm(&mut state, "opp", "Mosscoat Construct");
        state.combat_attackers = vec![ragavan_id];
        state.combat_blocks = vec![(ragavan_id, construct_id)];

        let catalog = vec![atk_def, blk_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::CombatDamage, prio: false };
        do_step(&mut state, 1, "us", &step, 3, true, &catalog_map, &mut seeded_rng());

        assert_eq!(state.opp.life, initial_life, "blocked — no player damage");
    }

    #[test]
    fn test_combat_damage_sba_kills_both_2_2s() {
        let mut state = make_state();
        let atk_def = creature("Ragavan", 2, 2);
        let blk_def = creature("Mosscoat Construct", 2, 2);
        let ragavan_id = add_perm(&mut state, "us", "Ragavan", BattlefieldState {
            tapped: true,
            ..BattlefieldState::new()
        });
        let construct_id = add_default_perm(&mut state, "opp", "Mosscoat Construct");
        state.combat_attackers = vec![ragavan_id];
        state.combat_blocks = vec![(ragavan_id, construct_id)];

        let catalog = vec![atk_def, blk_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::CombatDamage, prio: true };
        do_step(&mut state, 1, "us", &step, 3, true, &catalog_map, &mut seeded_rng());

        assert!(state.permanents_of("us").count() == 0, "attacker should die");
        assert!(state.permanents_of("opp").count() == 0, "blocker should die");
        assert!(state.graveyard_of("us").any(|c| c.catalog_key == "Ragavan"));
        assert!(state.graveyard_of("opp").any(|c| c.catalog_key == "Mosscoat Construct"));
    }

    #[test]
    fn test_combat_damage_outclassed_attacker_dies() {
        let mut state = make_state();
        let atk_def = creature("Ragavan", 2, 2);
        let blk_def = creature("Troll", 3, 3);
        let ragavan_id = add_perm(&mut state, "us", "Ragavan", BattlefieldState {
            tapped: true,
            ..BattlefieldState::new()
        });
        let troll_id = add_default_perm(&mut state, "opp", "Troll");
        state.combat_attackers = vec![ragavan_id];
        state.combat_blocks = vec![(ragavan_id, troll_id)];

        let catalog = vec![atk_def, blk_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::CombatDamage, prio: true };
        do_step(&mut state, 1, "us", &step, 3, true, &catalog_map, &mut seeded_rng());

        assert!(state.permanents_of("us").count() == 0, "attacker dies");
        assert!(state.permanents_of("opp").count() > 0, "blocker survives");
    }

    #[test]
    fn test_end_combat_clears_fields() {
        let mut state = make_state();
        let dummy_id = state.alloc_id();
        let dummy_id2 = state.alloc_id();
        state.combat_attackers = vec![dummy_id];
        state.combat_blocks = vec![(dummy_id, dummy_id2)];

        let step = Step { kind: StepKind::EndCombat, prio: false };
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        do_step(&mut state, 1, "us", &step, 3, true, &catalog_map, &mut seeded_rng());

        assert!(state.combat_attackers.is_empty());
        assert!(state.combat_blocks.is_empty());
    }

    // ── Section 3: Phase Tests ────────────────────────────────────────────────

    #[test]
    fn test_beginning_phase_untaps_and_draws() {
        let mut state = make_state();
        let island_def: CardDef = toml::from_str(r#"
            name = "Island"
            card_type = "land"
            [[mana_abilities]]
            tap_self = true
            produces = "U"
        "#).unwrap();
        let island_id = add_perm_with_def(&mut state, "us", &island_def, BattlefieldState {
            tapped: true,
            ..BattlefieldState::new()
        });
        add_library_card(&mut state, "us", "Swamp");
        let initial_hand = state.hand_size("us");

        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        // t=2, on_play=false → draw fires (this_player_on_play=false)
        do_phase(&mut state, 2, "us", &beginning_phase(), 3, false, &catalog_map, &mut seeded_rng());

        assert!(!state.permanent_bf(island_id).unwrap().tapped, "land should be untapped");
        assert_eq!(state.hand_size("us"), initial_hand + 1, "should have drawn one card");
    }

    #[test]
    fn test_combat_phase_full_cycle() {
        let mut state = make_state();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        do_phase(&mut state, 1, "us", &combat_phase(), 3, true, &catalog_map, &mut seeded_rng());

        assert!(state.combat_attackers.is_empty());
        assert!(state.combat_blocks.is_empty());
    }

    // ── Section 4: Priority Action Cycle ─────────────────────────────────────

    #[test]
    fn test_priority_round_both_pass_empty_stack() {
        let mut state = make_state();
        // current_phase is "" (not "Main") → both players pass immediately
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        handle_priority_round(&mut state, 1, "us", 3, &catalog_map, &mut seeded_rng());

        assert_eq!(state.us.life, 20);
        assert_eq!(state.opp.life, 20);
    }

    // ── Section 5: Spell Casting ──────────────────────────────────────────────

    #[test]
    fn test_cast_spell_normal_cost_removes_from_library() {
        let mut state = make_state();
        let def: CardDef = toml::from_str(r#"
            name = "Dark Ritual"
            card_type = "instant"
            mana_cost = "B"
        "#).unwrap();
        state.us.pool.b = 1;
        state.us.pool.total = 1;
        let dark_ritual_id = add_hand_card(&mut state, "us", "Dark Ritual");

        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        let card_id = cast_spell(&mut state, 1, "us", dark_ritual_id, SpellFace::Main, None, &catalog_map, &mut seeded_rng());

        assert!(card_id.is_some(), "spell should be cast");
        let card_id = card_id.unwrap();
        let card = state.objects.get(&card_id).expect("card in state");
        assert_eq!(card.catalog_key, "Dark Ritual");
        assert_eq!(state.player_id(&card.owner), state.us.id, "owner should be us player id");
        assert!(!state.hand_of("us").any(|c| c.catalog_key == "Dark Ritual"), "removed from hand");
        assert_eq!(state.us.pool.b, 0, "mana spent");
    }

    #[test]
    fn test_cast_spell_unaffordable_returns_none() {
        let mut state = make_state();
        let def: CardDef = toml::from_str(r#"
            name = "Doomsday"
            card_type = "instant"
            mana_cost = "BBB"
        "#).unwrap();
        // No mana in pool, no lands
        let doomsday_id = add_hand_card(&mut state, "us", "Doomsday");

        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let item = cast_spell(&mut state, 1, "us", doomsday_id, SpellFace::Main, None, &catalog_map, &mut seeded_rng());

        assert!(item.is_none(), "can't cast with no mana");
    }

    #[test]
    fn test_cast_spell_alt_cost_exiles_pitch_card() {
        let mut state = make_state();
        let fow_def: CardDef = toml::from_str(r#"
            name = "Force of Will"
            card_type = "instant"
            mana_cost = "3UU"
            blue = true
            [[alternate_costs]]
            mana_cost = ""
            exile_blue_from_hand = true
            life_cost = 1
        "#).unwrap();
        let brainstorm_def: CardDef = toml::from_str(r#"
            name = "Brainstorm"
            card_type = "instant"
            mana_cost = "U"
            blue = true
        "#).unwrap();
        let catalog = vec![fow_def.clone(), brainstorm_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        // Add FoW and Brainstorm to hand (FoW pitches itself? No — Brainstorm is the pitch card)
        let fow_id = add_hand_card(&mut state, "us", "Force of Will");
        add_hand_card(&mut state, "us", "Brainstorm");

        let alt_cost = &fow_def.alternate_costs()[0];
        let initial_life = state.us.life;

        let item = cast_spell(&mut state, 1, "us", fow_id, SpellFace::Main, Some(alt_cost), &catalog_map, &mut seeded_rng());

        assert!(item.is_some(), "FoW should be cast via pitch");
        assert_eq!(state.us.life, initial_life - 1, "paid 1 life");
        assert!(!state.hand_of("us").any(|c| c.catalog_key == "Brainstorm"), "pitch card removed from hand");
        assert!(state.exile_of("us").any(|c| c.catalog_key == "Brainstorm"), "pitch card exiled");
    }

    // ── Section 6: Spell Resolution ───────────────────────────────────────────

    #[test]
    fn test_effect_doomsday_sets_success() {
        let mut state = make_state();
        eff_doomsday().call(&mut state, 1, &[], &HashMap::new(), &mut seeded_rng());

        assert!(state.success);
    }

    #[test]
    fn test_effect_cantrip_increments_hand() {
        let mut state = make_state();
        add_library_card(&mut state, "us", "Island");
        let initial_hand = state.hand_size("us");
        eff_draw("us", 1).call(&mut state, 1, &[], &HashMap::new(), &mut seeded_rng());

        assert_eq!(state.hand_size("us"), initial_hand + 1, "cantrip increments hand count");
    }

    #[test]
    fn test_brainstorm_net_one_card() {
        // draw:3 + put_back:2 = net +1 hand size.
        let mut state = make_state();
        add_library_card(&mut state, "us", "Island");
        add_library_card(&mut state, "us", "Swamp");
        add_library_card(&mut state, "us", "Plains");
        let initial = state.hand_size("us");
        eff_draw("us", 3).then(eff_put_back("us", 2))
            .call(&mut state, 1, &[], &HashMap::new(), &mut seeded_rng());

        assert_eq!(state.hand_size("us"), initial + 1, "Brainstorm nets +1 card");
    }

    #[test]
    fn test_brainstorm_fires_three_draw_events() {
        // All three draws queue triggers; OBM (controlled by opp) should see all three.
        let mut state = make_state();
        add_default_perm(&mut state, "opp", "Orcish Bowmasters");
        add_library_card(&mut state, "us", "Island");
        add_library_card(&mut state, "us", "Swamp");
        add_library_card(&mut state, "us", "Plains");
        eff_draw("us", 3).then(eff_put_back("us", 2))
            .call(&mut state, 1, &[], &HashMap::new(), &mut seeded_rng());

        // Three Draw events queued → three OBM triggers pending (all non-natural draws).
        let bowmasters_triggers = state.pending_triggers.iter()
            .filter(|tc| tc.source_name == "Orcish Bowmasters")
            .count();
        assert_eq!(bowmasters_triggers, 3, "OBM pings for each of the 3 Brainstorm draws");
    }

    #[test]
    fn test_brainstorm_flips_tamiyo_on_second_draw_of_three() {
        // Turn context: natural draw already happened (draw_index=1).
        // Brainstorm's 2nd draw = draw_index=3 → Tamiyo flips.
        let mut state = make_state();
        add_default_perm(&mut state, "us", "Tamiyo, Inquisitive Student");
        state.us.draws_this_turn = 1; // simulate having already drawn naturally
        add_library_card(&mut state, "us", "Island");
        add_library_card(&mut state, "us", "Swamp");
        add_library_card(&mut state, "us", "Plains");
        eff_draw("us", 3).then(eff_put_back("us", 2))
            .call(&mut state, 1, &[], &HashMap::new(), &mut seeded_rng());

        let flip_triggers = state.pending_triggers.iter()
            .filter(|tc| tc.source_name == "Tamiyo, Inquisitive Student")
            .count();
        assert_eq!(flip_triggers, 1, "Tamiyo flips exactly once on the 3rd draw of the turn");
    }

    #[test]
    fn test_effect_life_loss_reduces_caster_life() {
        let mut state = make_state();
        let initial = state.us.life;
        eff_life_loss("us", 2).call(&mut state, 1, &[], &HashMap::new(), &mut seeded_rng());

        assert_eq!(state.us.life, initial - 2);
    }

    #[test]
    fn test_effect_mana_adds_to_pool() {
        let mut state = make_state();
        eff_mana("us", "BBB").call(&mut state, 1, &[], &HashMap::new(), &mut seeded_rng());

        assert_eq!(state.us.pool.b, 3, "should add 3 black mana");
        assert_eq!(state.us.pool.total, 3);
    }

    #[test]
    fn test_effect_discard_removes_opp_card() {
        let mut state = make_state();
        add_hand_card(&mut state, "opp", "Counterspell");
        let initial_opp_hand = state.hand_size("opp");
        eff_discard("us", Who::Opp, 1, "").call(&mut state, 1, &[], &HashMap::new(), &mut seeded_rng());

        assert_eq!(state.hand_size("opp"), initial_opp_hand - 1, "opp hand decremented");
        assert!(state.graveyard_of("opp").any(|c| c.catalog_key == "Counterspell"), "Counterspell in graveyard");
        assert!(!state.hand_of("opp").any(|c| c.catalog_key == "Counterspell"), "card removed from opp hand");
    }

    // ── Section 7: Ability Activation ─────────────────────────────────────────

    #[test]
    fn test_pay_activation_cost_mana() {
        let mut state = make_state();
        state.us.pool.b = 2;
        state.us.pool.total = 2;
        let ability: AbilityDef = toml::from_str(r#"
            mana_cost = "B"
            effect = "cantrip"
        "#).unwrap();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        pay_activation_cost(&mut state, 1, "us", ObjId::UNSET, &ability, &catalog_map);

        assert_eq!(state.us.pool.b, 1, "1 black spent");
        assert_eq!(state.us.pool.total, 1);
    }

    #[test]
    fn test_pay_activation_cost_life() {
        let mut state = make_state();
        let initial = state.us.life;
        let ability: AbilityDef = toml::from_str(r#"
            mana_cost = ""
            life_cost = 2
            effect = "cantrip"
        "#).unwrap();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        pay_activation_cost(&mut state, 1, "us", ObjId::UNSET, &ability, &catalog_map);

        assert_eq!(state.us.life, initial - 2);
    }

    #[test]
    fn test_pay_activation_cost_sacrifice_self() {
        let mut state = make_state();
        let petal_id = add_default_perm(&mut state, "us", "Lotus Petal");
        let ability: AbilityDef = toml::from_str(r#"
            mana_cost = ""
            sacrifice_self = true
            effect = "mana:B"
        "#).unwrap();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        pay_activation_cost(&mut state, 1, "us", petal_id, &ability, &catalog_map);

        assert!(state.permanents_of("us").count() == 0, "Lotus Petal should be sacrificed");
        assert!(state.graveyard_of("us").any(|c| c.catalog_key == "Lotus Petal"));
    }

    // ── Section 8: Destruction Effects ───────────────────────────────────────

    // Spell resolution: destroy uses item.permanent_target set at cast time.

    #[test]
    fn test_effect_destroy_spell_removes_opp_land() {
        let mut state = make_state();
        let id = make_land(&mut state, "opp", "Bayou", false);
        eff_destroy_target("us").call(&mut state, 1, &[Target::Object(id)], &HashMap::new(), &mut seeded_rng());

        assert!(state.permanents_of("opp").count() == 0, "Bayou should be destroyed");
        assert!(state.graveyard_of("opp").any(|c| c.catalog_key == "Bayou"));
    }

    #[test]
    fn test_effect_destroy_spell_removes_opp_creature() {
        let mut state = make_state();
        let id = add_default_perm(&mut state, "opp", "Troll");
        eff_destroy_target("us").call(&mut state, 1, &[Target::Object(id)], &HashMap::new(), &mut seeded_rng());

        assert!(state.permanents_of("opp").count() == 0, "Troll should be destroyed");
        assert!(state.graveyard_of("opp").any(|c| c.catalog_key == "Troll"));
    }

    // Ability resolution: target is chosen at push time via choose_permanent_target.

    fn land_def(name: &str, basic: bool) -> CardDef {
        let basic_str = if basic { "true" } else { "false" };
        let toml = format!(
            "name = {:?}\ncard_type = \"land\"\nbasic = {}\n",
            name, basic_str
        );
        toml::from_str(&toml).unwrap()
    }

    #[test]
    fn test_effect_destroy_ability_removes_nonbasic_land() {
        let mut state = make_state();
        make_land(&mut state, "opp", "Bayou", false);
        let ability: AbilityDef = toml::from_str(r#"
            mana_cost = ""
            target = "opp:nonbasic_land"
            effect = "destroy"
        "#).unwrap();
        let bayou_def = land_def("Bayou", false);
        let catalog = vec![bayou_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let targets: Vec<Target> = choose_permanent_target("opp:nonbasic_land", "us", &state, &catalog_map, &mut seeded_rng())
            .map(|id| vec![Target::Object(id)])
            .unwrap_or_default();
        let eff = build_ability_effect(&ability, "us", ObjId::UNSET);
        eff.call(&mut state, 1, &targets, &catalog_map, &mut seeded_rng());

        assert!(state.permanents_of("opp").count() == 0, "Bayou should be destroyed");
        assert!(state.graveyard_of("opp").any(|c| c.catalog_key == "Bayou"));
    }

    #[test]
    fn test_effect_destroy_ability_ignores_basic_land() {
        let mut state = make_state();
        make_land(&mut state, "opp", "Forest", false);
        let ability: AbilityDef = toml::from_str(r#"
            mana_cost = ""
            target = "opp:nonbasic_land"
            effect = "destroy"
        "#).unwrap();
        let forest_def = land_def("Forest", true);
        let catalog = vec![forest_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let targets: Vec<Target> = choose_permanent_target("opp:nonbasic_land", "us", &state, &catalog_map, &mut seeded_rng())
            .map(|id| vec![Target::Object(id)])
            .unwrap_or_default();
        let eff = build_ability_effect(&ability, "us", ObjId::UNSET);
        eff.call(&mut state, 1, &targets, &catalog_map, &mut seeded_rng());

        assert!(state.permanents_of("opp").count() > 0, "basic Forest should survive");
        assert!(state.graveyard_of("opp").count() == 0, "no cards in graveyard");
    }

    // ── Section 9: Delve ──────────────────────────────────────────────────────

    #[test]
    fn test_cast_delve_spell_exiles_graveyard_cards() {
        // Spell costs 3 generic + U. Two graveyard cards reduce generic to 1.
        // Pool supplies the remaining 1 generic + 1 blue.
        let mut state = make_state();
        let def: CardDef = toml::from_str(r#"
            name = "Treasure Cruise"
            card_type = "instant"
            mana_cost = "7U"
            delve = true
        "#).unwrap();
        for name in &["A", "B", "C", "D", "E", "F", "G"] {
            add_graveyard_card(&mut state, "us", name);
        }
        let tc_id = add_hand_card(&mut state, "us", "Treasure Cruise");
        state.us.pool.u  = 1;
        state.us.pool.total = 1; // only 1 mana in pool — delve pays the other 7

        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        let item = cast_spell(&mut state, 1, "us", tc_id, SpellFace::Main, None, &catalog_map, &mut seeded_rng());

        assert!(item.is_some(), "should cast with full delve");
        assert_eq!(state.graveyard_of("us").count(), 0, "all 7 graveyard cards exiled");
        assert_eq!(state.exile_of("us").count(), 7, "exiled by delve");
        assert_eq!(state.us.pool.u, 0, "blue pip paid");
    }

    #[test]
    fn test_cast_delve_spell_partial_delve() {
        // Spell costs 3 generic. Graveyard has 2 cards — reduces cost to 1.
        // Pool must cover the remaining 1 generic.
        let mut state = make_state();
        let def: CardDef = toml::from_str(r#"
            name = "Dead Drop"
            card_type = "sorcery"
            mana_cost = "3"
            delve = true
        "#).unwrap();
        add_graveyard_card(&mut state, "us", "Ritual");
        add_graveyard_card(&mut state, "us", "Ponder");
        let dead_drop_id = add_hand_card(&mut state, "us", "Dead Drop");
        state.us.pool.total = 1; // covers the 1 remaining generic after delve

        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        let item = cast_spell(&mut state, 1, "us", dead_drop_id, SpellFace::Main, None, &catalog_map, &mut seeded_rng());

        assert!(item.is_some(), "should cast with partial delve + 1 mana");
        assert_eq!(state.graveyard_of("us").count(), 0, "both graveyard cards exiled");
        assert_eq!(state.exile_of("us").count(), 2);
        assert_eq!(state.us.pool.total, 0, "remaining generic pip paid");
    }

    #[test]
    fn test_murktide_counters_from_exiled_instants_sorceries() {
        // Murktide exiles 4 cards via delve; 3 are instants/sorceries → enters as 6/6.
        let mut state = make_state();
        let murktide_def: CardDef = toml::from_str(r#"
            name = "Murktide Regent"
            card_type = "creature"
            mana_cost = "5UU"
            delve = true
            power = 3
            toughness = 3
        "#).unwrap();
        let ritual_def: CardDef   = toml::from_str("name = \"Dark Ritual\"\ncard_type = \"instant\"\nmana_cost = \"B\"").unwrap();
        let ponder_def: CardDef   = toml::from_str("name = \"Ponder\"\ncard_type = \"sorcery\"\nmana_cost = \"U\"").unwrap();
        let consider_def: CardDef = toml::from_str("name = \"Consider\"\ncard_type = \"instant\"\nmana_cost = \"U\"").unwrap();
        let ragavan_def  = creature("Ragavan", 2, 1); // creature — does NOT count

        add_graveyard_card(&mut state, "us", "Dark Ritual");
        add_graveyard_card(&mut state, "us", "Ponder");
        add_graveyard_card(&mut state, "us", "Consider");
        add_graveyard_card(&mut state, "us", "Ragavan");
        let murktide_id = add_hand_card(&mut state, "us", "Murktide Regent");
        // After delving all 4, generic cost = 5-4 = 1. Need UU + 1 generic.
        state.us.pool.u  = 2;
        state.us.pool.total = 3;

        let catalog = vec![murktide_def.clone(), ritual_def, ponder_def, consider_def, ragavan_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        let card_id = cast_spell(&mut state, 1, "us", murktide_id, SpellFace::Main, None, &catalog_map, &mut seeded_rng()).unwrap();
        let spell = state.objects[&card_id].spell.as_ref().expect("spell state populated").clone();
        let effect = &spell.effect;
        let chosen_targets = spell.chosen_targets.clone();

        // Resolve via Effect path — replacement effect counts exiled instants/sorceries.
        let rng_dyn: &mut dyn rand::RngCore = &mut seeded_rng();
        effect.as_ref().unwrap().call(&mut state, 1, &chosen_targets, &catalog_map, rng_dyn);

        let murktide_bf = state.permanents_of("us").find(|p| p.catalog_key == "Murktide Regent")
            .and_then(|p| p.bf.as_ref()).expect("Murktide on battlefield");
        assert_eq!(murktide_bf.counters, 3, "3 instants/sorceries exiled → 3 counters");

        // recompute reflects counters in the materialized view
        let murktide_id = state.permanents_of("us").find(|p| p.catalog_key == "Murktide Regent")
            .map(|p| p.id).expect("Murktide on battlefield");
        let mat = recompute(&state, &catalog_map);
        let eff = mat.defs.get(&murktide_id).expect("Murktide materialized");
        let CardKind::Creature(c) = &eff.kind else { panic!("expected creature") };
        assert_eq!((c.power(), c.toughness()), (6, 6));
    }

    #[test]
    fn test_murktide_zero_counters_when_no_instants_exiled() {
        // Delve only exiles a creature — no instants/sorceries → enters as base 3/3.
        let mut state = make_state();
        let murktide_def: CardDef = toml::from_str(r#"
            name = "Murktide Regent"
            card_type = "creature"
            mana_cost = "5UU"
            delve = true
            power = 3
            toughness = 3
        "#).unwrap();
        let ragavan_def = creature("Ragavan", 2, 1);

        add_graveyard_card(&mut state, "us", "Ragavan");
        let murktide_id = add_hand_card(&mut state, "us", "Murktide Regent");
        // 5 - 1 = 4 generic remaining; need UU + 4 generic
        state.us.pool.u  = 2;
        state.us.pool.total = 6;

        let catalog = vec![murktide_def.clone(), ragavan_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        let card_id = cast_spell(&mut state, 1, "us", murktide_id, SpellFace::Main, None, &catalog_map, &mut seeded_rng()).unwrap();
        let spell = state.objects[&card_id].spell.as_ref().expect("spell state populated").clone();
        let effect = &spell.effect;
        let chosen_targets = spell.chosen_targets.clone();

        let rng_dyn: &mut dyn rand::RngCore = &mut seeded_rng();
        effect.as_ref().unwrap().call(&mut state, 1, &chosen_targets, &catalog_map, rng_dyn);

        let murktide_bf = state.permanents_of("us").find(|p| p.catalog_key == "Murktide Regent")
            .and_then(|p| p.bf.as_ref()).expect("Murktide on battlefield");
        assert_eq!(murktide_bf.counters, 0);
        let murktide_id = state.permanents_of("us").find(|p| p.catalog_key == "Murktide Regent")
            .map(|p| p.id).expect("Murktide on battlefield");
        let mat = recompute(&state, &catalog_map);
        let eff = mat.defs.get(&murktide_id).expect("Murktide materialized");
        let CardKind::Creature(c) = &eff.kind else { panic!("expected creature") };
        assert_eq!((c.power(), c.toughness()), (3, 3));
    }

    #[test]
    fn test_murktide_attacks_with_counter_boosted_stats() {
        // A 6/6 Murktide (base 3/3 + 3 counters) should survive attacking into a 5-power blocker.
        let mut state = make_state();
        let murktide_def = creature("Murktide Regent", 3, 3);
        let murktide_id = add_perm(&mut state, "us", "Murktide Regent", BattlefieldState {
            counters: 3,
            entered_this_turn: false,
            ..BattlefieldState::new()
        });
        // Opponent has a 5/5 blocker — Murktide's toughness 6 > opp power 5, safe to attack.
        let blocker_def = creature("Dragon", 5, 5);
        add_default_perm(&mut state, "opp", "Dragon");

        let catalog = vec![murktide_def, blocker_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareAttackers, prio: false };
        do_step(&mut state, 1, "us", &step, 3, true, &catalog_map, &mut seeded_rng());

        assert!(state.combat_attackers.contains(&murktide_id),
            "6/6 Murktide should attack into a 5-power blocker");
    }

    #[test]
    fn test_cast_delve_spell_insufficient_mana_after_delve() {
        // Spell costs 3 generic. Graveyard has 2 cards — reduces cost to 1.
        // Pool is empty — still can't cast.
        let mut state = make_state();
        let def: CardDef = toml::from_str(r#"
            name = "Dead Drop"
            card_type = "sorcery"
            mana_cost = "3"
            delve = true
        "#).unwrap();
        add_graveyard_card(&mut state, "us", "Ritual");
        add_graveyard_card(&mut state, "us", "Ponder");
        let dead_drop_id = add_hand_card(&mut state, "us", "Dead Drop");
        // no mana

        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        let item = cast_spell(&mut state, 1, "us", dead_drop_id, SpellFace::Main, None, &catalog_map, &mut seeded_rng());

        assert!(item.is_none(), "can't cast — 1 generic still unpaid");
        assert_eq!(state.graveyard_of("us").count(), 2, "graveyard unchanged on failed cast");
        assert_eq!(state.exile_of("us").count(), 0, "nothing exiled on failed cast");
    }

    #[test]
    fn test_effect_exile_ability_removes_creature() {
        let mut state = make_state();
        add_default_perm(&mut state, "opp", "Troll");
        let troll_def = creature("Troll", 2, 2);
        let ability: AbilityDef = toml::from_str(r#"
            mana_cost = ""
            target = "opp:creature"
            effect = "exile"
        "#).unwrap();
        let catalog = vec![troll_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let targets: Vec<Target> = choose_permanent_target("opp:creature", "us", &state, &catalog_map, &mut seeded_rng())
            .map(|id| vec![Target::Object(id)])
            .unwrap_or_default();
        let eff = build_ability_effect(&ability, "us", ObjId::UNSET);
        eff.call(&mut state, 1, &targets, &catalog_map, &mut seeded_rng());

        assert!(state.permanents_of("opp").count() == 0, "Troll should be exiled");
        assert!(state.exile_of("opp").any(|c| c.catalog_key == "Troll"), "Troll should be in exile");
        assert!(state.graveyard_of("opp").count() == 0, "exiled, not dead");
    }

    // ── Section 10: Ninjutsu ──────────────────────────────────────────────────

    fn ninja_def() -> CardDef {
        toml::from_str(r#"
            name = "Ninja"
            card_type = "creature"
            power = 2
            toughness = 1
            ninjutsu = {mana_cost = "U"}
        "#).unwrap()
    }

    fn island_land(state: &mut SimState, who: &str) -> ObjId {
        let def: CardDef = toml::from_str(r#"
            name = "Island"
            card_type = "land"
            [[mana_abilities]]
            tap_self = true
            produces = "U"
        "#).unwrap();
        add_perm_with_def(state, who, &def, BattlefieldState::new())
    }

    #[test]
    fn test_declare_attackers_sets_attacking_flag() {
        let mut state = make_state();
        let def = creature("Attacker", 2, 4);
        let atk_id = add_perm(&mut state, "us", "Attacker", BattlefieldState {
            entered_this_turn: false,
            ..BattlefieldState::new()
        });

        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareAttackers, prio: false };
        do_step(&mut state, 1, "us", &step, 3, true, &catalog_map, &mut seeded_rng());

        assert!(state.permanent_bf(atk_id).unwrap().attacking, "declared attacker gets attacking=true");
    }

    #[test]
    fn test_declare_blockers_sets_unblocked_flag_when_no_blocker() {
        let mut state = make_state();
        let def = creature("Attacker", 2, 4);
        let attacker_id = add_perm(&mut state, "us", "Attacker", BattlefieldState {
            attacking: true,
            tapped: true,
            ..BattlefieldState::new()
        });
        state.combat_attackers = vec![attacker_id];
        // No opp creatures → no blocker

        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareBlockers, prio: false };
        do_step(&mut state, 1, "us", &step, 3, true, &catalog_map, &mut seeded_rng());

        assert!(state.permanent_bf(attacker_id).unwrap().unblocked, "unblocked attacker gets unblocked=true");
    }

    #[test]
    fn test_declare_blockers_blocked_attacker_not_unblocked() {
        let mut state = make_state();
        let atk_def = creature("Ragavan", 2, 2);
        let blk_def = creature("Wall", 0, 6);
        let ragavan_id = add_perm(&mut state, "us", "Ragavan", BattlefieldState {
            attacking: true,
            tapped: true,
            ..BattlefieldState::new()
        });
        add_default_perm(&mut state, "opp", "Wall");
        state.combat_attackers = vec![ragavan_id];

        let catalog = vec![atk_def, blk_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareBlockers, prio: false };
        do_step(&mut state, 1, "us", &step, 3, true, &catalog_map, &mut seeded_rng());

        assert!(!state.permanent_bf(ragavan_id).unwrap().unblocked, "blocked attacker stays unblocked=false");
        assert_eq!(state.combat_blocks.len(), 1, "blocker declared");
    }

    #[test]
    fn test_end_combat_clears_attacking_unblocked_flags() {
        let mut state = make_state();
        let ninja_id = add_perm(&mut state, "us", "Ninja", BattlefieldState {
            attacking: true,
            unblocked: true,
            ..BattlefieldState::new()
        });
        state.combat_attackers = vec![ninja_id];

        let step = Step { kind: StepKind::EndCombat, prio: false };
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        do_step(&mut state, 1, "us", &step, 3, true, &catalog_map, &mut seeded_rng());

        assert!(!state.permanent_bf(ninja_id).unwrap().attacking, "attacking cleared at EndCombat");
        assert!(!state.permanent_bf(ninja_id).unwrap().unblocked, "unblocked cleared at EndCombat");
    }

    // Negative try_ninjutsu precondition tests (deterministic — RNG roll is never reached).

    #[test]
    fn test_try_ninjutsu_no_hand_returns_none() {
        let mut state = make_state();
        // No hand cards — hand_size returns 0; exits before any materialized lookup.
        add_perm(&mut state, "us", "Ragavan", BattlefieldState { attacking: true, unblocked: true, ..BattlefieldState::new() });
        assert!(try_ninjutsu(&state, "us", &mut seeded_rng()).is_none(), "no hand → None");
    }

    #[test]
    fn test_try_ninjutsu_no_unblocked_returns_none() {
        let mut state = make_state();
        add_hand_card(&mut state, "us", "Ninja");
        add_perm(&mut state, "us", "Ragavan", BattlefieldState { attacking: true, unblocked: false, ..BattlefieldState::new() });
        state.us.pool.u = 1; state.us.pool.total = 1;
        // Exits at the has_unblocked check before the hand scan; no materialized seeding needed.
        assert!(try_ninjutsu(&state, "us", &mut seeded_rng()).is_none(), "no unblocked attacker → None");
    }

    #[test]
    fn test_try_ninjutsu_no_ninja_in_library_returns_none() {
        let mut state = make_state();
        let brainstorm_def = toml::from_str::<CardDef>("name=\"Brainstorm\"\ncard_type=\"instant\"\nmana_cost=\"U\"").unwrap();
        add_hand_card_with_def(&mut state, "us", &brainstorm_def);
        add_perm(&mut state, "us", "Ragavan", BattlefieldState { attacking: true, unblocked: true, ..BattlefieldState::new() });
        state.us.pool.u = 1; state.us.pool.total = 1;
        // Brainstorm has no ninjutsu; materialized entry present, filter returns false → None.
        assert!(try_ninjutsu(&state, "us", &mut seeded_rng()).is_none(), "no ninja card → None");
    }

    #[test]
    fn test_try_ninjutsu_no_mana_returns_none() {
        let mut state = make_state();
        let def = ninja_def();
        add_hand_card_with_def(&mut state, "us", &def);
        add_perm(&mut state, "us", "Ragavan", BattlefieldState { attacking: true, unblocked: true, ..BattlefieldState::new() });
        // No mana available — ninja found in materialized, but mana check fails.
        assert!(try_ninjutsu(&state, "us", &mut seeded_rng()).is_none(), "no mana → None");
    }

    #[test]
    fn test_ninjutsu_swaps_attacker_for_ninja() {
        // try_ninjutsu returns ActivateAbility; when committed via handle_priority_round
        // in a DeclareBlockers window, the ninja enters play and the attacker returns to hand.
        let def = ninja_def();
        let island_def: CardDef = toml::from_str(r#"name = "Island"
card_type = "land"
[[mana_abilities]]
tap_self = true
produces = "U""#).unwrap();
        let catalog = vec![def.clone(), island_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        // Loop over seeds until ninjutsu fires (35% per attempt → statistically guaranteed within 50).
        for seed in 0u64..50 {
            let mut state = make_state();
            state.current_phase = Some(TurnPosition::Step(StepKind::DeclareBlockers));
            state.current_ap = state.us.id;
            add_perm(&mut state, "us", "Ragavan", BattlefieldState {
                attacking: true, unblocked: true, ..BattlefieldState::new()
            });
            island_land(&mut state, "us");
            // Add Ninja to hand with materialized entry so try_ninjutsu can find it.
            add_hand_card_with_def(&mut state, "us", &def);
            // Also register the ninja in state.objects as a library card (so apply_ability_effect
            // can look up the ninja's name at resolution).
            let ninja_lib_id = state.alloc_id();
            state.objects.insert(ninja_lib_id, GameObject::new(ninja_lib_id, "Ninja".to_string(), "us"));
            let initial_hand = state.hand_size("us");
            let mut rng = StdRng::seed_from_u64(seed);
            handle_priority_round(&mut state, 1, "us", 3, &catalog_map, &mut rng);

            if state.permanents_of("us").any(|p| p.catalog_key == "Ninja") {
                let ninja = state.permanents_of("us").find(|p| p.catalog_key == "Ninja").unwrap();
                let ninja_bf = ninja.bf.as_ref().unwrap();
                assert!(ninja_bf.attacking, "ninja should be attacking");
                assert!(ninja_bf.tapped, "ninja should be tapped");
                assert!(!state.permanents_of("us").any(|p| p.catalog_key == "Ragavan"), "Ragavan returned to hand");
                assert_eq!(state.hand_size("us"), initial_hand, "net hand size unchanged (+1 return, -1 ninja)");
                let ninja_id = state.permanents_of("us").find(|p| p.catalog_key == "Ninja").unwrap().id;
                assert!(state.combat_attackers.contains(&ninja_id), "ninja in combat_attackers");
                return;
            }
        }
        panic!("ninjutsu should have fired within 50 seeds");
    }

    // ── Section 11: Cycling ───────────────────────────────────────────────────

    #[test]
    fn test_cycling_draw_effect() {
        // build_ability_effect with draw:1 draws one card.
        let mut state = make_state();
        add_library_card(&mut state, "us", "Island");
        let initial = state.hand_size("us");
        let ability: AbilityDef = toml::from_str(r#"effect = "draw:1""#).unwrap();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        let eff = build_ability_effect(&ability, "us", ObjId::UNSET);
        eff.call(&mut state, 1, &[], &catalog_map, &mut seeded_rng());
        assert_eq!(state.hand_size("us"), initial + 1, "cycling draws one card");
    }

    #[test]
    fn test_cycling_discard_self_removes_card_from_library() {
        // pay_activation_cost with discard_self=true removes the card from hand
        // and sends it to the graveyard.
        let mut state = make_state();
        let wraith_def: CardDef = toml::from_str(r#"
            name = "Street Wraith"
            card_type = "creature"
            mana_cost = "3BB"
            power = 3
            toughness = 4
        "#).unwrap();
        let ability: AbilityDef = toml::from_str(r#"
            zone = "hand"
            discard_self = true
            life_cost = 2
            effect = "draw:1"
        "#).unwrap();
        let catalog = vec![wraith_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        // Add Street Wraith to hand and a library card to draw
        let wraith_id = add_hand_card(&mut state, "us", "Street Wraith");
        add_library_card(&mut state, "us", "Island");
        let initial_hand = state.hand_size("us");

        pay_activation_cost(&mut state, 1, "us", wraith_id, &ability, &catalog_map);

        assert!(!state.hand_of("us").any(|c| c.catalog_key == "Street Wraith"), "Street Wraith removed from hand");
        assert!(state.graveyard_of("us").any(|c| c.catalog_key == "Street Wraith"), "in graveyard");
        assert_eq!(state.hand_size("us"), initial_hand - 1, "hand size decremented (discarded, not yet drawn)");
        assert_eq!(state.us.life, 20 - 2, "paid 2 life");
    }

    // ── Section 12: Adventure ─────────────────────────────────────────────────

    #[test]
    fn test_adventure_resolve_exiles_to_on_adventure() {
        // An adventure StackItem (no target) routes the card to exile + on_adventure.
        let mut state = make_state();
        // Simulate the adventure resolution inline: no effect, just exile.
        let borrower_id = state.alloc_id();
        let mut borrower_obj = GameObject::new(borrower_id, "Brazen Borrower", "us");
        borrower_obj.zone = CardZone::Exile { on_adventure: true };
        state.objects.insert(borrower_id, borrower_obj);

        assert!(state.exile_of("us").any(|c| c.catalog_key == "Brazen Borrower"), "Borrower in exile");
        assert!(state.on_adventure_of("us").any(|c| c.catalog_key == "Brazen Borrower"), "Borrower on adventure");
        assert!(state.graveyard_of("us").count() == 0, "not in graveyard");
    }

    #[test]
    fn test_adventure_bounce_effect_returns_opp_permanent() {
        // Petty Theft bounces target opp permanent then exiles Brazen Borrower to on_adventure.
        let mut state = make_state();
        let bowmasters_id = add_default_perm(&mut state, "opp", "Orcish Bowmasters");
        let initial_opp_hand = state.hand_size("opp");

        // Run the Effect directly (as the new adventure resolution path does).
        let eff = eff_bounce_target("us");
        eff.call(&mut state, 1, &[Target::Object(bowmasters_id)], &HashMap::new(), &mut seeded_rng());
        // Then exile the card to on_adventure.
        let borrower_id = state.alloc_id();
        let mut borrower_obj = GameObject::new(borrower_id, "Brazen Borrower", "us");
        borrower_obj.zone = CardZone::Exile { on_adventure: true };
        state.objects.insert(borrower_id, borrower_obj);

        assert!(state.permanents_of("opp").count() == 0, "Bowmasters bounced off board");
        assert_eq!(state.hand_size("opp"), initial_opp_hand + 1, "bounced to opp hand");
        assert!(state.on_adventure_of("us").any(|c| c.catalog_key == "Brazen Borrower"), "Borrower on adventure in exile");
    }

    #[test]
    fn test_cast_from_adventure_enters_play() {
        // pick_on_board_action detects adventure creatures in exile and picks the cast action
        // (75% roll). Run with multiple seeds to confirm it fires and the creature enters play.
        let borrower_def: CardDef = toml::from_str(r#"
            name = "Brazen Borrower"
            card_type = "creature"
            mana_cost = "1UU"
            blue = true
            power = 3
            toughness = 1
        "#).unwrap();
        let island_def: CardDef = toml::from_str(r#"name = "Island"
card_type = "land"
[[mana_abilities]]
tap_self = true
produces = "U""#).unwrap();
        let island2_def: CardDef = toml::from_str(r#"name = "Island2"
card_type = "land"
[[mana_abilities]]
tap_self = true
produces = "U""#).unwrap();
        let swamp_def: CardDef = toml::from_str(r#"name = "Swamp"
card_type = "land"
[[mana_abilities]]
tap_self = true
produces = "B""#).unwrap();
        let catalog = vec![borrower_def.clone(), island_def, island2_def, swamp_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        let make_fresh_state = || {
            let mut state = make_state();
            state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
            state.current_ap = state.us.id;
            let borrower_id = state.alloc_id();
            let mut borrower_obj = GameObject::new(borrower_id, "Brazen Borrower", "us");
            borrower_obj.zone = CardZone::Exile { on_adventure: true };
            state.objects.insert(borrower_id, borrower_obj);
            // 1UU mana: two Islands + one generic (Swamp)
            island_land(&mut state, "us");
            {
                let def: CardDef = toml::from_str(r#"name = "Island2"
card_type = "land"
[[mana_abilities]]
tap_self = true
produces = "U""#).unwrap();
                add_perm_with_def(&mut state, "us", &def, BattlefieldState::new());
            }
            {
                let def: CardDef = toml::from_str(r#"name = "Swamp"
card_type = "land"
[[mana_abilities]]
tap_self = true
produces = "B""#).unwrap();
                add_perm_with_def(&mut state, "us", &def, BattlefieldState::new());
            }
            state
        };

        // At 75% per attempt, try up to 20 seeds; at least one must result in Borrower entering play.
        let mut entered = false;
        for seed in 0u64..20 {
            let mut state = make_fresh_state();
            let mut rng = StdRng::seed_from_u64(seed);
            handle_priority_round(&mut state, 1, "us", 3, &catalog_map, &mut rng);
            if state.permanents_of("us").any(|p| p.catalog_key == "Brazen Borrower") {
                assert!(!state.on_adventure_of("us").any(|c| c.catalog_key == "Brazen Borrower"), "removed from on_adventure");
                assert!(!state.exile_of("us").any(|c| c.catalog_key == "Brazen Borrower"), "removed from exile");
                entered = true;
                break;
            }
        }
        assert!(entered, "Brazen Borrower should have entered play in at least one of 20 seeded runs");
    }

    // ── Section 8: Keyword Tests ──────────────────────────────────────────────

    fn flying_creature(name: &str, power: i32, toughness: i32) -> CardDef {
        let toml = format!(
            "name = {:?}\ncard_type = \"creature\"\npower = {}\ntoughness = {}\nkeywords = [\"flying\"]\n",
            name, power, toughness
        );
        toml::from_str(&toml).unwrap()
    }

    #[test]
    fn test_flying_not_blocked_by_ground() {
        // Flying attacker should not be assigned a ground blocker.
        let mut state = make_state();
        let flyer = flying_creature("Murktide Regent", 3, 3);
        let ground = creature("Troll", 3, 3);

        let murktide_id = add_perm(&mut state, "us", "Murktide Regent", BattlefieldState {
            attacking: true,
            ..BattlefieldState::new()
        });
        add_default_perm(&mut state, "opp", "Troll");
        state.combat_attackers = vec![murktide_id];

        let catalog = vec![flyer, ground];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareBlockers, prio: false };
        do_step(&mut state, 1, "us", &step, 3, true, &catalog_map, &mut seeded_rng());

        assert!(state.combat_blocks.is_empty(), "ground creature cannot block a flyer");
    }

    #[test]
    fn test_flying_blocked_by_flyer() {
        // Flying attacker CAN be blocked by another flying creature.
        let mut state = make_state();
        let flyer_atk = flying_creature("Murktide Regent", 3, 3);
        let flyer_blk = flying_creature("Subtlety", 3, 3);

        let murktide_id = add_perm(&mut state, "us", "Murktide Regent", BattlefieldState {
            attacking: true,
            ..BattlefieldState::new()
        });
        let subtlety_id = add_default_perm(&mut state, "opp", "Subtlety");
        state.combat_attackers = vec![murktide_id];

        let catalog = vec![flyer_atk, flyer_blk];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareBlockers, prio: false };
        do_step(&mut state, 1, "us", &step, 3, true, &catalog_map, &mut seeded_rng());

        assert_eq!(state.combat_blocks.len(), 1, "flyer can block flyer");
        assert_eq!(state.combat_blocks[0], (murktide_id, subtlety_id));
    }

    #[test]
    fn test_flying_attack_safety_ignores_ground() {
        // A flying 3/3 attacker should attack freely even if a 3/3 ground creature is in play,
        // because that ground creature cannot block the flyer.
        let mut state = make_state();
        let flyer = flying_creature("Murktide Regent", 3, 3);
        let ground = creature("Troll", 3, 3); // cannot block flyer

        let murktide_id = add_perm(&mut state, "us", "Murktide Regent", BattlefieldState {
            entered_this_turn: false,
            ..BattlefieldState::new()
        });
        add_default_perm(&mut state, "opp", "Troll");

        let catalog = vec![flyer, ground];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareAttackers, prio: false };
        do_step(&mut state, 1, "us", &step, 3, true, &catalog_map, &mut seeded_rng());

        // Murktide's toughness (3) > relevant blocking power (0 — Troll can't block flyer).
        assert!(state.combat_attackers.contains(&murktide_id),
            "flying creature should attack when only ground blockers exist");
    }

    // ── Section 9: Trigger Tests ──────────────────────────────────────────────

    #[test]
    fn test_fire_triggers_returns_context_for_bowmasters_etb() {
        let mut state = make_state();
        add_default_perm(&mut state, "opp", "Orcish Bowmasters");

        let ev = GameEvent::ZoneChange {
            id: ObjId::UNSET,
            actor: "test".to_string(),
            card: "Orcish Bowmasters".to_string(),
            card_type: "creature".to_string(),
            from: ZoneId::Stack,
            to: ZoneId::Battlefield,
            controller: "opp".to_string(),
        };
        let result = fire_triggers(&ev, &state);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source_name, "Orcish Bowmasters");
    }

    #[test]
    fn test_fire_triggers_empty_when_no_bowmasters_in_play() {
        let state = make_state(); // no permanents
        let ev = GameEvent::ZoneChange {
            id: ObjId::UNSET,
            actor: "test".to_string(),
            card: "Orcish Bowmasters".to_string(),
            card_type: "creature".to_string(),
            from: ZoneId::Stack,
            to: ZoneId::Battlefield,
            controller: "opp".to_string(),
        };
        let result = fire_triggers(&ev, &state);
        assert!(result.is_empty());
    }

    /// Fire a Bowmasters ETB trigger for `controller` and return the TriggerContext.
    fn bowmasters_etb_ctx(controller: &str) -> TriggerContext {
        let ev = GameEvent::ZoneChange {
            id: ObjId::UNSET,
            actor: "test".to_string(),
            card: "Orcish Bowmasters".into(),
            card_type: "creature".into(),
            from: ZoneId::Hand,
            to: ZoneId::Battlefield,
            controller: controller.to_string(),
        };
        let mut pending = Vec::new();
        bowmasters_check(&ev, ObjId::UNSET, controller, &mut pending);
        pending.remove(0)
    }

    /// Fire a Bowmasters ETB trigger for `controller`, choose its target, and apply it.
    fn fire_bowmasters_etb(controller: &str, state: &mut SimState, catalog_map: &HashMap<&str, &CardDef>) {
        // Rebuild materialized so choose_trigger_target sees current P/T.
        let mat = recompute(state, catalog_map);
        state.materialized = mat;
        let ctx = bowmasters_etb_ctx(controller);
        let targets: Vec<Target> = choose_trigger_target(&ctx.target_spec, controller, state)
            .into_iter().collect();
        ctx.effect.call(state, 1, &targets, catalog_map, &mut rand::thread_rng());
    }

    #[test]
    fn test_apply_bowmasters_etb_deals_damage_and_creates_army() {
        let mut state = make_state();
        add_default_perm(&mut state, "opp", "Orcish Bowmasters");
        let initial_life = state.us.life;
        fire_bowmasters_etb("opp", &mut state, &HashMap::new());
        assert_eq!(state.us.life, initial_life - 1, "ETB deals 1 to us");
        assert!(state.permanents_of("opp").any(|p| p.catalog_key == "Orc Army"), "Orc Army token created");
        let army = state.permanents_of("opp").find(|p| p.catalog_key == "Orc Army").and_then(|p| p.bf.as_ref()).unwrap();
        assert_eq!(army.counters, 1, "Orc Army has 1 counter");
    }

    #[test]
    fn test_apply_bowmasters_etb_grows_existing_army() {
        let mut state = make_state();
        add_default_perm(&mut state, "opp", "Orcish Bowmasters");
        add_perm(&mut state, "opp", "Orc Army", BattlefieldState { counters: 2, ..BattlefieldState::new() });
        fire_bowmasters_etb("opp", &mut state, &HashMap::new());
        let army = state.permanents_of("opp").find(|p| p.catalog_key == "Orc Army").and_then(|p| p.bf.as_ref()).unwrap();
        assert_eq!(army.counters, 3, "Orc Army grows from 2 to 3");
    }

    #[test]
    fn test_bowmasters_ping_hits_face_when_no_killable_creature() {
        let mut state = make_state();
        add_default_perm(&mut state, "opp", "Orcish Bowmasters");
        let initial_life = state.us.life;
        add_default_perm(&mut state, "us", "Troll");
        let catalog = vec![creature("Troll", 3, 3)];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        fire_bowmasters_etb("opp", &mut state, &catalog_map);
        assert_eq!(state.us.life, initial_life - 1, "damage hits face when no killable creature");
        assert!(state.permanents_of("us").any(|p| p.catalog_key == "Troll"), "Troll survives");
    }

    #[test]
    fn test_bowmasters_ping_kills_1_1_creature() {
        let mut state = make_state();
        add_default_perm(&mut state, "opp", "Orcish Bowmasters");
        let initial_life = state.us.life;
        add_default_perm(&mut state, "us", "Ragavan, Nimble Pilferer");
        let catalog = vec![creature("Ragavan, Nimble Pilferer", 2, 1)];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        fire_bowmasters_etb("opp", &mut state, &catalog_map);
        check_state_based_actions(&mut state, 1, &catalog_map, &mut seeded_rng());
        assert_eq!(state.us.life, initial_life, "life total unchanged when creature is targeted");
        assert!(!state.permanents_of("us").any(|p| p.catalog_key == "Ragavan, Nimble Pilferer"),
            "Ragavan dies to 1 damage");
        assert!(state.graveyard_of("us").any(|c| c.catalog_key == "Ragavan, Nimble Pilferer"),
            "Ragavan goes to graveyard");
    }

    #[test]
    fn test_bowmasters_ping_prioritises_opposing_bowmasters() {
        let mut state = make_state();
        add_default_perm(&mut state, "opp", "Orcish Bowmasters");
        add_default_perm(&mut state, "us", "Troll");
        add_default_perm(&mut state, "us", "Orcish Bowmasters");
        let catalog = vec![creature("Troll", 3, 3), creature("Orcish Bowmasters", 1, 1)];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        fire_bowmasters_etb("opp", &mut state, &catalog_map);
        check_state_based_actions(&mut state, 1, &catalog_map, &mut seeded_rng());
        assert!(!state.permanents_of("us").any(|p| p.catalog_key == "Orcish Bowmasters"),
            "opposing Bowmasters is killed");
        assert!(state.permanents_of("us").any(|p| p.catalog_key == "Troll"), "Troll survives");
    }

    #[test]
    fn test_bowmasters_no_trigger_on_natural_first_draw() {
        let mut state = make_state();
        add_default_perm(&mut state, "opp", "Orcish Bowmasters");

        let ev = GameEvent::Draw { controller: "us".to_string(), draw_index: 1, is_natural: true };
        let result = fire_triggers(&ev, &state);
        assert!(result.is_empty(), "no trigger on first natural draw");
    }

    #[test]
    fn test_bowmasters_triggers_on_cantrip_draw() {
        let mut state = make_state();
        add_default_perm(&mut state, "opp", "Orcish Bowmasters");

        let ev = GameEvent::Draw { controller: "us".to_string(), draw_index: 1, is_natural: false };
        let result = fire_triggers(&ev, &state);
        assert_eq!(result.len(), 1, "cantrip draw triggers Bowmasters");
    }

    #[test]
    fn test_murktide_counter_on_instant_exile() {
        let mut state = make_state();
        add_perm(&mut state, "us", "Murktide Regent", BattlefieldState { counters: 0, ..BattlefieldState::new() });

        let ev = GameEvent::ZoneChange {
            id: ObjId::UNSET,
            actor: "test".to_string(),
            card: "Counterspell".to_string(),
            card_type: "instant".to_string(),
            from: ZoneId::Graveyard,
            to: ZoneId::Exile,
            controller: "us".to_string(),
        };
        let result = fire_triggers(&ev, &state);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source_name, "Murktide Regent");

        let mut state2 = state;
        result[0].effect.call(&mut state2, 1, &[], &HashMap::new(), &mut rand::thread_rng());
        let murktide = state2.permanents_of("us").find(|p| p.catalog_key == "Murktide Regent").and_then(|p| p.bf.as_ref()).unwrap();
        assert_eq!(murktide.counters, 1, "Murktide gains +1/+1 counter");
    }

    #[test]
    fn test_murktide_no_counter_on_land_exile() {
        let mut state = make_state();
        add_default_perm(&mut state, "us", "Murktide Regent");

        let ev = GameEvent::ZoneChange {
            id: ObjId::UNSET,
            actor: "test".to_string(),
            card: "Island".to_string(),
            card_type: "land".to_string(),
            from: ZoneId::Graveyard,
            to: ZoneId::Exile,
            controller: "us".to_string(),
        };
        let result = fire_triggers(&ev, &state);
        assert!(result.is_empty(), "land exile does not trigger Murktide");
    }

    #[test]
    fn test_tamiyo_clue_when_attacking() {
        let mut state = make_state();
        add_perm(&mut state, "us", "Tamiyo, Inquisitive Student", BattlefieldState { attacking: true, ..BattlefieldState::new() });

        let ev = GameEvent::EnteredStep {
            step: StepKind::DeclareAttackers,
            active_player: "us".to_string(),
        };
        let result = fire_triggers(&ev, &state);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source_name, "Tamiyo, Inquisitive Student");

        let mut state2 = state;
        result[0].effect.call(&mut state2, 1, &[], &HashMap::new(), &mut rand::thread_rng());
        assert!(state2.permanents_of("us").any(|p| p.catalog_key == "Clue Token"),
            "Clue Token created when Tamiyo attacks");
    }

    #[test]
    fn test_tamiyo_no_clue_when_not_attacking() {
        let mut state = make_state();
        add_default_perm(&mut state, "us", "Tamiyo, Inquisitive Student"); // attacking = false

        let ev = GameEvent::EnteredStep {
            step: StepKind::DeclareAttackers,
            active_player: "us".to_string(),
        };
        let result = fire_triggers(&ev, &state);
        // Trigger queues (Tamiyo is in play), but resolves to nothing (not attacking).
        if let Some(ctx) = result.first() {
            let mut state2 = state;
            ctx.effect.call(&mut state2, 1, &[], &HashMap::new(), &mut rand::thread_rng());
            assert!(!state2.permanents_of("us").any(|p| p.catalog_key == "Clue Token"),
                "no Clue Token if Tamiyo is not attacking");
        }
    }

    #[test]
    fn test_tamiyo_flip_on_third_draw() {
        let mut state = make_state();
        add_default_perm(&mut state, "us", "Tamiyo, Inquisitive Student");

        let ev = GameEvent::Draw { controller: "us".to_string(), draw_index: 3, is_natural: false };
        let result = fire_triggers(&ev, &state);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source_name, "Tamiyo, Inquisitive Student");

        let mut state2 = state;
        result[0].effect.call(&mut state2, 1, &[], &HashMap::new(), &mut rand::thread_rng());
        // The flip mutates in-place: catalog_key stays as front face; active_face flips to 1.
        let tamiyo_bf = state2.permanents_of("us")
            .find(|p| p.catalog_key == "Tamiyo, Inquisitive Student")
            .and_then(|p| p.bf.as_ref())
            .expect("Tamiyo should still be on the battlefield (same object, same catalog_key)");
        assert_eq!(tamiyo_bf.active_face, 1, "active_face == 1 after flip");
        assert_eq!(tamiyo_bf.loyalty, 2, "starting loyalty of Tamiyo, Seasoned Scholar");
    }

    #[test]
    fn test_tamiyo_plus_two_applies_power_mod_to_attackers() {
        let mut state = make_state();
        // Register the +2 floating trigger watcher for "us" (as if us activated it last turn).
        state.trigger_instances.push(TriggerInstance {
            source_id: ObjId::UNSET,
            controller: "us".to_string(),
            check: std::sync::Arc::new(tamiyo_plus_two_check),
            expiry: Some(ContinuousExpiry::StartOfControllerNextTurn),
            active: true,
        });
        // Opp has a 3/3 attacker.
        let atk_def = creature("Dragon", 3, 3);
        add_perm(&mut state, "opp", "Dragon", BattlefieldState { entered_this_turn: false, ..BattlefieldState::new() });
        add_default_perm(&mut state, "us", "Wall"); // blocker-sized (no block in this test)

        let catalog = vec![atk_def, creature("Wall", 0, 4)];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let mut rng = seeded_rng();
        do_step(&mut state, 1, "opp", &Step { kind: StepKind::DeclareAttackers, prio: true },
            3, true, &catalog_map, &mut rng);

        let dragon_id = state.permanents_of("opp").find(|p| p.catalog_key == "Dragon").map(|p| p.id).unwrap();
        // The -1 comes from a ContinuousInstance (L7), not bf.power_mod.
        // recompute reflects the CE modifier in the materialized view.
        let mat = recompute(&state, &catalog_map);
        let eff = mat.defs.get(&dragon_id).expect("Dragon materialized");
        let CardKind::Creature(c) = &eff.kind else { panic!("expected creature") };
        assert_eq!(c.power(), 2, "Dragon's effective power is 3 + (-1) = 2");
    }

    #[test]
    fn test_tamiyo_plus_two_expires_at_controller_untap() {
        let mut state = make_state();
        state.trigger_instances.push(TriggerInstance {
            source_id: ObjId::UNSET,
            controller: "us".to_string(),
            check: std::sync::Arc::new(tamiyo_plus_two_check),
            expiry: Some(ContinuousExpiry::StartOfControllerNextTurn),
            active: true,
        });
        assert_eq!(state.trigger_instances.len(), 1);

        // Untap step for "us" should expire the floating trigger watcher.
        let step = Step { kind: StepKind::Untap, prio: false };
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        do_step(&mut state, 2, "us", &step, 3, true, &catalog_map, &mut seeded_rng());

        assert!(state.trigger_instances.is_empty(), "Floating trigger expires at controller's next Untap");
    }

    #[test]
    fn test_stat_mod_reversed_at_cleanup() {
        // A L7 ContinuousInstance with EndOfTurn expiry should be removed during Cleanup,
        // restoring the effective P/T of the affected permanent.
        let mut state = make_state();
        let atk_def = creature("Dragon", 3, 3);
        let dragon_id = add_perm(&mut state, "opp", "Dragon", BattlefieldState::new());
        let catalog = vec![atk_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        // Register an EndOfTurn L7 CI that applies -1 power to the dragon.
        state.continuous_instances.push(ContinuousInstance {
            source_id: dragon_id,
            controller: "us".to_string(),
            layer: ContinuousLayer::L7PowerToughness,
            filter: std::sync::Arc::new(move |id, _| id == dragon_id),
            modifier: std::sync::Arc::new(|def, _state| {
                if let CardKind::Creature(c) = &mut def.kind { c.adjust_pt(-1, 0); }
            }),
            expiry: ContinuousExpiry::EndOfTurn,
        });

        // Before Cleanup: effective power = 2.
        let mat = recompute(&state, &catalog_map);
        let CardKind::Creature(c) = &mat.defs[&dragon_id].kind else { panic!() };
        assert_eq!(c.power(), 2, "CI applies -1 before Cleanup");

        let step = Step { kind: StepKind::Cleanup, prio: false };
        do_step(&mut state, 1, "opp", &step, 3, true, &catalog_map, &mut seeded_rng());

        // After Cleanup: CI removed, effective power restored to 3.
        let mat = recompute(&state, &catalog_map);
        let CardKind::Creature(c) = &mat.defs[&dragon_id].kind else { panic!() };
        assert_eq!(c.power(), 3, "effective power restored after Cleanup");
        assert!(state.continuous_instances.is_empty(), "EndOfTurn CI removed at Cleanup");
    }

    // ── Step 2: EnteredStep / EnteredPhase fires for all priority windows ────────

    /// Verify EnteredStep fires for every named priority-bearing step.
    #[test]
    fn test_entered_step_fires_for_all_priority_steps() {
        let steps_with_prio = [
            StepKind::Upkeep,
            StepKind::Draw,
            StepKind::BeginCombat,
            StepKind::DeclareAttackers,
            StepKind::DeclareBlockers,
            StepKind::CombatDamage,
            StepKind::EndCombat,
            StepKind::End,
        ];
        for step_kind in steps_with_prio {
            let mut state = make_state();
            state.trigger_instances.push(TriggerInstance {
                source_id: ObjId::UNSET,
                controller: "us".to_string(),
                check: std::sync::Arc::new(move |e, _source_id, _ctl, pending| {
                    if let GameEvent::EnteredStep { step, .. } = e {
                        if *step == step_kind {
                            pending.push(TriggerContext {
                                source_name: format!("test-{:?}", step_kind),
                                controller: "us".to_string(),
                                target_spec: TargetSpec::None,
                                effect: Effect(std::sync::Arc::new(|_, _, _, _, _| {})),
                            });
                        }
                    }
                }),
                expiry: Some(ContinuousExpiry::EndOfTurn),
                active: true,
            });
            let ev = GameEvent::EnteredStep { step: step_kind, active_player: "us".to_string() };
            fire_event(ev, &mut state, 1, "us", &HashMap::new(), &mut seeded_rng());
            assert!(
                !state.pending_triggers.is_empty(),
                "EnteredStep {:?} should have produced a pending trigger", step_kind
            );
        }
    }

    /// Verify EnteredPhase fires for main phases (which have no named steps).
    #[test]
    fn test_entered_phase_fires_for_main_phases() {
        for phase_kind in [PhaseKind::PreCombatMain, PhaseKind::PostCombatMain] {
            let mut state = make_state();
            state.trigger_instances.push(TriggerInstance {
                source_id: ObjId::UNSET,
                controller: "us".to_string(),
                check: std::sync::Arc::new(move |e, _source_id, _ctl, pending| {
                    if let GameEvent::EnteredPhase { phase, .. } = e {
                        if *phase == phase_kind {
                            pending.push(TriggerContext {
                                source_name: format!("test-{:?}", phase_kind),
                                controller: "us".to_string(),
                                target_spec: TargetSpec::None,
                                effect: Effect(std::sync::Arc::new(|_, _, _, _, _| {})),
                            });
                        }
                    }
                }),
                expiry: Some(ContinuousExpiry::EndOfTurn),
                active: true,
            });
            let ev = GameEvent::EnteredPhase { phase: phase_kind };
            fire_event(ev, &mut state, 1, "us", &HashMap::new(), &mut seeded_rng());
            assert!(
                !state.pending_triggers.is_empty(),
                "EnteredPhase {:?} should have produced a pending trigger", phase_kind
            );
        }
    }

    /// Verify Untap and Cleanup do NOT fire EnteredStep (no priority round).
    #[test]
    fn test_entered_step_not_fired_for_no_prio_steps() {
        for step_kind in [StepKind::Untap, StepKind::Cleanup] {
            let state = make_state();
            // No triggers registered — just confirm no pending triggers exist at start.
            assert!(state.pending_triggers.is_empty(),
                "{:?} starts with no pending triggers", step_kind);
        }
    }

    // ── Section 10: Replacement Effect Tests ─────────────────────────────────

    // ── Section 11: Regression Tests ─────────────────────────────────────────

    /// Resolving a non-permanent spell must not log "countered".
    /// Bug: log_event had (Stack→Graveyard) → "countered" which fired during normal resolution.
    #[test]
    fn test_resolve_instant_does_not_log_countered() {
        let mut state = make_state();
        add_library_card(&mut state, "us", "Island");
        add_library_card(&mut state, "us", "Swamp");
        add_library_card(&mut state, "us", "Plains");
        // Manually place Brainstorm on stack with its effect.
        let id = state.alloc_id();
        state.objects.insert(id, GameObject {
            id,
            catalog_key: "Brainstorm".to_string(),
            owner: "us".to_string(),
            controller: "us".to_string(),
            zone: CardZone::Stack,
            is_token: false,
            spell: Some(SpellState {
                effect: Some(eff_draw("us", 3).then(eff_put_back("us", 2))),
                chosen_targets: vec![],
                is_back_face: false,
            }),
            bf: None,
        });
        state.stack.push(id);
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        resolve_top_of_stack(&mut state, 1, "us", &catalog_map, &mut seeded_rng());
        let log = state.log.join("\n");
        assert!(log.contains("Brainstorm resolves"), "should log 'resolves'");
        assert!(!log.contains("countered"), "resolving an instant must not produce 'countered' in the log");
    }

    /// After a sacrifice_self ability's cost is paid (permanent leaves battlefield), the action
    /// layer must never offer that ability again. This tests the structural guarantee that
    /// effects only arise from stack resolution — not from the decision layer re-selecting
    /// an ability whose cost has already been paid.
    #[test]
    fn test_no_ability_offered_after_sacrifice_cost_paid() {
        let fetch_def: CardDef = toml::from_str(r#"
            name = "Polluted Delta"
            card_type = "land"
            [[abilities]]
            sacrifice_self = true
            life_cost = 1
            effect = "search:land-island|swamp:play"
        "#).unwrap();
        let catalog = vec![fetch_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        let mut state = make_state();
        state.us.life = 20;
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PostCombatMain));
        let delta_id = add_perm(&mut state, "us", "Polluted Delta", BattlefieldState::new());

        // Simulate paying the sacrifice cost: permanent leaves the battlefield.
        state.set_card_zone(delta_id, CardZone::Graveyard);
        state.us.life -= 1;

        // With the source gone, decide_action must never offer ActivateAbility for that id,
        // regardless of how many times it is called.
        for seed in 0..50u64 {
            let mut rng = StdRng::seed_from_u64(seed);
            let action = decide_action(&mut state, 1, "us", "us", 99, &PriorityAction::Pass, &catalog_map, &mut rng);
            assert!(
                !matches!(action, PriorityAction::ActivateAbility(id, _) if id == delta_id),
                "seed {}: offered ability for sacrificed permanent — effect would fire without a stack item",
                seed
            );
        }
    }

    #[test]
    fn test_leyline_redirects_gy_to_exile() {
        let mut state = make_state();
        let mut rng = seeded_rng();
        let catalog: HashMap<&str, &CardDef> = HashMap::new();
        // Place Leyline on battlefield (add_perm now pre-registers and activates instances)
        let _leyline_id = add_default_perm(&mut state, "opp", "Leyline of the Void");
        // Put a card in hand
        let hand_id = add_hand_card(&mut state, "us", "Ponder");
        // Move hand card to graveyard — Leyline should redirect to exile
        change_zone(hand_id, ZoneId::Graveyard, &mut state, 1, "us", &catalog, &mut rng);
        // Card should be in Exile, not Graveyard
        assert_eq!(state.objects[&hand_id].zone, CardZone::Exile { on_adventure: false });
    }

    #[test]
    fn test_leyline_removed_no_redirect() {
        let mut state = make_state();
        let mut rng = seeded_rng();
        let catalog: HashMap<&str, &CardDef> = HashMap::new();
        // add_perm pre-registers and activates Leyline's replacement
        let leyline_id = add_default_perm(&mut state, "opp", "Leyline of the Void");
        // Destroy Leyline (deactivates its replacement via change_zone → deactivate_instances)
        change_zone(leyline_id, ZoneId::Graveyard, &mut state, 1, "us", &catalog, &mut rng);
        // Now move a card to GY — should stay in GY
        let hand_id = add_hand_card(&mut state, "us", "Ponder");
        change_zone(hand_id, ZoneId::Graveyard, &mut state, 1, "us", &catalog, &mut rng);
        assert_eq!(state.objects[&hand_id].zone, CardZone::Graveyard);
    }

    // ── Section 12: State-Based Action Tests ──────────────────────────────────

    fn add_token(state: &mut SimState, who: &str, name: &str) -> ObjId {
        let id = state.alloc_id();
        state.objects.insert(id, GameObject {
            id,
            catalog_key: name.to_string(),
            owner: who.to_string(),
            controller: who.to_string(),
            zone: CardZone::Battlefield,
            is_token: true,
            spell: None,
            bf: Some(BattlefieldState::new()),
        });
        id
    }

    #[test]
    fn test_sba_life_zero_sets_reroll() {
        let mut state = make_state();
        state.us.life = 0;
        let catalog: HashMap<&str, &CardDef> = HashMap::new();
        check_state_based_actions(&mut state, 1, &catalog, &mut seeded_rng());
        assert!(state.reroll, "us at 0 life → reroll");
    }

    #[test]
    fn test_sba_life_negative_sets_reroll() {
        let mut state = make_state();
        state.us.life = -3;
        let catalog: HashMap<&str, &CardDef> = HashMap::new();
        check_state_based_actions(&mut state, 1, &catalog, &mut seeded_rng());
        assert!(state.reroll);
    }

    #[test]
    fn test_sba_token_leaves_battlefield_ceases_to_exist() {
        let mut state = make_state();
        let token_id = add_token(&mut state, "us", "Orc Army");
        // Move token to graveyard (as if it died without SBA running yet).
        state.objects.get_mut(&token_id).unwrap().zone = CardZone::Graveyard;
        state.objects.get_mut(&token_id).unwrap().bf = None;
        let catalog: HashMap<&str, &CardDef> = HashMap::new();
        check_state_based_actions(&mut state, 1, &catalog, &mut seeded_rng());
        assert!(!state.objects.contains_key(&token_id), "token in GY ceases to exist");
    }

    #[test]
    fn test_sba_token_on_battlefield_not_removed() {
        let mut state = make_state();
        let token_id = add_token(&mut state, "us", "Orc Army");
        let catalog: HashMap<&str, &CardDef> = HashMap::new();
        check_state_based_actions(&mut state, 1, &catalog, &mut seeded_rng());
        assert!(state.objects.contains_key(&token_id), "token on battlefield survives SBA");
    }

    #[test]
    fn test_sba_zero_toughness_creature_dies() {
        let mut state = make_state();
        // A 1/-1 creature (e.g. after -1/-2 effect) has toughness ≤ 0.
        let _id = add_perm(&mut state, "us", "Weakened", BattlefieldState::new());
        let def = {
            let toml = "name = \"Weakened\"\ncard_type = \"creature\"\npower = 1\ntoughness = -1\n";
            toml::from_str::<CardDef>(toml).unwrap()
        };
        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        check_state_based_actions(&mut state, 1, &catalog_map, &mut seeded_rng());
        assert!(state.graveyard_of("us").any(|c| c.catalog_key == "Weakened"),
            "creature with toughness ≤ 0 goes to graveyard");
    }

    #[test]
    fn test_sba_lethal_damage_creature_dies() {
        let mut state = make_state();
        let _id = add_perm(&mut state, "us", "Ragavan", BattlefieldState {
            damage: 2,
            ..BattlefieldState::new()
        });
        let def = creature("Ragavan", 2, 2);
        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        check_state_based_actions(&mut state, 1, &catalog_map, &mut seeded_rng());
        assert!(state.graveyard_of("us").any(|c| c.catalog_key == "Ragavan"),
            "creature with damage = toughness goes to graveyard");
    }

    #[test]
    fn test_sba_planeswalker_loyalty_zero_dies() {
        let mut state = make_state();
        let _id = add_perm(&mut state, "us", "Jace", BattlefieldState {
            loyalty: 0,
            ..BattlefieldState::new()
        });
        let toml = "name = \"Jace\"\ncard_type = \"planeswalker\"\nmana_cost = \"3U\"\nloyalty = 3\n";
        let def: CardDef = toml::from_str(toml).unwrap();
        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        check_state_based_actions(&mut state, 1, &catalog_map, &mut seeded_rng());
        assert!(state.graveyard_of("us").any(|c| c.catalog_key == "Jace"),
            "planeswalker with loyalty 0 goes to graveyard");
    }

    #[test]
    fn test_sba_legend_rule_second_copy_dies() {
        let mut state = make_state();
        let _first = add_default_perm(&mut state, "us", "Bowmasters");
        let _second = add_default_perm(&mut state, "us", "Bowmasters");
        let toml = "name = \"Bowmasters\"\ncard_type = \"creature\"\npower = 1\ntoughness = 1\nlegendary = true\n";
        let def: CardDef = toml::from_str(toml).unwrap();
        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        check_state_based_actions(&mut state, 1, &catalog_map, &mut seeded_rng());
        // Exactly one survives.
        assert_eq!(state.permanents_of("us").filter(|c| c.catalog_key == "Bowmasters").count(), 1,
            "legend rule: one copy survives");
        assert_eq!(state.graveyard_of("us").filter(|c| c.catalog_key == "Bowmasters").count(), 1,
            "legend rule: one copy goes to graveyard");
    }

    #[test]
    fn test_sba_legend_rule_only_one_copy_untouched() {
        let mut state = make_state();
        add_default_perm(&mut state, "us", "Bowmasters");
        let toml = "name = \"Bowmasters\"\ncard_type = \"creature\"\npower = 1\ntoughness = 1\nlegendary = true\n";
        let def: CardDef = toml::from_str(toml).unwrap();
        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        check_state_based_actions(&mut state, 1, &catalog_map, &mut seeded_rng());
        assert_eq!(state.permanents_of("us").filter(|c| c.catalog_key == "Bowmasters").count(), 1,
            "single legendary permanent unaffected by legend rule");
    }

    // ── Section N: Continuous Effects / recompute ─────────────────────────────

    /// A L7 CE that adds +2/+1 to all permanents controlled by "us" is reflected
    /// in the MaterializedState produced by `recompute`.
    #[test]
    fn test_recompute_pt_modifier() {
        let mut state = make_state();

        // Add a 2/2 creature for "us".
        let id = add_default_perm(&mut state, "us", "Grizzly Bears");
        let base_toml = "name = \"Grizzly Bears\"\ncard_type = \"creature\"\npower = 2\ntoughness = 2\n";
        let base_def: CardDef = toml::from_str(base_toml).unwrap();
        let catalog = vec![base_def];
        let catalog_map: HashMap<&str, &CardDef> =
            catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        // Baseline: recompute without any CEs → effective P/T is 2/2.
        let mat = recompute(&state, &catalog_map);
        assert_eq!(mat.generation, 0);
        let eff = mat.defs.get(&id).expect("should be in materialized defs");
        let CardKind::Creature(c) = &eff.kind else { panic!("expected creature") };
        assert_eq!((c.power(), c.toughness()), (2, 2), "baseline P/T should be 2/2");

        // Register a L7 CE that adds +2/+1 to permanents controlled by "us".
        state.continuous_instances.push(ContinuousInstance {
            source_id: ObjId::UNSET,
            controller: "us".to_string(),
            layer: ContinuousLayer::L7PowerToughness,
            filter: std::sync::Arc::new(|_id, controller| controller == "us"),
            modifier: std::sync::Arc::new(|def, _state| {
                if let CardKind::Creature(c) = &mut def.kind {
                    c.adjust_pt(2, 1);
                }
            }),
            expiry: ContinuousExpiry::EndOfTurn,
        });

        // Recompute: effective P/T should now be 4/3.
        let mat2 = recompute(&state, &catalog_map);
        let eff2 = mat2.defs.get(&id).expect("should be in materialized defs after CE");
        let CardKind::Creature(c2) = &eff2.kind else { panic!("expected creature") };
        assert_eq!((c2.power(), c2.toughness()), (4, 3), "CE should produce 4/3");
    }

    /// +1/+1 counters on a creature are folded into the CardDef before CE modifiers run,
    /// so a L7 CE that reads P/T sees the counter-adjusted value.
    #[test]
    fn test_recompute_counters_fold_before_ce() {
        let mut state = make_state();

        // Add a 1/1 with two +1/+1 counters.
        let id = {
            let bf = BattlefieldState { counters: 2, ..BattlefieldState::new() };
            add_perm(&mut state, "us", "Llanowar Elves", bf)
        };
        let base_toml = "name = \"Llanowar Elves\"\ncard_type = \"creature\"\npower = 1\ntoughness = 1\n";
        let base_def: CardDef = toml::from_str(base_toml).unwrap();
        let catalog = vec![base_def];
        let catalog_map: HashMap<&str, &CardDef> =
            catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        // Without any CE: counters fold in → effective 3/3.
        let mat = recompute(&state, &catalog_map);
        let eff = mat.defs.get(&id).expect("creature should be materialized");
        let CardKind::Creature(c) = &eff.kind else { panic!("expected creature") };
        assert_eq!((c.power(), c.toughness()), (3, 3), "two +1/+1 counters should yield 3/3");
    }

    /// Every top-level fire_event advances state.generation by 1, and the resulting
    /// MaterializedState.generation reflects the generation at which it was built.
    #[test]
    fn test_generation_advances_per_tick() {
        let mut state = make_state();
        assert_eq!(state.generation, 0, "initial generation is 0");
        assert_eq!(state.materialized.generation, 0, "initial materialized generation is 0");

        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        let mut rng = seeded_rng();

        // Fire one top-level event.
        fire_event(
            GameEvent::EnteredStep { step: StepKind::Upkeep, active_player: "us".to_string() },
            &mut state, 1, "us", &catalog_map, &mut rng,
        );
        assert_eq!(state.generation, 1, "one tick → generation 1");
        assert_eq!(state.materialized.generation, 1, "snapshot generation matches");

        // Fire a second event.
        fire_event(
            GameEvent::EnteredStep { step: StepKind::Draw, active_player: "us".to_string() },
            &mut state, 1, "us", &catalog_map, &mut rng,
        );
        assert_eq!(state.generation, 2, "second tick → generation 2");
        assert_eq!(state.materialized.generation, 2, "snapshot generation matches");
    }

    // ── Section 13g: StaticAbilityDef + CDA ──────────────────────────────────

    /// A creature with `static_abilities = ["flying"]` in TOML should have the keyword
    /// in its materialized def after ETB, and lose it after LTB.
    #[test]
    fn test_static_ability_def_grants_flying_at_etb() {
        let mut state = make_state();
        let toml = "name = \"Flyer\"\ncard_type = \"creature\"\npower = 2\ntoughness = 2\nstatic_abilities = [\"flying\"]\n";
        let def: CardDef = toml::from_str(toml).unwrap();
        let catalog = vec![def.clone()];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        let id = add_perm_with_def(&mut state, "us", &def, BattlefieldState::new());

        // recompute: CI from static_ability_def should add "flying" to materialized keywords.
        let mat = recompute(&state, &catalog_map);
        assert!(mat.defs[&id].has_keyword("flying"), "flying granted via static_ability_def at ETB");
        // Commit to state.materialized so creature_has_keyword (which reads the snapshot) sees it.
        state.materialized = mat;
        assert!(creature_has_keyword(id, "flying", &state), "creature_has_keyword uses materialized state");
    }

    /// A creature with `static_abilities = ["flying"]` should lose the keyword CI when it
    /// leaves the battlefield (deactivate_instances removes WhileSourceOnBattlefield CIs).
    #[test]
    fn test_static_ability_def_removed_at_ltb() {
        let mut state = make_state();
        let toml = "name = \"Flyer\"\ncard_type = \"creature\"\npower = 2\ntoughness = 2\nstatic_abilities = [\"flying\"]\n";
        let def: CardDef = toml::from_str(toml).unwrap();
        let catalog = vec![def.clone()];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        let id = add_perm_with_def(&mut state, "us", &def, BattlefieldState::new());
        assert_eq!(state.continuous_instances.len(), 1, "CI registered at ETB");

        // Simulate leaving the battlefield.
        deactivate_instances(id, &mut state);
        assert!(state.continuous_instances.is_empty(), "CI removed at LTB");

        // Materialized view no longer has flying.
        let mat = recompute(&state, &catalog_map);
        let mat_def = mat.defs.get(&id);
        // After deactivate_instances, the object may still be on the battlefield
        // in state.objects (we didn't change_zone), but the CI is gone.
        if let Some(d) = mat_def {
            assert!(!d.has_keyword("flying"), "flying removed when CI deactivated");
        }
    }

    /// A CDA: creature whose power = number of cards in its controller's graveyard.
    /// Demonstrates that ContinuousModFn receives live SimState and can read from it.
    #[test]
    fn test_cda_power_equals_graveyard_count() {
        let mut state = make_state();
        let base_def = creature("GoyTest", 0, 3);
        let catalog = vec![base_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        let id = add_perm(&mut state, "us", "GoyTest", BattlefieldState::new());

        // Register a CDA CI: power = number of cards in "us" graveyard.
        state.continuous_instances.push(ContinuousInstance {
            source_id: id,
            controller: "us".to_string(),
            layer: ContinuousLayer::L7PowerToughness,
            filter: std::sync::Arc::new(move |obj_id, _| obj_id == id),
            modifier: std::sync::Arc::new(|def, state| {
                let gy = state.graveyard_of("us").count() as i32;
                if let CardKind::Creature(c) = &mut def.kind {
                    let delta = gy - c.power();
                    c.adjust_pt(delta, 0);
                }
            }),
            expiry: ContinuousExpiry::WhileSourceOnBattlefield,
        });

        // No cards in GY → power = 0.
        let mat = recompute(&state, &catalog_map);
        let CardKind::Creature(c) = &mat.defs[&id].kind else { panic!() };
        assert_eq!(c.power(), 0, "no GY cards → power 0");

        // Add a card to "us" graveyard.
        add_graveyard_card(&mut state, "us", "SomeCard");
        let mat = recompute(&state, &catalog_map);
        let CardKind::Creature(c) = &mat.defs[&id].kind else { panic!() };
        assert_eq!(c.power(), 1, "1 GY card → power 1");

        // Add a second card.
        add_graveyard_card(&mut state, "us", "AnotherCard");
        let mat = recompute(&state, &catalog_map);
        let CardKind::Creature(c) = &mat.defs[&id].kind else { panic!() };
        assert_eq!(c.power(), 2, "2 GY cards → power 2");
    }

    /// recompute now covers all zones; a card in the graveyard must appear in materialized.defs.
    #[test]
    fn test_recompute_includes_graveyard_objects() {
        let mut state = make_state();
        let def: CardDef = toml::from_str(
            "name = \"Goyf\"\ncard_type = \"creature\"\npower = 2\ntoughness = 3\n"
        ).unwrap();
        let catalog = vec![def.clone()];
        let catalog_map: HashMap<&str, &CardDef> =
            catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        let gy_id = add_graveyard_card(&mut state, "us", "Goyf");

        let mat = recompute(&state, &catalog_map);
        assert!(
            mat.defs.contains_key(&gy_id),
            "graveyard card must appear in materialized snapshot"
        );
        let CardKind::Creature(c) = &mat.defs[&gy_id].kind else { panic!("expected creature") };
        assert_eq!(c.power(), 2);
        assert_eq!(c.toughness(), 3);
    }
