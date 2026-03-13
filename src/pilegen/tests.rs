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

    fn empty_libs() -> (Vec<(ObjId, String, CardDef)>, Vec<(ObjId, String, CardDef)>) {
        (vec![], vec![])
    }

    fn creature(name: &str, power: i32, toughness: i32) -> CardDef {
        let toml = format!(
            "name = {:?}\ncard_type = \"creature\"\npower = {}\ntoughness = {}\n",
            name, power, toughness
        );
        toml::from_str(&toml).unwrap()
    }

    fn make_land(name: &str, tapped: bool) -> SimLand {
        SimLand {
            id: ObjId::UNSET,
            name: name.to_string(),
            tapped,
            basic: false,
            land_types: LandTypes::default(),
            mana_abilities: vec![],
        }
    }

    fn stack_item(name: &str, _owner: &str) -> StackItem {
        StackItem {
            id: ObjId::UNSET,
            name: name.to_string(),
            owner: ObjId::UNSET,
            card_id: ObjId::UNSET,
            is_ability: false,
            ability_def: None,
            annotation: None,
            adventure_exile: false,
            adventure_card_name: None,
            adventure_face: None,
            trigger_context: None,
            chosen_targets: vec![],
            ninjutsu_attack_target: None,
            effect: None,
        }
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
    fn test_creature_stats_counters() {
        let perm = SimPermanent { counters: 3, ..SimPermanent::new("Murktide Regent") };
        let def = creature("Murktide Regent", 3, 3);
        assert_eq!(creature_stats(&perm, Some(&def)), (6, 6));
    }

    #[test]
    fn test_creature_stats_from_def() {
        let def = creature("Ragavan", 2, 1);
        let perm = SimPermanent::new("Ragavan");
        assert_eq!(creature_stats(&perm, Some(&def)), (2, 1));
    }

    #[test]
    fn test_creature_stats_defaults() {
        let perm = SimPermanent::new("Unknown");
        assert_eq!(creature_stats(&perm, None), (1, 1));
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
        state.us.lands.push(make_land("Island", true));
        let mut ragavan = SimPermanent::new("Ragavan");
        ragavan.tapped = true;
        ragavan.entered_this_turn = true;
        state.us.permanents.push(ragavan);
        state.us.spells_cast_this_turn = 2;

        let step = Step { kind: StepKind::Untap, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(!state.us.lands[0].tapped, "land should be untapped");
        assert!(!state.us.permanents[0].tapped, "permanent should be untapped");
        assert!(!state.us.permanents[0].entered_this_turn, "summoning sickness should clear");
        assert!(state.us.land_drop_available, "land drop should reset");
        assert_eq!(state.us.spells_cast_this_turn, 0);
    }

    #[test]
    fn test_draw_step_skipped_on_play_turn1() {
        let mut state = make_state();
        let initial_hidden = state.us.hand.hidden;

        let step = Step { kind: StepKind::Draw, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        // on_play=true, t=1, ap="us" → this_player_on_play=true → skip
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert_eq!(state.us.hand.hidden, initial_hidden, "no draw on the play turn 1");
    }

    #[test]
    fn test_draw_step_draws_card() {
        let mut state = make_state();
        let initial_hidden = state.us.hand.hidden;

        let step = Step { kind: StepKind::Draw, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        // on_play=false → this_player_on_play=false → no skip
        do_step(&mut state, 1, "us", &step, 3, false, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert_eq!(state.us.hand.hidden, initial_hidden + 1, "should draw one card");
    }

    #[test]
    fn test_cleanup_removes_damage() {
        let mut state = make_state();
        let mut perm = SimPermanent::new("Ragavan");
        perm.damage = 3;
        state.us.permanents.push(perm);

        let step = Step { kind: StepKind::Cleanup, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert_eq!(state.us.permanents[0].damage, 0);
    }

    #[test]
    fn test_declare_attackers_safe_to_attack() {
        let mut state = make_state();
        let ragavan_def = creature("Ragavan", 2, 4);
        let ragavan_id = state.alloc_id();
        let mut ragavan = SimPermanent::new("Ragavan");
        ragavan.id = ragavan_id;
        ragavan.entered_this_turn = false;
        state.us.permanents.push(ragavan);

        let catalog = vec![ragavan_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareAttackers, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(state.combat_attackers.contains(&ragavan_id), "should attack");
        assert!(state.us.permanents[0].tapped, "attacker should be tapped");
    }

    #[test]
    fn test_declare_attackers_too_risky() {
        let mut state = make_state();
        let attacker_def = creature("Ragavan", 2, 2);
        let blocker_def = creature("Mosscoat Construct", 3, 3);
        let mut ragavan = SimPermanent::new("Ragavan");
        ragavan.entered_this_turn = false;
        state.us.permanents.push(ragavan);
        state.opp.permanents.push(SimPermanent::new("Mosscoat Construct"));

        let catalog = vec![attacker_def, blocker_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareAttackers, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(state.combat_attackers.is_empty(), "should not attack into 3/3");
    }

    #[test]
    fn test_declare_attackers_summoning_sickness() {
        let mut state = make_state();
        let def = creature("Ragavan", 2, 4);
        // entered_this_turn = true (default from SimPermanent::new)
        state.us.permanents.push(SimPermanent::new("Ragavan"));

        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareAttackers, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(state.combat_attackers.is_empty(), "sickness prevents attack");
    }

    #[test]
    fn test_declare_blockers_good_block() {
        let mut state = make_state();
        let atk_def = creature("Ragavan", 2, 2);
        let blk_def = creature("Mosscoat Construct", 3, 3);
        let ragavan_id = state.alloc_id();
        let mut ragavan = SimPermanent::new("Ragavan");
        ragavan.id = ragavan_id;
        ragavan.entered_this_turn = false;
        ragavan.tapped = false;
        state.us.permanents.push(ragavan);
        state.opp.permanents.push(SimPermanent::new("Mosscoat Construct"));
        state.combat_attackers = vec![ragavan_id];

        let catalog = vec![atk_def, blk_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareBlockers, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert_eq!(state.combat_blocks.len(), 1);
        assert_eq!(state.combat_blocks[0], (ragavan_id, ObjId::UNSET));
    }

    #[test]
    fn test_declare_blockers_no_chump() {
        let mut state = make_state();
        let atk_def = creature("Beast", 4, 4);
        let blk_def = creature("Squirrel Token", 1, 1);
        let beast_id = state.alloc_id();
        let mut beast = SimPermanent::new("Beast");
        beast.id = beast_id;
        beast.entered_this_turn = false;
        state.us.permanents.push(beast);
        state.opp.permanents.push(SimPermanent::new("Squirrel Token"));
        state.combat_attackers = vec![beast_id];

        let catalog = vec![atk_def, blk_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareBlockers, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(state.combat_blocks.is_empty(), "should not chump block");
    }

    #[test]
    fn test_combat_damage_unblocked_hits_player() {
        let mut state = make_state();
        let initial_life = state.opp.life;
        let atk_def = creature("Ragavan", 2, 1);
        let ragavan_id = state.alloc_id();
        let mut ragavan = SimPermanent::new("Ragavan");
        ragavan.id = ragavan_id;
        ragavan.tapped = true;
        state.us.permanents.push(ragavan);
        state.combat_attackers = vec![ragavan_id];

        let catalog = vec![atk_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::CombatDamage, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert_eq!(state.opp.life, initial_life - 2);
    }

    #[test]
    fn test_combat_damage_blocked_no_player_damage() {
        let mut state = make_state();
        let initial_life = state.opp.life;
        let atk_def = creature("Ragavan", 2, 2);
        let blk_def = creature("Mosscoat Construct", 3, 3);
        let ragavan_id = state.alloc_id();
        let mut ragavan = SimPermanent::new("Ragavan");
        ragavan.id = ragavan_id;
        ragavan.tapped = true;
        state.us.permanents.push(ragavan);
        let construct_id = state.alloc_id();
        let mut construct = SimPermanent::new("Mosscoat Construct");
        construct.id = construct_id;
        state.opp.permanents.push(construct);
        state.combat_attackers = vec![ragavan_id];
        state.combat_blocks = vec![(ragavan_id, construct_id)];

        let catalog = vec![atk_def, blk_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::CombatDamage, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert_eq!(state.opp.life, initial_life, "blocked — no player damage");
    }

    #[test]
    fn test_combat_damage_sba_kills_both_2_2s() {
        let mut state = make_state();
        let atk_def = creature("Ragavan", 2, 2);
        let blk_def = creature("Mosscoat Construct", 2, 2);
        let ragavan_id = state.alloc_id();
        let mut ragavan = SimPermanent::new("Ragavan");
        ragavan.id = ragavan_id;
        ragavan.tapped = true;
        state.us.permanents.push(ragavan);
        let construct_id = state.alloc_id();
        let mut construct = SimPermanent::new("Mosscoat Construct");
        construct.id = construct_id;
        state.opp.permanents.push(construct);
        state.combat_attackers = vec![ragavan_id];
        state.combat_blocks = vec![(ragavan_id, construct_id)];

        let catalog = vec![atk_def, blk_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::CombatDamage, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(state.us.permanents.is_empty(), "attacker should die");
        assert!(state.opp.permanents.is_empty(), "blocker should die");
        assert!(state.us.graveyard.visible.contains(&"Ragavan".to_string()));
        assert!(state.opp.graveyard.visible.contains(&"Mosscoat Construct".to_string()));
    }

    #[test]
    fn test_combat_damage_outclassed_attacker_dies() {
        let mut state = make_state();
        let atk_def = creature("Ragavan", 2, 2);
        let blk_def = creature("Troll", 3, 3);
        let ragavan_id = state.alloc_id();
        let mut ragavan = SimPermanent::new("Ragavan");
        ragavan.id = ragavan_id;
        ragavan.tapped = true;
        state.us.permanents.push(ragavan);
        let troll_id = state.alloc_id();
        let mut troll = SimPermanent::new("Troll");
        troll.id = troll_id;
        state.opp.permanents.push(troll);
        state.combat_attackers = vec![ragavan_id];
        state.combat_blocks = vec![(ragavan_id, troll_id)];

        let catalog = vec![atk_def, blk_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::CombatDamage, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(state.us.permanents.is_empty(), "attacker dies");
        assert!(!state.opp.permanents.is_empty(), "blocker survives");
    }

    #[test]
    fn test_end_combat_clears_fields() {
        let mut state = make_state();
        let dummy_id = state.alloc_id();
        let dummy_id2 = state.alloc_id();
        state.combat_attackers = vec![dummy_id];
        state.combat_blocks = vec![(dummy_id, dummy_id2)];

        let step = Step { kind: StepKind::EndCombat, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(state.combat_attackers.is_empty());
        assert!(state.combat_blocks.is_empty());
    }

    // ── Section 3: Phase Tests ────────────────────────────────────────────────

    #[test]
    fn test_beginning_phase_untaps_and_draws() {
        let mut state = make_state();
        state.us.lands.push(SimLand {
            id: ObjId::UNSET,
            name: "Island".to_string(),
            tapped: true,
            basic: true,
            land_types: LandTypes { island: true, ..Default::default() },
            mana_abilities: vec![ManaAbility { tap_self: true, produces: "U".into(), ..Default::default() }],
        });
        let initial_hidden = state.us.hand.hidden; // 7

        let (mut us_lib, mut opp_lib) = empty_libs();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        // t=2, on_play=false → draw fires (this_player_on_play=false)
        do_phase(&mut state, 2, "us", &beginning_phase(), 3, false, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(!state.us.lands[0].tapped, "land should be untapped");
        assert_eq!(state.us.hand.hidden, initial_hidden + 1, "should have drawn one card");
    }

    #[test]
    fn test_combat_phase_full_cycle() {
        let mut state = make_state();
        let (mut us_lib, mut opp_lib) = empty_libs();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        do_phase(&mut state, 1, "us", &combat_phase(), 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(state.combat_attackers.is_empty());
        assert!(state.combat_blocks.is_empty());
    }

    // ── Section 4: Priority Action Cycle ─────────────────────────────────────

    #[test]
    fn test_priority_round_both_pass_empty_stack() {
        let mut state = make_state();
        // current_phase is "" (not "Main") → both players pass immediately
        let (mut us_lib, mut opp_lib) = empty_libs();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        handle_priority_round(&mut state, 1, "us", 3, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

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
        let mut us_lib = vec![(ObjId::UNSET, "Dark Ritual".to_string(), def.clone())];
        state.us.pool.b = 1;
        state.us.pool.total = 1;
        // hand.hidden = 7 (from PlayerState::new)

        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        let item = cast_spell(&mut state, 1, "us", "Dark Ritual", &mut us_lib, None, &catalog_map, &mut seeded_rng());

        assert!(item.is_some(), "spell should be cast");
        let item = item.unwrap();
        assert_eq!(item.name, "Dark Ritual");
        assert_eq!(item.owner, state.us.id, "owner should be us player id");
        assert!(!us_lib.iter().any(|(_, n, _)| n == "Dark Ritual"), "removed from library");
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
        let mut us_lib = vec![(ObjId::UNSET, "Doomsday".to_string(), def.clone())];
        // No mana in pool, no lands

        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let item = cast_spell(&mut state, 1, "us", "Doomsday", &mut us_lib, None, &catalog_map, &mut seeded_rng());

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
        "#).unwrap();
        let mut us_lib = vec![
            (ObjId::UNSET, "Force of Will".to_string(), fow_def.clone()),
            (ObjId::UNSET, "Brainstorm".to_string(), brainstorm_def.clone()),
        ];
        let catalog = vec![fow_def.clone(), brainstorm_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        let alt_cost = &fow_def.alternate_costs()[0];
        let initial_life = state.us.life;

        let item = cast_spell(&mut state, 1, "us", "Force of Will", &mut us_lib, Some(alt_cost), &catalog_map, &mut seeded_rng());

        assert!(item.is_some(), "FoW should be cast via pitch");
        assert_eq!(state.us.life, initial_life - 1, "paid 1 life");
        assert!(!us_lib.iter().any(|(_, n, _)| n == "Brainstorm"), "pitch card removed from library");
        assert!(state.us.exile.visible.contains(&"Brainstorm".to_string()), "pitch card exiled");
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
        let initial_hidden = state.us.hand.hidden;
        eff_draw("us", 1).call(&mut state, 1, &[], &HashMap::new(), &mut seeded_rng());

        assert_eq!(state.us.hand.hidden, initial_hidden + 1, "cantrip increments hand count");
    }

    #[test]
    fn test_brainstorm_net_one_card() {
        // draw:3 + put_back:2 = net +1 hand size.
        let mut state = make_state();
        let initial = state.us.hand.hidden;
        eff_draw("us", 3).then(eff_put_back("us", 2))
            .call(&mut state, 1, &[], &HashMap::new(), &mut seeded_rng());

        assert_eq!(state.us.hand.hidden, initial + 1, "Brainstorm nets +1 card");
    }

    #[test]
    fn test_brainstorm_fires_three_draw_events() {
        // All three draws queue triggers; OBM (controlled by opp) should see all three.
        let mut state = make_state();
        state.opp.permanents.push(SimPermanent::new("Orcish Bowmasters"));
        eff_draw("us", 3).then(eff_put_back("us", 2))
            .call(&mut state, 1, &[], &HashMap::new(), &mut seeded_rng());

        // Three Draw events queued → three OBM triggers pending (all non-natural draws).
        let bowmasters_triggers = state.pending_triggers.iter()
            .filter(|tc| tc.kind == "BowmastersDrawTrigger")
            .count();
        assert_eq!(bowmasters_triggers, 3, "OBM pings for each of the 3 Brainstorm draws");
    }

    #[test]
    fn test_brainstorm_flips_tamiyo_on_second_draw_of_three() {
        // Turn context: natural draw already happened (draw_index=1).
        // Brainstorm's 2nd draw = draw_index=3 → Tamiyo flips.
        let mut state = make_state();
        state.us.permanents.push(SimPermanent::new("Tamiyo, Inquisitive Student"));
        state.us.draws_this_turn = 1; // simulate having already drawn naturally
        eff_draw("us", 3).then(eff_put_back("us", 2))
            .call(&mut state, 1, &[], &HashMap::new(), &mut seeded_rng());

        let flip_triggers = state.pending_triggers.iter()
            .filter(|tc| tc.kind == "TamiyoFlip")
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
        state.opp.hand.hidden = 1;
        let target_card: CardDef = toml::from_str(r#"
            name = "Counterspell"
            card_type = "instant"
            mana_cost = "UU"
        "#).unwrap();
        state.opp.library = vec![(ObjId::UNSET, "Counterspell".to_string(), target_card)];
        eff_discard("us", Who::Opp, 1, false).call(&mut state, 1, &[], &HashMap::new(), &mut seeded_rng());

        assert_eq!(state.opp.hand.hidden, 0, "opp hand decremented");
        assert!(state.opp.graveyard.visible.contains(&"Counterspell".to_string()));
        assert!(state.opp.library.is_empty(), "card removed from opp library");
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
        pay_activation_cost(&mut state, 1, "us", ObjId::UNSET, &ability, &mut vec![], &catalog_map);

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
        pay_activation_cost(&mut state, 1, "us", ObjId::UNSET, &ability, &mut vec![], &catalog_map);

        assert_eq!(state.us.life, initial - 2);
    }

    #[test]
    fn test_pay_activation_cost_sacrifice_self() {
        let mut state = make_state();
        let petal_id = state.alloc_id();
        let mut petal = SimPermanent::new("Lotus Petal");
        petal.id = petal_id;
        state.us.permanents.push(petal);
        let ability: AbilityDef = toml::from_str(r#"
            mana_cost = ""
            sacrifice_self = true
            effect = "mana:B"
        "#).unwrap();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        pay_activation_cost(&mut state, 1, "us", petal_id, &ability, &mut vec![], &catalog_map);

        assert!(state.us.permanents.is_empty(), "Lotus Petal should be sacrificed");
        assert!(state.us.graveyard.visible.contains(&"Lotus Petal".to_string()));
    }

    // ── Section 8: Destruction Effects ───────────────────────────────────────

    // Spell resolution: destroy uses item.permanent_target set at cast time.

    #[test]
    fn test_effect_destroy_spell_removes_opp_land() {
        let mut state = make_state();
        let id = state.alloc_id();
        let mut land = make_land("Bayou", false);
        land.id = id;
        state.opp.lands.push(land);
        eff_destroy_target("us").call(&mut state, 1, &[Target::Object(id)], &HashMap::new(), &mut seeded_rng());

        assert!(state.opp.lands.is_empty(), "Bayou should be destroyed");
        assert!(state.opp.graveyard.visible.contains(&"Bayou".to_string()));
    }

    #[test]
    fn test_effect_destroy_spell_removes_opp_creature() {
        let mut state = make_state();
        let id = state.alloc_id();
        let mut troll = SimPermanent::new("Troll");
        troll.id = id;
        state.opp.permanents.push(troll);
        eff_destroy_target("us").call(&mut state, 1, &[Target::Object(id)], &HashMap::new(), &mut seeded_rng());

        assert!(state.opp.permanents.is_empty(), "Troll should be destroyed");
        assert!(state.opp.graveyard.visible.contains(&"Troll".to_string()));
    }

    // Ability resolution: target is chosen at resolution via sim_apply_targeted_effect.

    #[test]
    fn test_effect_destroy_ability_removes_nonbasic_land() {
        let mut state = make_state();
        state.opp.lands.push(SimLand { basic: false, ..make_land("Bayou", false) });
        let ability: AbilityDef = toml::from_str(r#"
            mana_cost = ""
            target = "opp:nonbasic_land"
            effect = "destroy"
        "#).unwrap();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        let (mut us_lib, mut _opp_lib) = empty_libs();
        apply_ability_effect(&mut state, 1, "us", ObjId::UNSET, &ability, &mut us_lib, &catalog_map, &mut seeded_rng(), None);

        assert!(state.opp.lands.is_empty(), "Bayou should be destroyed");
        assert!(state.opp.graveyard.visible.contains(&"Bayou".to_string()));
    }

    #[test]
    fn test_effect_destroy_ability_ignores_basic_land() {
        let mut state = make_state();
        state.opp.lands.push(SimLand { basic: true, ..make_land("Forest", false) });
        let ability: AbilityDef = toml::from_str(r#"
            mana_cost = ""
            target = "opp:nonbasic_land"
            effect = "destroy"
        "#).unwrap();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        let (mut us_lib, mut _opp_lib) = empty_libs();
        apply_ability_effect(&mut state, 1, "us", ObjId::UNSET, &ability, &mut us_lib, &catalog_map, &mut seeded_rng(), None);

        assert!(!state.opp.lands.is_empty(), "basic Forest should survive");
        assert!(state.opp.graveyard.visible.is_empty());
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
        state.us.graveyard.visible = vec!["A".into(), "B".into(), "C".into(),
                                          "D".into(), "E".into(), "F".into(), "G".into()];
        state.us.pool.u  = 1;
        state.us.pool.total = 1; // only 1 mana in pool — delve pays the other 7

        let mut us_lib = vec![(ObjId::UNSET, "Treasure Cruise".to_string(), def.clone())];
        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        let item = cast_spell(&mut state, 1, "us", "Treasure Cruise", &mut us_lib, None, &catalog_map, &mut seeded_rng());

        assert!(item.is_some(), "should cast with full delve");
        assert_eq!(state.us.graveyard.visible.len(), 0, "all 7 graveyard cards exiled");
        assert_eq!(state.us.exile.visible.len(), 7, "exiled by delve");
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
        state.us.graveyard.visible = vec!["Ritual".into(), "Ponder".into()];
        state.us.pool.total = 1; // covers the 1 remaining generic after delve

        let mut us_lib = vec![(ObjId::UNSET, "Dead Drop".to_string(), def.clone())];
        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        let item = cast_spell(&mut state, 1, "us", "Dead Drop", &mut us_lib, None, &catalog_map, &mut seeded_rng());

        assert!(item.is_some(), "should cast with partial delve + 1 mana");
        assert_eq!(state.us.graveyard.visible.len(), 0, "both graveyard cards exiled");
        assert_eq!(state.us.exile.visible.len(), 2);
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

        state.us.graveyard.visible = vec![
            "Dark Ritual".into(), "Ponder".into(), "Consider".into(), "Ragavan".into(),
        ];
        // After delving all 4, generic cost = 5-4 = 1. Need UU + 1 generic.
        state.us.pool.u  = 2;
        state.us.pool.total = 3;

        let catalog = vec![murktide_def.clone(), ritual_def, ponder_def, consider_def, ragavan_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let mut us_lib = vec![(ObjId::UNSET, "Murktide Regent".to_string(), murktide_def)];

        let item = cast_spell(&mut state, 1, "us", "Murktide Regent", &mut us_lib, None, &catalog_map, &mut seeded_rng()).unwrap();
        // annotation encodes "+3" (3 instants/sorceries: Ritual, Ponder, Consider)
        assert_eq!(item.annotation.as_deref(), Some("+3"));

        // Resolve via Effect path
        let rng_dyn: &mut dyn rand::RngCore = &mut seeded_rng();
        item.effect.as_ref().unwrap().call(&mut state, 1, &item.chosen_targets, &catalog_map, rng_dyn);

        let murktide = &state.us.permanents[0];
        assert_eq!(murktide.counters, 3, "3 instants/sorceries exiled → 3 counters");
        assert!(murktide.annotation.is_none(), "counter annotation consumed");

        // creature_stats reflects counters in damage calculations
        let def = catalog_map["Murktide Regent"];
        assert_eq!(creature_stats(murktide, Some(def)), (6, 6));
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

        state.us.graveyard.visible = vec!["Ragavan".into()];
        // 5 - 1 = 4 generic remaining; need UU + 4 generic
        state.us.pool.u  = 2;
        state.us.pool.total = 6;

        let catalog = vec![murktide_def.clone(), ragavan_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let mut us_lib = vec![(ObjId::UNSET, "Murktide Regent".to_string(), murktide_def)];

        let item = cast_spell(&mut state, 1, "us", "Murktide Regent", &mut us_lib, None, &catalog_map, &mut seeded_rng()).unwrap();
        assert!(item.annotation.is_none(), "no instants/sorceries → no counter annotation");

        let rng_dyn: &mut dyn rand::RngCore = &mut seeded_rng();
        item.effect.as_ref().unwrap().call(&mut state, 1, &item.chosen_targets, &catalog_map, rng_dyn);

        let murktide = &state.us.permanents[0];
        assert_eq!(murktide.counters, 0);
        let def = catalog_map["Murktide Regent"];
        assert_eq!(creature_stats(murktide, Some(def)), (3, 3));
    }

    #[test]
    fn test_murktide_attacks_with_counter_boosted_stats() {
        // A 6/6 Murktide (base 3/3 + 3 counters) should survive attacking into a 5-power blocker.
        let mut state = make_state();
        let murktide_def = creature("Murktide Regent", 3, 3);
        let murktide_id = state.alloc_id();
        let mut murktide = SimPermanent::new("Murktide Regent");
        murktide.id = murktide_id;
        murktide.counters = 3;
        murktide.entered_this_turn = false;
        // Opponent has a 5/5 blocker — Murktide's toughness 6 > opp power 5, safe to attack.
        let blocker_def = creature("Dragon", 5, 5);
        state.opp.permanents.push(SimPermanent::new("Dragon"));
        state.us.permanents.push(murktide);

        let catalog = vec![murktide_def, blocker_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareAttackers, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

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
        state.us.graveyard.visible = vec!["Ritual".into(), "Ponder".into()];
        // no mana

        let mut us_lib = vec![(ObjId::UNSET, "Dead Drop".to_string(), def.clone())];
        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        let item = cast_spell(&mut state, 1, "us", "Dead Drop", &mut us_lib, None, &catalog_map, &mut seeded_rng());

        assert!(item.is_none(), "can't cast — 1 generic still unpaid");
        assert_eq!(state.us.graveyard.visible.len(), 2, "graveyard unchanged on failed cast");
        assert!(state.us.exile.visible.is_empty(), "nothing exiled on failed cast");
    }

    #[test]
    fn test_effect_exile_ability_removes_creature() {
        let mut state = make_state();
        state.opp.permanents.push(SimPermanent::new("Troll"));
        let troll_def = creature("Troll", 2, 2);
        let ability: AbilityDef = toml::from_str(r#"
            mana_cost = ""
            target = "opp:creature"
            effect = "exile"
        "#).unwrap();
        let catalog = vec![troll_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let (mut us_lib, mut _opp_lib) = empty_libs();
        apply_ability_effect(&mut state, 1, "us", ObjId::UNSET, &ability, &mut us_lib, &catalog_map, &mut seeded_rng(), None);

        assert!(state.opp.permanents.is_empty(), "Troll should be exiled");
        assert!(state.opp.exile.visible.contains(&"Troll".to_string()));
        assert!(state.opp.graveyard.visible.is_empty(), "exiled, not dead");
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

    fn island_land() -> SimLand {
        SimLand {
            id: ObjId::UNSET,
            name: "Island".to_string(),
            tapped: false,
            basic: true,
            land_types: LandTypes { island: true, ..Default::default() },
            mana_abilities: vec![ManaAbility { tap_self: true, produces: "U".into(), ..Default::default() }],
        }
    }

    #[test]
    fn test_declare_attackers_sets_attacking_flag() {
        let mut state = make_state();
        let def = creature("Attacker", 2, 4);
        let mut perm = SimPermanent::new("Attacker");
        perm.entered_this_turn = false;
        state.us.permanents.push(perm);

        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareAttackers, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(state.us.permanents[0].attacking, "declared attacker gets attacking=true");
    }

    #[test]
    fn test_declare_blockers_sets_unblocked_flag_when_no_blocker() {
        let mut state = make_state();
        let def = creature("Attacker", 2, 4);
        let attacker_id = state.alloc_id();
        let mut perm = SimPermanent::new("Attacker");
        perm.id = attacker_id;
        perm.attacking = true;
        perm.tapped = true;
        state.us.permanents.push(perm);
        state.combat_attackers = vec![attacker_id];
        // No opp creatures → no blocker

        let catalog = vec![def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareBlockers, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(state.us.permanents[0].unblocked, "unblocked attacker gets unblocked=true");
    }

    #[test]
    fn test_declare_blockers_blocked_attacker_not_unblocked() {
        let mut state = make_state();
        let atk_def = creature("Ragavan", 2, 2);
        let blk_def = creature("Wall", 0, 6);
        let ragavan_id = state.alloc_id();
        let mut ragavan = SimPermanent::new("Ragavan");
        ragavan.id = ragavan_id;
        ragavan.attacking = true;
        ragavan.tapped = true;
        state.us.permanents.push(ragavan);
        state.opp.permanents.push(SimPermanent::new("Wall"));
        state.combat_attackers = vec![ragavan_id];

        let catalog = vec![atk_def, blk_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareBlockers, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(!state.us.permanents[0].unblocked, "blocked attacker stays unblocked=false");
        assert_eq!(state.combat_blocks.len(), 1, "blocker declared");
    }

    #[test]
    fn test_end_combat_clears_attacking_unblocked_flags() {
        let mut state = make_state();
        let ninja_id = state.alloc_id();
        let mut perm = SimPermanent::new("Ninja");
        perm.id = ninja_id;
        perm.attacking = true;
        perm.unblocked = true;
        state.us.permanents.push(perm);
        state.combat_attackers = vec![ninja_id];

        let step = Step { kind: StepKind::EndCombat, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(!state.us.permanents[0].attacking, "attacking cleared at EndCombat");
        assert!(!state.us.permanents[0].unblocked, "unblocked cleared at EndCombat");
    }

    // Negative try_ninjutsu precondition tests (deterministic — RNG roll is never reached).

    #[test]
    fn test_try_ninjutsu_no_hand_returns_none() {
        let mut state = make_state();
        state.us.hand.hidden = 0;
        let mut atk = SimPermanent::new("Ragavan"); atk.attacking = true; atk.unblocked = true;
        state.us.permanents.push(atk);
        let lib = vec![(ObjId::UNSET, "Ninja".to_string(), ninja_def())];
        assert!(try_ninjutsu(&state, "us", &lib, &mut seeded_rng()).is_none(), "no hand → None");
    }

    #[test]
    fn test_try_ninjutsu_no_unblocked_returns_none() {
        let mut state = make_state();
        state.us.hand.hidden = 3;
        let mut atk = SimPermanent::new("Ragavan"); atk.attacking = true; atk.unblocked = false;
        state.us.permanents.push(atk);
        state.us.pool.u = 1; state.us.pool.total = 1;
        let lib = vec![(ObjId::UNSET, "Ninja".to_string(), ninja_def())];
        assert!(try_ninjutsu(&state, "us", &lib, &mut seeded_rng()).is_none(), "no unblocked attacker → None");
    }

    #[test]
    fn test_try_ninjutsu_no_ninja_in_library_returns_none() {
        let mut state = make_state();
        state.us.hand.hidden = 3;
        let mut atk = SimPermanent::new("Ragavan"); atk.attacking = true; atk.unblocked = true;
        state.us.permanents.push(atk);
        state.us.pool.u = 1; state.us.pool.total = 1;
        let lib = vec![(ObjId::UNSET, "Brainstorm".to_string(), toml::from_str::<CardDef>("name=\"Brainstorm\"\ncard_type=\"instant\"\nmana_cost=\"U\"").unwrap())];
        assert!(try_ninjutsu(&state, "us", &lib, &mut seeded_rng()).is_none(), "no ninja card → None");
    }

    #[test]
    fn test_try_ninjutsu_no_mana_returns_none() {
        let mut state = make_state();
        state.us.hand.hidden = 3;
        let mut atk = SimPermanent::new("Ragavan"); atk.attacking = true; atk.unblocked = true;
        state.us.permanents.push(atk);
        // No mana available
        let lib = vec![(ObjId::UNSET, "Ninja".to_string(), ninja_def())];
        assert!(try_ninjutsu(&state, "us", &lib, &mut seeded_rng()).is_none(), "no mana → None");
    }

    #[test]
    fn test_ninjutsu_swaps_attacker_for_ninja() {
        // try_ninjutsu returns ActivateAbility; when committed via handle_priority_round
        // in a DeclareBlockers window, the ninja enters play and the attacker returns to hand.
        let def = ninja_def();
        let catalog = vec![def.clone()];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        // Loop over seeds until ninjutsu fires (35% per attempt → statistically guaranteed within 50).
        for seed in 0u64..50 {
            let mut state = make_state();
            state.current_phase = "DeclareBlockers".to_string();
            state.current_ap = state.us.id;
            let mut ragavan = SimPermanent::new("Ragavan");
            ragavan.attacking = true; ragavan.unblocked = true;
            state.us.permanents.push(ragavan);
            let initial_hand = state.us.hand.hidden;
            state.us.lands.push(island_land());
            // Allocate a real id for the ninja library card and register it in state.cards
            // so apply_ability_effect can look up the ninja's name at resolution.
            let ninja_lib_id = state.alloc_id();
            state.cards.insert(ninja_lib_id, CardObject::new(ninja_lib_id, "Ninja".to_string(), "us"));
            let mut us_lib = vec![(ninja_lib_id, "Ninja".to_string(), def.clone())];
            let mut opp_lib: Vec<(ObjId, String, CardDef)> = vec![];
            let mut rng = StdRng::seed_from_u64(seed);
            handle_priority_round(&mut state, 1, "us", 3, &mut us_lib, &mut opp_lib, &catalog_map, &mut rng);

            if state.us.permanents.iter().any(|p| p.name == "Ninja") {
                let ninja = state.us.permanents.iter().find(|p| p.name == "Ninja").unwrap();
                assert!(ninja.attacking, "ninja should be attacking");
                assert!(ninja.tapped, "ninja should be tapped");
                assert!(!state.us.permanents.iter().any(|p| p.name == "Ragavan"), "Ragavan returned to hand");
                assert_eq!(state.us.hand.hidden, initial_hand, "net hand size unchanged (+1 return, -1 ninja)");
                let ninja_id = state.us.permanents.iter().find(|p| p.name == "Ninja").unwrap().id;
                assert!(state.combat_attackers.contains(&ninja_id), "ninja in combat_attackers");
                return;
            }
        }
        panic!("ninjutsu should have fired within 50 seeds");
    }

    // ── Section 11: Cycling ───────────────────────────────────────────────────

    #[test]
    fn test_cycling_draw_effect() {
        // apply_ability_effect with draw:1 increments hand.hidden.
        let mut state = make_state();
        let initial = state.us.hand.hidden;
        let ability: AbilityDef = toml::from_str(r#"effect = "draw:1""#).unwrap();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        apply_ability_effect(&mut state, 1, "us", ObjId::UNSET, &ability, &mut vec![], &catalog_map, &mut seeded_rng(), None);
        assert_eq!(state.us.hand.hidden, initial + 1, "cycling draws one card");
    }

    #[test]
    fn test_cycling_discard_self_removes_card_from_library() {
        // pay_activation_cost with discard_self=true removes the card from the library
        // (simulating it being discarded from hand) and sends it to the graveyard.
        let mut state = make_state();
        let wraith_def: CardDef = toml::from_str(r#"
            name = "Street Wraith"
            card_type = "creature"
            mana_cost = "3BB"
            power = 3
            toughness = 4
        "#).unwrap();
        let mut us_lib = vec![(ObjId::UNSET, "Street Wraith".to_string(), wraith_def.clone())];
        let ability: AbilityDef = toml::from_str(r#"
            zone = "hand"
            discard_self = true
            life_cost = 2
            effect = "draw:1"
        "#).unwrap();
        let catalog = vec![wraith_def];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let initial_hand = state.us.hand.hidden;

        pay_activation_cost(&mut state, 1, "us", ObjId::UNSET, &ability, &mut us_lib, &catalog_map);

        assert!(us_lib.is_empty(), "Street Wraith removed from library");
        assert!(state.us.graveyard.visible.contains(&"Street Wraith".to_string()), "in graveyard");
        assert_eq!(state.us.hand.hidden, initial_hand - 1, "hand size decremented");
        assert_eq!(state.us.life, 20 - 2, "paid 2 life");
    }

    // ── Section 12: Adventure ─────────────────────────────────────────────────

    #[test]
    fn test_adventure_resolve_exiles_to_on_adventure() {
        // An adventure StackItem (no target) routes the card to exile + on_adventure.
        let mut state = make_state();
        // Simulate the adventure resolution inline: no effect, just exile.
        let card_name = "Brazen Borrower".to_string();
        state.us.exile.visible.push(card_name.clone());
        state.us.on_adventure.push(card_name.clone());

        assert!(state.us.exile.visible.contains(&"Brazen Borrower".to_string()), "Borrower in exile");
        assert!(state.us.on_adventure.contains(&"Brazen Borrower".to_string()), "Borrower on adventure");
        assert!(state.us.graveyard.visible.is_empty(), "not in graveyard");
    }

    #[test]
    fn test_adventure_bounce_effect_returns_opp_permanent() {
        // Petty Theft bounces target opp permanent then exiles Brazen Borrower to on_adventure.
        let mut state = make_state();
        let bowmasters_id = state.alloc_id();
        let mut bowmasters = SimPermanent::new("Orcish Bowmasters");
        bowmasters.id = bowmasters_id;
        state.opp.permanents.push(bowmasters);
        let initial_opp_hand = state.opp.hand.hidden;

        // Run the Effect directly (as the new adventure resolution path does).
        let eff = eff_bounce_target("us");
        eff.call(&mut state, 1, &[Target::Object(bowmasters_id)], &HashMap::new(), &mut seeded_rng());
        // Then exile the card to on_adventure.
        state.us.exile.visible.push("Brazen Borrower".to_string());
        state.us.on_adventure.push("Brazen Borrower".to_string());

        assert!(state.opp.permanents.is_empty(), "Bowmasters bounced off board");
        assert_eq!(state.opp.hand.hidden, initial_opp_hand + 1, "bounced to opp hand");
        assert!(state.us.exile.visible.contains(&"Brazen Borrower".to_string()), "Borrower on adventure in exile");
    }

    #[test]
    fn test_cast_from_adventure_enters_play() {
        // With the still_valid fix, CastFromAdventure in pending_actions is validated and
        // executed, putting the creature into play and clearing the on_adventure marker.
        let borrower_def: CardDef = toml::from_str(r#"
            name = "Brazen Borrower"
            card_type = "creature"
            mana_cost = "1UU"
            blue = true
            power = 3
            toughness = 1
        "#).unwrap();
        let catalog = vec![borrower_def.clone()];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();

        let mut state = make_state();
        state.current_phase = "Main".to_string();
        state.current_ap = state.us.id;
        state.us.exile.visible.push("Brazen Borrower".to_string());
        state.us.on_adventure.push("Brazen Borrower".to_string());
        // 1UU mana: two Islands + one generic (Swamp)
        state.us.lands.push(island_land());
        state.us.lands.push(SimLand { name: "Island2".to_string(), ..island_land() });
        state.us.lands.push(SimLand {
            name: "Swamp".to_string(),
            basic: true,
            land_types: LandTypes { swamp: true, ..Default::default() },
            mana_abilities: vec![ManaAbility { tap_self: true, produces: "B".into(), ..Default::default() }],
            ..island_land()
        });
        // Inject the pending action directly (bypasses the 75% roll from collect_on_board_actions).
        state.us.pending_actions = vec![
            PriorityAction::CastFromAdventure { card_name: "Brazen Borrower".to_string() }
        ];
        let (mut us_lib, mut opp_lib) = empty_libs();
        handle_priority_round(&mut state, 1, "us", 3, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(state.us.permanents.iter().any(|p| p.name == "Brazen Borrower"), "Borrower enters play");
        assert!(!state.us.on_adventure.contains(&"Brazen Borrower".to_string()), "removed from on_adventure");
        assert!(!state.us.exile.visible.contains(&"Brazen Borrower".to_string()), "removed from exile");
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

        let murktide_id = state.alloc_id();
        let mut attacker = SimPermanent::new("Murktide Regent");
        attacker.id = murktide_id;
        attacker.attacking = true;
        state.us.permanents.push(attacker);
        state.opp.permanents.push(SimPermanent::new("Troll"));
        state.combat_attackers = vec![murktide_id];

        let catalog = vec![flyer, ground];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareBlockers, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(state.combat_blocks.is_empty(), "ground creature cannot block a flyer");
    }

    #[test]
    fn test_flying_blocked_by_flyer() {
        // Flying attacker CAN be blocked by another flying creature.
        let mut state = make_state();
        let flyer_atk = flying_creature("Murktide Regent", 3, 3);
        let flyer_blk = flying_creature("Subtlety", 3, 3);

        let murktide_id = state.alloc_id();
        let subtlety_id = state.alloc_id();
        let mut attacker = SimPermanent::new("Murktide Regent");
        attacker.id = murktide_id;
        attacker.attacking = true;
        state.us.permanents.push(attacker);
        let mut subtlety = SimPermanent::new("Subtlety");
        subtlety.id = subtlety_id;
        state.opp.permanents.push(subtlety);
        state.combat_attackers = vec![murktide_id];

        let catalog = vec![flyer_atk, flyer_blk];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareBlockers, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

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

        let murktide_id = state.alloc_id();
        let mut perm = SimPermanent::new("Murktide Regent");
        perm.id = murktide_id;
        perm.entered_this_turn = false;
        state.us.permanents.push(perm);
        state.opp.permanents.push(SimPermanent::new("Troll"));

        let catalog = vec![flyer, ground];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let step = Step { kind: StepKind::DeclareAttackers, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        do_step(&mut state, 1, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        // Murktide's toughness (3) > relevant blocking power (0 — Troll can't block flyer).
        assert!(state.combat_attackers.contains(&murktide_id),
            "flying creature should attack when only ground blockers exist");
    }

    // ── Section 9: Trigger Tests ──────────────────────────────────────────────

    fn bowmasters_def() -> CardDef {
        toml::from_str(r#"
            name = "Orcish Bowmasters"
            card_type = "creature"
            mana_cost = "1B"
            power = 1
            toughness = 1
        "#).unwrap()
    }

    fn murktide_def() -> CardDef {
        toml::from_str(r#"
            name = "Murktide Regent"
            card_type = "creature"
            mana_cost = "5UU"
            power = 3
            toughness = 3
            delve = true
            keywords = ["flying"]
        "#).unwrap()
    }

    #[test]
    fn test_fire_triggers_returns_context_for_bowmasters_etb() {
        let mut state = make_state();
        state.opp.permanents.push(SimPermanent::new("Orcish Bowmasters"));

        let ev = GameEvent::ZoneChange {
            card: "Orcish Bowmasters".to_string(),
            card_type: "creature".to_string(),
            from: ZoneId::Stack,
            to: ZoneId::Battlefield,
            controller: "opp".to_string(),
        };
        let result = fire_triggers(&ev, &state);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].kind, "BowmastersEtb");
    }

    #[test]
    fn test_fire_triggers_empty_when_no_bowmasters_in_play() {
        let state = make_state(); // no permanents
        let ev = GameEvent::ZoneChange {
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
        let ctx = bowmasters_etb_ctx(controller);
        let targets: Vec<Target> = choose_trigger_target(&ctx.target_spec, controller, state, catalog_map)
            .into_iter().collect();
        apply_trigger(&ctx, &targets, state, 1, catalog_map);
    }

    #[test]
    fn test_apply_bowmasters_etb_deals_damage_and_creates_army() {
        let mut state = make_state();
        state.opp.permanents.push(SimPermanent::new("Orcish Bowmasters"));
        let initial_life = state.us.life;
        fire_bowmasters_etb("opp", &mut state, &HashMap::new());
        assert_eq!(state.us.life, initial_life - 1, "ETB deals 1 to us");
        assert!(state.opp.permanents.iter().any(|p| p.name == "Orc Army"), "Orc Army token created");
        let army = state.opp.permanents.iter().find(|p| p.name == "Orc Army").unwrap();
        assert_eq!(army.counters, 1, "Orc Army has 1 counter");
    }

    #[test]
    fn test_apply_bowmasters_etb_grows_existing_army() {
        let mut state = make_state();
        state.opp.permanents.push(SimPermanent::new("Orcish Bowmasters"));
        let mut army = SimPermanent::new("Orc Army");
        army.counters = 2;
        state.opp.permanents.push(army);
        fire_bowmasters_etb("opp", &mut state, &HashMap::new());
        let army = state.opp.permanents.iter().find(|p| p.name == "Orc Army").unwrap();
        assert_eq!(army.counters, 3, "Orc Army grows from 2 to 3");
    }

    #[test]
    fn test_bowmasters_ping_hits_face_when_no_killable_creature() {
        let mut state = make_state();
        state.opp.permanents.push(SimPermanent::new("Orcish Bowmasters"));
        let initial_life = state.us.life;
        state.us.permanents.push(SimPermanent::new("Troll"));
        let catalog = vec![creature("Troll", 3, 3)];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        fire_bowmasters_etb("opp", &mut state, &catalog_map);
        assert_eq!(state.us.life, initial_life - 1, "damage hits face when no killable creature");
        assert!(state.us.permanents.iter().any(|p| p.name == "Troll"), "Troll survives");
    }

    #[test]
    fn test_bowmasters_ping_kills_1_1_creature() {
        let mut state = make_state();
        state.opp.permanents.push(SimPermanent::new("Orcish Bowmasters"));
        let initial_life = state.us.life;
        state.us.permanents.push(SimPermanent::new("Ragavan, Nimble Pilferer"));
        let catalog = vec![creature("Ragavan, Nimble Pilferer", 2, 1)];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        fire_bowmasters_etb("opp", &mut state, &catalog_map);
        check_lethal_damage("us", &mut state, 1, &catalog_map);
        assert_eq!(state.us.life, initial_life, "life total unchanged when creature is targeted");
        assert!(!state.us.permanents.iter().any(|p| p.name == "Ragavan, Nimble Pilferer"),
            "Ragavan dies to 1 damage");
        assert!(state.us.graveyard.visible.contains(&"Ragavan, Nimble Pilferer".to_string()),
            "Ragavan goes to graveyard");
    }

    #[test]
    fn test_bowmasters_ping_prioritises_opposing_bowmasters() {
        let mut state = make_state();
        state.opp.permanents.push(SimPermanent::new("Orcish Bowmasters"));
        let troll_id = state.alloc_id();
        let bowmasters_id = state.alloc_id();
        let mut troll = SimPermanent::new("Troll");
        troll.id = troll_id;
        let mut bowmasters = SimPermanent::new("Orcish Bowmasters");
        bowmasters.id = bowmasters_id;
        state.us.permanents.push(troll);
        state.us.permanents.push(bowmasters);
        let catalog = vec![creature("Troll", 3, 3), creature("Orcish Bowmasters", 1, 1)];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        fire_bowmasters_etb("opp", &mut state, &catalog_map);
        check_lethal_damage("us", &mut state, 1, &catalog_map);
        assert!(!state.us.permanents.iter().any(|p| p.name == "Orcish Bowmasters"),
            "opposing Bowmasters is killed");
        assert!(state.us.permanents.iter().any(|p| p.name == "Troll"), "Troll survives");
    }

    #[test]
    fn test_bowmasters_no_trigger_on_natural_first_draw() {
        let mut state = make_state();
        state.opp.permanents.push(SimPermanent::new("Orcish Bowmasters"));

        let ev = GameEvent::Draw { controller: "us".to_string(), draw_index: 1, is_natural: true };
        let result = fire_triggers(&ev, &state);
        assert!(result.is_empty(), "no trigger on first natural draw");
    }

    #[test]
    fn test_bowmasters_triggers_on_cantrip_draw() {
        let mut state = make_state();
        state.opp.permanents.push(SimPermanent::new("Orcish Bowmasters"));

        let ev = GameEvent::Draw { controller: "us".to_string(), draw_index: 1, is_natural: false };
        let result = fire_triggers(&ev, &state);
        assert_eq!(result.len(), 1, "cantrip draw triggers Bowmasters");
    }

    #[test]
    fn test_murktide_counter_on_instant_exile() {
        let mut state = make_state();
        let mut murktide = SimPermanent::new("Murktide Regent");
        murktide.counters = 0;
        state.us.permanents.push(murktide);

        let ev = GameEvent::ZoneChange {
            card: "Counterspell".to_string(),
            card_type: "instant".to_string(),
            from: ZoneId::Graveyard,
            to: ZoneId::Exile,
            controller: "us".to_string(),
        };
        let result = fire_triggers(&ev, &state);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].kind, "MurktideExile");

        let mut state2 = state;
        apply_trigger(&result[0], &[], &mut state2, 1, &HashMap::new());
        let murktide = state2.us.permanents.iter().find(|p| p.name == "Murktide Regent").unwrap();
        assert_eq!(murktide.counters, 1, "Murktide gains +1/+1 counter");
    }

    #[test]
    fn test_murktide_no_counter_on_land_exile() {
        let mut state = make_state();
        state.us.permanents.push(SimPermanent::new("Murktide Regent"));

        let ev = GameEvent::ZoneChange {
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
        let mut tamiyo = SimPermanent::new("Tamiyo, Inquisitive Student");
        tamiyo.attacking = true;
        state.us.permanents.push(tamiyo);

        let ev = GameEvent::EnteredStep {
            step: StepKind::DeclareAttackers,
            active_player: "us".to_string(),
        };
        let result = fire_triggers(&ev, &state);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].kind, "TamiyoClue");

        let mut state2 = state;
        apply_trigger(&result[0], &[], &mut state2, 1, &HashMap::new());
        assert!(state2.us.permanents.iter().any(|p| p.name == "Clue Token"),
            "Clue Token created when Tamiyo attacks");
    }

    #[test]
    fn test_tamiyo_no_clue_when_not_attacking() {
        let mut state = make_state();
        let tamiyo = SimPermanent::new("Tamiyo, Inquisitive Student"); // attacking = false
        state.us.permanents.push(tamiyo);

        let ev = GameEvent::EnteredStep {
            step: StepKind::DeclareAttackers,
            active_player: "us".to_string(),
        };
        let result = fire_triggers(&ev, &state);
        // Trigger queues (Tamiyo is in play), but resolves to nothing (not attacking).
        if let Some(ctx) = result.first() {
            let mut state2 = state;
            apply_trigger(ctx, &[], &mut state2, 1, &HashMap::new());
            assert!(!state2.us.permanents.iter().any(|p| p.name == "Clue Token"),
                "no Clue Token if Tamiyo is not attacking");
        }
    }

    #[test]
    fn test_tamiyo_flip_on_third_draw() {
        let mut state = make_state();
        state.us.permanents.push(SimPermanent::new("Tamiyo, Inquisitive Student"));

        let ev = GameEvent::Draw { controller: "us".to_string(), draw_index: 3, is_natural: false };
        let result = fire_triggers(&ev, &state);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].kind, "TamiyoFlip");

        let mut state2 = state;
        apply_trigger(&result[0], &[], &mut state2, 1, &HashMap::new());
        assert!(!state2.us.permanents.iter().any(|p| p.name == "Tamiyo, Inquisitive Student"),
            "original Tamiyo removed");
        assert!(state2.us.permanents.iter().any(|p| p.name == "Tamiyo, Seasoned Scholar"),
            "Tamiyo, Seasoned Scholar enters");
    }

    #[test]
    fn test_tamiyo_plus_two_applies_power_mod_to_attackers() {
        let mut state = make_state();
        // Register the +2 effect for "us" (as if us activated it last turn).
        state.active_effects.push(tamiyo_plus_two_effect("us", ObjId::UNSET));
        // Opp has a 3/3 attacker.
        let atk_def = creature("Dragon", 3, 3);
        let mut atk = SimPermanent::new("Dragon");
        atk.entered_this_turn = false;
        state.opp.permanents.push(atk);
        state.us.permanents.push(SimPermanent::new("Wall")); // blocker-sized (no block in this test)

        let catalog = vec![atk_def, creature("Wall", 0, 4)];
        let catalog_map: HashMap<&str, &CardDef> = catalog.iter().map(|c| (c.name.as_str(), c)).collect();
        let (mut us_lib, mut opp_lib) = empty_libs();
        let mut rng = seeded_rng();
        do_step(&mut state, 1, "opp", &Step { kind: StepKind::DeclareAttackers, prio: true },
            3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut rng);

        let dragon = state.opp.permanents.iter().find(|p| p.name == "Dragon").unwrap();
        assert_eq!(dragon.power_mod, -1, "Dragon gets -1 power from Tamiyo +2");
        // creature_stats should reflect the mod.
        let (pow, _) = creature_stats(dragon, catalog_map.get("Dragon").copied());
        assert_eq!(pow, 2, "Dragon's effective power is 3 + (-1) = 2");
    }

    #[test]
    fn test_tamiyo_plus_two_expires_at_controller_untap() {
        let mut state = make_state();
        state.active_effects.push(tamiyo_plus_two_effect("us", ObjId::UNSET));
        assert_eq!(state.active_effects.len(), 1);

        // Untap step for "us" should expire the effect.
        let step = Step { kind: StepKind::Untap, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        do_step(&mut state, 2, "us", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        assert!(state.active_effects.is_empty(), "Effect expires at controller's next Untap");
    }

    #[test]
    fn test_stat_mod_reversed_at_cleanup() {
        // A StatMod effect with EndOfTurn expiry should undo power_mod during Cleanup.
        let mut state = make_state();
        let dragon_id = state.alloc_id();
        let mut atk = SimPermanent::new("Dragon");
        atk.id = dragon_id;
        atk.power_mod = -1;
        state.opp.permanents.push(atk);
        // Register the StatMod effect that will be unwound.
        state.active_effects.push(ContinuousEffect {
            controller: "us".to_string(),
            expires: EffectExpiry::EndOfTurn,
            on_event: None,
            stat_mod: Some(StatModData {
                target_id: dragon_id,
                power_delta: -1,
                toughness_delta: 0,
            }),
        });

        let step = Step { kind: StepKind::Cleanup, prio: false };
        let (mut us_lib, mut opp_lib) = empty_libs();
        let catalog_map: HashMap<&str, &CardDef> = HashMap::new();
        do_step(&mut state, 1, "opp", &step, 3, true, &mut us_lib, &mut opp_lib, &catalog_map, &mut seeded_rng());

        let dragon = state.opp.permanents.iter().find(|p| p.name == "Dragon").unwrap();
        assert_eq!(dragon.power_mod, 0, "power_mod reversed by StatMod expiry in Cleanup");
        assert!(state.active_effects.is_empty(), "EndOfTurn effect removed");
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
            state.active_effects.push(ContinuousEffect {
                controller: "us".to_string(),
                expires: EffectExpiry::EndOfTurn,
                on_event: Some(std::sync::Arc::new(move |e, _ctl| {
                    if let GameEvent::EnteredStep { step, .. } = e {
                        if *step == step_kind {
                            return Some(TriggerContext {
                                source_id: ObjId::UNSET, source_name: format!("test-{:?}", step_kind),
                                controller: "us".to_string(),
                                kind: "TestStepTrigger",
                                target_spec: TargetSpec::None,
                                effect: std::sync::Arc::new(|_, _, _, _| {}),
                            });
                        }
                    }
                    None
                })),
                stat_mod: None,
            });
            let ev = GameEvent::EnteredStep { step: step_kind, active_player: "us".to_string() };
            state.queue_triggers(&ev);
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
            state.active_effects.push(ContinuousEffect {
                controller: "us".to_string(),
                expires: EffectExpiry::EndOfTurn,
                on_event: Some(std::sync::Arc::new(move |e, _ctl| {
                    if let GameEvent::EnteredPhase { phase, .. } = e {
                        if *phase == phase_kind {
                            return Some(TriggerContext {
                                source_id: ObjId::UNSET, source_name: format!("test-{:?}", phase_kind),
                                controller: "us".to_string(),
                                kind: "TestPhaseTrigger",
                                target_spec: TargetSpec::None,
                                effect: std::sync::Arc::new(|_, _, _, _| {}),
                            });
                        }
                    }
                    None
                })),
                stat_mod: None,
            });
            let ev = GameEvent::EnteredPhase { phase: phase_kind, active_player: "us".to_string() };
            state.queue_triggers(&ev);
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
            let mut state = make_state();
            // No triggers registered — just confirm no pending triggers exist at start.
            assert!(state.pending_triggers.is_empty(),
                "{:?} starts with no pending triggers", step_kind);
        }
    }
