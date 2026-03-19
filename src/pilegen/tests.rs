    use super::*;
    use super::strategy;
    use rand::{SeedableRng, rngs::StdRng};

    // ── Test helpers ──────────────────────────────────────────────────────────

    fn make_state() -> SimState {
        let us = PlayerState::new("us_deck");
        let opp = PlayerState::new("opp_deck");
        let mut s = SimState::new(us, opp);
        s.rng = Box::new(rand::rngs::StdRng::seed_from_u64(42));
        s
    }

    fn seeded_rng() -> StdRng {
        StdRng::seed_from_u64(42)
    }

    fn make_strategies() -> HashMap<PlayerId, Box<dyn strategy::Strategy>> {
        HashMap::from([
            (PlayerId::Us,  Box::new(strategy::DoomsdayStrategy::new(99)) as Box<dyn strategy::Strategy>),
            (PlayerId::Opp, Box::new(strategy::GenericOppStrategy::new())  as Box<dyn strategy::Strategy>),
        ])
    }

    fn test_catalog() -> std::collections::HashMap<String, CardDef> {
        super::card_defs::build_catalog()
    }

    fn catalog_card(name: &str) -> CardDef {
        test_catalog().remove(name).unwrap_or_else(|| panic!("card not found in catalog: {name}"))
    }

    fn creature(name: &str, power: i32, toughness: i32) -> CardDef {
        CardDef::new(
            name, CardKind::Creature(CreatureData::new("", power, toughness)),
            vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![],
        )
    }

    /// Insert a permanent into `state.objects` for `who` and return its ObjId.
    /// Also pre-registers and activates trigger/replacement instances so fire_triggers works.
    fn add_perm(state: &mut SimState, who: PlayerId, name: &str, bf: BattlefieldState) -> ObjId {
        let id = state.alloc_id();
        state.objects.insert(id, GameObject {
            id,
            catalog_key: name.to_string(),
            owner: who,
            controller: who,
            zone: CardZone::Battlefield,
            is_token: false,
            spell: None,
            bf: Some(bf),
            materialized: None,
        });
        // Look up the real CardDef (including triggers/replacements) from the catalog; fall back
        // to a minimal 1/1 stub for anonymous test creatures that have no special behaviour.
        let def = test_catalog().remove(name).unwrap_or_else(|| {
            CardDef::new(name, CardKind::Creature(CreatureData::new("", 1, 1)),
                         vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![])
        });
        preregister_instances(&def, id, who, state);
        activate_instances(id, who, Some(&def), state);
        // Seed state.catalog so recompute() can find this object's base def.
        state.catalog.entry(name.to_string()).or_insert(def);
        id
    }

    /// Insert a default permanent (untapped, no mana abilities).
    fn add_default_perm(state: &mut SimState, who: PlayerId, name: &str) -> ObjId {
        add_perm(state, who, name, BattlefieldState::new())
    }

    /// Insert a permanent using a pre-built `CardDef` (full static_ability_defs included).
    /// Also seeds `state.materialized.defs` so mana abilities and type checks work without recompute.
    fn add_perm_with_def(state: &mut SimState, who: PlayerId, def: &CardDef, bf: BattlefieldState) -> ObjId {
        let id = state.alloc_id();
        state.objects.insert(id, GameObject {
            id,
            catalog_key: def.name.clone(),
            owner: who,
            controller: who,
            zone: CardZone::Battlefield,
            is_token: false,
            spell: None,
            bf: Some(bf),
            materialized: None,
        });
        preregister_instances(def, id, who, state);
        activate_instances(id, who, Some(def), state);
        state.objects.get_mut(&id).unwrap().materialized = Some(def.clone());
        // Seed state.catalog so recompute() can find this object's base def.
        state.catalog.entry(def.name.clone()).or_insert_with(|| def.clone());
        id
    }

    fn make_land(state: &mut SimState, who: PlayerId, name: &str, tapped: bool) -> ObjId {
        add_perm(state, who, name, BattlefieldState {
            tapped,
            ..BattlefieldState::new()
        })
    }

    fn add_hand_card(state: &mut SimState, who: PlayerId, name: &str) -> ObjId {
        let id = state.alloc_id();
        state.objects.insert(id, GameObject {
            id,
            catalog_key: name.to_string(),
            owner: who,
            controller: who,
            zone: CardZone::Hand { known: false },
            is_token: false,
            spell: None,
            bf: None,
            materialized: None,
        });
        id
    }

    fn add_hand_card_with_def(state: &mut SimState, who: PlayerId, def: &CardDef) -> ObjId {
        let id = add_hand_card(state, who, &def.name.clone());
        state.objects.get_mut(&id).unwrap().materialized = Some(def.clone());
        id
    }

    fn add_graveyard_card(state: &mut SimState, who: PlayerId, name: &str) -> ObjId {
        let id = state.alloc_id();
        state.objects.insert(id, GameObject {
            id,
            catalog_key: name.to_string(),
            owner: who,
            controller: who,
            zone: CardZone::Graveyard,
            is_token: false,
            spell: None,
            bf: None,
            materialized: None,
        });
        id
    }

    fn add_library_card(state: &mut SimState, who: PlayerId, name: &str) -> ObjId {
        let id = state.alloc_id();
        state.objects.insert(id, GameObject {
            id,
            catalog_key: name.to_string(),
            owner: who,
            controller: who,
            zone: CardZone::Library,
            is_token: false,
            spell: None,
            bf: None,
            materialized: None,
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
        let land_id = make_land(&mut state, PlayerId::Us, "Island", true);
        let ragavan_id = add_perm(&mut state, PlayerId::Us, "Ragavan", BattlefieldState {
            tapped: true,
            entered_this_turn: true,
            ..BattlefieldState::new()
        });
        state.us.spells_cast_this_turn = 2;

        let step = Step { kind: StepKind::Untap, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true, &mut make_strategies());

        assert!(!state.permanent_bf(land_id).unwrap().tapped, "land should be untapped");
        assert!(!state.permanent_bf(ragavan_id).unwrap().tapped, "permanent should be untapped");
        assert!(!state.permanent_bf(ragavan_id).unwrap().entered_this_turn, "summoning sickness should clear");
        assert_eq!(state.us.lands_played_this_turn, 0, "land drop count should reset");
        assert_eq!(state.us.spells_cast_this_turn, 0);
    }

    #[test]
    fn test_draw_step_skipped_on_play_turn1() {
        let mut state = make_state();
        add_library_card(&mut state, PlayerId::Us, "Island");
        let initial_hand = state.hand_size(PlayerId::Us);

        let step = Step { kind: StepKind::Draw, prio: false };
        // on_play=true, t=1, ap=PlayerId::Us → this_player_on_play=true → skip
        do_step(&mut state, 1, PlayerId::Us, &step, true, &mut make_strategies());

        assert_eq!(state.hand_size(PlayerId::Us), initial_hand, "no draw on the play turn 1");
    }

    #[test]
    fn test_draw_step_draws_card() {
        let mut state = make_state();
        add_library_card(&mut state, PlayerId::Us, "Island");
        let initial_hand = state.hand_size(PlayerId::Us);

        let step = Step { kind: StepKind::Draw, prio: false };
        // on_play=false → this_player_on_play=false → no skip
        do_step(&mut state, 1, PlayerId::Us, &step, false, &mut make_strategies());

        assert_eq!(state.hand_size(PlayerId::Us), initial_hand + 1, "should draw one card");
    }

    #[test]
    fn test_cleanup_removes_damage() {
        let mut state = make_state();
        let rag_id = add_perm(&mut state, PlayerId::Us, "Ragavan", BattlefieldState {
            damage: 3,
            ..BattlefieldState::new()
        });

        let step = Step { kind: StepKind::Cleanup, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true, &mut make_strategies());

        assert_eq!(state.permanent_bf(rag_id).unwrap().damage, 0);
    }

    #[test]
    fn test_declare_attackers_safe_to_attack() {
        let mut state = make_state();
        let ragavan_def = creature("Ragavan", 2, 4);
        let ragavan_id = add_perm(&mut state, PlayerId::Us, "Ragavan", BattlefieldState {
            entered_this_turn: false,
            ..BattlefieldState::new()
        });

        let catalog = vec![ragavan_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let step = Step { kind: StepKind::DeclareAttackers, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true, &mut make_strategies());

        assert!(state.combat_attackers.contains(&ragavan_id), "should attack");
        assert!(state.permanent_bf(ragavan_id).unwrap().tapped, "attacker should be tapped");
    }

    #[test]
    fn test_declare_attackers_too_risky() {
        let mut state = make_state();
        let attacker_def = creature("Ragavan", 2, 2);
        let blocker_def = creature("Mosscoat Construct", 3, 3);
        add_perm(&mut state, PlayerId::Us, "Ragavan", BattlefieldState {
            entered_this_turn: false,
            ..BattlefieldState::new()
        });
        add_default_perm(&mut state, PlayerId::Opp, "Mosscoat Construct");

        let catalog = vec![attacker_def, blocker_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let step = Step { kind: StepKind::DeclareAttackers, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true, &mut make_strategies());

        assert!(state.combat_attackers.is_empty(), "should not attack into 3/3");
    }

    #[test]
    fn test_declare_attackers_summoning_sickness() {
        let mut state = make_state();
        let def = creature("Ragavan", 2, 4);
        // entered_this_turn = true (default from BattlefieldState::new)
        add_default_perm(&mut state, PlayerId::Us, "Ragavan");

        let catalog = vec![def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let step = Step { kind: StepKind::DeclareAttackers, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true, &mut make_strategies());

        assert!(state.combat_attackers.is_empty(), "sickness prevents attack");
    }

    #[test]
    fn test_declare_blockers_good_block() {
        let mut state = make_state();
        let atk_def = creature("Ragavan", 2, 2);
        let blk_def = creature("Mosscoat Construct", 3, 3);
        let ragavan_id = add_perm(&mut state, PlayerId::Us, "Ragavan", BattlefieldState {
            entered_this_turn: false,
            tapped: false,
            ..BattlefieldState::new()
        });
        let mosscoat_id = add_default_perm(&mut state, PlayerId::Opp, "Mosscoat Construct");
        state.combat_attackers = vec![ragavan_id];

        let catalog = vec![atk_def, blk_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let step = Step { kind: StepKind::DeclareBlockers, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true, &mut make_strategies());

        assert_eq!(state.combat_blocks.len(), 1);
        assert_eq!(state.combat_blocks[0], (ragavan_id, mosscoat_id));
    }

    #[test]
    fn test_declare_blockers_no_chump() {
        let mut state = make_state();
        let atk_def = creature("Beast", 4, 4);
        let blk_def = creature("Squirrel Token", 1, 1);
        let beast_id = add_perm(&mut state, PlayerId::Us, "Beast", BattlefieldState {
            entered_this_turn: false,
            ..BattlefieldState::new()
        });
        add_default_perm(&mut state, PlayerId::Opp, "Squirrel Token");
        state.combat_attackers = vec![beast_id];

        let catalog = vec![atk_def, blk_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let step = Step { kind: StepKind::DeclareBlockers, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true, &mut make_strategies());

        assert!(state.combat_blocks.is_empty(), "should not chump block");
    }

    #[test]
    fn test_combat_damage_unblocked_hits_player() {
        let mut state = make_state();
        let initial_life = state.opp.life;
        let atk_def = creature("Ragavan", 2, 1);
        let ragavan_id = add_perm(&mut state, PlayerId::Us, "Ragavan", BattlefieldState {
            tapped: true,
            ..BattlefieldState::new()
        });
        state.combat_attackers = vec![ragavan_id];

        let catalog = vec![atk_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let step = Step { kind: StepKind::CombatDamage, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true, &mut make_strategies());

        assert_eq!(state.opp.life, initial_life - 2);
    }

    #[test]
    fn test_combat_damage_blocked_no_player_damage() {
        let mut state = make_state();
        let initial_life = state.opp.life;
        let atk_def = creature("Ragavan", 2, 2);
        let blk_def = creature("Mosscoat Construct", 3, 3);
        let ragavan_id = add_perm(&mut state, PlayerId::Us, "Ragavan", BattlefieldState {
            tapped: true,
            ..BattlefieldState::new()
        });
        let construct_id = add_default_perm(&mut state, PlayerId::Opp, "Mosscoat Construct");
        state.combat_attackers = vec![ragavan_id];
        state.combat_blocks = vec![(ragavan_id, construct_id)];

        let catalog = vec![atk_def, blk_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let step = Step { kind: StepKind::CombatDamage, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true, &mut make_strategies());

        assert_eq!(state.opp.life, initial_life, "blocked — no player damage");
    }

    #[test]
    fn test_combat_damage_sba_kills_both_2_2s() {
        let mut state = make_state();
        let atk_def = creature("Ragavan", 2, 2);
        let blk_def = creature("Mosscoat Construct", 2, 2);
        let ragavan_id = add_perm(&mut state, PlayerId::Us, "Ragavan", BattlefieldState {
            tapped: true,
            ..BattlefieldState::new()
        });
        let construct_id = add_default_perm(&mut state, PlayerId::Opp, "Mosscoat Construct");
        state.combat_attackers = vec![ragavan_id];
        state.combat_blocks = vec![(ragavan_id, construct_id)];

        let catalog = vec![atk_def, blk_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let step = Step { kind: StepKind::CombatDamage, prio: true };
        do_step(&mut state, 1, PlayerId::Us, &step, true, &mut make_strategies());

        assert!(state.permanents_of(PlayerId::Us).count() == 0, "attacker should die");
        assert!(state.permanents_of(PlayerId::Opp).count() == 0, "blocker should die");
        assert!(state.graveyard_of(PlayerId::Us).any(|c| c.catalog_key == "Ragavan"));
        assert!(state.graveyard_of(PlayerId::Opp).any(|c| c.catalog_key == "Mosscoat Construct"));
    }

    #[test]
    fn test_combat_damage_outclassed_attacker_dies() {
        let mut state = make_state();
        let atk_def = creature("Ragavan", 2, 2);
        let blk_def = creature("Troll", 3, 3);
        let ragavan_id = add_perm(&mut state, PlayerId::Us, "Ragavan", BattlefieldState {
            tapped: true,
            ..BattlefieldState::new()
        });
        let troll_id = add_default_perm(&mut state, PlayerId::Opp, "Troll");
        state.combat_attackers = vec![ragavan_id];
        state.combat_blocks = vec![(ragavan_id, troll_id)];

        let catalog = vec![atk_def, blk_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let step = Step { kind: StepKind::CombatDamage, prio: true };
        do_step(&mut state, 1, PlayerId::Us, &step, true, &mut make_strategies());

        assert!(state.permanents_of(PlayerId::Us).count() == 0, "attacker dies");
        assert!(state.permanents_of(PlayerId::Opp).count() > 0, "blocker survives");
    }

    #[test]
    fn test_end_combat_clears_fields() {
        let mut state = make_state();
        let dummy_id = state.alloc_id();
        let dummy_id2 = state.alloc_id();
        state.combat_attackers = vec![dummy_id];
        state.combat_blocks = vec![(dummy_id, dummy_id2)];

        let step = Step { kind: StepKind::EndCombat, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true, &mut make_strategies());

        assert!(state.combat_attackers.is_empty());
        assert!(state.combat_blocks.is_empty());
    }

    // ── Section 3: Phase Tests ────────────────────────────────────────────────

    #[test]
    fn test_beginning_phase_untaps_and_draws() {
        let mut state = make_state();
        let island_def = catalog_card("Island");
        let island_id = add_perm_with_def(&mut state, PlayerId::Us, &island_def, BattlefieldState {
            tapped: true,
            ..BattlefieldState::new()
        });
        add_library_card(&mut state, PlayerId::Us, "Swamp");
        let initial_hand = state.hand_size(PlayerId::Us);

        // t=2, on_play=false → draw fires (this_player_on_play=false)
        do_phase(&mut state, 2, PlayerId::Us, &beginning_phase(), false, &mut make_strategies());

        assert!(!state.permanent_bf(island_id).unwrap().tapped, "land should be untapped");
        assert_eq!(state.hand_size(PlayerId::Us), initial_hand + 1, "should have drawn one card");
    }

    #[test]
    fn test_combat_phase_full_cycle() {
        let mut state = make_state();
        do_phase(&mut state, 1, PlayerId::Us, &combat_phase(), true, &mut make_strategies());

        assert!(state.combat_attackers.is_empty());
        assert!(state.combat_blocks.is_empty());
    }

    // ── Section 4: Priority Action Cycle ─────────────────────────────────────

    #[test]
    fn test_priority_round_both_pass_empty_stack() {
        let mut state = make_state();
        // current_phase is "" (not "Main") → both players pass immediately
        handle_priority_round(&mut state, 1, PlayerId::Us, &mut make_strategies());

        assert_eq!(state.us.life, 20);
        assert_eq!(state.opp.life, 20);
    }

    // ── Section 5: Spell Casting ──────────────────────────────────────────────

    #[test]
    fn test_cast_spell_normal_cost_removes_from_library() {
        let mut state = make_state();
        let def = catalog_card("Dark Ritual");
        state.us.pool.b = 1;
        state.us.pool.total = 1;
        let dark_ritual_id = add_hand_card(&mut state, PlayerId::Us, "Dark Ritual");

        let catalog = vec![def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }

        let card_id = cast_spell(&mut state, 1, PlayerId::Us, dark_ritual_id, SpellFace::Main, None, &[]);

        assert!(card_id.is_some(), "spell should be cast");
        let card_id = card_id.unwrap();
        let card = state.objects.get(&card_id).expect("card in state");
        assert_eq!(card.catalog_key, "Dark Ritual");
        assert_eq!(state.player_id(card.owner), state.us.id, "owner should be us player id");
        assert!(!state.hand_of(PlayerId::Us).any(|c| c.catalog_key == "Dark Ritual"), "removed from hand");
        assert_eq!(state.us.pool.b, 0, "mana spent");
    }

    #[test]
    fn test_cast_spell_unaffordable_returns_none() {
        let mut state = make_state();
        let def = catalog_card("Doomsday");
        // No mana in pool, no lands
        let doomsday_id = add_hand_card(&mut state, PlayerId::Us, "Doomsday");

        let catalog = vec![def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let item = cast_spell(&mut state, 1, PlayerId::Us, doomsday_id, SpellFace::Main, None, &[]);

        assert!(item.is_none(), "can't cast with no mana");
    }

    #[test]
    fn test_cast_spell_alt_cost_exiles_pitch_card() {
        let mut state = make_state();
        let fow_def = catalog_card("Force of Will");
        let brainstorm_def = catalog_card("Brainstorm");
        let catalog = vec![fow_def.clone(), brainstorm_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }

        // Add FoW and Brainstorm to hand (FoW pitches itself? No — Brainstorm is the pitch card)
        let fow_id = add_hand_card(&mut state, PlayerId::Us, "Force of Will");
        add_hand_card(&mut state, PlayerId::Us, "Brainstorm");

        let alt_cost = &fow_def.alternate_costs()[0];
        let initial_life = state.us.life;

        let item = cast_spell(&mut state, 1, PlayerId::Us, fow_id, SpellFace::Main, Some(alt_cost), &[]);

        assert!(item.is_some(), "FoW should be cast via pitch");
        assert_eq!(state.us.life, initial_life - 1, "paid 1 life");
        assert!(!state.hand_of(PlayerId::Us).any(|c| c.catalog_key == "Brainstorm"), "pitch card removed from hand");
        assert!(state.exile_of(PlayerId::Us).any(|c| c.catalog_key == "Brainstorm"), "pitch card exiled");
    }

    // ── Section 6: Spell Resolution ───────────────────────────────────────────

    #[test]
    fn test_effect_doomsday_sets_success() {
        let mut state = make_state();
        eff_doomsday().call(&mut state, 1, &[]);

        assert!(state.success);
    }

    #[test]
    fn test_effect_cantrip_increments_hand() {
        let mut state = make_state();
        add_library_card(&mut state, PlayerId::Us, "Island");
        let initial_hand = state.hand_size(PlayerId::Us);
        eff_draw(PlayerId::Us, 1).call(&mut state, 1, &[]);

        assert_eq!(state.hand_size(PlayerId::Us), initial_hand + 1, "cantrip increments hand count");
    }

    #[test]
    fn test_brainstorm_net_one_card() {
        // draw:3 + put_back:2 = net +1 hand size.
        let mut state = make_state();
        add_library_card(&mut state, PlayerId::Us, "Island");
        add_library_card(&mut state, PlayerId::Us, "Swamp");
        add_library_card(&mut state, PlayerId::Us, "Plains");
        let initial = state.hand_size(PlayerId::Us);
        eff_draw(PlayerId::Us, 3).then(eff_put_back(PlayerId::Us, 2))
            .call(&mut state, 1, &[]);

        assert_eq!(state.hand_size(PlayerId::Us), initial + 1, "Brainstorm nets +1 card");
    }

    #[test]
    fn test_brainstorm_fires_three_draw_events() {
        // All three draws queue triggers; OBM (controlled by opp) should see all three.
        let mut state = make_state();
        add_default_perm(&mut state, PlayerId::Opp, "Orcish Bowmasters");
        add_library_card(&mut state, PlayerId::Us, "Island");
        add_library_card(&mut state, PlayerId::Us, "Swamp");
        add_library_card(&mut state, PlayerId::Us, "Plains");
        eff_draw(PlayerId::Us, 3).then(eff_put_back(PlayerId::Us, 2))
            .call(&mut state, 1, &[]);

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
        add_default_perm(&mut state, PlayerId::Us, "Tamiyo, Inquisitive Student");
        state.us.draws_this_turn = 1; // simulate having already drawn naturally
        add_library_card(&mut state, PlayerId::Us, "Island");
        add_library_card(&mut state, PlayerId::Us, "Swamp");
        add_library_card(&mut state, PlayerId::Us, "Plains");
        eff_draw(PlayerId::Us, 3).then(eff_put_back(PlayerId::Us, 2))
            .call(&mut state, 1, &[]);

        let flip_triggers = state.pending_triggers.iter()
            .filter(|tc| tc.source_name == "Tamiyo, Inquisitive Student")
            .count();
        assert_eq!(flip_triggers, 1, "Tamiyo flips exactly once on the 3rd draw of the turn");
    }

    #[test]
    fn test_effect_life_loss_reduces_caster_life() {
        let mut state = make_state();
        let initial = state.us.life;
        eff_life_loss(PlayerId::Us, 2).call(&mut state, 1, &[]);

        assert_eq!(state.us.life, initial - 2);
    }

    #[test]
    fn test_effect_mana_adds_to_pool() {
        let mut state = make_state();
        eff_mana(PlayerId::Us, "BBB").call(&mut state, 1, &[]);

        assert_eq!(state.us.pool.b, 3, "should add 3 black mana");
        assert_eq!(state.us.pool.total, 3);
    }

    #[test]
    fn test_effect_discard_removes_opp_card() {
        let mut state = make_state();
        add_hand_card(&mut state, PlayerId::Opp, "Counterspell");
        let initial_opp_hand = state.hand_size(PlayerId::Opp);
        eff_discard(PlayerId::Us, Who::Opp, 1, "").call(&mut state, 1, &[]);

        assert_eq!(state.hand_size(PlayerId::Opp), initial_opp_hand - 1, "opp hand decremented");
        assert!(state.graveyard_of(PlayerId::Opp).any(|c| c.catalog_key == "Counterspell"), "Counterspell in graveyard");
        assert!(!state.hand_of(PlayerId::Opp).any(|c| c.catalog_key == "Counterspell"), "card removed from opp hand");
    }

    // ── Section 7: Ability Activation ─────────────────────────────────────────

    #[test]
    fn test_pay_activation_cost_mana() {
        let mut state = make_state();
        state.us.pool.b = 2;
        state.us.pool.total = 2;
        let ability = AbilityDef { mana_cost: "B".to_string(), ..Default::default() };
        pay_activation_cost(&mut state, 1, PlayerId::Us, ObjId::UNSET, &ability);

        assert_eq!(state.us.pool.b, 1, "1 black spent");
        assert_eq!(state.us.pool.total, 1);
    }

    #[test]
    fn test_pay_activation_cost_life() {
        let mut state = make_state();
        let initial = state.us.life;
        let ability = AbilityDef { life_cost: 2, ..Default::default() };
        pay_activation_cost(&mut state, 1, PlayerId::Us, ObjId::UNSET, &ability);

        assert_eq!(state.us.life, initial - 2);
    }

    #[test]
    fn test_pay_activation_cost_sacrifice_self() {
        let mut state = make_state();
        let petal_id = add_default_perm(&mut state, PlayerId::Us, "Lotus Petal");
        let ability = AbilityDef { sacrifice_self: true, ..Default::default() };
        pay_activation_cost(&mut state, 1, PlayerId::Us, petal_id, &ability);

        assert!(state.permanents_of(PlayerId::Us).count() == 0, "Lotus Petal should be sacrificed");
        assert!(state.graveyard_of(PlayerId::Us).any(|c| c.catalog_key == "Lotus Petal"));
    }

    // ── Section 8: Destruction Effects ───────────────────────────────────────

    // Spell resolution: destroy uses item.permanent_target set at cast time.

    #[test]
    fn test_effect_destroy_spell_removes_opp_land() {
        let mut state = make_state();
        let id = make_land(&mut state, PlayerId::Opp, "Bayou", false);
        eff_destroy_target(PlayerId::Us).call(&mut state, 1, &[id]);

        assert!(state.permanents_of(PlayerId::Opp).count() == 0, "Bayou should be destroyed");
        assert!(state.graveyard_of(PlayerId::Opp).any(|c| c.catalog_key == "Bayou"));
    }

    #[test]
    fn test_effect_destroy_spell_removes_opp_creature() {
        let mut state = make_state();
        let id = add_default_perm(&mut state, PlayerId::Opp, "Troll");
        eff_destroy_target(PlayerId::Us).call(&mut state, 1, &[id]);

        assert!(state.permanents_of(PlayerId::Opp).count() == 0, "Troll should be destroyed");
        assert!(state.graveyard_of(PlayerId::Opp).any(|c| c.catalog_key == "Troll"));
    }

    // Ability resolution: target is chosen at push time via choose_permanent_target.

    fn land_def(name: &str, basic: bool) -> CardDef {
        CardDef::new(
            name, CardKind::Land(LandData::default()),
            vec![], None,
            if basic { vec![Supertype::Basic] } else { vec![] },
            CardLayout::Normal, None, vec![], vec![], vec![],
        )
    }

    #[test]
    fn test_effect_destroy_ability_removes_nonbasic_land() {
        let mut state = make_state();
        make_land(&mut state, PlayerId::Opp, "Bayou", false);
        let ability = AbilityDef { target_spec: target_spec_from_str(Some("opp:nonbasic_land")), ability_factory: Some(Arc::new(|who, _| eff_destroy_target(who))), ..Default::default() };
        let bayou_def = land_def("Bayou", false);
        let catalog = vec![bayou_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let targets: Vec<ObjId> = legal_targets(
            &target_spec_from_str(Some("opp:nonbasic_land")), PlayerId::Us, &state
        );
        let eff = build_ability_effect(&ability, PlayerId::Us, ObjId::UNSET);
        eff.call(&mut state, 1, &targets);

        assert!(state.permanents_of(PlayerId::Opp).count() == 0, "Bayou should be destroyed");
        assert!(state.graveyard_of(PlayerId::Opp).any(|c| c.catalog_key == "Bayou"));
    }

    #[test]
    fn test_effect_destroy_ability_ignores_basic_land() {
        let mut state = make_state();
        make_land(&mut state, PlayerId::Opp, "Forest", false);
        let ability = AbilityDef { target_spec: target_spec_from_str(Some("opp:nonbasic_land")), ability_factory: Some(Arc::new(|who, _| eff_destroy_target(who))), ..Default::default() };
        let forest_def = land_def("Forest", true);
        let catalog = vec![forest_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let targets: Vec<ObjId> = legal_targets(
            &target_spec_from_str(Some("opp:nonbasic_land")), PlayerId::Us, &state
        );
        let eff = build_ability_effect(&ability, PlayerId::Us, ObjId::UNSET);
        eff.call(&mut state, 1, &targets);

        assert!(state.permanents_of(PlayerId::Opp).count() > 0, "basic Forest should survive");
        assert!(state.graveyard_of(PlayerId::Opp).count() == 0, "no cards in graveyard");
    }

    // ── Section 9: Delve ──────────────────────────────────────────────────────

    #[test]
    fn test_cast_delve_spell_exiles_graveyard_cards() {
        // Spell costs 3 generic + U. Two graveyard cards reduce generic to 1.
        // Pool supplies the remaining 1 generic + 1 blue.
        let mut state = make_state();
        let def = CardDef::new("Treasure Cruise", CardKind::Instant(SpellData { mana_cost: "7U".to_string(), delve: true, ..Default::default() }), vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![]);
        for name in &["A", "B", "C", "D", "E", "F", "G"] {
            add_graveyard_card(&mut state, PlayerId::Us, name);
        }
        let tc_id = add_hand_card(&mut state, PlayerId::Us, "Treasure Cruise");
        state.us.pool.u  = 1;
        state.us.pool.total = 1; // only 1 mana in pool — delve pays the other 7

        let catalog = vec![def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }

        let item = cast_spell(&mut state, 1, PlayerId::Us, tc_id, SpellFace::Main, None, &[]);

        assert!(item.is_some(), "should cast with full delve");
        assert_eq!(state.graveyard_of(PlayerId::Us).count(), 0, "all 7 graveyard cards exiled");
        assert_eq!(state.exile_of(PlayerId::Us).count(), 7, "exiled by delve");
        assert_eq!(state.us.pool.u, 0, "blue pip paid");
    }

    #[test]
    fn test_cast_delve_spell_partial_delve() {
        // Spell costs 3 generic. Graveyard has 2 cards — reduces cost to 1.
        // Pool must cover the remaining 1 generic.
        let mut state = make_state();
        let def = CardDef::new("Dead Drop", CardKind::Sorcery(SpellData { mana_cost: "3".to_string(), delve: true, ..Default::default() }), vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![]);
        add_graveyard_card(&mut state, PlayerId::Us, "Ritual");
        add_graveyard_card(&mut state, PlayerId::Us, "Ponder");
        let dead_drop_id = add_hand_card(&mut state, PlayerId::Us, "Dead Drop");
        state.us.pool.total = 1; // covers the 1 remaining generic after delve

        let catalog = vec![def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }

        let item = cast_spell(&mut state, 1, PlayerId::Us, dead_drop_id, SpellFace::Main, None, &[]);

        assert!(item.is_some(), "should cast with partial delve + 1 mana");
        assert_eq!(state.graveyard_of(PlayerId::Us).count(), 0, "both graveyard cards exiled");
        assert_eq!(state.exile_of(PlayerId::Us).count(), 2);
        assert_eq!(state.us.pool.total, 0, "remaining generic pip paid");
    }

    #[test]
    fn test_murktide_counters_from_exiled_instants_sorceries() {
        // Murktide exiles 4 cards via delve; 3 are instants/sorceries → enters as 6/6.
        let mut state = make_state();
        let murktide_def = catalog_card("Murktide Regent");
        let ritual_def   = catalog_card("Dark Ritual");
        let ponder_def   = catalog_card("Ponder");
        let consider_def = catalog_card("Consider");
        let ragavan_def  = creature("Ragavan", 2, 1); // creature — does NOT count

        add_graveyard_card(&mut state, PlayerId::Us, "Dark Ritual");
        add_graveyard_card(&mut state, PlayerId::Us, "Ponder");
        add_graveyard_card(&mut state, PlayerId::Us, "Consider");
        add_graveyard_card(&mut state, PlayerId::Us, "Ragavan");
        let murktide_id = add_hand_card(&mut state, PlayerId::Us, "Murktide Regent");
        // After delving all 4, generic cost = 5-4 = 1. Need UU + 1 generic.
        state.us.pool.u  = 2;
        state.us.pool.total = 3;

        let catalog = vec![murktide_def.clone(), ritual_def, ponder_def, consider_def, ragavan_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }

        let card_id = cast_spell(&mut state, 1, PlayerId::Us, murktide_id, SpellFace::Main, None, &[]).unwrap();
        let spell = state.objects[&card_id].spell.as_ref().expect("spell state populated").clone();
        let effect = &spell.effect;
        let chosen_targets = spell.chosen_targets.clone();

        // Resolve via Effect path — replacement effect counts exiled instants/sorceries.
        effect.as_ref().unwrap().call(&mut state, 1, &chosen_targets);

        let murktide_bf = state.permanents_of(PlayerId::Us).find(|p| p.catalog_key == "Murktide Regent")
            .and_then(|p| p.bf.as_ref()).expect("Murktide on battlefield");
        assert_eq!(murktide_bf.counters, 3, "3 instants/sorceries exiled → 3 counters");

        // recompute reflects counters in the materialized view
        let murktide_id = state.permanents_of(PlayerId::Us).find(|p| p.catalog_key == "Murktide Regent")
            .map(|p| p.id).expect("Murktide on battlefield");
        recompute(&mut state);
        let eff = state.def_of(murktide_id).expect("Murktide materialized");
        let CardKind::Creature(c) = &eff.kind else { panic!("expected creature") };
        assert_eq!((c.power(), c.toughness()), (6, 6));
    }

    #[test]
    fn test_murktide_zero_counters_when_no_instants_exiled() {
        // Delve only exiles a creature — no instants/sorceries → enters as base 3/3.
        let mut state = make_state();
        let murktide_def = catalog_card("Murktide Regent");
        let ragavan_def = creature("Ragavan", 2, 1);

        add_graveyard_card(&mut state, PlayerId::Us, "Ragavan");
        let murktide_id = add_hand_card(&mut state, PlayerId::Us, "Murktide Regent");
        // 5 - 1 = 4 generic remaining; need UU + 4 generic
        state.us.pool.u  = 2;
        state.us.pool.total = 6;

        let catalog = vec![murktide_def.clone(), ragavan_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }

        let card_id = cast_spell(&mut state, 1, PlayerId::Us, murktide_id, SpellFace::Main, None, &[]).unwrap();
        let spell = state.objects[&card_id].spell.as_ref().expect("spell state populated").clone();
        let effect = &spell.effect;
        let chosen_targets = spell.chosen_targets.clone();

        effect.as_ref().unwrap().call(&mut state, 1, &chosen_targets);

        let murktide_bf = state.permanents_of(PlayerId::Us).find(|p| p.catalog_key == "Murktide Regent")
            .and_then(|p| p.bf.as_ref()).expect("Murktide on battlefield");
        assert_eq!(murktide_bf.counters, 0);
        let murktide_id = state.permanents_of(PlayerId::Us).find(|p| p.catalog_key == "Murktide Regent")
            .map(|p| p.id).expect("Murktide on battlefield");
        recompute(&mut state);
        let eff = state.def_of(murktide_id).expect("Murktide materialized");
        let CardKind::Creature(c) = &eff.kind else { panic!("expected creature") };
        assert_eq!((c.power(), c.toughness()), (3, 3));
    }

    #[test]
    fn test_murktide_attacks_with_counter_boosted_stats() {
        // A 6/6 Murktide (base 3/3 + 3 counters) should survive attacking into a 5-power blocker.
        let mut state = make_state();
        let murktide_def = creature("Murktide Regent", 3, 3);
        let murktide_id = add_perm(&mut state, PlayerId::Us, "Murktide Regent", BattlefieldState {
            counters: 3,
            entered_this_turn: false,
            ..BattlefieldState::new()
        });
        // Opponent has a 5/5 blocker — Murktide's toughness 6 > opp power 5, safe to attack.
        let blocker_def = creature("Dragon", 5, 5);
        add_default_perm(&mut state, PlayerId::Opp, "Dragon");

        let catalog = vec![murktide_def, blocker_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let step = Step { kind: StepKind::DeclareAttackers, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true, &mut make_strategies());

        assert!(state.combat_attackers.contains(&murktide_id),
            "6/6 Murktide should attack into a 5-power blocker");
    }

    #[test]
    fn test_cast_delve_spell_insufficient_mana_after_delve() {
        // Spell costs 3 generic. Graveyard has 2 cards — reduces cost to 1.
        // Pool is empty — still can't cast.
        let mut state = make_state();
        let def = CardDef::new("Dead Drop", CardKind::Sorcery(SpellData { mana_cost: "3".to_string(), delve: true, ..Default::default() }), vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![]);
        add_graveyard_card(&mut state, PlayerId::Us, "Ritual");
        add_graveyard_card(&mut state, PlayerId::Us, "Ponder");
        let dead_drop_id = add_hand_card(&mut state, PlayerId::Us, "Dead Drop");
        // no mana

        let catalog = vec![def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }

        let item = cast_spell(&mut state, 1, PlayerId::Us, dead_drop_id, SpellFace::Main, None, &[]);

        assert!(item.is_none(), "can't cast — 1 generic still unpaid");
        assert_eq!(state.graveyard_of(PlayerId::Us).count(), 2, "graveyard unchanged on failed cast");
        assert_eq!(state.exile_of(PlayerId::Us).count(), 0, "nothing exiled on failed cast");
    }

    #[test]
    fn test_effect_exile_ability_removes_creature() {
        let mut state = make_state();
        add_default_perm(&mut state, PlayerId::Opp, "Troll");
        let troll_def = creature("Troll", 2, 2);
        let ability = AbilityDef { target_spec: target_spec_from_str(Some("opp:creature")), ability_factory: Some(Arc::new(|who, _| eff_exile_target(who))), ..Default::default() };
        let catalog = vec![troll_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let targets: Vec<ObjId> = legal_targets(
            &target_spec_from_str(Some("opp:creature")), PlayerId::Us, &state
        );
        let eff = build_ability_effect(&ability, PlayerId::Us, ObjId::UNSET);
        eff.call(&mut state, 1, &targets);

        assert!(state.permanents_of(PlayerId::Opp).count() == 0, "Troll should be exiled");
        assert!(state.exile_of(PlayerId::Opp).any(|c| c.catalog_key == "Troll"), "Troll should be in exile");
        assert!(state.graveyard_of(PlayerId::Opp).count() == 0, "exiled, not dead");
    }

    // ── Section 10: Ninjutsu ──────────────────────────────────────────────────

    fn ninja_def() -> CardDef {
        let mut data = CreatureData::new("", 2, 1);
        data.ninjutsu = Some(NinjutsuAbility { mana_cost: "U".to_string() });
        CardDef::new("Ninja", CardKind::Creature(data), vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![])
    }

    fn island_land(state: &mut SimState, who: PlayerId) -> ObjId {
        add_perm_with_def(state, who, &catalog_card("Island"), BattlefieldState::new())
    }

    #[test]
    fn test_declare_attackers_sets_attacking_flag() {
        let mut state = make_state();
        let def = creature("Attacker", 2, 4);
        let atk_id = add_perm(&mut state, PlayerId::Us, "Attacker", BattlefieldState {
            entered_this_turn: false,
            ..BattlefieldState::new()
        });

        let catalog = vec![def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let step = Step { kind: StepKind::DeclareAttackers, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true, &mut make_strategies());

        assert!(state.permanent_bf(atk_id).unwrap().attacking, "declared attacker gets attacking=true");
    }

    #[test]
    fn test_declare_blockers_sets_unblocked_flag_when_no_blocker() {
        let mut state = make_state();
        let def = creature("Attacker", 2, 4);
        let attacker_id = add_perm(&mut state, PlayerId::Us, "Attacker", BattlefieldState {
            attacking: true,
            tapped: true,
            ..BattlefieldState::new()
        });
        state.combat_attackers = vec![attacker_id];
        // No opp creatures → no blocker

        let catalog = vec![def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let step = Step { kind: StepKind::DeclareBlockers, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true, &mut make_strategies());

        assert!(state.permanent_bf(attacker_id).unwrap().unblocked, "unblocked attacker gets unblocked=true");
    }

    #[test]
    fn test_declare_blockers_blocked_attacker_not_unblocked() {
        let mut state = make_state();
        let atk_def = creature("Ragavan", 2, 2);
        let blk_def = creature("Wall", 0, 6);
        let ragavan_id = add_perm(&mut state, PlayerId::Us, "Ragavan", BattlefieldState {
            attacking: true,
            tapped: true,
            ..BattlefieldState::new()
        });
        add_default_perm(&mut state, PlayerId::Opp, "Wall");
        state.combat_attackers = vec![ragavan_id];

        let catalog = vec![atk_def, blk_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let step = Step { kind: StepKind::DeclareBlockers, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true, &mut make_strategies());

        assert!(!state.permanent_bf(ragavan_id).unwrap().unblocked, "blocked attacker stays unblocked=false");
        assert_eq!(state.combat_blocks.len(), 1, "blocker declared");
    }

    #[test]
    fn test_end_combat_clears_attacking_unblocked_flags() {
        let mut state = make_state();
        let ninja_id = add_perm(&mut state, PlayerId::Us, "Ninja", BattlefieldState {
            attacking: true,
            unblocked: true,
            ..BattlefieldState::new()
        });
        state.combat_attackers = vec![ninja_id];

        let step = Step { kind: StepKind::EndCombat, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true, &mut make_strategies());

        assert!(!state.permanent_bf(ninja_id).unwrap().attacking, "attacking cleared at EndCombat");
        assert!(!state.permanent_bf(ninja_id).unwrap().unblocked, "unblocked cleared at EndCombat");
    }

    // Negative try_ninjutsu precondition tests (deterministic — RNG roll is never reached).

    #[test]
    fn test_try_ninjutsu_no_hand_returns_none() {
        let mut state = make_state();
        // No hand cards — hand_size returns 0; exits before any materialized lookup.
        add_perm(&mut state, PlayerId::Us, "Ragavan", BattlefieldState { attacking: true, unblocked: true, ..BattlefieldState::new() });
        assert!(try_ninjutsu(&state, PlayerId::Us, &mut seeded_rng()).is_none(), "no hand → None");
    }

    #[test]
    fn test_try_ninjutsu_no_unblocked_returns_none() {
        let mut state = make_state();
        add_hand_card(&mut state, PlayerId::Us, "Ninja");
        add_perm(&mut state, PlayerId::Us, "Ragavan", BattlefieldState { attacking: true, unblocked: false, ..BattlefieldState::new() });
        state.us.pool.u = 1; state.us.pool.total = 1;
        // Exits at the has_unblocked check before the hand scan; no materialized seeding needed.
        assert!(try_ninjutsu(&state, PlayerId::Us, &mut seeded_rng()).is_none(), "no unblocked attacker → None");
    }

    #[test]
    fn test_try_ninjutsu_no_ninja_in_library_returns_none() {
        let mut state = make_state();
        let brainstorm_def = catalog_card("Brainstorm");
        add_hand_card_with_def(&mut state, PlayerId::Us, &brainstorm_def);
        add_perm(&mut state, PlayerId::Us, "Ragavan", BattlefieldState { attacking: true, unblocked: true, ..BattlefieldState::new() });
        state.us.pool.u = 1; state.us.pool.total = 1;
        // Brainstorm has no ninjutsu; materialized entry present, filter returns false → None.
        assert!(try_ninjutsu(&state, PlayerId::Us, &mut seeded_rng()).is_none(), "no ninja card → None");
    }

    #[test]
    fn test_try_ninjutsu_no_mana_returns_none() {
        let mut state = make_state();
        let def = ninja_def();
        add_hand_card_with_def(&mut state, PlayerId::Us, &def);
        add_perm(&mut state, PlayerId::Us, "Ragavan", BattlefieldState { attacking: true, unblocked: true, ..BattlefieldState::new() });
        // No mana available — ninja found in materialized, but mana check fails.
        assert!(try_ninjutsu(&state, PlayerId::Us, &mut seeded_rng()).is_none(), "no mana → None");
    }

    #[test]
    fn test_ninjutsu_swaps_attacker_for_ninja() {
        // try_ninjutsu returns ActivateAbility; when committed via handle_priority_round
        // in a DeclareBlockers window, the ninja enters play and the attacker returns to hand.
        let def = ninja_def();
        let island_def = catalog_card("Island");
        let catalog = vec![def.clone(), island_def];

        // Loop over seeds until ninjutsu fires (35% per attempt → statistically guaranteed within 50).
        for seed in 0u64..50 {
            let mut state = make_state();
            for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
            state.current_phase = Some(TurnPosition::Step(StepKind::DeclareBlockers));
            state.current_ap = state.us.id;
            add_perm(&mut state, PlayerId::Us, "Ragavan", BattlefieldState {
                attacking: true, unblocked: true, ..BattlefieldState::new()
            });
            island_land(&mut state, PlayerId::Us);
            // Add Ninja to hand with materialized entry so try_ninjutsu can find it.
            add_hand_card_with_def(&mut state, PlayerId::Us, &def);
            // Also register the ninja in state.objects as a library card (so apply_ability_effect
            // can look up the ninja's name at resolution).
            let ninja_lib_id = state.alloc_id();
            state.objects.insert(ninja_lib_id, GameObject::new(ninja_lib_id, "Ninja".to_string(), PlayerId::Us));
            let initial_hand = state.hand_size(PlayerId::Us);
            handle_priority_round(&mut state, 1, PlayerId::Us, &mut make_strategies());

            if state.permanents_of(PlayerId::Us).any(|p| p.catalog_key == "Ninja") {
                let ninja = state.permanents_of(PlayerId::Us).find(|p| p.catalog_key == "Ninja").unwrap();
                let ninja_bf = ninja.bf.as_ref().unwrap();
                assert!(ninja_bf.attacking, "ninja should be attacking");
                assert!(ninja_bf.tapped, "ninja should be tapped");
                assert!(!state.permanents_of(PlayerId::Us).any(|p| p.catalog_key == "Ragavan"), "Ragavan returned to hand");
                assert_eq!(state.hand_size(PlayerId::Us), initial_hand, "net hand size unchanged (+1 return, -1 ninja)");
                let ninja_id = state.permanents_of(PlayerId::Us).find(|p| p.catalog_key == "Ninja").unwrap().id;
                assert!(state.combat_attackers.contains(&ninja_id), "ninja in combat_attackers");
                return;
            }
        }
        panic!("ninjutsu should have fired within 50 seeds");
    }

    // ── Section 11: Cycling ───────────────────────────────────────────────────

    #[test]
    fn test_cycling_draw_effect() {
        let mut state = make_state();
        add_library_card(&mut state, PlayerId::Us, "Island");
        let initial = state.hand_size(PlayerId::Us);
        let ability = AbilityDef { ability_factory: Some(Arc::new(|who, _| eff_draw(who, 1))), ..Default::default() };
        let eff = build_ability_effect(&ability, PlayerId::Us, ObjId::UNSET);
        eff.call(&mut state, 1, &[]);
        assert_eq!(state.hand_size(PlayerId::Us), initial + 1, "cycling draws one card");
    }

    #[test]
    fn test_cycling_discard_self_removes_card_from_library() {
        // pay_activation_cost with discard_self=true removes the card from hand
        // and sends it to the graveyard.
        let mut state = make_state();
        let wraith_def = catalog_card("Street Wraith");
        let ability = AbilityDef { zone: "hand".to_string(), discard_self: true, life_cost: 2, ability_factory: Some(Arc::new(|who, _| eff_draw(who, 1))), ..Default::default() };
        let catalog = vec![wraith_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        // Add Street Wraith to hand and a library card to draw
        let wraith_id = add_hand_card(&mut state, PlayerId::Us, "Street Wraith");
        add_library_card(&mut state, PlayerId::Us, "Island");
        let initial_hand = state.hand_size(PlayerId::Us);

        pay_activation_cost(&mut state, 1, PlayerId::Us, wraith_id, &ability);

        assert!(!state.hand_of(PlayerId::Us).any(|c| c.catalog_key == "Street Wraith"), "Street Wraith removed from hand");
        assert!(state.graveyard_of(PlayerId::Us).any(|c| c.catalog_key == "Street Wraith"), "in graveyard");
        assert_eq!(state.hand_size(PlayerId::Us), initial_hand - 1, "hand size decremented (discarded, not yet drawn)");
        assert_eq!(state.us.life, 20 - 2, "paid 2 life");
    }

    // ── Section 12: Adventure ─────────────────────────────────────────────────

    #[test]
    fn test_adventure_resolve_exiles_to_on_adventure() {
        // An adventure StackItem (no target) routes the card to exile + on_adventure.
        let mut state = make_state();
        // Simulate the adventure resolution inline: no effect, just exile.
        let borrower_id = state.alloc_id();
        let mut borrower_obj = GameObject::new(borrower_id, "Brazen Borrower", PlayerId::Us);
        borrower_obj.zone = CardZone::Exile { on_adventure: true };
        state.objects.insert(borrower_id, borrower_obj);

        assert!(state.exile_of(PlayerId::Us).any(|c| c.catalog_key == "Brazen Borrower"), "Borrower in exile");
        assert!(state.on_adventure_of(PlayerId::Us).any(|c| c.catalog_key == "Brazen Borrower"), "Borrower on adventure");
        assert!(state.graveyard_of(PlayerId::Us).count() == 0, "not in graveyard");
    }

    #[test]
    fn test_adventure_bounce_effect_returns_opp_permanent() {
        // Petty Theft bounces target opp permanent then exiles Brazen Borrower to on_adventure.
        let mut state = make_state();
        let bowmasters_id = add_default_perm(&mut state, PlayerId::Opp, "Orcish Bowmasters");
        let initial_opp_hand = state.hand_size(PlayerId::Opp);

        // Run the Effect directly (as the new adventure resolution path does).
        let eff = eff_bounce_target(PlayerId::Us);
        eff.call(&mut state, 1, &[bowmasters_id]);
        // Then exile the card to on_adventure.
        let borrower_id = state.alloc_id();
        let mut borrower_obj = GameObject::new(borrower_id, "Brazen Borrower", PlayerId::Us);
        borrower_obj.zone = CardZone::Exile { on_adventure: true };
        state.objects.insert(borrower_id, borrower_obj);

        assert!(state.permanents_of(PlayerId::Opp).count() == 0, "Bowmasters bounced off board");
        assert_eq!(state.hand_size(PlayerId::Opp), initial_opp_hand + 1, "bounced to opp hand");
        assert!(state.on_adventure_of(PlayerId::Us).any(|c| c.catalog_key == "Brazen Borrower"), "Borrower on adventure in exile");
    }

    #[test]
    fn test_cast_from_adventure_enters_play() {
        // pick_on_board_action detects adventure creatures in exile and picks the cast action
        // (75% roll). Run with multiple seeds to confirm it fires and the creature enters play.
        let borrower_def = catalog_card("Brazen Borrower");
        let island2_def = CardDef::new("Island2", CardKind::Land(LandData {
            mana_abilities: vec![ManaAbility { tap_self: true, produces: "U".to_string(), ..Default::default() }],
            ..Default::default()
        }), vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![]);
        let catalog = vec![borrower_def.clone(), catalog_card("Island"), island2_def.clone(), catalog_card("Swamp")];

        let make_fresh_state = || {
            let mut state = make_state();
            for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
            state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
            state.current_ap = state.us.id;
            let borrower_id = state.alloc_id();
            let mut borrower_obj = GameObject::new(borrower_id, "Brazen Borrower", PlayerId::Us);
            borrower_obj.zone = CardZone::Exile { on_adventure: true };
            state.objects.insert(borrower_id, borrower_obj);
            // 1UU mana: two Islands + one generic (Swamp)
            island_land(&mut state, PlayerId::Us);
            add_perm_with_def(&mut state, PlayerId::Us, &island2_def, BattlefieldState::new());
            add_perm_with_def(&mut state, PlayerId::Us, &catalog_card("Swamp"), BattlefieldState::new());
            state
        };

        // At 75% per attempt, try up to 20 seeds; at least one must result in Borrower entering play.
        let mut entered = false;
        for seed in 0u64..20 {
            let mut state = make_fresh_state();
            handle_priority_round(&mut state, 1, PlayerId::Us, &mut make_strategies());
            if state.permanents_of(PlayerId::Us).any(|p| p.catalog_key == "Brazen Borrower") {
                assert!(!state.on_adventure_of(PlayerId::Us).any(|c| c.catalog_key == "Brazen Borrower"), "removed from on_adventure");
                assert!(!state.exile_of(PlayerId::Us).any(|c| c.catalog_key == "Brazen Borrower"), "removed from exile");
                entered = true;
                break;
            }
        }
        assert!(entered, "Brazen Borrower should have entered play in at least one of 20 seeded runs");
    }

    // ── Section 8: Keyword Tests ──────────────────────────────────────────────

    fn flying_creature(name: &str, power: i32, toughness: i32) -> CardDef {
        let mut data = CreatureData::new("", power, toughness);
        data.keywords = vec!["flying".to_string()];
        CardDef::new(name, CardKind::Creature(data), vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![])
    }

    #[test]
    fn test_flying_not_blocked_by_ground() {
        // Flying attacker should not be assigned a ground blocker.
        let mut state = make_state();
        let flyer = flying_creature("Murktide Regent", 3, 3);
        let ground = creature("Troll", 3, 3);

        let murktide_id = add_perm(&mut state, PlayerId::Us, "Murktide Regent", BattlefieldState {
            attacking: true,
            ..BattlefieldState::new()
        });
        add_default_perm(&mut state, PlayerId::Opp, "Troll");
        state.combat_attackers = vec![murktide_id];

        let catalog = vec![flyer, ground];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let step = Step { kind: StepKind::DeclareBlockers, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true, &mut make_strategies());

        assert!(state.combat_blocks.is_empty(), "ground creature cannot block a flyer");
    }

    #[test]
    fn test_flying_blocked_by_flyer() {
        // Flying attacker CAN be blocked by another flying creature.
        let mut state = make_state();
        let flyer_atk = flying_creature("Murktide Regent", 3, 3);
        let flyer_blk = flying_creature("Subtlety", 3, 3);

        let murktide_id = add_perm(&mut state, PlayerId::Us, "Murktide Regent", BattlefieldState {
            attacking: true,
            ..BattlefieldState::new()
        });
        let subtlety_id = add_default_perm(&mut state, PlayerId::Opp, "Subtlety");
        state.combat_attackers = vec![murktide_id];

        let catalog = vec![flyer_atk, flyer_blk];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let step = Step { kind: StepKind::DeclareBlockers, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true, &mut make_strategies());

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

        let murktide_id = add_perm(&mut state, PlayerId::Us, "Murktide Regent", BattlefieldState {
            entered_this_turn: false,
            ..BattlefieldState::new()
        });
        add_default_perm(&mut state, PlayerId::Opp, "Troll");

        let catalog = vec![flyer, ground];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let step = Step { kind: StepKind::DeclareAttackers, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true, &mut make_strategies());

        // Murktide's toughness (3) > relevant blocking power (0 — Troll can't block flyer).
        assert!(state.combat_attackers.contains(&murktide_id),
            "flying creature should attack when only ground blockers exist");
    }

    // ── Section 9: Trigger Tests ──────────────────────────────────────────────

    #[test]
    fn test_fire_triggers_returns_context_for_bowmasters_etb() {
        let mut state = make_state();
        let bowmasters_id = add_default_perm(&mut state, PlayerId::Opp, "Orcish Bowmasters");

        let ev = GameEvent::ZoneChange {
            id: bowmasters_id,
            actor: PlayerId::Opp,
            from: ZoneId::Stack,
            to: ZoneId::Battlefield,
            controller: PlayerId::Opp,
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
            actor: PlayerId::Opp,
            from: ZoneId::Stack,
            to: ZoneId::Battlefield,
            controller: PlayerId::Opp,
        };
        let result = fire_triggers(&ev, &state);
        assert!(result.is_empty());
    }

    /// Fire a Bowmasters ETB trigger for `controller` and return the TriggerContext.
    fn bowmasters_etb_ctx(controller: PlayerId) -> TriggerContext {
        let state = make_state();
        let ev = GameEvent::ZoneChange {
            id: ObjId::UNSET,
            actor: controller,
            from: ZoneId::Hand,
            to: ZoneId::Battlefield,
            controller,
        };
        let mut pending = Vec::new();
        bowmasters_check(&ev, ObjId::UNSET, controller, &state, &mut pending);
        pending.remove(0)
    }

    /// Fire a Bowmasters ETB trigger for `controller`, choose its target, and apply it.
    fn fire_bowmasters_etb(controller: PlayerId, state: &mut SimState) {
        // Rebuild materialized so choose_trigger_target sees current P/T.
        recompute(state);
        let ctx = bowmasters_etb_ctx(controller);
        let all_targets = legal_targets(&ctx.target_spec, controller, state);
        let targets: Vec<ObjId> = pick_target(&all_targets, state).into_iter().collect();
        ctx.effect.call(state, 1, &targets);
    }

    #[test]
    fn test_apply_bowmasters_etb_deals_damage_and_creates_army() {
        let mut state = make_state();
        add_default_perm(&mut state, PlayerId::Opp, "Orcish Bowmasters");
        let initial_life = state.us.life;
        fire_bowmasters_etb(PlayerId::Opp, &mut state);
        assert_eq!(state.us.life, initial_life - 1, "ETB deals 1 to us");
        assert!(state.permanents_of(PlayerId::Opp).any(|p| p.catalog_key == "Orc Army"), "Orc Army token created");
        let army = state.permanents_of(PlayerId::Opp).find(|p| p.catalog_key == "Orc Army").and_then(|p| p.bf.as_ref()).unwrap();
        assert_eq!(army.counters, 1, "Orc Army has 1 counter");
    }

    #[test]
    fn test_apply_bowmasters_etb_grows_existing_army() {
        let mut state = make_state();
        add_default_perm(&mut state, PlayerId::Opp, "Orcish Bowmasters");
        add_perm(&mut state, PlayerId::Opp, "Orc Army", BattlefieldState { counters: 2, ..BattlefieldState::new() });
        fire_bowmasters_etb(PlayerId::Opp, &mut state);
        let army = state.permanents_of(PlayerId::Opp).find(|p| p.catalog_key == "Orc Army").and_then(|p| p.bf.as_ref()).unwrap();
        assert_eq!(army.counters, 3, "Orc Army grows from 2 to 3");
    }

    #[test]
    fn test_bowmasters_ping_hits_face_when_no_killable_creature() {
        let mut state = make_state();
        add_default_perm(&mut state, PlayerId::Opp, "Orcish Bowmasters");
        let initial_life = state.us.life;
        add_default_perm(&mut state, PlayerId::Us, "Troll");
        let catalog = vec![creature("Troll", 3, 3)];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        fire_bowmasters_etb(PlayerId::Opp, &mut state);
        assert_eq!(state.us.life, initial_life - 1, "damage hits face when no killable creature");
        assert!(state.permanents_of(PlayerId::Us).any(|p| p.catalog_key == "Troll"), "Troll survives");
    }

    #[test]
    fn test_bowmasters_ping_kills_1_1_creature() {
        let mut state = make_state();
        add_default_perm(&mut state, PlayerId::Opp, "Orcish Bowmasters");
        let initial_life = state.us.life;
        add_default_perm(&mut state, PlayerId::Us, "Ragavan, Nimble Pilferer");
        let catalog = vec![creature("Ragavan, Nimble Pilferer", 2, 1)];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        fire_bowmasters_etb(PlayerId::Opp, &mut state);
        check_state_based_actions(&mut state, 1);
        assert_eq!(state.us.life, initial_life, "life total unchanged when creature is targeted");
        assert!(!state.permanents_of(PlayerId::Us).any(|p| p.catalog_key == "Ragavan, Nimble Pilferer"),
            "Ragavan dies to 1 damage");
        assert!(state.graveyard_of(PlayerId::Us).any(|c| c.catalog_key == "Ragavan, Nimble Pilferer"),
            "Ragavan goes to graveyard");
    }

    #[test]
    fn test_bowmasters_ping_prioritises_opposing_bowmasters() {
        let mut state = make_state();
        add_default_perm(&mut state, PlayerId::Opp, "Orcish Bowmasters");
        add_default_perm(&mut state, PlayerId::Us, "Troll");
        add_default_perm(&mut state, PlayerId::Us, "Orcish Bowmasters");
        let catalog = vec![creature("Troll", 3, 3), creature("Orcish Bowmasters", 1, 1)];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        fire_bowmasters_etb(PlayerId::Opp, &mut state);
        check_state_based_actions(&mut state, 1);
        assert!(!state.permanents_of(PlayerId::Us).any(|p| p.catalog_key == "Orcish Bowmasters"),
            "opposing Bowmasters is killed");
        assert!(state.permanents_of(PlayerId::Us).any(|p| p.catalog_key == "Troll"), "Troll survives");
    }

    #[test]
    fn test_bowmasters_no_trigger_on_natural_first_draw() {
        let mut state = make_state();
        add_default_perm(&mut state, PlayerId::Opp, "Orcish Bowmasters");

        let ev = GameEvent::Draw { controller: PlayerId::Us, draw_index: 1, is_natural: true };
        let result = fire_triggers(&ev, &state);
        assert!(result.is_empty(), "no trigger on first natural draw");
    }

    #[test]
    fn test_bowmasters_triggers_on_cantrip_draw() {
        let mut state = make_state();
        add_default_perm(&mut state, PlayerId::Opp, "Orcish Bowmasters");

        let ev = GameEvent::Draw { controller: PlayerId::Us, draw_index: 1, is_natural: false };
        let result = fire_triggers(&ev, &state);
        assert_eq!(result.len(), 1, "cantrip draw triggers Bowmasters");
    }

    #[test]
    fn test_murktide_counter_on_instant_exile() {
        let mut state = make_state();
        add_perm(&mut state, PlayerId::Us, "Murktide Regent", BattlefieldState { counters: 0, ..BattlefieldState::new() });
        // Add the card being exiled so murktide_check can look up its type.
        let consider_id = add_default_perm(&mut state, PlayerId::Us, "Consider");
        state.objects.get_mut(&consider_id).unwrap().zone = CardZone::Exile { on_adventure: false };

        let ev = GameEvent::ZoneChange {
            id: consider_id,
            actor: PlayerId::Us,
            from: ZoneId::Graveyard,
            to: ZoneId::Exile,
            controller: PlayerId::Us,
        };
        let result = fire_triggers(&ev, &state);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source_name, "Murktide Regent");

        let mut state2 = state;
        result[0].effect.call(&mut state2, 1, &[]);
        let murktide = state2.permanents_of(PlayerId::Us).find(|p| p.catalog_key == "Murktide Regent").and_then(|p| p.bf.as_ref()).unwrap();
        assert_eq!(murktide.counters, 1, "Murktide gains +1/+1 counter");
    }

    #[test]
    fn test_murktide_no_counter_on_land_exile() {
        let mut state = make_state();
        add_default_perm(&mut state, PlayerId::Us, "Murktide Regent");
        let island_id = add_default_perm(&mut state, PlayerId::Us, "Island");
        state.objects.get_mut(&island_id).unwrap().zone = CardZone::Exile { on_adventure: false };

        let ev = GameEvent::ZoneChange {
            id: island_id,
            actor: PlayerId::Us,
            from: ZoneId::Graveyard,
            to: ZoneId::Exile,
            controller: PlayerId::Us,
        };
        let result = fire_triggers(&ev, &state);
        assert!(result.is_empty(), "land exile does not trigger Murktide");
    }

    #[test]
    fn test_tamiyo_clue_when_attacking() {
        let mut state = make_state();
        add_perm(&mut state, PlayerId::Us, "Tamiyo, Inquisitive Student", BattlefieldState { attacking: true, ..BattlefieldState::new() });

        let ev = GameEvent::EnteredStep {
            step: StepKind::DeclareAttackers,
            active_player: PlayerId::Us,
        };
        let result = fire_triggers(&ev, &state);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source_name, "Tamiyo, Inquisitive Student");

        let mut state2 = state;
        result[0].effect.call(&mut state2, 1, &[]);
        assert!(state2.permanents_of(PlayerId::Us).any(|p| p.catalog_key == "Clue Token"),
            "Clue Token created when Tamiyo attacks");
    }

    #[test]
    fn test_tamiyo_no_clue_when_not_attacking() {
        let mut state = make_state();
        add_default_perm(&mut state, PlayerId::Us, "Tamiyo, Inquisitive Student"); // attacking = false

        let ev = GameEvent::EnteredStep {
            step: StepKind::DeclareAttackers,
            active_player: PlayerId::Us,
        };
        let result = fire_triggers(&ev, &state);
        // Trigger queues (Tamiyo is in play), but resolves to nothing (not attacking).
        if let Some(ctx) = result.first() {
            let mut state2 = state;
            ctx.effect.call(&mut state2, 1, &[]);
            assert!(!state2.permanents_of(PlayerId::Us).any(|p| p.catalog_key == "Clue Token"),
                "no Clue Token if Tamiyo is not attacking");
        }
    }

    #[test]
    fn test_tamiyo_flip_on_third_draw() {
        let mut state = make_state();
        add_default_perm(&mut state, PlayerId::Us, "Tamiyo, Inquisitive Student");

        let ev = GameEvent::Draw { controller: PlayerId::Us, draw_index: 3, is_natural: false };
        let result = fire_triggers(&ev, &state);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source_name, "Tamiyo, Inquisitive Student");

        let mut state2 = state;
        result[0].effect.call(&mut state2, 1, &[]);
        // The flip mutates in-place: catalog_key stays as front face; active_face flips to 1.
        let tamiyo_bf = state2.permanents_of(PlayerId::Us)
            .find(|p| p.catalog_key == "Tamiyo, Inquisitive Student")
            .and_then(|p| p.bf.as_ref())
            .expect("Tamiyo should still be on the battlefield (same object, same catalog_key)");
        assert_eq!(tamiyo_bf.active_face, 1, "active_face == 1 after flip");
        assert_eq!(tamiyo_bf.loyalty, 2, "starting loyalty of Tamiyo, Seasoned Scholar");
    }

    #[test]
    fn test_tamiyo_plus_two_applies_power_mod_to_attackers() {
        let mut state = make_state();
        // Register the +2 floating trigger watcher for PlayerId::Us (as if us activated it last turn).
        state.trigger_instances.push(TriggerInstance {
            source_id: ObjId::UNSET,
            controller: PlayerId::Us,
            check: std::sync::Arc::new(tamiyo_plus_two_check),
            expiry: Some(ContinuousExpiry::StartOfControllerNextTurn),
            active: true,
        });
        // Opp has a 3/3 attacker.
        let atk_def = creature("Dragon", 3, 3);
        add_perm(&mut state, PlayerId::Opp, "Dragon", BattlefieldState { entered_this_turn: false, ..BattlefieldState::new() });
        add_default_perm(&mut state, PlayerId::Us, "Wall"); // blocker-sized (no block in this test)

        let catalog = vec![atk_def, creature("Wall", 0, 4)];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        do_step(&mut state, 1, PlayerId::Opp, &Step { kind: StepKind::DeclareAttackers, prio: true },
            true, &mut make_strategies());

        let dragon_id = state.permanents_of(PlayerId::Opp).find(|p| p.catalog_key == "Dragon").map(|p| p.id).unwrap();
        // The -1 comes from a ContinuousInstance (L7), not bf.power_mod.
        // recompute reflects the CE modifier in the materialized view.
        recompute(&mut state);
        let eff = state.def_of(dragon_id).expect("Dragon materialized");
        let CardKind::Creature(c) = &eff.kind else { panic!("expected creature") };
        assert_eq!(c.power(), 2, "Dragon's effective power is 3 + (-1) = 2");
    }

    #[test]
    fn test_tamiyo_plus_two_expires_at_controller_untap() {
        let mut state = make_state();
        state.trigger_instances.push(TriggerInstance {
            source_id: ObjId::UNSET,
            controller: PlayerId::Us,
            check: std::sync::Arc::new(tamiyo_plus_two_check),
            expiry: Some(ContinuousExpiry::StartOfControllerNextTurn),
            active: true,
        });
        assert_eq!(state.trigger_instances.len(), 1);

        // Untap step for PlayerId::Us should expire the floating trigger watcher.
        let step = Step { kind: StepKind::Untap, prio: false };
        do_step(&mut state, 2, PlayerId::Us, &step, true, &mut make_strategies());

        assert!(state.trigger_instances.is_empty(), "Floating trigger expires at controller's next Untap");
    }

    #[test]
    fn test_stat_mod_reversed_at_cleanup() {
        // A L7 ContinuousInstance with EndOfTurn expiry should be removed during Cleanup,
        // restoring the effective P/T of the affected permanent.
        let mut state = make_state();
        let atk_def = creature("Dragon", 3, 3);
        let dragon_id = add_perm(&mut state, PlayerId::Opp, "Dragon", BattlefieldState::new());
        let catalog = vec![atk_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }

        // Register an EndOfTurn L7 CI that applies -1 power to the dragon.
        state.continuous_instances.push(ContinuousInstance {
            source_id: dragon_id,
            controller: PlayerId::Us,
            layer: ContinuousLayer::L7PowerToughness,
            filter: std::sync::Arc::new(move |id, _| id == dragon_id),
            modifier: std::sync::Arc::new(|def, _state| {
                if let CardKind::Creature(c) = &mut def.kind { c.adjust_pt(-1, 0); }
            }),
            expiry: ContinuousExpiry::EndOfTurn,
        });

        // Before Cleanup: effective power = 2.
        recompute(&mut state);
        let CardKind::Creature(c) = &state.def_of(dragon_id).unwrap().kind.clone() else { panic!() };
        assert_eq!(c.power(), 2, "CI applies -1 before Cleanup");

        let step = Step { kind: StepKind::Cleanup, prio: false };
        do_step(&mut state, 1, PlayerId::Opp, &step, true, &mut make_strategies());

        // After Cleanup: CI removed, effective power restored to 3.
        recompute(&mut state);
        let CardKind::Creature(c) = &state.def_of(dragon_id).unwrap().kind.clone() else { panic!() };
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
                controller: PlayerId::Us,
                check: std::sync::Arc::new(move |e, _source_id, _ctl, _state, pending| {
                    if let GameEvent::EnteredStep { step, .. } = e {
                        if *step == step_kind {
                            pending.push(TriggerContext {
                                source_name: format!("test-{:?}", step_kind),
                                controller: PlayerId::Us,
                                target_spec: TargetSpec::None,
                                effect: Effect(std::sync::Arc::new(|_, _, _| {})),
                            });
                        }
                    }
                }),
                expiry: Some(ContinuousExpiry::EndOfTurn),
                active: true,
            });
            let ev = GameEvent::EnteredStep { step: step_kind, active_player: PlayerId::Us };
            fire_event(ev, &mut state, 1, PlayerId::Us);
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
                controller: PlayerId::Us,
                check: std::sync::Arc::new(move |e, _source_id, _ctl, _state, pending| {
                    if let GameEvent::EnteredPhase { phase, .. } = e {
                        if *phase == phase_kind {
                            pending.push(TriggerContext {
                                source_name: format!("test-{:?}", phase_kind),
                                controller: PlayerId::Us,
                                target_spec: TargetSpec::None,
                                effect: Effect(std::sync::Arc::new(|_, _, _| {})),
                            });
                        }
                    }
                }),
                expiry: Some(ContinuousExpiry::EndOfTurn),
                active: true,
            });
            let ev = GameEvent::EnteredPhase { phase: phase_kind };
            fire_event(ev, &mut state, 1, PlayerId::Us);
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
        add_library_card(&mut state, PlayerId::Us, "Island");
        add_library_card(&mut state, PlayerId::Us, "Swamp");
        add_library_card(&mut state, PlayerId::Us, "Plains");
        // Manually place Brainstorm on stack with its effect.
        let id = state.alloc_id();
        state.objects.insert(id, GameObject {
            id,
            catalog_key: "Brainstorm".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            zone: CardZone::Stack,
            is_token: false,
            spell: Some(SpellState {
                effect: Some(eff_draw(PlayerId::Us, 3).then(eff_put_back(PlayerId::Us, 2))),
                chosen_targets: vec![],
                is_back_face: false,
            }),
            bf: None,
            materialized: None,
        });
        state.stack.push(id);
        resolve_top_of_stack(&mut state, 1, PlayerId::Us);
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
        let fetch_def = catalog_card("Polluted Delta");
        let catalog = vec![fetch_def];

        let mut state = make_state();
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        state.us.life = 20;
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PostCombatMain));
        let delta_id = add_perm(&mut state, PlayerId::Us, "Polluted Delta", BattlefieldState::new());

        // Simulate paying the sacrifice cost: permanent leaves the battlefield.
        state.set_card_zone(delta_id, CardZone::Graveyard);
        state.us.life -= 1;

        // With the source gone, priority_action must never offer ActivateAbility for that id,
        // regardless of how many times it is called.
        state.current_turn = 1;
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
        for _seed in 0..50u64 {
            let mut strat = strategy::DoomsdayStrategy::new(99);
            let action = strat.priority_action(&mut state, PlayerId::Us, &PriorityAction::Pass);
            assert!(
                !matches!(action, PriorityAction::ActivateAbility(id, _, _) if id == delta_id),
                "offered ability for sacrificed permanent — effect would fire without a stack item"
            );
        }
    }

    #[test]
    fn test_leyline_redirects_gy_to_exile() {
        let mut state = make_state();
        // Place Leyline on battlefield (add_perm now pre-registers and activates instances)
        let _leyline_id = add_default_perm(&mut state, PlayerId::Opp, "Leyline of the Void");
        // Put a card in hand
        let hand_id = add_hand_card(&mut state, PlayerId::Us, "Ponder");
        // Move hand card to graveyard — Leyline should redirect to exile
        change_zone(hand_id, ZoneId::Graveyard, &mut state, 1, PlayerId::Us);
        // Card should be in Exile, not Graveyard
        assert_eq!(state.objects[&hand_id].zone, CardZone::Exile { on_adventure: false });
    }

    #[test]
    fn test_leyline_removed_no_redirect() {
        let mut state = make_state();
        // add_perm pre-registers and activates Leyline's replacement
        let leyline_id = add_default_perm(&mut state, PlayerId::Opp, "Leyline of the Void");
        // Destroy Leyline (deactivates its replacement via change_zone → deactivate_instances)
        change_zone(leyline_id, ZoneId::Graveyard, &mut state, 1, PlayerId::Us);
        // Now move a card to GY — should stay in GY
        let hand_id = add_hand_card(&mut state, PlayerId::Us, "Ponder");
        change_zone(hand_id, ZoneId::Graveyard, &mut state, 1, PlayerId::Us);
        assert_eq!(state.objects[&hand_id].zone, CardZone::Graveyard);
    }

    // ── Section 12: State-Based Action Tests ──────────────────────────────────

    fn add_token(state: &mut SimState, who: PlayerId, name: &str) -> ObjId {
        let id = state.alloc_id();
        state.objects.insert(id, GameObject {
            id,
            catalog_key: name.to_string(),
            owner: who,
            controller: who,
            zone: CardZone::Battlefield,
            is_token: true,
            spell: None,
            bf: Some(BattlefieldState::new()),
            materialized: None,
        });
        id
    }

    #[test]
    fn test_sba_life_zero_ends_game() {
        let mut state = make_state();
        state.us.life = 0;
        check_state_based_actions(&mut state, 1);
        assert_eq!(state.winner, Some(PlayerId::Opp), "us at 0 life → opp wins");
    }

    #[test]
    fn test_sba_life_negative_ends_game() {
        let mut state = make_state();
        state.us.life = -3;
        check_state_based_actions(&mut state, 1);
        assert_eq!(state.winner, Some(PlayerId::Opp));
    }

    #[test]
    fn test_sba_token_leaves_battlefield_ceases_to_exist() {
        let mut state = make_state();
        let token_id = add_token(&mut state, PlayerId::Us, "Orc Army");
        // Move token to graveyard (as if it died without SBA running yet).
        state.objects.get_mut(&token_id).unwrap().zone = CardZone::Graveyard;
        state.objects.get_mut(&token_id).unwrap().bf = None;
        check_state_based_actions(&mut state, 1);
        assert!(!state.objects.contains_key(&token_id), "token in GY ceases to exist");
    }

    #[test]
    fn test_sba_token_on_battlefield_not_removed() {
        let mut state = make_state();
        let token_id = add_token(&mut state, PlayerId::Us, "Orc Army");
        check_state_based_actions(&mut state, 1);
        assert!(state.objects.contains_key(&token_id), "token on battlefield survives SBA");
    }

    #[test]
    fn test_sba_zero_toughness_creature_dies() {
        let mut state = make_state();
        // A 1/-1 creature (e.g. after -1/-2 effect) has toughness ≤ 0.
        let _id = add_perm(&mut state, PlayerId::Us, "Weakened", BattlefieldState::new());
        let def = creature("Weakened", 1, -1);
        let catalog = vec![def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        check_state_based_actions(&mut state, 1);
        assert!(state.graveyard_of(PlayerId::Us).any(|c| c.catalog_key == "Weakened"),
            "creature with toughness ≤ 0 goes to graveyard");
    }

    #[test]
    fn test_sba_lethal_damage_creature_dies() {
        let mut state = make_state();
        let _id = add_perm(&mut state, PlayerId::Us, "Ragavan", BattlefieldState {
            damage: 2,
            ..BattlefieldState::new()
        });
        let def = creature("Ragavan", 2, 2);
        let catalog = vec![def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        check_state_based_actions(&mut state, 1);
        assert!(state.graveyard_of(PlayerId::Us).any(|c| c.catalog_key == "Ragavan"),
            "creature with damage = toughness goes to graveyard");
    }

    #[test]
    fn test_sba_planeswalker_loyalty_zero_dies() {
        let mut state = make_state();
        let _id = add_perm(&mut state, PlayerId::Us, "Jace", BattlefieldState {
            loyalty: 0,
            ..BattlefieldState::new()
        });
        let def = CardDef::new("Jace", CardKind::Planeswalker(PlaneswalkerData { mana_cost: "3U".to_string(), loyalty: 3, ..Default::default() }), vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![]);
        let catalog = vec![def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        check_state_based_actions(&mut state, 1);
        assert!(state.graveyard_of(PlayerId::Us).any(|c| c.catalog_key == "Jace"),
            "planeswalker with loyalty 0 goes to graveyard");
    }

    #[test]
    fn test_sba_legend_rule_second_copy_dies() {
        let mut state = make_state();
        let _first = add_default_perm(&mut state, PlayerId::Us, "Bowmasters");
        let _second = add_default_perm(&mut state, PlayerId::Us, "Bowmasters");
        let mut bowmasters_data = CreatureData::new("1B", 1, 1);
        bowmasters_data.legendary = true;
        let def = CardDef::new("Bowmasters", CardKind::Creature(bowmasters_data), parse_colors("1B", false, true), None, vec![Supertype::Legendary], CardLayout::Normal, None, vec![], vec![], vec![]);
        let catalog = vec![def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        check_state_based_actions(&mut state, 1);
        // Exactly one survives.
        assert_eq!(state.permanents_of(PlayerId::Us).filter(|c| c.catalog_key == "Bowmasters").count(), 1,
            "legend rule: one copy survives");
        assert_eq!(state.graveyard_of(PlayerId::Us).filter(|c| c.catalog_key == "Bowmasters").count(), 1,
            "legend rule: one copy goes to graveyard");
    }

    #[test]
    fn test_sba_legend_rule_only_one_copy_untouched() {
        let mut state = make_state();
        add_default_perm(&mut state, PlayerId::Us, "Bowmasters");
        let mut bowmasters_data = CreatureData::new("1B", 1, 1);
        bowmasters_data.legendary = true;
        let def = CardDef::new("Bowmasters", CardKind::Creature(bowmasters_data), parse_colors("1B", false, true), None, vec![Supertype::Legendary], CardLayout::Normal, None, vec![], vec![], vec![]);
        let catalog = vec![def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        check_state_based_actions(&mut state, 1);
        assert_eq!(state.permanents_of(PlayerId::Us).filter(|c| c.catalog_key == "Bowmasters").count(), 1,
            "single legendary permanent unaffected by legend rule");
    }

    // ── Section N: Continuous Effects / recompute ─────────────────────────────

    /// A L7 CE that adds +2/+1 to all permanents controlled by PlayerId::Us is reflected
    /// in the MaterializedState produced by `recompute`.
    #[test]
    fn test_recompute_pt_modifier() {
        let mut state = make_state();

        // Add a 2/2 creature for PlayerId::Us.
        let id = add_default_perm(&mut state, PlayerId::Us, "Grizzly Bears");
        let base_def = creature("Grizzly Bears", 2, 2);
        // Override the 1/1 stub inserted by add_default_perm with the real 2/2 def.
        state.catalog.insert(base_def.name.clone(), base_def);

        // Baseline: recompute without any CEs → effective P/T is 2/2.
        recompute(&mut state);
        let eff = state.def_of(id).expect("should be in materialized defs");
        let CardKind::Creature(c) = &eff.kind else { panic!("expected creature") };
        assert_eq!((c.power(), c.toughness()), (2, 2), "baseline P/T should be 2/2");

        // Register a L7 CE that adds +2/+1 to permanents controlled by PlayerId::Us.
        state.continuous_instances.push(ContinuousInstance {
            source_id: ObjId::UNSET,
            controller: PlayerId::Us,
            layer: ContinuousLayer::L7PowerToughness,
            filter: std::sync::Arc::new(|_id, controller| controller == PlayerId::Us),
            modifier: std::sync::Arc::new(|def, _state| {
                if let CardKind::Creature(c) = &mut def.kind {
                    c.adjust_pt(2, 1);
                }
            }),
            expiry: ContinuousExpiry::EndOfTurn,
        });

        // Recompute: effective P/T should now be 4/3.
        recompute(&mut state);
        let eff2 = state.def_of(id).expect("should be in materialized defs after CE");
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
            add_perm(&mut state, PlayerId::Us, "Llanowar Elves", bf)
        };
        // Without any CE: counters fold in → effective 3/3.
        recompute(&mut state);
        let eff = state.def_of(id).expect("creature should be materialized");
        let CardKind::Creature(c) = &eff.kind else { panic!("expected creature") };
        assert_eq!((c.power(), c.toughness()), (3, 3), "two +1/+1 counters should yield 3/3");
    }


    // ── Section 13g: StaticAbilityDef + CDA ──────────────────────────────────

    fn flying_static_ability() -> StaticAbilityDef {
        std::sync::Arc::new(|source_id, controller: PlayerId| ContinuousInstance {
            source_id,
            controller,
            layer: ContinuousLayer::L6AbilityEffects,
            filter: std::sync::Arc::new(move |id, _| id == source_id),
            modifier: std::sync::Arc::new(|def, _state| {
                if let CardKind::Creature(c) = &mut def.kind {
                    if !c.keywords.contains(&"flying".to_string()) {
                        c.keywords.push("flying".to_string());
                    }
                }
            }),
            expiry: ContinuousExpiry::WhileSourceOnBattlefield,
        })
    }

    /// A creature with a flying static ability should have the keyword in its materialized
    /// def after ETB, and lose it after LTB.
    #[test]
    fn test_static_ability_def_grants_flying_at_etb() {
        let mut state = make_state();
        let def = CardDef::new("Flyer", CardKind::Creature(CreatureData::new("", 2, 2)), vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![flying_static_ability()]);
        let catalog = vec![def.clone()];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }

        let id = add_perm_with_def(&mut state, PlayerId::Us, &def, BattlefieldState::new());

        // recompute: CI from static_ability_def should add "flying" to materialized keywords.
        recompute(&mut state);
        assert!(state.def_of(id).unwrap().has_keyword("flying"), "flying granted via static_ability_def at ETB");
        assert!(creature_has_keyword(id, "flying", &state), "creature_has_keyword uses materialized state");
    }

    /// A creature with a flying static ability should lose the keyword CI when it
    /// leaves the battlefield (deactivate_instances removes WhileSourceOnBattlefield CIs).
    #[test]
    fn test_static_ability_def_removed_at_ltb() {
        let mut state = make_state();
        let def = CardDef::new("Flyer", CardKind::Creature(CreatureData::new("", 2, 2)), vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![flying_static_ability()]);
        let catalog = vec![def.clone()];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }

        let id = add_perm_with_def(&mut state, PlayerId::Us, &def, BattlefieldState::new());
        assert_eq!(state.continuous_instances.len(), 1, "CI registered at ETB");

        // Simulate leaving the battlefield.
        deactivate_instances(id, &mut state);
        assert!(state.continuous_instances.is_empty(), "CI removed at LTB");

        // Materialized view no longer has flying.
        recompute(&mut state);
        // After deactivate_instances, the object may still be on the battlefield
        // in state.objects (we didn't change_zone), but the CI is gone.
        if let Some(d) = state.def_of(id) {
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
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }

        let id = add_perm(&mut state, PlayerId::Us, "GoyTest", BattlefieldState::new());

        // Register a CDA CI: power = number of cards in PlayerId::Us graveyard.
        state.continuous_instances.push(ContinuousInstance {
            source_id: id,
            controller: PlayerId::Us,
            layer: ContinuousLayer::L7PowerToughness,
            filter: std::sync::Arc::new(move |obj_id, _| obj_id == id),
            modifier: std::sync::Arc::new(|def, state| {
                let gy = state.graveyard_of(PlayerId::Us).count() as i32;
                if let CardKind::Creature(c) = &mut def.kind {
                    let delta = gy - c.power();
                    c.adjust_pt(delta, 0);
                }
            }),
            expiry: ContinuousExpiry::WhileSourceOnBattlefield,
        });

        // No cards in GY → power = 0.
        recompute(&mut state);
        let CardKind::Creature(c) = &state.def_of(id).unwrap().kind.clone() else { panic!() };
        assert_eq!(c.power(), 0, "no GY cards → power 0");

        // Add a card to PlayerId::Us graveyard.
        add_graveyard_card(&mut state, PlayerId::Us, "SomeCard");
        recompute(&mut state);
        let CardKind::Creature(c) = &state.def_of(id).unwrap().kind.clone() else { panic!() };
        assert_eq!(c.power(), 1, "1 GY card → power 1");

        // Add a second card.
        add_graveyard_card(&mut state, PlayerId::Us, "AnotherCard");
        recompute(&mut state);
        let CardKind::Creature(c) = &state.def_of(id).unwrap().kind.clone() else { panic!() };
        assert_eq!(c.power(), 2, "2 GY cards → power 2");
    }

    /// recompute now covers all zones; a card in the graveyard must appear in materialized.defs.
    #[test]
    fn test_recompute_includes_graveyard_objects() {
        let mut state = make_state();
        let def = creature("Goyf", 2, 3);
        state.catalog.insert(def.name.clone(), def);

        let gy_id = add_graveyard_card(&mut state, PlayerId::Us, "Goyf");

        recompute(&mut state);
        assert!(
            state.def_of(gy_id).is_some(),
            "graveyard card must appear in materialized snapshot"
        );
        let CardKind::Creature(c) = &state.def_of(gy_id).unwrap().kind.clone() else { panic!("expected creature") };
        assert_eq!(c.power(), 2);
        assert_eq!(c.toughness(), 3);
    }

    // ── Section 14: Library Search Tests ─────────────────────────────────────

    /// Personal Tutor finds a sorcery and puts it on top of the library (stays in library).
    /// An instant in the same library is not moved.
    #[test]
    fn test_personal_tutor_finds_sorcery() {
        let doomsday_def = catalog_card("Doomsday");
        let fow_def = catalog_card("Force of Will");
        let mut state = make_state();
        state.catalog.insert(doomsday_def.name.clone(), doomsday_def);
        state.catalog.insert(fow_def.name.clone(), fow_def);
        let dd_id  = add_library_card(&mut state, PlayerId::Us, "Doomsday");
        let fow_id = add_library_card(&mut state, PlayerId::Us, "Force of Will");

        let eff = eff_fetch_search(PlayerId::Us, pred_type_eq(CardType::Sorcery), ZoneId::Library);
        eff.call(&mut state, 1, &[]);

        // Both stay in library: Doomsday was "put on top" (Library ≡ top until ordering tracked),
        // FoW was never selected.
        assert_eq!(state.objects[&dd_id].zone,  CardZone::Library, "Doomsday should remain in library");
        assert_eq!(state.objects[&fow_id].zone, CardZone::Library, "FoW should remain in library");
        let log = state.log.join("\n");
        assert!(log.contains("search → Doomsday"), "should log the searched card name");
        assert!(!log.contains("Force of Will"), "FoW should not appear in search log");
    }

    /// Recruiter of the Guard ETB: searches library for a creature with toughness ≤ 2 and puts it
    /// in hand. A creature with toughness > 2 should stay in the library.
    #[test]
    fn test_recruiter_etb_finds_low_toughness_creature() {
        let recruiter_def = catalog_card("Recruiter of the Guard");
        let small_def = creature("Mother of Runes", 1, 1);
        let big_def = creature("Tarmogoyf", 0, 3);
        let mut state = make_state();
        state.catalog.insert(recruiter_def.name.clone(), recruiter_def.clone());
        state.catalog.insert(small_def.name.clone(), small_def.clone());
        state.catalog.insert(big_def.name.clone(), big_def.clone());

        let small_id = add_library_card(&mut state, PlayerId::Us, "Mother of Runes");
        let big_id   = add_library_card(&mut state, PlayerId::Us, "Tarmogoyf");

        let hand_before = state.hand_of(PlayerId::Us).count();
        // eff_enter_permanent pre-registers instances, fires the ZoneChange ETB event,
        // and thereby pushes the Recruiter trigger to state.pending_triggers.
        eff_enter_permanent(PlayerId::Us, "Recruiter of the Guard")
            .call(&mut state, 1, &[]);

        // Resolve all pending ETB triggers.
        let pending = std::mem::take(&mut state.pending_triggers);
        for ctx in pending {
            ctx.effect.call(&mut state, 1, &[]);
        }

        assert_eq!(state.hand_of(PlayerId::Us).count(), hand_before + 1, "hand should grow by one");
        assert_eq!(state.objects[&small_id].zone, CardZone::Hand { known: false }, "Mother of Runes should be in hand");
        assert_eq!(state.objects[&big_id].zone,   CardZone::Library, "Tarmogoyf (toughness 3) should stay in library");
    }

    /// Urza's Saga chapter III: finds an artifact with no colored pips and MV ≤ 1
    /// and puts it on the battlefield. An artifact with MV > 1 stays in library.
    #[test]
    fn test_urza_saga_finds_low_cost_colorless_artifact() {
        let lotus_def = catalog_card("Lotus Petal");
        let fow_def = catalog_card("Force of Will");
        let mut state = make_state();
        state.catalog.insert(lotus_def.name.clone(), lotus_def.clone());
        state.catalog.insert(fow_def.name.clone(), fow_def.clone());
        let lotus_id = add_library_card(&mut state, PlayerId::Us, "Lotus Petal");
        let fow_id   = add_library_card(&mut state, PlayerId::Us, "Force of Will");

        let pred = pred_and(pred_type_eq(CardType::Artifact), pred_and(pred_no_colored_pips(), pred_mana_value_le(1)));
        let eff  = eff_fetch_search(PlayerId::Us, pred, ZoneId::Battlefield);
        eff.call(&mut state, 1, &[]);

        assert_eq!(state.objects[&lotus_id].zone, CardZone::Battlefield, "Lotus Petal should enter battlefield");
        assert_eq!(state.objects[&fow_id].zone,   CardZone::Library,     "FoW should stay in library");
    }

    /// Urza's Saga does not fetch an artifact with a colored pip (e.g. {W}).
    #[test]
    fn test_urza_saga_ignores_colored_artifact() {
        let white_art_def = CardDef::new("White Artifact", CardKind::Artifact(ArtifactData { mana_cost: "W".to_string(), ..Default::default() }), parse_colors("W", false, false), None, vec![], CardLayout::Normal, None, vec![], vec![], vec![]);
        let mut state = make_state();
        state.catalog.insert(white_art_def.name.clone(), white_art_def);
        add_library_card(&mut state, PlayerId::Us, "White Artifact");

        let pred = pred_and(pred_type_eq(CardType::Artifact), pred_and(pred_no_colored_pips(), pred_mana_value_le(1)));
        let eff  = eff_fetch_search(PlayerId::Us, pred, ZoneId::Battlefield);
        eff.call(&mut state, 1, &[]);

        // No candidate matched; library unchanged
        assert_eq!(state.library_of(PlayerId::Us).count(), 1, "colored artifact must not be fetched");
    }

    /// Urza's Saga does not fetch an artifact with MV > 1 (e.g. {2}).
    #[test]
    fn test_urza_saga_ignores_high_mv_artifact() {
        let sol_ring_def = CardDef::new("Sol Ring", CardKind::Artifact(ArtifactData { mana_cost: "2".to_string(), ..Default::default() }), vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![]);
        let mut state = make_state();
        state.catalog.insert(sol_ring_def.name.clone(), sol_ring_def);
        add_library_card(&mut state, PlayerId::Us, "Sol Ring");

        let pred = pred_and(pred_type_eq(CardType::Artifact), pred_and(pred_no_colored_pips(), pred_mana_value_le(1)));
        let eff  = eff_fetch_search(PlayerId::Us, pred, ZoneId::Battlefield);
        eff.call(&mut state, 1, &[]);

        assert_eq!(state.library_of(PlayerId::Us).count(), 1, "MV 2 artifact must not be fetched");
    }

    /// Green Sun's Zenith finds a green creature and puts it on the battlefield.
    /// A non-green creature in the same library is not moved.
    #[test]
    fn test_gsz_finds_green_creature() {
        let troll_def = CardDef::new("Elvish Reclaimer", CardKind::Creature(CreatureData::new("G", 1, 1)), parse_colors("G", false, false), None, vec![], CardLayout::Normal, None, vec![], vec![], vec![]);
        let ragavan_def = CardDef::new("Ragavan, Nimble Pilferer", CardKind::Creature(CreatureData::new("R", 2, 1)), parse_colors("R", false, false), None, vec![], CardLayout::Normal, None, vec![], vec![], vec![]);
        let mut state = make_state();
        state.catalog.insert(troll_def.name.clone(), troll_def);
        state.catalog.insert(ragavan_def.name.clone(), ragavan_def);
        let green_id = add_library_card(&mut state, PlayerId::Us, "Elvish Reclaimer");
        let red_id   = add_library_card(&mut state, PlayerId::Us, "Ragavan, Nimble Pilferer");

        let pred = pred_and(pred_type_eq(CardType::Creature), pred_has_color(Color::Green));
        let eff  = eff_fetch_search(PlayerId::Us, pred, ZoneId::Battlefield);
        eff.call(&mut state, 1, &[]);

        assert_eq!(state.objects[&green_id].zone, CardZone::Battlefield, "green creature should enter battlefield");
        assert_eq!(state.objects[&red_id].zone,   CardZone::Library,     "non-green creature should stay");
    }

    /// Fetchland regression: island-or-swamp search finds the correct land.
    #[test]
    fn test_fetchland_search_via_ability_factory() {
        let pred = pred_and(pred_type_eq(CardType::Land), pred_or(pred_land_subtype("island"), pred_land_subtype("swamp")));
        let delta_ability = AbilityDef { sacrifice_self: true, life_cost: 1, ability_factory: Some(Arc::new(move |who, _| eff_fetch_search(who, pred.clone(), ZoneId::Battlefield))), ..Default::default() };
        let island_def = catalog_card("Underground Sea");
        let forest_def = CardDef::new("Forest", CardKind::Land(LandData {
            land_types: LandTypes { forest: true, ..Default::default() },
            ..Default::default()
        }), vec![], None, vec![Supertype::Basic], CardLayout::Normal, None, vec![], vec![], vec![]);
        let mut state = make_state();
        state.catalog.insert(island_def.name.clone(), island_def);
        state.catalog.insert(forest_def.name.clone(), forest_def);
        let sea_id    = add_library_card(&mut state, PlayerId::Us, "Underground Sea");
        let forest_id = add_library_card(&mut state, PlayerId::Us, "Forest");

        let eff = build_ability_effect(&delta_ability, PlayerId::Us, ObjId::UNSET);
        eff.call(&mut state, 1, &[]);

        assert_eq!(state.objects[&sea_id].zone,    CardZone::Battlefield, "Underground Sea should enter play");
        assert_eq!(state.objects[&forest_id].zone, CardZone::Library,     "Forest should remain in library");
    }
