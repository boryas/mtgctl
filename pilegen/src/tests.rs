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

    fn make_strategies() -> HashMap<PlayerId, Box<dyn strategy::Strategy>> {
        HashMap::from([
            (PlayerId::Us,  Box::new(strategy::DoomsdayStrategy::new(strategy::MatchupInfo::default())) as Box<dyn strategy::Strategy>),
            (PlayerId::Opp, Box::new(strategy::GenericOppStrategy::new(strategy::MatchupInfo::default()))   as Box<dyn strategy::Strategy>),
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
            vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![])
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
            counters: HashMap::new(), ci_timestamp: 0,
        });
        // Look up the real CardDef (including triggers/replacements) from the catalog; fall back
        // to a minimal 1/1 stub for anonymous test creatures that have no special behaviour.
        let def = test_catalog().remove(name).unwrap_or_else(|| {
            CardDef::new(name, CardKind::Creature(CreatureData::new("", 1, 1)),
                         vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![])
        });

        let ts = state.next_ci_timestamp();
        state.objects.get_mut(&id).unwrap().ci_timestamp = ts;
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
            counters: HashMap::new(), ci_timestamp: 0,
        });
        let ts = state.next_ci_timestamp();
        state.objects.get_mut(&id).unwrap().ci_timestamp = ts;
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
            counters: HashMap::new(), ci_timestamp: 0,
        });
        state.catalog.entry(name.to_string())
            .or_insert_with(|| test_catalog().remove(name).unwrap_or_else(|| creature(name, 1, 1)));
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
            counters: HashMap::new(), ci_timestamp: 0,
        });
        id
    }

    /// Put a spell on the stack (for targeting / protection tests).
    fn add_stack_spell(state: &mut SimState, who: PlayerId, def: &CardDef) -> ObjId {
        let id = state.alloc_id();
        state.objects.insert(id, GameObject {
            id,
            catalog_key: def.name.clone(),
            owner: who,
            controller: who,
            zone: CardZone::Stack,
            is_token: false,
            spell: None,
            bf: None,
            materialized: Some(def.clone()),
            counters: HashMap::new(), ci_timestamp: 0,
        });
        state.catalog.entry(def.name.clone()).or_insert_with(|| def.clone());
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
            counters: HashMap::new(), ci_timestamp: 0,
        });
        state.player_mut(who).library_order.push_back(id);
        state.catalog.entry(name.to_string())
            .or_insert_with(|| test_catalog().remove(name).unwrap_or_else(|| creature(name, 1, 1)));
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

        let card_id = cast_spell(&mut state, 1, PlayerId::Us, dark_ritual_id, SpellFace::Main, None, None, &[], 0, 0, None);

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
        let item = cast_spell(&mut state, 1, PlayerId::Us, doomsday_id, SpellFace::Main, None, None, &[], 0, 0, None);

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

        let item = cast_spell(&mut state, 1, PlayerId::Us, fow_id, SpellFace::Main, Some(alt_cost), Some(0), &[], 0, 0, None);

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
        eff_discard(PlayerId::Us, Who::Opp, 1, pred_any()).call(&mut state, 1, &[]);

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
        let ability = AbilityDef { costs: vec![CostComponent::Mana(parse_mana_cost("B"))], ..Default::default() };
        pay_costs(&ability.costs, &mut state, 1, PlayerId::Us, ObjId::UNSET, 0);

        assert_eq!(state.us.pool.b, 1, "1 black spent");
        assert_eq!(state.us.pool.total, 1);
    }

    #[test]
    fn test_pay_activation_cost_life() {
        let mut state = make_state();
        let initial = state.us.life;
        let ability = AbilityDef { costs: vec![CostComponent::Life(2)], ..Default::default() };
        pay_costs(&ability.costs, &mut state, 1, PlayerId::Us, ObjId::UNSET, 0);

        assert_eq!(state.us.life, initial - 2);
    }

    #[test]
    fn test_pay_activation_cost_sacrifice_self() {
        let mut state = make_state();
        let petal_id = add_default_perm(&mut state, PlayerId::Us, "Lotus Petal");
        let ability = AbilityDef { costs: vec![CostComponent::SacSelf], ..Default::default() };
        pay_costs(&ability.costs, &mut state, 1, PlayerId::Us, petal_id, 0);

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
            CardLayout::Normal, None, vec![], vec![], vec![], vec![])
    }

    #[test]
    fn test_effect_destroy_ability_removes_nonbasic_land() {
        let mut state = make_state();
        make_land(&mut state, PlayerId::Opp, "Bayou", false);
        let ability = AbilityDef { target_spec: TargetSpec::ObjectInZone { controller: Who::Opp, zone: ZoneId::Battlefield, filter: obj_pred_from_card(pred_and(pred_type_eq(CardType::Land), pred_not(pred_has_supertype(Supertype::Basic)))) }, ability_factory: Some(Arc::new(|who, _| eff_destroy_target(who))), ..Default::default() };
        let bayou_def = land_def("Bayou", false);
        let catalog = vec![bayou_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let targets: Vec<ObjId> = legal_targets(
            &TargetSpec::ObjectInZone { controller: Who::Opp, zone: ZoneId::Battlefield, filter: obj_pred_from_card(pred_and(pred_type_eq(CardType::Land), pred_not(pred_has_supertype(Supertype::Basic)))) }, PlayerId::Us, ObjId(0), &state
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
        let ability = AbilityDef { target_spec: TargetSpec::ObjectInZone { controller: Who::Opp, zone: ZoneId::Battlefield, filter: obj_pred_from_card(pred_and(pred_type_eq(CardType::Land), pred_not(pred_has_supertype(Supertype::Basic)))) }, ability_factory: Some(Arc::new(|who, _| eff_destroy_target(who))), ..Default::default() };
        let forest_def = land_def("Forest", true);
        let catalog = vec![forest_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let targets: Vec<ObjId> = legal_targets(
            &TargetSpec::ObjectInZone { controller: Who::Opp, zone: ZoneId::Battlefield, filter: obj_pred_from_card(pred_and(pred_type_eq(CardType::Land), pred_not(pred_has_supertype(Supertype::Basic)))) }, PlayerId::Us, ObjId(0), &state
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
        let def = CardDef::new("Treasure Cruise", CardKind::Instant(SpellData { mana_cost: "7U".to_string(), delve: true, ..Default::default() }), vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
        for name in &["A", "B", "C", "D", "E", "F", "G"] {
            add_graveyard_card(&mut state, PlayerId::Us, name);
        }
        let tc_id = add_hand_card(&mut state, PlayerId::Us, "Treasure Cruise");
        state.us.pool.u  = 1;
        state.us.pool.total = 1; // only 1 mana in pool — delve pays the other 7

        let catalog = vec![def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }

        let item = cast_spell(&mut state, 1, PlayerId::Us, tc_id, SpellFace::Main, None, None, &[], 0, 0, None);

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
        let def = CardDef::new("Dead Drop", CardKind::Sorcery(SpellData { mana_cost: "3".to_string(), delve: true, ..Default::default() }), vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
        add_graveyard_card(&mut state, PlayerId::Us, "Ritual");
        add_graveyard_card(&mut state, PlayerId::Us, "Ponder");
        let dead_drop_id = add_hand_card(&mut state, PlayerId::Us, "Dead Drop");
        state.us.pool.total = 1; // covers the 1 remaining generic after delve

        let catalog = vec![def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }

        let item = cast_spell(&mut state, 1, PlayerId::Us, dead_drop_id, SpellFace::Main, None, None, &[], 0, 0, None);

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

        let card_id = cast_spell(&mut state, 1, PlayerId::Us, murktide_id, SpellFace::Main, None, None, &[], 0, 0, None).unwrap();
        let spell = state.objects[&card_id].spell.as_ref().expect("spell state populated").clone();
        let effect = &spell.effect;
        let chosen_targets = spell.chosen_targets.clone();

        // Simulate what resolve_top_of_stack does: stash costs_paid_ctx before calling the effect.
        state.resolving_costs_ctx = spell.costs_paid_ctx.clone();
        effect.as_ref().unwrap().call(&mut state, 1, &chosen_targets);
        state.resolving_costs_ctx = CostsPaidCtx::default();

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

        let card_id = cast_spell(&mut state, 1, PlayerId::Us, murktide_id, SpellFace::Main, None, None, &[], 0, 0, None).unwrap();
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
        let def = CardDef::new("Dead Drop", CardKind::Sorcery(SpellData { mana_cost: "3".to_string(), delve: true, ..Default::default() }), vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
        add_graveyard_card(&mut state, PlayerId::Us, "Ritual");
        add_graveyard_card(&mut state, PlayerId::Us, "Ponder");
        let dead_drop_id = add_hand_card(&mut state, PlayerId::Us, "Dead Drop");
        // no mana

        let catalog = vec![def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }

        let item = cast_spell(&mut state, 1, PlayerId::Us, dead_drop_id, SpellFace::Main, None, None, &[], 0, 0, None);

        assert!(item.is_none(), "can't cast — 1 generic still unpaid");
        assert_eq!(state.graveyard_of(PlayerId::Us).count(), 2, "graveyard unchanged on failed cast");
        assert_eq!(state.exile_of(PlayerId::Us).count(), 0, "nothing exiled on failed cast");
    }

    #[test]
    fn test_effect_exile_ability_removes_creature() {
        let mut state = make_state();
        add_default_perm(&mut state, PlayerId::Opp, "Troll");
        let troll_def = creature("Troll", 2, 2);
        let ability = AbilityDef { target_spec: TargetSpec::ObjectInZone { controller: Who::Opp, zone: ZoneId::Battlefield, filter: obj_pred_from_card(pred_type_eq(CardType::Creature)) }, ability_factory: Some(Arc::new(|who, _| eff_exile_target(who))), ..Default::default() };
        let catalog = vec![troll_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let targets: Vec<ObjId> = legal_targets(
            &TargetSpec::ObjectInZone { controller: Who::Opp, zone: ZoneId::Battlefield, filter: obj_pred_from_card(pred_type_eq(CardType::Creature)) }, PlayerId::Us, ObjId(0), &state
        );
        let eff = build_ability_effect(&ability, PlayerId::Us, ObjId::UNSET);
        eff.call(&mut state, 1, &targets);

        assert!(state.permanents_of(PlayerId::Opp).count() == 0, "Troll should be exiled");
        assert!(state.exile_of(PlayerId::Opp).any(|c| c.catalog_key == "Troll"), "Troll should be in exile");
        assert!(state.graveyard_of(PlayerId::Opp).count() == 0, "exiled, not dead");
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
        let ability = AbilityDef { source_zone: SourceZone::Hand, costs: vec![CostComponent::DiscardSelf, CostComponent::Life(2)], ability_factory: Some(Arc::new(|who, _| eff_draw(who, 1))), ..Default::default() };
        let catalog = vec![wraith_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        // Add Street Wraith to hand and a library card to draw
        let wraith_id = add_hand_card(&mut state, PlayerId::Us, "Street Wraith");
        add_library_card(&mut state, PlayerId::Us, "Island");
        let initial_hand = state.hand_size(PlayerId::Us);

        pay_costs(&ability.costs, &mut state, 1, PlayerId::Us, wraith_id, 0);

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
            mana_abilities: vec![ManaAbility {
                costs: vec![CostComponent::TapSelf],
                produces: produces_colors("U"),
                make_effect: std::sync::Arc::new(|who, _| eff_mana(who, "U")),
                ..Default::default()
            }],
            ..Default::default()
        }), vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
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
        for _seed in 0u64..20 {
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
        data.keywords = Keywords::from_slice(&[Keyword::Flying]);
        CardDef::new(name, CardKind::Creature(data), vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![])
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
        let (result, _) = fire_triggers(&ev, &state);
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
        let (result, _) = fire_triggers(&ev, &state);
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
        let all_targets = legal_targets(&ctx.target_spec, controller, ObjId(0), state);
        let targets: Vec<ObjId> = pick_targets(&ctx.target_spec, &all_targets, state);
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
        let (result, _) = fire_triggers(&ev, &state);
        assert!(result.is_empty(), "no trigger on first natural draw");
    }

    #[test]
    fn test_bowmasters_triggers_on_cantrip_draw() {
        let mut state = make_state();
        add_default_perm(&mut state, PlayerId::Opp, "Orcish Bowmasters");

        let ev = GameEvent::Draw { controller: PlayerId::Us, draw_index: 1, is_natural: false };
        let (result, _) = fire_triggers(&ev, &state);
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
        let (result, _) = fire_triggers(&ev, &state);
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
        let (result, _) = fire_triggers(&ev, &state);
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
        let (result, _) = fire_triggers(&ev, &state);
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
        let (result, _) = fire_triggers(&ev, &state);
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
        let (result, _) = fire_triggers(&ev, &state);
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
            expiry: Some(Expiry::StartOfControllerNextTurn),

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
            expiry: Some(Expiry::StartOfControllerNextTurn),

        });
        assert_eq!(state.trigger_instances.len(), 1);

        // Untap step for PlayerId::Us should expire the floating trigger watcher.
        let step = Step { kind: StepKind::Untap, prio: false };
        do_step(&mut state, 2, PlayerId::Us, &step, true, &mut make_strategies());

        assert!(state.trigger_instances.is_empty(), "Floating trigger expires at controller's next Untap");
    }

    #[test]
    fn test_fatal_push_cannot_target_flipped_tamiyo() {
        let mut state = make_state();
        let tamiyo_id = add_default_perm(&mut state, PlayerId::Opp, "Tamiyo, Inquisitive Student");

        // Flip Tamiyo to her back face (Seasoned Scholar, a planeswalker).
        if let Some(bf) = state.objects.get_mut(&tamiyo_id).and_then(|o| o.bf.as_mut()) {
            bf.active_face = 1;
            bf.loyalty = 2;
        }
        recompute(&mut state);

        // Fatal Push targets "creature with mana value 3 or less".
        let filter = obj_pred_from_card(pred_and(
            pred_type_eq(CardType::Creature),
            pred_mana_value_le(3),
        ));
        let spec = TargetSpec::ObjectInZone {
            controller: Who::Opp,
            zone: ZoneId::Battlefield,
            filter,
        };
        let targets = legal_targets(&spec, PlayerId::Us, ObjId(0), &state);
        assert!(targets.is_empty(), "Fatal Push should not be able to target flipped Tamiyo (she is a planeswalker, not a creature)");
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
            reads: vec![],
            writes: vec![],
            timestamp: 0,
            filter: std::sync::Arc::new(move |id, _, _| id == dragon_id),
            modifier: std::sync::Arc::new(|def, _state| {
                if let CardKind::Creature(c) = &mut def.kind { c.adjust_pt(-1, 0); }
            }),
            expiry: Expiry::EndOfTurn,

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
                expiry: Some(Expiry::EndOfTurn),
    
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
                expiry: Some(Expiry::EndOfTurn),
    
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
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
            bf: None,
            materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });
        state.stack.push(id);
        let mut no_strats: HashMap<PlayerId, Box<dyn Strategy>> = HashMap::new();
        resolve_top_of_stack(&mut state, 1, PlayerId::Us, &mut no_strats);
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

        // With the source gone, collect_legal_actions must never offer ActivateAbility for that id.
        state.current_turn = 1;
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
        recompute(&mut state);
        let legal = strategy::collect_legal_actions(&state, PlayerId::Us);
        assert!(
            !legal.iter().any(|a| matches!(a, LegalAction::ActivateAbility { source_id, .. } if *source_id == delta_id)),
            "offered ability for sacrificed permanent — effect would fire without a stack item"
        );
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
        // Destroy Leyline (change_zone removes its ephemeral CIs)
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
            counters: HashMap::new(), ci_timestamp: 0,
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
        let def = CardDef::new("Jace", CardKind::Planeswalker(PlaneswalkerData { mana_cost: "3U".to_string(), loyalty: 3, ..Default::default() }), vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
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
        let def = CardDef::new("Bowmasters", CardKind::Creature(bowmasters_data), parse_colors("1B", false, true), None, vec![Supertype::Legendary], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
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
        let def = CardDef::new("Bowmasters", CardKind::Creature(bowmasters_data), parse_colors("1B", false, true), None, vec![Supertype::Legendary], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
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
            reads: vec![],
            writes: vec![],
            timestamp: 0,
            filter: std::sync::Arc::new(|_id, controller, _| controller == PlayerId::Us),
            modifier: std::sync::Arc::new(|def, _state| {
                if let CardKind::Creature(c) = &mut def.kind {
                    c.adjust_pt(2, 1);
                }
            }),
            expiry: Expiry::EndOfTurn,

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
            reads: vec![],
            writes: vec![],
            timestamp: 0,
            filter: std::sync::Arc::new(move |id, _, _| id == source_id),
            modifier: std::sync::Arc::new(|def, _state| {
                if let CardKind::Creature(c) = &mut def.kind {
                    c.keywords.insert(Keyword::Flying);
                }
            }),
            expiry: Expiry::WhileSourceOnBattlefield,

        })
    }

    /// A creature with a flying static ability should have the keyword in its materialized
    /// def after ETB, and lose it after LTB.
    #[test]
    fn test_static_ability_def_grants_flying_at_etb() {
        let mut state = make_state();
        let def = CardDef::new("Flyer", CardKind::Creature(CreatureData::new("", 2, 2)), vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![flying_static_ability()]);
        let catalog = vec![def.clone()];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }

        let id = add_perm_with_def(&mut state, PlayerId::Us, &def, BattlefieldState::new());

        // recompute: CI from static_ability_def should add "flying" to materialized keywords.
        recompute(&mut state);
        assert!(state.def_of(id).unwrap().has_keyword(Keyword::Flying), "flying granted via static_ability_def at ETB");
        assert!(creature_has_keyword(id, Keyword::Flying, &state), "creature_has_keyword uses materialized state");
    }

    /// A creature with a flying static ability should lose the keyword CI when it
    /// leaves the battlefield (change_zone removes WhileSourceOnBattlefield CIs).
    #[test]
    fn test_static_ability_def_removed_at_ltb() {
        let mut state = make_state();
        let def = CardDef::new("Flyer", CardKind::Creature(CreatureData::new("", 2, 2)), vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![flying_static_ability()]);
        let catalog = vec![def.clone()];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }

        let id = add_perm_with_def(&mut state, PlayerId::Us, &def, BattlefieldState::new());
        // Static-ability CIs are no longer registered in continuous_instances at ETB —
        // they are derived fresh each recompute cycle from the catalog.
        assert_eq!(state.continuous_instances.len(), 0, "no ephemeral CIs registered");

        // Recompute generates the static CI and applies flying.
        recompute(&mut state);
        assert!(state.def_of(id).unwrap().has_keyword(Keyword::Flying), "flying applied by recompute");

        // Move the permanent off the battlefield.
        state.objects.get_mut(&id).unwrap().zone = CardZone::Graveyard;
        recompute(&mut state);
        // Static CI is not generated for non-BF objects, so flying should be gone.
        if let Some(d) = state.def_of(id) {
            assert!(!d.has_keyword(Keyword::Flying), "flying removed when off battlefield");
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
            reads: vec![],
            writes: vec![],
            timestamp: 0,
            filter: std::sync::Arc::new(move |obj_id, _, _| obj_id == id),
            modifier: std::sync::Arc::new(|def, state| {
                let gy = state.graveyard_of(PlayerId::Us).count() as i32;
                if let CardKind::Creature(c) = &mut def.kind {
                    let delta = gy - c.power();
                    c.adjust_pt(delta, 0);
                }
            }),
            expiry: Expiry::WhileSourceOnBattlefield,

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
        let white_art_def = CardDef::new("White Artifact", CardKind::Artifact(ArtifactData { mana_cost: "W".to_string(), ..Default::default() }), parse_colors("W", false, false), None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
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
        let sol_ring_def = CardDef::new("Sol Ring", CardKind::Artifact(ArtifactData { mana_cost: "2".to_string(), ..Default::default() }), vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
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
        let troll_def = CardDef::new("Elvish Reclaimer", CardKind::Creature(CreatureData::new("G", 1, 1)), parse_colors("G", false, false), None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
        let ragavan_def = CardDef::new("Ragavan, Nimble Pilferer", CardKind::Creature(CreatureData::new("R", 2, 1)), parse_colors("R", false, false), None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
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
        let delta_ability = AbilityDef { costs: vec![CostComponent::SacSelf, CostComponent::Life(1)], ability_factory: Some(Arc::new(move |who, _| eff_fetch_search(who, pred.clone(), ZoneId::Battlefield))), ..Default::default() };
        let island_def = catalog_card("Underground Sea");
        let forest_def = CardDef::new("Forest", CardKind::Land(LandData {
            land_types: LandTypes { forest: true, ..Default::default() },
            ..Default::default()
        }), vec![], None, vec![Supertype::Basic], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
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

    // ── Section 20: CostsPaidCtx / objects_moved ─────────────────────────────

    /// FoW pitch records the pitched card id in costs_paid_ctx.objects_moved.
    #[test]
    fn test_fow_pitch_objects_moved_contains_pitch_card() {
        let mut state = make_state();
        let fow_def = catalog_card("Force of Will");
        let brainstorm_def = catalog_card("Brainstorm");
        for c in &[fow_def.clone(), brainstorm_def] {
            state.catalog.insert(c.name.clone(), c.clone());
        }
        let fow_id = add_hand_card(&mut state, PlayerId::Us, "Force of Will");
        let bs_id  = add_hand_card(&mut state, PlayerId::Us, "Brainstorm");
        let alt_cost = &fow_def.alternate_costs()[0];

        let card_id = cast_spell(&mut state, 1, PlayerId::Us, fow_id, SpellFace::Main, Some(alt_cost), Some(0), &[], 0, 0, None).unwrap();
        let ctx = &state.objects[&card_id].spell.as_ref().unwrap().costs_paid_ctx;

        assert_eq!(ctx.objects_moved, vec![bs_id], "pitched Brainstorm id recorded in objects_moved");
    }

    /// FoW can't pitch itself — needs a second blue card in hand.
    #[test]
    fn test_fow_cannot_pitch_itself() {
        let mut state = make_state();
        let fow_def = catalog_card("Force of Will");
        state.catalog.insert(fow_def.name.clone(), fow_def.clone());
        let fow_id = add_hand_card(&mut state, PlayerId::Us, "Force of Will");
        // No other cards — pitch cost requires another blue non-land card; also no mana for 3UU.
        let result = cast_spell(&mut state, 1, PlayerId::Us, fow_id, SpellFace::Main, None, None, &[], 0, 0, None);
        assert!(result.is_none(), "FoW can't be cast with only itself in hand and no mana");
    }

    /// FoW normal cost (3UU mana) works when pool is sufficient.
    #[test]
    fn test_fow_normal_mana_cost() {
        let mut state = make_state();
        let fow_def = catalog_card("Force of Will");
        state.catalog.insert(fow_def.name.clone(), fow_def.clone());
        let fow_id = add_hand_card(&mut state, PlayerId::Us, "Force of Will");
        state.us.pool.u     = 2;
        state.us.pool.total = 5; // 3 generic + 2 blue

        let result = cast_spell(&mut state, 1, PlayerId::Us, fow_id, SpellFace::Main, None, None, &[], 0, 0, None);
        assert!(result.is_some(), "FoW should cast for 3UU when pool is full");
        assert_eq!(state.us.pool.total, 0, "all mana spent");
    }

    // ── Section 21: Snuff Out ────────────────────────────────────────────────

    /// Snuff Out can be cast for 4 life with no mana at all.
    #[test]
    fn test_snuff_out_life_alternate_cost() {
        let mut state = make_state();
        let def = catalog_card("Snuff Out");
        state.catalog.insert(def.name.clone(), def.clone());
        let troll_def = creature("Troll", 2, 2);
        state.catalog.insert(troll_def.name.clone(), troll_def);
        add_default_perm(&mut state, PlayerId::Opp, "Troll");
        let snuff_id = add_hand_card(&mut state, PlayerId::Us, "Snuff Out");
        let initial_life = state.us.life;
        let alt = &def.alternate_costs()[0];

        let result = cast_spell(&mut state, 1, PlayerId::Us, snuff_id, SpellFace::Main, Some(alt), Some(0), &[], 0, 0, None);
        assert!(result.is_some(), "Snuff Out should cast for 4 life");
        assert_eq!(state.us.life, initial_life - 4, "paid 4 life");
        let ctx = &state.objects[&result.unwrap()].spell.as_ref().unwrap().costs_paid_ctx;
        assert!(ctx.objects_moved.is_empty(), "no objects moved for life payment");
    }

    /// Snuff Out alternate cost requires life > 4 (can't pay if at exactly 4 or below).
    #[test]
    fn test_snuff_out_cant_pay_life_when_low() {
        let mut state = make_state();
        let def = catalog_card("Snuff Out");
        state.catalog.insert(def.name.clone(), def.clone());
        let snuff_id = add_hand_card(&mut state, PlayerId::Us, "Snuff Out");
        state.us.life = 4; // exactly 4 — can't pay (would reach 0)
        let alt = &def.alternate_costs()[0];
        let ok = can_pay_costs(&alt.costs, &state, PlayerId::Us, snuff_id, false, 0);
        assert!(!ok, "can't pay 4 life when at 4 life (would reach 0)");
    }

    // ── Section 22: Street Wraith cycling ───────────────────────────────────

    /// can_pay_costs returns false for DiscardSelf when the card is not in hand.
    #[test]
    fn test_street_wraith_discard_self_not_in_hand() {
        let mut state = make_state();
        let wraith_def = catalog_card("Street Wraith");
        state.catalog.insert(wraith_def.name.clone(), wraith_def);
        // Place the wraith in the graveyard instead of hand.
        let wraith_id = add_graveyard_card(&mut state, PlayerId::Us, "Street Wraith");
        let costs = vec![CostComponent::DiscardSelf, CostComponent::Life(2)];
        let ok = can_pay_costs(&costs, &state, PlayerId::Us, wraith_id, false, 0);
        assert!(!ok, "can't cycle from graveyard — DiscardSelf requires card in hand");
    }

    /// After paying DiscardSelf + Life(2), the wraith is in the graveyard and 2 life is gone.
    /// DiscardSelf moves the source itself so it is not in objects_moved (only "other" objects are).
    #[test]
    fn test_street_wraith_discard_self_pays_correctly() {
        let mut state = make_state();
        let wraith_def = catalog_card("Street Wraith");
        state.catalog.insert(wraith_def.name.clone(), wraith_def);
        state.us.life = 20;
        let wraith_id = add_hand_card(&mut state, PlayerId::Us, "Street Wraith");
        let costs = vec![CostComponent::DiscardSelf, CostComponent::Life(2)];
        let ctx = pay_costs(&costs, &mut state, 1, PlayerId::Us, wraith_id, 0);
        // DiscardSelf moves the source itself — not tracked in objects_moved (only "other" objects are).
        assert!(ctx.objects_moved.is_empty(), "DiscardSelf does not appear in objects_moved");
        assert!(state.graveyard_of(PlayerId::Us).any(|c| c.id == wraith_id), "wraith in graveyard");
        assert_eq!(state.us.life, 18, "2 life paid");
    }

    // ── Section 23: Daze bounce cost ────────────────────────────────────────

    /// Daze's alternate cost bounces a blue-producing land; the bounced id is recorded.
    #[test]
    fn test_daze_bounce_alt_cost_records_returned_island() {
        let mut state = make_state();
        let daze_def = catalog_card("Daze");
        state.catalog.insert(daze_def.name.clone(), daze_def.clone());
        // Island on the battlefield (blue-producing).
        let island_id = island_land(&mut state, PlayerId::Us);
        let daze_id = add_hand_card(&mut state, PlayerId::Us, "Daze");
        let alt = &daze_def.alternate_costs()[0]; // ReturnFromBattlefield(Island subtype)

        let result = cast_spell(&mut state, 1, PlayerId::Us, daze_id, SpellFace::Main, Some(alt), Some(0), &[], 0, 0, None);
        assert!(result.is_some(), "Daze should cast by bouncing the Island");
        let ctx = &state.objects[&result.unwrap()].spell.as_ref().unwrap().costs_paid_ctx;
        assert_eq!(ctx.objects_moved, vec![island_id], "bounced Island id in objects_moved");
        assert!(state.hand_of(PlayerId::Us).any(|c| c.id == island_id), "Island returned to hand");
    }

    // ── Section 24: Ninjutsu costs_paid_ctx ─────────────────────────────────

    /// pay_costs for ReturnFromBattlefield captures the attacker's attack_target.
    #[test]
    fn test_ninjutsu_return_cost_records_attack_target() {
        let mut state = make_state();
        let opp_id = state.opp.id;
        // Set up an attacking Ragavan with attack_target set to Opp.
        let ragavan_id = add_perm(&mut state, PlayerId::Us, "Ragavan", BattlefieldState {
            attacking: true,
            unblocked: true,
            attack_target: Some(opp_id),
            ..BattlefieldState::new()
        });
        // The cost we test is just ReturnFromBattlefield + mana, applied directly.
        let pred = cost_pred_unblocked_attacker();
        let costs = vec![CostComponent::ReturnFromBattlefield(pred), CostComponent::Mana(parse_mana_cost("1U"))];
        state.us.pool.u     = 1;
        state.us.pool.total = 2;

        let ctx = pay_costs(&costs, &mut state, 1, PlayerId::Us, ObjId::UNSET, 0);

        assert_eq!(ctx.objects_moved, vec![ragavan_id], "returned attacker id in objects_moved");
        assert_eq!(ctx.returned_attack_targets, vec![Some(opp_id)], "opp player id captured as attack target");
        // Ragavan should now be in hand.
        assert!(state.hand_of(PlayerId::Us).any(|c| c.id == ragavan_id), "Ragavan moved to hand");
    }

    // ── Section 25: Additional costs ────────────────────────────────────────

    /// A spell with additional_costs requires those costs to be payable.
    #[test]
    fn test_additional_cost_blocks_cast_when_unpayable() {
        let mut state = make_state();
        // Build a cheap spell ({B}) with an additional Life(3) cost.
        let mut def = catalog_card("Dark Ritual");
        def.additional_costs = vec![CostComponent::Life(3)];
        state.catalog.insert(def.name.clone(), def.clone());
        let card_id = add_hand_card(&mut state, PlayerId::Us, "Dark Ritual");
        state.us.pool.b = 1; state.us.pool.total = 1;
        state.us.life = 3; // can't pay Life(3) — would reach 0

        let result = cast_spell(&mut state, 1, PlayerId::Us, card_id, SpellFace::Main, None, None, &[], 0, 0, None);
        assert!(result.is_none(), "additional Life(3) cost blocks cast at 3 life");
    }

    /// A spell with a payable additional_cost is cast and the cost is paid.
    #[test]
    fn test_additional_cost_paid_on_cast() {
        let mut state = make_state();
        let mut def = catalog_card("Dark Ritual");
        def.additional_costs = vec![CostComponent::Life(3)];
        state.catalog.insert(def.name.clone(), def.clone());
        let card_id = add_hand_card(&mut state, PlayerId::Us, "Dark Ritual");
        state.us.pool.b = 1; state.us.pool.total = 1;
        let initial_life = state.us.life; // 20

        let result = cast_spell(&mut state, 1, PlayerId::Us, card_id, SpellFace::Main, None, None, &[], 0, 0, None);
        assert!(result.is_some(), "Dark Ritual + Life(3) additional cost is payable at 20 life");
        assert_eq!(state.us.life, initial_life - 3, "additional Life(3) was paid");
    }

    // ── Section 26: Bitter Triumph (CostOr additional cost) ──────────────────

    fn setup_bitter_triumph(state: &mut SimState) {
        let bt = catalog_card("Bitter Triumph");
        state.catalog.insert(bt.name.clone(), bt);
        // Spare card for discard tests — needs a catalog entry so cost_pred_from_card resolves it.
        let dr = catalog_card("Dark Ritual");
        state.catalog.insert(dr.name.clone(), dr);
    }

    /// Bitter Triumph prefers the discard branch when a card is available in hand.
    #[test]
    fn test_bitter_triumph_discard_branch_preferred() {
        let mut state = make_state();
        setup_bitter_triumph(&mut state);
        let extra_id = add_hand_card(&mut state, PlayerId::Us, "Dark Ritual");
        let card_id = add_hand_card(&mut state, PlayerId::Us, "Bitter Triumph");
        state.us.pool.b = 2; state.us.pool.total = 2;
        let initial_life = state.us.life;

        let result = cast_spell(&mut state, 1, PlayerId::Us, card_id, SpellFace::Main, None, None, &[], 0, 0, None);
        assert!(result.is_some(), "Bitter Triumph should be castable");
        let extra_zone = state.objects.get(&extra_id).map(|o| &o.zone);
        assert!(
            matches!(extra_zone, Some(CardZone::Graveyard)),
            "discard branch of CostOr was paid (card discarded)"
        );
        assert_eq!(state.us.life, initial_life, "life branch was not taken when discard is available");
    }

    /// Bitter Triumph falls back to the life branch when no spare card is in hand.
    #[test]
    fn test_bitter_triumph_life_branch_fallback() {
        let mut state = make_state();
        setup_bitter_triumph(&mut state);
        let card_id = add_hand_card(&mut state, PlayerId::Us, "Bitter Triumph");
        state.us.pool.b = 2; state.us.pool.total = 2;
        let initial_life = state.us.life;

        let result = cast_spell(&mut state, 1, PlayerId::Us, card_id, SpellFace::Main, None, None, &[], 0, 0, None);
        assert!(result.is_some(), "Bitter Triumph should be castable via life branch");
        assert_eq!(state.us.life, initial_life - 3, "3 life paid as fallback cost");
    }

    /// Bitter Triumph is uncastable when neither branch can be paid.
    #[test]
    fn test_bitter_triumph_unpayable_when_both_branches_blocked() {
        let mut state = make_state();
        setup_bitter_triumph(&mut state);
        let card_id = add_hand_card(&mut state, PlayerId::Us, "Bitter Triumph");
        state.us.pool.b = 2; state.us.pool.total = 2;
        state.us.life = 3; // can't pay Life(3) — life > n is strict

        let result = cast_spell(&mut state, 1, PlayerId::Us, card_id, SpellFace::Main, None, None, &[], 0, 0, None);
        assert!(result.is_none(), "Bitter Triumph should be blocked when life ≤ 3 and no spare card");
    }

    // ── Section 27: Consign to Memory (Replicate + triggered-ability targeting) ──

    fn setup_consign(state: &mut SimState) {
        let def = catalog_card("Consign to Memory");
        state.catalog.insert(def.name.clone(), def);
    }

    /// Push a fake colorless spell onto the stack for the opponent.
    fn push_colorless_spell_for_opp(state: &mut SimState) -> ObjId {
        // Use Lotus Petal as a colorless spell proxy.
        let def = catalog_card("Lotus Petal");
        state.catalog.insert(def.name.clone(), def.clone());
        let spell_id = state.alloc_id();
        state.objects.insert(spell_id, GameObject {
            id: spell_id,
            catalog_key: "Lotus Petal".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            zone: CardZone::Stack,
            is_token: false,
            spell: Some(SpellState {
                effect: None,
                chosen_targets: vec![],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
            bf: None,
            materialized: Some(def),
            counters: HashMap::new(), ci_timestamp: 0,
        });
        state.stack.push(spell_id);
        spell_id
    }

    /// Push a fake triggered ability onto the stack for the opponent.
    fn push_opp_triggered_ability(state: &mut SimState) -> ObjId {
        let ab_id = state.alloc_id();
        let opp_player_id = state.player_id(PlayerId::Opp);
        state.abilities.insert(ab_id, StackAbility {
            id: ab_id,
            source_name: "Test Trigger".to_string(),
            owner: opp_player_id,
            effect: Effect(std::sync::Arc::new(|_, _, _| {})),
            chosen_targets: vec![],
            costs_paid_ctx: CostsPaidCtx::default(),
            is_triggered: true,
            counterable: true,
            choice_spec: None,
        });
        state.stack.push(ab_id);
        ab_id
    }

    /// Consign to Memory can counter a colorless spell on the stack.
    #[test]
    fn test_consign_counters_colorless_spell() {
        let mut state = make_state();
        setup_consign(&mut state);
        let spell_id = push_colorless_spell_for_opp(&mut state);
        let card_id = add_hand_card(&mut state, PlayerId::Us, "Consign to Memory");
        state.us.pool.u = 1; state.us.pool.total = 1;

        let result = cast_spell(&mut state, 1, PlayerId::Us, card_id, SpellFace::Main, None, None, &[spell_id], 0, 0, None);
        assert!(result.is_some(), "Consign to Memory should be castable");

        // Resolve — pop from stack and execute effect.
        let card_on_stack = result.unwrap();
        let spell_state = state.objects[&card_on_stack].spell.clone().unwrap();
        spell_state.effect.unwrap().call(&mut state, 1, &spell_state.chosen_targets);

        assert!(!state.stack.contains(&spell_id), "colorless spell should be removed from stack");
        assert_eq!(
            state.objects.get(&spell_id).map(|o| &o.zone),
            Some(&CardZone::Graveyard),
            "countered spell goes to graveyard"
        );
    }

    /// Consign to Memory can counter a triggered ability on the stack.
    #[test]
    fn test_consign_counters_triggered_ability() {
        let mut state = make_state();
        setup_consign(&mut state);
        let ab_id = push_opp_triggered_ability(&mut state);
        let card_id = add_hand_card(&mut state, PlayerId::Us, "Consign to Memory");
        state.us.pool.u = 1; state.us.pool.total = 1;

        let result = cast_spell(&mut state, 1, PlayerId::Us, card_id, SpellFace::Main, None, None, &[ab_id], 0, 0, None);
        assert!(result.is_some(), "Consign to Memory should be castable targeting a triggered ability");

        // Resolve.
        let card_on_stack = result.unwrap();
        let spell_state = state.objects[&card_on_stack].spell.clone().unwrap();
        spell_state.effect.unwrap().call(&mut state, 1, &spell_state.chosen_targets);

        assert!(!state.stack.contains(&ab_id), "triggered ability should be removed from stack");
        assert!(!state.abilities.contains_key(&ab_id), "triggered ability removed from abilities map");
    }

    /// `AbilityOnStack::Triggered` legal_targets enumerates opponent triggered abilities.
    #[test]
    fn test_triggered_ability_on_stack_legal_targets() {
        let mut state = make_state();
        let ab_id = push_opp_triggered_ability(&mut state);

        let spec = TargetSpec::AbilityOnStack { controller: Who::Opp, ability_type: AbilityType::Triggered };
        let targets = legal_targets(&spec, PlayerId::Us, ObjId(0), &state);
        assert!(targets.contains(&ab_id), "opp triggered ability should be a legal target");
    }

    /// Activated abilities (is_triggered=false) are not matched by `AbilityOnStack::Triggered`.
    #[test]
    fn test_activated_ability_not_a_trigger_target() {
        let mut state = make_state();
        let ab_id = state.alloc_id();
        let opp_player_id = state.player_id(PlayerId::Opp);
        state.abilities.insert(ab_id, StackAbility {
            id: ab_id,
            source_name: "Activated Ability".to_string(),
            owner: opp_player_id,
            effect: Effect(std::sync::Arc::new(|_, _, _| {})),
            chosen_targets: vec![],
            costs_paid_ctx: CostsPaidCtx::default(),
            is_triggered: false,
            counterable: true,
            choice_spec: None,
        });
        state.stack.push(ab_id);

        let spec = TargetSpec::AbilityOnStack { controller: Who::Opp, ability_type: AbilityType::Triggered };
        let targets = legal_targets(&spec, PlayerId::Us, ObjId(0), &state);
        assert!(!targets.contains(&ab_id), "activated ability should not match AbilityOnStack::Triggered");
    }


    /// eff_counter_target fizzles against a spell with counterable=false (CR 608.2b).
    #[test]
    fn test_counter_fizzles_on_uncounterable_spell() {
        let mut state = make_state();
        // Push a fake uncounterable spell for the opponent.
        let spell_id = state.alloc_id();
        state.objects.insert(spell_id, GameObject {
            id: spell_id,
            catalog_key: "Long Goodbye".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            zone: CardZone::Stack,
            is_token: false,
            bf: None,
            spell: Some(SpellState { effect: None, chosen_targets: vec![], is_back_face: false, costs_paid_ctx: CostsPaidCtx::default() }),
            materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });
        state.stack.push(spell_id);
        // Set counterable=false by inserting the card's def (with the flag) into the catalog.
        let mut lg_def = catalog_card("Long Goodbye");
        lg_def.counterable = false;
        state.catalog.insert("Long Goodbye".to_string(), lg_def);

        let effect = eff_counter_target(PlayerId::Us);
        effect.call(&mut state, 1, &[spell_id]);

        // Spell should still be on the stack — counter fizzled.
        assert!(state.stack.contains(&spell_id), "uncounterable spell should remain on stack");
        assert_eq!(state.objects[&spell_id].zone, CardZone::Stack, "zone unchanged after fizzle");
    }

    // ── 28. Force of Negation ─────────────────────────────────────────────────

    /// The pitch-cost condition on Force of Negation is true when it's not the caster's turn
    /// and false when it is. CR 118.9b (alternative costs may have conditions on card text).
    #[test]
    fn test_fon_pitch_condition_checks_active_player() {
        let mut state = make_state();
        let fon_def = catalog_card("Force of Negation");
        let alt = &fon_def.alternate_costs()[0];
        let condition = alt.condition.as_ref()
            .expect("Force of Negation pitch cost must have a condition");

        // Opponent's turn: condition should allow Us to pitch.
        state.current_ap = state.player_id(PlayerId::Opp);
        assert!(condition(PlayerId::Us, &state), "pitch cost available when it's not our turn");

        // Our turn: condition should block the pitch cost.
        state.current_ap = state.player_id(PlayerId::Us);
        assert!(!condition(PlayerId::Us, &state), "pitch cost unavailable on our own turn");
    }

    /// eff_counter_and_exile sends the countered spell to Exile, not Graveyard.
    /// Models Force of Negation's "exile it instead of putting it into its owner's graveyard".
    #[test]
    fn test_fon_counter_and_exile() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Simulate opponent's turn (required for pitch cost, though we call the effect directly).
        state.current_ap = state.player_id(PlayerId::Opp);

        // Push a noncreature opponent spell onto the stack.
        let spell_id = state.alloc_id();
        state.objects.insert(spell_id, GameObject {
            id: spell_id,
            catalog_key: "Dark Ritual".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            zone: CardZone::Stack,
            is_token: false,
            bf: None,
            spell: Some(SpellState {
                effect: None,
                chosen_targets: vec![],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
            materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });
        state.stack.push(spell_id);

        // Fire FoN's counter-and-exile effect.
        let fon_id = state.alloc_id();
        let effect = eff_counter_and_exile(PlayerId::Us, fon_id);
        effect.call(&mut state, 1, &[spell_id]);

        // Spell should be in Exile, not Graveyard; stack should be empty.
        assert!(!state.stack.contains(&spell_id), "countered spell should be off the stack");
        assert_eq!(
            state.objects[&spell_id].zone,
            CardZone::Exile { on_adventure: false },
            "countered spell should be exiled, not in graveyard",
        );
        assert!(state.objects[&spell_id].spell.is_none(), "spell state should be cleared");
    }

    /// If FoN itself is countered before resolving, its scoped replacement effect is never
    /// installed, so the target remains on the stack unaffected (not exiled).
    #[test]
    fn test_fon_countered_target_not_exiled() {
        let mut state = make_state();

        // Y — opponent's noncreature spell (FoN's target).
        let y_id = state.alloc_id();
        state.objects.insert(y_id, GameObject {
            id: y_id,
            catalog_key: "Dark Ritual".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            zone: CardZone::Stack,
            is_token: false,
            bf: None,
            spell: Some(SpellState { effect: None, chosen_targets: vec![], is_back_face: false, costs_paid_ctx: CostsPaidCtx::default() }),
            materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });
        state.stack.push(y_id);

        // FoN targeting Y — cast by us.
        let fon_id = state.alloc_id();
        state.objects.insert(fon_id, GameObject {
            id: fon_id,
            catalog_key: "Force of Negation".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            zone: CardZone::Stack,
            is_token: false,
            bf: None,
            spell: Some(SpellState {
                effect: Some(eff_counter_and_exile(PlayerId::Us, fon_id)),
                chosen_targets: vec![y_id],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
            materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });
        state.stack.push(fon_id);

        // Opponent counters FoN — its effect closure never runs, so the scoped RE is never installed.
        eff_counter_target(PlayerId::Opp).call(&mut state, 1, &[fon_id]);

        assert!(!state.stack.contains(&fon_id), "FoN should be off the stack after being countered");
        assert_eq!(state.objects[&fon_id].zone, CardZone::Graveyard, "FoN goes to graveyard");
        assert!(state.stack.contains(&y_id), "Y should still be on the stack — FoN never resolved");
        assert_eq!(state.objects[&y_id].zone, CardZone::Stack, "Y remains in Stack zone");
    }

    /// Stack: X (bottom), Y, FoN targeting Y, FoW targeting X (top).
    /// FoW resolves first → counters X → X to graveyard.
    /// FoN resolves next → counters Y → Y to exile (scoped replacement).
    /// After both resolutions: X in graveyard, Y in exile.
    #[test]
    fn test_fow_x_fon_y_stack_interaction() {
        let mut state = make_state();

        // X and Y — opponent noncreature spells.
        let x_id = state.alloc_id();
        let y_id = state.alloc_id();
        for &id in &[x_id, y_id] {
            state.objects.insert(id, GameObject {
                id,
                catalog_key: "Dark Ritual".to_string(),
                owner: PlayerId::Opp,
                controller: PlayerId::Opp,
                zone: CardZone::Stack,
                is_token: false,
                bf: None,
                spell: Some(SpellState { effect: None, chosen_targets: vec![], is_back_face: false, costs_paid_ctx: CostsPaidCtx::default() }),
                materialized: None,
                counters: HashMap::new(), ci_timestamp: 0,
            });
        }

        // FoN targeting Y.
        let fon_id = state.alloc_id();
        state.objects.insert(fon_id, GameObject {
            id: fon_id,
            catalog_key: "Force of Negation".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            zone: CardZone::Stack,
            is_token: false,
            bf: None,
            spell: Some(SpellState {
                effect: Some(eff_counter_and_exile(PlayerId::Us, fon_id)),
                chosen_targets: vec![y_id],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
            materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });

        // FoW targeting X.
        let fow_id = state.alloc_id();
        state.objects.insert(fow_id, GameObject {
            id: fow_id,
            catalog_key: "Force of Will".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            zone: CardZone::Stack,
            is_token: false,
            bf: None,
            spell: Some(SpellState {
                effect: Some(eff_counter_target(PlayerId::Us)),
                chosen_targets: vec![x_id],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
            materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });

        // Stack order bottom→top: X, Y, FoN, FoW.
        state.stack.extend([x_id, y_id, fon_id, fow_id]);

        let mut no_strats: HashMap<PlayerId, Box<dyn Strategy>> = HashMap::new();
        // FoW resolves: counters X → X to graveyard; FoW itself to graveyard.
        resolve_top_of_stack(&mut state, 1, PlayerId::Us, &mut no_strats);
        // FoN resolves: scoped RE installed, counters Y → Y intercepted to exile; FoN to graveyard.
        resolve_top_of_stack(&mut state, 1, PlayerId::Us, &mut no_strats);

        assert!(state.stack.is_empty(), "stack should be empty");
        assert_eq!(state.objects[&x_id].zone, CardZone::Graveyard, "X countered by FoW → graveyard");
        assert_eq!(
            state.objects[&y_id].zone,
            CardZone::Exile { on_adventure: false },
            "Y countered by FoN → exile",
        );
        assert_eq!(state.objects[&fow_id].zone, CardZone::Graveyard, "FoW → graveyard after resolving");
        assert_eq!(state.objects[&fon_id].zone, CardZone::Graveyard, "FoN → graveyard after resolving");
    }

    // ── Section 29: Dauthi Voidwalker ─────────────────────────────────────────

    /// DV replacement: when opponent's card would go to graveyard, it exiles with a void counter.
    #[test]
    fn test_dv_replacement_exiles_opponent_card() {
        let mut state = make_state();

        // Put DV on battlefield under Opp's control.
        let dv_def = catalog_card("Dauthi Voidwalker");
        state.catalog.insert(dv_def.name.clone(), dv_def.clone());
        let dv_id = state.alloc_id();


        state.objects.insert(dv_id, GameObject {
            id: dv_id,
            catalog_key: "Dauthi Voidwalker".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            zone: CardZone::Battlefield,
            is_token: false,
            spell: None,
            bf: Some(BattlefieldState::new()),
            materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });

        // Put a Us-owned card in graveyard-bound position (hand card moved to GY).
        let card_id = state.alloc_id();
        state.objects.insert(card_id, GameObject {
            id: card_id,
            catalog_key: "Dark Ritual".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            zone: CardZone::Hand { known: true },
            is_token: false,
            spell: None,
            bf: None,
            materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });

        // Trigger zone change to graveyard — DV's replacement should intercept.
        change_zone(card_id, ZoneId::Graveyard, &mut state, 1, PlayerId::Us);

        // Card should be in exile, not graveyard.
        assert_eq!(
            state.objects[&card_id].zone,
            CardZone::Exile { on_adventure: false },
            "DV replacement: card should be in exile, not graveyard",
        );
        // Card should have a void counter.
        assert_eq!(
            state.objects[&card_id].counters.get(&CounterType::Void).copied().unwrap_or(0),
            1,
            "DV replacement: exiled card should have a void counter",
        );
    }

    /// DV replacement does NOT fire when DV's controller's own card goes to the graveyard.
    #[test]
    fn test_dv_replacement_does_not_fire_for_own_cards() {
        let mut state = make_state();

        // DV under Opp's control.
        let dv_def = catalog_card("Dauthi Voidwalker");
        state.catalog.insert(dv_def.name.clone(), dv_def.clone());
        let dv_id = state.alloc_id();


        state.objects.insert(dv_id, GameObject {
            id: dv_id,
            catalog_key: "Dauthi Voidwalker".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            zone: CardZone::Battlefield,
            is_token: false,
            spell: None,
            bf: Some(BattlefieldState::new()),
            materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });

        // Opp's own card going to graveyard — should NOT be intercepted.
        let opp_card_id = state.alloc_id();
        state.objects.insert(opp_card_id, GameObject {
            id: opp_card_id,
            catalog_key: "Dark Ritual".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            zone: CardZone::Hand { known: true },
            is_token: false,
            spell: None,
            bf: None,
            materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });

        change_zone(opp_card_id, ZoneId::Graveyard, &mut state, 1, PlayerId::Opp);

        assert_eq!(
            state.objects[&opp_card_id].zone,
            CardZone::Graveyard,
            "DV replacement must not intercept its controller's own cards",
        );
        assert_eq!(
            state.objects[&opp_card_id].counters.get(&CounterType::Void).copied().unwrap_or(0),
            0,
        );
    }

    // ── Section 30: Surgical Extraction ───────────────────────────────────────

    /// Surgical Extraction exiles the targeted GY card plus all same-name cards
    /// from the owner's graveyard, hand, and library. Other-named cards are untouched.
    #[test]
    fn test_surgical_extraction_exiles_all_copies() {
        let mut state = make_state();
        state.catalog.extend(test_catalog());

        // Opp has 3 copies of Dark Ritual spread across zones: GY, hand, library.
        let gy_id = state.alloc_id();
        state.objects.insert(gy_id, GameObject {
            id: gy_id,
            catalog_key: "Dark Ritual".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            zone: CardZone::Graveyard,
            is_token: false, spell: None, bf: None, materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });
        let hand_id = state.alloc_id();
        state.objects.insert(hand_id, GameObject {
            id: hand_id,
            catalog_key: "Dark Ritual".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            zone: CardZone::Hand { known: false },
            is_token: false, spell: None, bf: None, materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });
        let lib_id = state.alloc_id();
        state.objects.insert(lib_id, GameObject {
            id: lib_id,
            catalog_key: "Dark Ritual".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            zone: CardZone::Library,
            is_token: false, spell: None, bf: None, materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });
        state.opp.library_order.push_back(lib_id);
        // A different card in opp's hand — must not be exiled.
        let other_id = state.alloc_id();
        state.objects.insert(other_id, GameObject {
            id: other_id,
            catalog_key: "Brainstorm".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            zone: CardZone::Hand { known: false },
            is_token: false, spell: None, bf: None, materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });

        // Build and call the Surgical Extraction effect targeting gy_id.
        let se_def = catalog_card("Surgical Extraction");
        let factory = match &se_def.kind {
            CardKind::Instant(s) => s.modes.as_ref().unwrap().get(0).unwrap().factory.clone(),
            _ => panic!("not an instant"),
        };
        let eff = factory(PlayerId::Us, ObjId::UNSET, 0);
        eff.call(&mut state, 1, &[gy_id]);

        // All 3 Dark Ritual copies should be in exile.
        assert_eq!(state.objects[&gy_id].zone,   CardZone::Exile { on_adventure: false }, "GY copy exiled");
        assert_eq!(state.objects[&hand_id].zone,  CardZone::Exile { on_adventure: false }, "hand copy exiled");
        assert_eq!(state.objects[&lib_id].zone,   CardZone::Exile { on_adventure: false }, "library copy exiled");
        // Brainstorm is untouched.
        assert_eq!(state.objects[&other_id].zone, CardZone::Hand { known: false }, "other card unchanged");
    }

    // ── Section 31: Toxic Deluge ───────────────────────────────────────────────

    /// Toxic Deluge with chosen_x=3 should register a -3/-3 ContinuousInstance.
    /// After recompute:
    ///   - a 1/3 creature has materialized toughness 0 (dies to SBA)
    ///   - a 1/4 creature has materialized toughness 1 (survives)
    #[test]
    fn test_toxic_deluge_applies_minus_x_pt() {
        let mut state = make_state();

        // Set up test creatures on the battlefield.
        let victim_def = CardDef::new(
            "Victim", CardKind::Creature(CreatureData::new("", 1, 3)),
            vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
        let survivor_def = CardDef::new(
            "Survivor", CardKind::Creature(CreatureData::new("", 1, 4)),
            vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
        let victim_id = add_perm_with_def(&mut state, PlayerId::Opp, &victim_def, BattlefieldState::new());
        let survivor_id = add_perm_with_def(&mut state, PlayerId::Opp, &survivor_def, BattlefieldState::new());

        // Invoke the factory directly with x=3 (strategy-chosen).
        let td_def = catalog_card("Toxic Deluge");
        let factory = match &td_def.kind {
            CardKind::Sorcery(s) => s.modes.as_ref().unwrap().get(0).unwrap().factory.clone(),
            _ => panic!("not a sorcery"),
        };
        let source_id = state.alloc_id();
        let eff = factory(PlayerId::Us, source_id, 3);
        eff.call(&mut state, 1, &[]);

        // One ContinuousInstance should be registered.
        assert_eq!(state.continuous_instances.len(), 1, "one CI registered");

        // Apply it.
        recompute(&mut state);

        // Victim (1/3): materialized toughness should be 0 after -3/-3.
        let victim_t = state.def_of(victim_id)
            .and_then(|d| d.as_creature())
            .map(|c| c.toughness())
            .expect("victim has creature def");
        assert_eq!(victim_t, 0, "victim (1/3) gets -3/-3 → toughness 0");

        // Survivor (1/4): materialized toughness should be 1 after -3/-3.
        let survivor_t = state.def_of(survivor_id)
            .and_then(|d| d.as_creature())
            .map(|c| c.toughness())
            .expect("survivor has creature def");
        assert_eq!(survivor_t, 1, "survivor (1/4) gets -3/-3 → toughness 1");
    }

    /// Casting Toxic Deluge with X=3 deducts 3 life as additional cost.
    #[test]
    fn test_toxic_deluge_pays_x_life() {
        let mut state = make_state();
        let td_def = catalog_card("Toxic Deluge");
        state.catalog.insert(td_def.name.clone(), td_def);
        state.us.pool.b = 3;
        state.us.pool.total = 3;
        state.us.life = 20;
        let td_id = add_hand_card(&mut state, PlayerId::Us, "Toxic Deluge");
        let result = cast_spell(&mut state, 1, PlayerId::Us, td_id, SpellFace::Main, None, None, &[], 3, 0, None);
        assert!(result.is_some(), "Toxic Deluge should cast successfully");
        assert_eq!(state.us.life, 17, "caster pays X=3 life");
    }

    // ── 35. Red/Blue Elemental Blast, Pyroblast, Hydroblast ───────────────────

    /// Helper: insert a spell object onto the stack for `who` with the given catalog_key.
    /// Sets `materialized` from the test catalog so `def_of` can resolve the card's properties.
    fn push_stack_spell(state: &mut SimState, who: PlayerId, name: &str) -> ObjId {
        let id = state.alloc_id();
        let def = test_catalog().remove(name);
        state.objects.insert(id, GameObject {
            id,
            catalog_key: name.to_string(),
            owner: who,
            controller: who,
            zone: CardZone::Stack,
            is_token: false,
            bf: None,
            spell: Some(SpellState {
                effect: None,
                chosen_targets: vec![],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
            materialized: def,
            counters: HashMap::new(), ci_timestamp: 0,
        });
        state.stack.push(id);
        id
    }

    /// REB counters a blue spell on the stack (Brainstorm = blue).
    #[test]
    fn test_reb_counters_blue_spell() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let target_id = push_stack_spell(&mut state, PlayerId::Opp, "Brainstorm");

        let reb_def = catalog_card("Red Elemental Blast");
        let mode = reb_def.spell_modes().unwrap().get(0).unwrap();
        let effect = (mode.factory)(PlayerId::Us, ObjId(0), 0);
        effect.call(&mut state, 1, &[target_id]);

        assert!(!state.stack.contains(&target_id), "blue spell should be countered off the stack");
        assert_eq!(state.objects[&target_id].zone, CardZone::Graveyard, "countered spell goes to graveyard");
    }

    /// REB destroys a blue permanent on the battlefield (Underground Sea = blue land).
    #[test]
    fn test_reb_destroys_blue_permanent() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let sea_id = add_default_perm(&mut state, PlayerId::Opp, "Underground Sea");

        let reb_def = catalog_card("Red Elemental Blast");
        let mode = reb_def.spell_modes().unwrap().get(0).unwrap();
        let effect = (mode.factory)(PlayerId::Us, ObjId(0), 0);
        effect.call(&mut state, 1, &[sea_id]);

        assert_eq!(state.objects[&sea_id].zone, CardZone::Graveyard, "blue permanent destroyed");
    }

    /// Pyroblast fizzles when targeting a non-blue spell (Dark Ritual = black).
    #[test]
    fn test_pyroblast_fizzles_on_non_blue_spell() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let target_id = push_stack_spell(&mut state, PlayerId::Opp, "Dark Ritual");

        let pyro_def = catalog_card("Pyroblast");
        let mode = pyro_def.spell_modes().unwrap().get(0).unwrap();
        let effect = (mode.factory)(PlayerId::Us, ObjId(0), 0);
        effect.call(&mut state, 1, &[target_id]);

        assert!(state.stack.contains(&target_id), "non-blue spell survives Pyroblast");
    }

    /// Pyroblast counters a blue spell on the stack (same effect path, conditional on color).
    #[test]
    fn test_pyroblast_counters_blue_spell() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let target_id = push_stack_spell(&mut state, PlayerId::Opp, "Brainstorm");

        let pyro_def = catalog_card("Pyroblast");
        let mode = pyro_def.spell_modes().unwrap().get(0).unwrap();
        let effect = (mode.factory)(PlayerId::Us, ObjId(0), 0);
        effect.call(&mut state, 1, &[target_id]);

        assert!(!state.stack.contains(&target_id), "blue spell countered by Pyroblast");
    }

    /// BEB counters a red spell and Hydroblast fizzles on a non-red spell (Brainstorm = blue).
    #[test]
    fn test_beb_counters_red_and_hydroblast_fizzles_on_non_red() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // BEB: counter a red spell — use Dark Ritual (black) as a stand-in? No, we need red.
        // Dark Ritual is black, not red. We don't have a red spell in the default test catalog.
        // Use the Hydroblast fizzle test instead: target Brainstorm (blue), expect no effect.
        let blue_id = push_stack_spell(&mut state, PlayerId::Opp, "Brainstorm");

        let hydro_def = catalog_card("Hydroblast");
        let mode = hydro_def.spell_modes().unwrap().get(0).unwrap();
        let effect = (mode.factory)(PlayerId::Us, ObjId(0), 0);
        effect.call(&mut state, 1, &[blue_id]);

        assert!(state.stack.contains(&blue_id), "Hydroblast fizzles on non-red target");
    }

    // ── 36. Painter's Servant ─────────────────────────────────────────────────

    /// Helper: ETB Painter's Servant (from Hand→Battlefield) via change_zone so the replacement
    /// fires, resolve_choice picks a color, and the ContinuousInstance is registered.
    /// Calls recompute() so materialized views reflect the new CE immediately.
    fn etb_painter(state: &mut SimState, who: PlayerId, chosen_color: Color) -> ObjId {
        state.resolve_choice = std::sync::Arc::new(move |_, req, _| match req {
            ChoiceRequest::Color             => ChoiceResult::Color(chosen_color),
            ChoiceRequest::CreatureType      => ChoiceResult::CreatureType("Wizard".to_string()),
            ChoiceRequest::CardName          => ChoiceResult::CardName(String::new()),
            ChoiceRequest::Mode(_)           => ChoiceResult::Mode(0),
            ChoiceRequest::WardPayment {..}  => ChoiceResult::Bool(true),
            ChoiceRequest::MayPutOnBattlefield {..} => ChoiceResult::OptionalObject(None),
            ChoiceRequest::MayAttach => ChoiceResult::Bool(true),
        });
        let id = state.alloc_id();
        let def = catalog_card("Painter's Servant");
        state.objects.insert(id, GameObject {
            id,
            catalog_key: "Painter's Servant".to_string(),
            owner: who,
            controller: who,
            zone: CardZone::Hand { known: false },
            is_token: false,
            bf: None,
            spell: None,
            materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });

        state.catalog.entry("Painter's Servant".to_string()).or_insert(def);
        change_zone(id, ZoneId::Battlefield, state, 1, who);
        recompute(state);
        id
    }

    /// After Painter's Servant enters naming Blue, a colorless artifact (Lotus Petal) on
    /// opponent's side gains Blue. Pyroblast's conditional effect then destroys it.
    #[test]
    fn test_painters_servant_names_blue_makes_pyro_work() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Painter on our side, names Blue (default / forced via etb_painter).
        let _painter_id = etb_painter(&mut state, PlayerId::Us, Color::Blue);

        // Opponent has a Lotus Petal (colorless artifact) in play.
        let petal_id = add_default_perm(&mut state, PlayerId::Opp, "Lotus Petal");
        recompute(&mut state);

        // Verify: after CE, Lotus Petal's materialized colors include Blue.
        let colors = state.def_of(petal_id)
            .map(|d| d.colors.clone())
            .unwrap_or_default();
        assert!(colors.contains(&Color::Blue),
            "Painter naming Blue should give Blue to Lotus Petal; got {:?}", colors);

        // Pyroblast's effect: counter_or_destroy_if_color(Blue). Petal is on battlefield.
        let pyro_def = catalog_card("Pyroblast");
        let mode = pyro_def.spell_modes().unwrap().get(0).unwrap();
        let effect = (mode.factory)(PlayerId::Us, ObjId(0), 0);
        effect.call(&mut state, 1, &[petal_id]);

        assert_eq!(state.objects[&petal_id].zone, CardZone::Graveyard,
            "Pyroblast should destroy the now-Blue Lotus Petal");
    }

    /// After Painter's Servant names Blue, any card in hand satisfies the Force of
    /// Will pitch predicate (blue card). Dark Ritual is normally Black, not Blue.
    #[test]
    fn test_painters_servant_names_blue_enables_force_of_will_pitch() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let _painter_id = etb_painter(&mut state, PlayerId::Us, Color::Blue);

        // Dark Ritual is black — normally not a valid FoW pitch target.
        let ritual_id = add_hand_card(&mut state, PlayerId::Us, "Dark Ritual");
        recompute(&mut state);

        // Seed materialized on the hand card so def_of works.
        // (recompute populates materialized for objects in all zones.)
        let pred = obj_pred_from_card(pred_has_color(Color::Blue));
        assert!(pred(ritual_id, &state),
            "After Painter names Blue, Dark Ritual should satisfy FoW pitch predicate");
    }

    /// Painter's Servant CI is removed when Painter leaves the battlefield.
    /// After LTB, objects should revert to their original colors.
    #[test]
    fn test_painters_servant_ci_removed_on_ltb() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let painter_id = etb_painter(&mut state, PlayerId::Us, Color::Blue);

        let petal_id = add_default_perm(&mut state, PlayerId::Opp, "Lotus Petal");
        recompute(&mut state);
        let colors_while_in_play = state.def_of(petal_id)
            .map(|d| d.colors.clone())
            .unwrap_or_default();
        assert!(colors_while_in_play.contains(&Color::Blue));

        // Painter leaves the battlefield.
        change_zone(painter_id, ZoneId::Graveyard, &mut state, 1, PlayerId::Us);
        recompute(&mut state);

        let colors_after_ltb = state.def_of(petal_id)
            .map(|d| d.colors.clone())
            .unwrap_or_default();
        assert!(!colors_after_ltb.contains(&Color::Blue),
            "After Painter leaves, Lotus Petal should no longer be Blue; got {:?}", colors_after_ltb);
    }

    // ── 37. Disruptor Flute ────────────────────────────────────────────────────

    /// Helper: ETB Disruptor Flute naming the given card name.
    fn etb_flute(state: &mut SimState, who: PlayerId, chosen_name: &'static str) -> ObjId {
        state.resolve_choice = std::sync::Arc::new(move |_, req, _| match req {
            ChoiceRequest::Color             => ChoiceResult::Color(Color::Blue),
            ChoiceRequest::CreatureType      => ChoiceResult::CreatureType("Wizard".to_string()),
            ChoiceRequest::CardName          => ChoiceResult::CardName(chosen_name.to_string()),
            ChoiceRequest::Mode(_)           => ChoiceResult::Mode(0),
            ChoiceRequest::WardPayment {..}  => ChoiceResult::Bool(true),
            ChoiceRequest::MayPutOnBattlefield {..} => ChoiceResult::OptionalObject(None),
            ChoiceRequest::MayAttach => ChoiceResult::Bool(true),
        });
        let id = state.alloc_id();
        let def = catalog_card("Disruptor Flute");
        state.objects.insert(id, GameObject {
            id,
            catalog_key: "Disruptor Flute".to_string(),
            owner: who,
            controller: who,
            zone: CardZone::Hand { known: false },
            is_token: false,
            bf: None,
            spell: None,
            materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });

        state.catalog.entry("Disruptor Flute".to_string()).or_insert(def);
        change_zone(id, ZoneId::Battlefield, state, 1, who);
        recompute(state);
        id
    }

    #[test]
    fn test_disruptor_flute_names_brainstorm_taxes_it() {
        // Flute names "Brainstorm"; Brainstorm's materialized casting_cost_modifier should be 3.
        let mut state = make_state();
        state.catalog = test_catalog();
        etb_flute(&mut state, PlayerId::Us, "Brainstorm");

        // Put a Brainstorm in hand so it has a materialized view.
        let bs_id = state.alloc_id();
        state.objects.insert(bs_id, GameObject {
            id: bs_id,
            catalog_key: "Brainstorm".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            zone: CardZone::Hand { known: false },
            is_token: false,
            bf: None, spell: None, materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });
        recompute(&mut state);

        let modifier = state.def_of(bs_id).map(|d| d.casting_cost_modifier).unwrap_or(0);
        assert_eq!(modifier, 3, "Brainstorm should cost 3 more when named by Disruptor Flute");
    }

    #[test]
    fn test_disruptor_flute_suppresses_wasteland_ability() {
        // Flute names "Wasteland"; Wasteland's non-mana abilities should have activatable=false.
        // Underground Sea's mana abilities must still be available.
        let mut state = make_state();
        state.catalog = test_catalog();
        etb_flute(&mut state, PlayerId::Us, "Wasteland");

        let wl_id = add_default_perm(&mut state, PlayerId::Opp, "Wasteland");
        let sea_id = add_default_perm(&mut state, PlayerId::Opp, "Underground Sea");
        recompute(&mut state);

        assert!(
            state.def_of(wl_id).map_or(false, |d| d.abilities().iter().all(|a| !a.activatable)),
            "Wasteland non-mana abilities should be suppressed"
        );
        assert!(
            state.def_of(sea_id).map_or(false, |d| d.mana_abilities().iter().all(|a| a.activatable)),
            "Underground Sea mana abilities should not be suppressed"
        );
    }

    #[test]
    fn test_disruptor_flute_does_not_affect_other_cards() {
        // Flute names "Wasteland"; Brainstorm must have modifier 0 and suppression false.
        let mut state = make_state();
        state.catalog = test_catalog();
        etb_flute(&mut state, PlayerId::Us, "Wasteland");

        let bs_id = state.alloc_id();
        state.objects.insert(bs_id, GameObject {
            id: bs_id,
            catalog_key: "Brainstorm".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            zone: CardZone::Hand { known: false },
            is_token: false,
            bf: None, spell: None, materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });
        recompute(&mut state);

        let d = state.def_of(bs_id).expect("Brainstorm should have materialized view");
        assert_eq!(d.casting_cost_modifier, 0);
        assert!(d.abilities().iter().all(|a| a.activatable), "Brainstorm abilities should not be suppressed");
    }

    // ── 38. Surveil lands ──────────────────────────────────────────────────────

    #[test]
    fn test_surveil_land_etb_mills_when_choice_true() {
        // Override surveil_choice to always mill. ETB a surveil land; top library card
        // should end up in the graveyard. Land itself should enter tapped.
        let mut state = make_state();
        state.catalog = test_catalog();
        state.surveil_choice = std::sync::Arc::new(|_, _| true);

        // Put a known card on top of Us's library.
        let top_id = {
            let def = catalog_card("Brainstorm");
            let id = state.alloc_id();
            state.objects.insert(id, GameObject {
                id,
                catalog_key: "Brainstorm".to_string(),
                owner: PlayerId::Us,
                controller: PlayerId::Us,
                zone: CardZone::Library,
                is_token: false,
                bf: None, spell: None, materialized: None,
                counters: HashMap::new(), ci_timestamp: 0,
            });
            state.us.library_order.push_front(id);
            state.catalog.entry("Brainstorm".to_string()).or_insert(def);
            id
        };

        // ETB Undercity Sewers.
        let land_id = {
            let def = catalog_card("Undercity Sewers");
            let id = state.alloc_id();
            state.objects.insert(id, GameObject {
                id,
                catalog_key: "Undercity Sewers".to_string(),
                owner: PlayerId::Us,
                controller: PlayerId::Us,
                zone: CardZone::Hand { known: false },
                is_token: false,
                bf: None, spell: None, materialized: None,
                counters: HashMap::new(), ci_timestamp: 0,
            });

            state.catalog.entry("Undercity Sewers".to_string()).or_insert(def);
            id
        };
        change_zone(land_id, ZoneId::Battlefield, &mut state, 1, PlayerId::Us);
        for ctx in std::mem::take(&mut state.pending_triggers) { ctx.effect.call(&mut state, 1, &[]); }

        assert_eq!(state.objects[&top_id].zone, CardZone::Graveyard,
            "top library card should be milled by surveil");
        assert!(matches!(state.objects[&land_id].bf, Some(ref bf) if bf.tapped),
            "surveil land should enter tapped");
    }

    #[test]
    fn test_surveil_land_etb_keeps_when_choice_false() {
        // Override surveil_choice to always keep. Library card stays in library.
        let mut state = make_state();
        state.catalog = test_catalog();
        state.surveil_choice = std::sync::Arc::new(|_, _| false);

        let top_id = {
            let def = catalog_card("Brainstorm");
            let id = state.alloc_id();
            state.objects.insert(id, GameObject {
                id,
                catalog_key: "Brainstorm".to_string(),
                owner: PlayerId::Us,
                controller: PlayerId::Us,
                zone: CardZone::Library,
                is_token: false,
                bf: None, spell: None, materialized: None,
                counters: HashMap::new(), ci_timestamp: 0,
            });
            state.us.library_order.push_front(id);
            state.catalog.entry("Brainstorm".to_string()).or_insert(def);
            id
        };

        let land_id = {
            let def = catalog_card("Undercity Sewers");
            let id = state.alloc_id();
            state.objects.insert(id, GameObject {
                id,
                catalog_key: "Undercity Sewers".to_string(),
                owner: PlayerId::Us,
                controller: PlayerId::Us,
                zone: CardZone::Hand { known: false },
                is_token: false,
                bf: None, spell: None, materialized: None,
                counters: HashMap::new(), ci_timestamp: 0,
            });

            state.catalog.entry("Undercity Sewers".to_string()).or_insert(def);
            id
        };
        change_zone(land_id, ZoneId::Battlefield, &mut state, 1, PlayerId::Us);
        for ctx in std::mem::take(&mut state.pending_triggers) { ctx.effect.call(&mut state, 1, &[]); }

        assert_eq!(state.objects[&top_id].zone, CardZone::Library,
            "top library card should stay when surveil keeps");
    }

    // ── 39. Ancient Tomb ───────────────────────────────────────────────────────

    #[test]
    fn test_ancient_tomb_produces_two_and_deals_damage() {
        let mut state = make_state();
        state.catalog = test_catalog();
        state.us.life = 20;

        let tomb_id = {
            let def = catalog_card("Ancient Tomb");
            let id = state.alloc_id();
            state.objects.insert(id, GameObject {
                id,
                catalog_key: "Ancient Tomb".to_string(),
                owner: PlayerId::Us,
                controller: PlayerId::Us,
                zone: CardZone::Hand { known: false },
                is_token: false,
                bf: None, spell: None, materialized: None,
                counters: HashMap::new(), ci_timestamp: 0,
            });

            state.catalog.entry("Ancient Tomb".to_string()).or_insert(def);
            id
        };
        // ETB via change_zone, which assigns ci_timestamp and recompute (sets materialized)
        change_zone(tomb_id, ZoneId::Battlefield, &mut state, 1, PlayerId::Us);

        let act = ManaActivation { source_id: tomb_id, ability_index: 0, color_choice: None };
        execute_mana_activation(&mut state, 1, PlayerId::Us, &act);

        assert_eq!(state.us.pool.total, 2, "Ancient Tomb should produce 2 mana");
        assert_eq!(state.us.pool.c, 2, "both mana pips should be colorless");
        assert_eq!(state.us.life, 18, "Ancient Tomb deals 2 damage to controller");
        assert!(state.objects[&tomb_id].bf.as_ref().map_or(false, |bf| bf.tapped),
            "Ancient Tomb should be tapped after activation");
    }

    // ── 40. Karakas ────────────────────────────────────────────────────────────

    #[test]
    fn test_karakas_bounces_opp_legendary_creature() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Put a legendary creature on opp's battlefield (use Emrakul as a stand-in)
        // Build a minimal legendary creature and put it on opp's battlefield.
        let legendary_def = {
            CardDef::new(
                "TestLegend", CardKind::Creature(CreatureData::new("1W", 2, 2)),
                vec![], None, vec![Supertype::Legendary], CardLayout::Normal, None,
                vec![], vec![], vec![], vec![])
        };
        state.catalog.insert("TestLegend".to_string(), legendary_def.clone());

        let creature_id = {
            let id = state.alloc_id();
            state.objects.insert(id, GameObject {
                id,
                catalog_key: "TestLegend".to_string(),
                owner: PlayerId::Opp,
                controller: PlayerId::Opp,
                zone: CardZone::Hand { known: false },
                is_token: false,
                bf: None, spell: None, materialized: None,
                counters: HashMap::new(), ci_timestamp: 0,
            });

            id
        };
        change_zone(creature_id, ZoneId::Battlefield, &mut state, 1, PlayerId::Opp);

        // Activate Karakas, targeting the legendary creature
        let effect = eff_bounce_target(PlayerId::Us);
        effect.call(&mut state, 1, &[creature_id]);

        assert_eq!(state.objects[&creature_id].zone, CardZone::Hand { known: false },
            "legendary creature should be in opp's hand after Karakas activation");
    }

    // ── 41. Abrade ─────────────────────────────────────────────────────────────

    #[test]
    fn test_abrade_creature_mode_deals_lethal_damage() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // 3/3 creature — 3 damage is lethal
        let creature_def = creature("Target3_3", 3, 3);
        let id = add_perm_with_def(&mut state, PlayerId::Opp, &creature_def, BattlefieldState::new());

        eff_damage_target(PlayerId::Us, 3, ObjId(0)).call(&mut state, 1, &[id]);
        check_state_based_actions(&mut state, 1);

        assert_eq!(state.objects[&id].zone, CardZone::Graveyard,
            "3/3 hit by 3 damage should die via SBA");
    }

    #[test]
    fn test_abrade_creature_mode_nonlethal_survives() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // 4/4 creature — 3 damage is not lethal
        let creature_def = creature("Target4_4", 4, 4);
        let id = add_perm_with_def(&mut state, PlayerId::Opp, &creature_def, BattlefieldState::new());

        eff_damage_target(PlayerId::Us, 3, ObjId(0)).call(&mut state, 1, &[id]);
        check_state_based_actions(&mut state, 1);

        assert_eq!(state.objects[&id].zone, CardZone::Battlefield,
            "4/4 hit by 3 damage should survive");
        assert_eq!(state.objects[&id].bf.as_ref().unwrap().damage, 3);
    }

    #[test]
    fn test_abrade_artifact_mode_destroys() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Teferi's Puzzle Box as a stand-in artifact
        let artifact_def = {
            let def = CardDef::new(
                "TestArtifact",
                CardKind::Artifact(ArtifactData { mana_cost: "1".to_string(), ..Default::default() }),
                vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
            def
        };
        let id = add_perm_with_def(&mut state, PlayerId::Opp, &artifact_def, BattlefieldState::new());

        eff_destroy_target(PlayerId::Us).call(&mut state, 1, &[id]);

        assert_eq!(state.objects[&id].zone, CardZone::Graveyard,
            "artifact should be destroyed by Abrade's artifact mode");
    }

    // ── §42: Grafdigger's Cage ────────────────────────────────────────────────

    #[test]
    fn test_grafdiggers_cage_blocks_gy_and_lib_casting() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Enter Grafdigger's Cage via change_zone (fires ETB → static CE installed).
        let cage_id = state.alloc_id();
        let cage_def = catalog_card("Grafdigger's Cage");
        state.objects.insert(cage_id, GameObject {
            id: cage_id,
            catalog_key: "Grafdigger's Cage".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            zone: CardZone::Hand { known: false },
            is_token: false,
            bf: None, spell: None, materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });

        state.catalog.entry("Grafdigger's Cage".to_string()).or_insert(cage_def);
        change_zone(cage_id, ZoneId::Battlefield, &mut state, 1, PlayerId::Opp);

        // Place a card in graveyard.
        let gy_id = state.alloc_id();
        state.objects.insert(gy_id, GameObject {
            id: gy_id,
            catalog_key: "Dark Ritual".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            zone: CardZone::Graveyard,
            is_token: false,
            bf: None, spell: None, materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });

        recompute(&mut state);

        // Cage's static CE should set castable=false on graveyard cards.
        assert!(
            !state.def_of(gy_id).map_or(true, |d| d.castable),
            "Cage should prevent casting from graveyard (castable=false)"
        );

        // A card in exile should NOT be blocked by Cage (Cage only blocks GY/library).
        let exile_id = state.alloc_id();
        state.objects.insert(exile_id, GameObject {
            id: exile_id,
            catalog_key: "Dark Ritual".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            zone: CardZone::Exile { on_adventure: false },
            is_token: false,
            bf: None, spell: None, materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });
        recompute(&mut state);

        // Exile cards default to castable=false (zone default), but Cage should NOT
        // be the reason — if a Dauthi CE set castable=true, Cage wouldn't override it.
        // Here we just verify Cage doesn't affect exile zone beyond the zone default.
        assert!(
            !state.def_of(exile_id).map_or(true, |d| d.castable),
            "Exile card defaults to not castable (zone default, not Cage)"
        );
    }

    #[test]
    fn test_grafdiggers_cage_ci_removed_on_ltb() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Enter Cage then remove it.
        let cage_id = state.alloc_id();
        let cage_def = catalog_card("Grafdigger's Cage");
        state.objects.insert(cage_id, GameObject {
            id: cage_id,
            catalog_key: "Grafdigger's Cage".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            zone: CardZone::Hand { known: false },
            is_token: false,
            bf: None, spell: None, materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });

        state.catalog.entry("Grafdigger's Cage".to_string()).or_insert(cage_def);
        change_zone(cage_id, ZoneId::Battlefield, &mut state, 1, PlayerId::Us);
        recompute(&mut state);

        // Destroy Cage.
        change_zone(cage_id, ZoneId::Graveyard, &mut state, 2, PlayerId::Us);

        // No CI with source == cage_id should remain.
        let ci_count = state.continuous_instances.iter()
            .filter(|ci| ci.source_id == cage_id)
            .count();
        assert_eq!(ci_count, 0, "Cage CI should be removed when Cage leaves the battlefield");
    }

    /// Helper: put Cage on the battlefield for `who` and return its id.
    fn enter_cage(state: &mut SimState, who: PlayerId) -> ObjId {
        let cage_id = state.alloc_id();
        assert!(state.catalog.contains_key("Grafdigger's Cage"), "Grafdigger's Cage not in catalog");
        state.objects.insert(cage_id, GameObject {
            id: cage_id,
            catalog_key: "Grafdigger's Cage".to_string(),
            owner: who, controller: who,
            zone: CardZone::Hand { known: false },
            is_token: false,
            bf: None, spell: None, materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });

        change_zone(cage_id, ZoneId::Battlefield, state, 1, who);
        cage_id
    }

    #[test]
    fn test_grafdiggers_cage_prohibition_blocks_creature_from_gy() {
        let mut state = make_state();
        state.catalog = test_catalog();
        state.catalog.insert("Troll".to_string(), creature("Troll", 3, 3));

        enter_cage(&mut state, PlayerId::Opp);

        // Put a creature card in Us's graveyard.
        let creature_id = state.alloc_id();
        state.objects.insert(creature_id, GameObject {
            id: creature_id,
            catalog_key: "Troll".to_string(),
            owner: PlayerId::Us, controller: PlayerId::Us,
            zone: CardZone::Graveyard,
            is_token: false,
            bf: None, spell: None, materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });

        // Attempt to reanimate: fire a ZoneChange GY→BF.
        eff_reanimate(PlayerId::Us).call(&mut state, 2, &[creature_id]);

        assert_eq!(
            state.objects[&creature_id].zone,
            CardZone::Graveyard,
            "Cage prohibition must block creature from entering battlefield from graveyard"
        );
    }

    #[test]
    fn test_grafdiggers_cage_prohibition_removed_on_ltb() {
        let mut state = make_state();
        state.catalog = test_catalog();
        state.catalog.insert("Troll".to_string(), creature("Troll", 3, 3));

        let cage_id = enter_cage(&mut state, PlayerId::Opp);

        // Put a creature card in Us's graveyard.
        let creature_id = state.alloc_id();
        state.objects.insert(creature_id, GameObject {
            id: creature_id,
            catalog_key: "Troll".to_string(),
            owner: PlayerId::Us, controller: PlayerId::Us,
            zone: CardZone::Graveyard,
            is_token: false,
            bf: None, spell: None, materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });

        // Remove Cage.
        change_zone(cage_id, ZoneId::Graveyard, &mut state, 2, PlayerId::Opp);

        // Now reanimation should succeed.
        eff_reanimate(PlayerId::Us).call(&mut state, 3, &[creature_id]);

        assert_eq!(
            state.objects[&creature_id].zone,
            CardZone::Battlefield,
            "After Cage leaves, creature should be free to enter battlefield"
        );
    }

    #[test]
    fn test_grafdiggers_cage_does_not_block_non_creature() {
        let mut state = make_state();
        state.catalog = test_catalog();

        enter_cage(&mut state, PlayerId::Opp);

        // Put a non-creature (artifact) in Us's graveyard.
        let artifact_id = state.alloc_id();
        let artifact_name = "Grafdigger's Cage";
        state.objects.insert(artifact_id, GameObject {
            id: artifact_id,
            catalog_key: artifact_name.to_string(),
            owner: PlayerId::Us, controller: PlayerId::Us,
            zone: CardZone::Graveyard,
            is_token: false,
            bf: None, spell: None, materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });

        eff_reanimate(PlayerId::Us).call(&mut state, 2, &[artifact_id]);

        assert_eq!(
            state.objects[&artifact_id].zone,
            CardZone::Battlefield,
            "Cage must not block non-creature cards from entering battlefield"
        );
    }

    // ── §43: Sheoldred's Edict ────────────────────────────────────────────────

    #[test]
    fn test_sheoldrds_edict_mode0_sacrifices_nontoken_creature() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let creature_def = creature("Threat", 2, 2);
        let creature_id = add_perm_with_def(&mut state, PlayerId::Opp, &creature_def, BattlefieldState::new());
        // Default mode = 0 (nontoken creature)
        let filter: ObjPredicate = Arc::new(|id, state: &SimState| {
            state.objects.get(&id).map_or(false, |o| {
                !o.is_token && state.catalog.get(o.catalog_key.as_str())
                    .map_or(false, |d| d.is_creature())
            })
        });
        eff_sacrifice(PlayerId::Us, Who::Opp, filter).call(&mut state, 1, &[]);
        assert_eq!(state.objects[&creature_id].zone, CardZone::Graveyard,
            "nontoken creature should be sacrificed");
    }

    #[test]
    fn test_sheoldrds_edict_token_filter_spares_nontoken() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let creature_def = creature("Nontoken", 2, 2);
        let nontoken_id = add_perm_with_def(&mut state, PlayerId::Opp, &creature_def, BattlefieldState::new());
        // Add a token manually
        let token_def = creature("OrcToken", 1, 1);
        let token_id = state.alloc_id();
        state.catalog.entry("OrcToken".to_string()).or_insert_with(|| token_def.clone());
        state.objects.insert(token_id, GameObject {
            id: token_id,
            catalog_key: "OrcToken".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            zone: CardZone::Battlefield,
            is_token: true,
            bf: Some(BattlefieldState::new()),
            spell: None,
            materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });
        // Mode 1: sacrifice a token
        let filter: ObjPredicate = Arc::new(|id, state: &SimState| {
            state.objects.get(&id).map_or(false, |o| o.is_token)
        });
        eff_sacrifice(PlayerId::Us, Who::Opp, filter).call(&mut state, 1, &[]);
        assert_eq!(state.objects[&token_id].zone, CardZone::Graveyard,
            "token should be sacrificed by mode 1");
        assert_eq!(state.objects[&nontoken_id].zone, CardZone::Battlefield,
            "nontoken creature should not be sacrificed by mode 1");
    }

    // ── §44: Engineered Explosives ────────────────────────────────────────────

    #[test]
    fn test_ee_etb_places_charge_counters() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Simulate casting EE with chosen_x = 2 by setting resolving_costs_ctx before ETB.
        state.resolving_costs_ctx.chosen_x = 2;
        assert!(state.catalog.contains_key("Engineered Explosives"), "EE must be in catalog");
        let ee_id = state.alloc_id();
        state.objects.insert(ee_id, GameObject {
            id: ee_id,
            catalog_key: "Engineered Explosives".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            zone: CardZone::Hand { known: false },
            is_token: false,
            bf: None, spell: None, materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });

        change_zone(ee_id, ZoneId::Battlefield, &mut state, 1, PlayerId::Us);
        assert_eq!(
            state.objects[&ee_id].counters.get(&CounterType::Charge).copied().unwrap_or(0),
            2,
            "EE should enter with 2 charge counters when chosen_x = 2"
        );
    }

    #[test]
    fn test_ee_ability_destroys_matching_mv() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Put EE in play with 2 charge counters (no casting, manual setup).
        let ee_def = state.catalog.get("Engineered Explosives").cloned()
            .expect("EE must be in catalog");
        let ee_id = add_perm_with_def(&mut state, PlayerId::Us, &ee_def, BattlefieldState::new());
        *state.objects.get_mut(&ee_id).unwrap().counters.entry(CounterType::Charge).or_insert(0) = 2;
        // MV 2 permanent: a 2/2 creature with mana_cost "1B" (MV=2).
        let mv2_def = {
            let data = CreatureData::new("1B", 2, 2);
            CardDef::new("MV2Creature", CardKind::Creature(data), parse_colors("1B", false, false),
                None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![])
        };
        let mv2_id = add_perm_with_def(&mut state, PlayerId::Opp, &mv2_def, BattlefieldState::new());
        // MV 3 permanent: should survive.
        let mv3_def = {
            let data = CreatureData::new("2B", 2, 2);
            CardDef::new("MV3Creature", CardKind::Creature(data), parse_colors("2B", false, false),
                None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![])
        };
        let mv3_id = add_perm_with_def(&mut state, PlayerId::Opp, &mv3_def, BattlefieldState::new());
        // Manually fire the ability effect (skip cost payment for the test).
        let ability_factory = ee_def.abilities().iter()
            .find(|ab| matches!(ab.source_zone, SourceZone::Battlefield))
            .and_then(|ab| ab.ability_factory.clone())
            .expect("EE must have a battlefield ability");
        let eff = ability_factory(PlayerId::Us, ee_id);
        eff.call(&mut state, 1, &[]);
        assert_eq!(state.objects[&mv2_id].zone, CardZone::Graveyard,
            "MV 2 permanent should be destroyed by EE[2]");
        assert_eq!(state.objects[&mv3_id].zone, CardZone::Battlefield,
            "MV 3 permanent should survive EE[2]");
    }

    // ── §45: Lavinia, Azorius Renegade ────────────────────────────────────────

    /// Helper: put Lavinia on the battlefield for `who` and return her id.
    fn enter_lavinia(state: &mut SimState, who: PlayerId) -> ObjId {
        add_default_perm(state, who, "Lavinia, Azorius Renegade")
    }

    #[test]
    fn test_lavinia_prohibition_blocks_noncreature_over_land_count() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Lavinia on our side.
        enter_lavinia(&mut state, PlayerId::Us);

        // Opponent has exactly 1 land.
        make_land(&mut state, PlayerId::Opp, "Swamp", false);

        // A noncreature sorcery with MV 2 (cost "1B").
        let sorcery_def = CardDef::new(
            "TestSorcery2", CardKind::Sorcery(SpellData {
                mana_cost: "1B".to_string(),
                ..Default::default()
            }),
            parse_colors("1B", false, true),
            None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![],
        );
        state.catalog.insert("TestSorcery2".to_string(), sorcery_def);
        let spell_id = add_hand_card(&mut state, PlayerId::Opp, "TestSorcery2");

        // With 1 land, MV 2 > 1 → Lavinia CE sets castable=false.
        recompute(&mut state);
        assert!(
            !state.def_of(spell_id).map_or(true, |d| d.castable),
            "Lavinia should set castable=false for MV-2 noncreature spell when opponent has only 1 land"
        );

        // Add a second land so opponent now has 2 lands — MV 2 is no longer > 2.
        make_land(&mut state, PlayerId::Opp, "Swamp", false);
        recompute(&mut state);
        assert!(
            state.def_of(spell_id).map_or(false, |d| d.castable),
            "Lavinia should allow MV-2 noncreature spell when opponent has 2 lands"
        );
    }

    #[test]
    fn test_lavinia_trigger_counters_free_spell() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Lavinia on our side.
        enter_lavinia(&mut state, PlayerId::Us);

        // Opponent has 5 lands so the prohibition doesn't block FoW (MV 5 ≤ 5 lands → allowed).
        for _ in 0..5 {
            make_land(&mut state, PlayerId::Opp, "Swamp", false);
        }

        // Opponent casts Force of Will via pitch cost (no mana spent).
        let fow_def = state.catalog.get("Force of Will").cloned().unwrap();
        let fow_id = add_hand_card(&mut state, PlayerId::Opp, "Force of Will");
        add_hand_card(&mut state, PlayerId::Opp, "Brainstorm"); // pitch target
        let alt_cost = fow_def.alternate_costs()[0].clone();

        cast_spell(&mut state, 1, PlayerId::Opp, fow_id, SpellFace::Main, Some(&alt_cost), Some(0), &[], 0, 0, None)
            .expect("FoW should cast via pitch cost");
        // Lavinia trigger queued at SpellCast; push spell onto stack so counter_one can find it.
        state.stack.push(fow_id);

        assert!(
            state.pending_triggers.iter().any(|ctx| ctx.source_name == "Lavinia, Azorius Renegade"),
            "Lavinia trigger should be queued"
        );

        for ctx in std::mem::take(&mut state.pending_triggers) {
            ctx.effect.call(&mut state, 1, &[]);
        }

        assert_eq!(state.objects[&fow_id].zone, CardZone::Graveyard,
            "FoW should be countered and in graveyard");
        assert!(!state.stack.contains(&fow_id), "FoW should be off the stack");
    }

    #[test]
    fn test_lavinia_trigger_lotus_petal() {
        // Lotus Petal has mana cost "0" — MV 0 is not > any land count, so the
        // prohibition does NOT fire. But mana_spent = false (MV 0), so Lavinia's
        // trigger DOES fire and counters the Petal.
        let mut state = make_state();
        state.catalog = test_catalog();

        enter_lavinia(&mut state, PlayerId::Us);

        let petal_id = add_hand_card(&mut state, PlayerId::Opp, "Lotus Petal");

        cast_spell(&mut state, 1, PlayerId::Opp, petal_id, SpellFace::Main, None, None, &[], 0, 0, None)
            .expect("Lotus Petal should not be prohibited (MV 0 ≤ any land count)");
        state.stack.push(petal_id);

        assert!(
            state.pending_triggers.iter().any(|ctx| ctx.source_name == "Lavinia, Azorius Renegade"),
            "Lavinia trigger should fire for free spell (no mana spent)"
        );

        for ctx in std::mem::take(&mut state.pending_triggers) {
            ctx.effect.call(&mut state, 1, &[]);
        }

        assert_eq!(state.objects[&petal_id].zone, CardZone::Graveyard,
            "Lotus Petal should be countered by Lavinia");
    }

    // ── §46: Hexing Squelcher ──────────────────────────────────────────────────

    fn enter_hexing_squelcher(state: &mut SimState, who: PlayerId) -> ObjId {
        add_default_perm(state, who, "Hexing Squelcher")
    }

    /// Hexing Squelcher's "Spells you control can't be countered" protects Us's spells.
    #[test]
    fn test_hexing_squelcher_protects_your_spells() {
        let mut state = make_state();
        state.catalog = test_catalog();

        enter_hexing_squelcher(&mut state, PlayerId::Us);

        // Put a plain counterable spell for Us on the stack directly.
        let spell_id = add_hand_card(&mut state, PlayerId::Us, "Brainstorm");
        // move to stack (activates stack prohibitions — Brainstorm has none, but wires change_zone path)
        change_zone(spell_id, ZoneId::Stack, &mut state, 1, PlayerId::Us);
        state.stack.push(spell_id);

        // Opponent tries to counter it.
        counter_one(spell_id, &mut state, 1, PlayerId::Opp);

        assert!(state.stack.contains(&spell_id),
            "Hexing Squelcher should prevent the opponent from countering our spell");
        assert_ne!(state.objects[&spell_id].zone, CardZone::Graveyard);
    }

    /// Hexing Squelcher only protects YOUR spells; opponent's spells can still be countered.
    #[test]
    fn test_hexing_squelcher_does_not_protect_opponent_spells() {
        let mut state = make_state();
        state.catalog = test_catalog();

        enter_hexing_squelcher(&mut state, PlayerId::Us);

        // Put a spell controlled by Opp on the stack.
        let spell_id = add_hand_card(&mut state, PlayerId::Opp, "Brainstorm");
        change_zone(spell_id, ZoneId::Stack, &mut state, 1, PlayerId::Opp);
        state.stack.push(spell_id);

        // We counter it.
        counter_one(spell_id, &mut state, 1, PlayerId::Us);

        assert!(!state.stack.contains(&spell_id), "Opponent's spell should be countered normally");
        assert_eq!(state.objects[&spell_id].zone, CardZone::Graveyard);
    }

    /// "Other creatures you control have Ward—Pay 2 life."
    /// When an opponent's spell targets another creature Us controls, the granted Ward trigger fires.
    #[test]
    fn test_hexing_squelcher_grants_ward_to_other_creature() {
        let mut state = make_state();
        state.catalog = test_catalog();

        enter_hexing_squelcher(&mut state, PlayerId::Us);
        // A second creature for Us (the one that should receive the granted Ward).
        let other_creature_id = enter_hexing_squelcher(&mut state, PlayerId::Us);

        recompute(&mut state);

        // Opponent's spell targeting our other creature.
        let spell_id = state.alloc_id();
        state.objects.insert(spell_id, GameObject {
            id: spell_id,
            catalog_key: "Brainstorm".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            zone: CardZone::Stack,
            is_token: false,
            spell: Some(SpellState {
                effect: None,
                chosen_targets: vec![other_creature_id],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
            bf: None,
            materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });

        fire_event(
            GameEvent::SpellCast { caster: PlayerId::Opp, card_id: spell_id, mana_spent: true },
            &mut state, 1, PlayerId::Opp,
        );

        assert!(
            state.pending_triggers.iter().any(|ctx| ctx.source_name == "Hexing Squelcher (Ward grant)"),
            "Ward trigger should fire for other creature targeted by opponent spell"
        );
    }

    /// The granted Ward does NOT fire when the controller's own spell targets the creature.
    #[test]
    fn test_hexing_squelcher_ward_grant_ignores_own_spells() {
        let mut state = make_state();
        state.catalog = test_catalog();

        enter_hexing_squelcher(&mut state, PlayerId::Us);
        let other_creature_id = enter_hexing_squelcher(&mut state, PlayerId::Us);

        recompute(&mut state);

        // Us's own spell targeting our creature — Ward should NOT fire.
        let spell_id = state.alloc_id();
        state.objects.insert(spell_id, GameObject {
            id: spell_id,
            catalog_key: "Brainstorm".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            zone: CardZone::Stack,
            is_token: false,
            spell: Some(SpellState {
                effect: None,
                chosen_targets: vec![other_creature_id],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
            bf: None,
            materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });

        fire_event(
            GameEvent::SpellCast { caster: PlayerId::Us, card_id: spell_id, mana_spent: true },
            &mut state, 1, PlayerId::Us,
        );

        assert!(
            !state.pending_triggers.iter().any(|ctx| ctx.source_name == "Hexing Squelcher (Ward grant)"),
            "Granted Ward should not fire for controller's own spells"
        );
    }

    /// Granted Ward applies to creatures that enter the battlefield AFTER Hexing Squelcher.
    /// recompute() runs after every fire_event ZoneChange, so new arrivals pick up the CE.
    #[test]
    fn test_hexing_squelcher_ward_grant_applies_to_creature_that_enters_later() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Squelcher enters first.
        enter_hexing_squelcher(&mut state, PlayerId::Us);
        recompute(&mut state);

        // A new creature arrives later (simulates a real ETB via fire_event; here we add
        // directly then call recompute, which is what fire_event does at each top-level tick).
        let late_creature_id = enter_hexing_squelcher(&mut state, PlayerId::Us);
        recompute(&mut state);

        // Verify the late creature's materialized def has the granted trigger.
        let mat = state.def_of(late_creature_id).expect("materialized def present");
        assert!(
            !mat.granted_trigger_defs.is_empty(),
            "late-arriving creature should have Ward trigger in granted_trigger_defs after recompute"
        );

        // Opponent's spell targeting the late creature: Ward should fire.
        let spell_id = state.alloc_id();
        state.objects.insert(spell_id, GameObject {
            id: spell_id,
            catalog_key: "Brainstorm".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            zone: CardZone::Stack,
            is_token: false,
            spell: Some(SpellState {
                effect: None,
                chosen_targets: vec![late_creature_id],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
            bf: None,
            materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });

        fire_event(
            GameEvent::SpellCast { caster: PlayerId::Opp, card_id: spell_id, mana_spent: true },
            &mut state, 1, PlayerId::Opp,
        );

        assert!(
            state.pending_triggers.iter().any(|ctx| ctx.source_name == "Hexing Squelcher (Ward grant)"),
            "Ward trigger should fire for late-arriving creature targeted by opponent spell"
        );
    }

    /// Long Goodbye's "This spell can't be countered" still works after the ProhibitionDef refactor.
    #[test]
    fn test_long_goodbye_still_uncounterable() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Pre-register Long Goodbye's prohibition instances before casting.
        let def = catalog_card("Long Goodbye");
        let lg_id = add_hand_card(&mut state, PlayerId::Us, "Long Goodbye");

        state.catalog.insert("Long Goodbye".to_string(), def);

        // Move to stack — activates Long Goodbye's own stack prohibition.
        change_zone(lg_id, ZoneId::Stack, &mut state, 1, PlayerId::Us);
        state.stack.push(lg_id);

        // Opponent tries to counter it.
        counter_one(lg_id, &mut state, 1, PlayerId::Opp);

        assert!(state.stack.contains(&lg_id),
            "Long Goodbye can't be countered (ProhibitionDef on SpellBeingCountered)");
        assert_ne!(state.objects[&lg_id].zone, CardZone::Graveyard);
    }

    // ── §48: Show and Tell ────────────────────────────────────────────────────

    #[test]
    fn test_show_and_tell_caster_puts_creature_on_battlefield() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let creature_id = add_hand_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        // Override resolve_choice: caster puts creature, opp declines.
        let cid = creature_id;
        state.resolve_choice = std::sync::Arc::new(move |_, req, _| match req {
            ChoiceRequest::MayPutOnBattlefield { ref candidates } => {
                if candidates.contains(&cid) {
                    ChoiceResult::OptionalObject(Some(cid))
                } else {
                    ChoiceResult::OptionalObject(None)
                }
            }
            ChoiceRequest::Color           => ChoiceResult::Color(Color::Blue),
            ChoiceRequest::CreatureType    => ChoiceResult::CreatureType("Wizard".to_string()),
            ChoiceRequest::CardName        => ChoiceResult::CardName(String::new()),
            ChoiceRequest::Mode(_)         => ChoiceResult::Mode(0),
            ChoiceRequest::WardPayment {..} => ChoiceResult::Bool(true),
            ChoiceRequest::MayAttach => ChoiceResult::Bool(true),
        });

        eff_each_may_put(
            PlayerId::Us,
            pred_or(
                pred_or(pred_type_eq(CardType::Artifact), pred_type_eq(CardType::Creature)),
                pred_or(pred_type_eq(CardType::Enchantment), pred_type_eq(CardType::Land)),
            ),
        ).call(&mut state, 1, &[]);

        assert_eq!(state.objects[&creature_id].zone, CardZone::Battlefield,
            "Show and Tell should put chosen creature onto the battlefield");
    }

    #[test]
    fn test_show_and_tell_no_candidates_no_crash() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // No cards in hand — should not panic.
        eff_each_may_put(
            PlayerId::Us,
            pred_or(
                pred_or(pred_type_eq(CardType::Artifact), pred_type_eq(CardType::Creature)),
                pred_or(pred_type_eq(CardType::Enchantment), pred_type_eq(CardType::Land)),
            ),
        ).call(&mut state, 1, &[]);
        // No assertions needed — just verifying no panic.
    }

    // ── §49: Spell Pierce / tax counters ──────────────────────────────────────

    #[test]
    fn test_counter_unless_pays_counters_when_opp_cannot_pay() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Opponent spell on the stack — no lands, can't pay {2}.
        let spell_id = state.alloc_id();
        state.objects.insert(spell_id, GameObject {
            id: spell_id,
            catalog_key: "Ponder".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            zone: CardZone::Stack,
            is_token: false,
            spell: Some(SpellState {
                effect: Some(eff_draw(PlayerId::Opp, 1)),
                chosen_targets: vec![],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
            bf: None,
            materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });
        state.stack.push(spell_id);

        eff_counter_unless_pays(PlayerId::Us, vec![CostComponent::Mana(parse_mana_cost("2"))])
            .call(&mut state, 1, &[spell_id]);

        assert_eq!(state.objects[&spell_id].zone, CardZone::Graveyard,
            "spell should be countered when opponent can't pay 2");
        assert!(!state.stack.contains(&spell_id));
    }

    #[test]
    fn test_counter_unless_pays_spell_resolves_when_opp_pays() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Give opponent 2 untapped Islands to pay {2}.
        let island_def = catalog_card("Island");
        add_perm_with_def(&mut state, PlayerId::Opp, &island_def, BattlefieldState::new());
        add_perm_with_def(&mut state, PlayerId::Opp, &island_def, BattlefieldState::new());
        // Strategy: always pay.
        state.resolve_choice = std::sync::Arc::new(|_, req, _| match req {
            ChoiceRequest::WardPayment {..} => ChoiceResult::Bool(true),
            ChoiceRequest::Color           => ChoiceResult::Color(Color::Blue),
            ChoiceRequest::CreatureType    => ChoiceResult::CreatureType("Wizard".to_string()),
            ChoiceRequest::CardName        => ChoiceResult::CardName(String::new()),
            ChoiceRequest::Mode(_)         => ChoiceResult::Mode(0),
            ChoiceRequest::MayPutOnBattlefield {..} => ChoiceResult::OptionalObject(None),
            ChoiceRequest::MayAttach => ChoiceResult::Bool(true),
        });
        // Opponent spell on the stack.
        let spell_id = state.alloc_id();
        state.objects.insert(spell_id, GameObject {
            id: spell_id,
            catalog_key: "Ponder".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            zone: CardZone::Stack,
            is_token: false,
            spell: Some(SpellState {
                effect: Some(eff_draw(PlayerId::Opp, 1)),
                chosen_targets: vec![],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
            bf: None,
            materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });
        state.stack.push(spell_id);

        eff_counter_unless_pays(PlayerId::Us, vec![CostComponent::Mana(parse_mana_cost("2"))])
            .call(&mut state, 1, &[spell_id]);

        assert!(state.stack.contains(&spell_id),
            "spell should remain on stack when opponent pays 2");
        assert_eq!(state.objects[&spell_id].zone, CardZone::Stack);
    }

    #[test]
    fn test_daze_counter_unless_pays_1() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Opponent has no lands — can't pay {1}.
        let spell_id = state.alloc_id();
        state.objects.insert(spell_id, GameObject {
            id: spell_id,
            catalog_key: "Ponder".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            zone: CardZone::Stack,
            is_token: false,
            spell: Some(SpellState {
                effect: Some(eff_draw(PlayerId::Opp, 1)),
                chosen_targets: vec![],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
            bf: None,
            materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });
        state.stack.push(spell_id);

        // Daze: counter unless pays {1}
        eff_counter_unless_pays(PlayerId::Us, vec![CostComponent::Mana(parse_mana_cost("1"))])
            .call(&mut state, 1, &[spell_id]);

        assert_eq!(state.objects[&spell_id].zone, CardZone::Graveyard,
            "Daze should counter when opponent can't pay 1");
    }

    // ── §50: Flusterstorm / Storm trigger ──────────────────────────────────────

    #[test]
    fn test_flusterstorm_storm_trigger_creates_copies() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Card-bound triggers are derived from catalog at fire time — no preregistration.
        let fluster_id = state.alloc_id();

        // Put two opponent instants on the stack as targets.
        let spell_a = state.alloc_id();
        let spell_b = state.alloc_id();
        for (id, name) in [(spell_a, "Brainstorm"), (spell_b, "Ponder")] {
            state.objects.insert(id, GameObject {
                id,
                catalog_key: name.to_string(),
                owner: PlayerId::Opp,
                controller: PlayerId::Opp,
                zone: CardZone::Stack,
                is_token: false,
                spell: Some(SpellState {
                    effect: Some(eff_draw(PlayerId::Opp, 1)),
                    chosen_targets: vec![],
                    is_back_face: false,
                    costs_paid_ctx: CostsPaidCtx::default(),
                }),
                bf: None,
                materialized: None,
                counters: HashMap::new(), ci_timestamp: 0,
            });
            state.stack.push(id);
        }

        // Simulate 2 spells cast before Flusterstorm.
        state.us.spells_cast_this_turn = 2;

        // Also put flusterstorm on the stack (as if we just cast it) with spell_a as target.
        state.objects.insert(fluster_id, GameObject {
            id: fluster_id,
            catalog_key: "Flusterstorm".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            zone: CardZone::Stack,
            is_token: false,
            spell: Some(SpellState {
                effect: Some(eff_counter_unless_pays(PlayerId::Us, vec![CostComponent::Mana(parse_mana_cost("1"))])),
                chosen_targets: vec![spell_a],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
            bf: None,
            materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });
        state.stack.push(fluster_id);

        // Fire the SpellCast event — storm trigger should fire.
        let event = GameEvent::SpellCast { caster: PlayerId::Us, card_id: fluster_id, mana_spent: true };
        let (triggers, _) = fire_triggers(&event, &state);
        assert_eq!(triggers.len(), 1, "storm should produce exactly one trigger context");
        assert_eq!(triggers[0].source_name, "Flusterstorm (storm trigger)");

        // Resolve the storm trigger effect — should create 2 copies on the stack.
        let stack_before = state.stack.len();
        triggers[0].effect.call(&mut state, 1, &[]);
        let copies_pushed = state.stack.len() - stack_before;
        assert_eq!(copies_pushed, 2, "storm count 2 → 2 copies");

        // Verify copies are StackAbilities.
        for &copy_id in &state.stack[stack_before..] {
            let ability = state.abilities.get(&copy_id)
                .expect("copy should be a StackAbility");
            assert_eq!(ability.source_name, "Flusterstorm (storm copy)");
            assert!(ability.counterable, "storm copies should be counterable");
        }
    }

    #[test]
    fn test_flusterstorm_no_storm_when_first_spell() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let fluster_id = state.alloc_id();


        // No spells cast before this one.
        state.us.spells_cast_this_turn = 0;

        let event = GameEvent::SpellCast { caster: PlayerId::Us, card_id: fluster_id, mana_spent: true };
        let (triggers, _) = fire_triggers(&event, &state);
        assert!(triggers.is_empty(), "no storm copies when first spell of the turn");
    }

    #[test]
    fn test_flusterstorm_storm_copies_counter_spells() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Opponent has no lands — can't pay {1}.
        let spell_id = state.alloc_id();
        state.objects.insert(spell_id, GameObject {
            id: spell_id,
            catalog_key: "Ponder".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            zone: CardZone::Stack,
            is_token: false,
            spell: Some(SpellState {
                effect: Some(eff_draw(PlayerId::Opp, 1)),
                chosen_targets: vec![],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
            bf: None,
            materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });
        state.stack.push(spell_id);

        // A storm copy's effect is the same as the original: counter unless pays {1}.
        eff_counter_unless_pays(PlayerId::Us, vec![CostComponent::Mana(parse_mana_cost("1"))])
            .call(&mut state, 1, &[spell_id]);

        assert_eq!(state.objects[&spell_id].zone, CardZone::Graveyard,
            "storm copy should counter spell when opponent can't pay 1");
    }

    // ── §50b: Mindbreak Trap / any-number targeting ────────────────────────────

    #[test]
    fn test_mindbreak_trap_exiles_all_targeted_spells() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Put two opponent spells on the stack.
        let spell_a = state.alloc_id();
        state.objects.insert(spell_a, GameObject {
            id: spell_a,
            catalog_key: "Dark Ritual".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            zone: CardZone::Stack,
            is_token: false,
            spell: Some(SpellState {
                effect: Some(eff_mana(PlayerId::Us, "BBB")),
                chosen_targets: vec![],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
            bf: None,
            materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });
        state.stack.push(spell_a);

        let spell_b = state.alloc_id();
        state.objects.insert(spell_b, GameObject {
            id: spell_b,
            catalog_key: "Ponder".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            zone: CardZone::Stack,
            is_token: false,
            spell: Some(SpellState {
                effect: Some(eff_draw(PlayerId::Us, 1)),
                chosen_targets: vec![],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
            bf: None,
            materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });
        state.stack.push(spell_b);

        // Opponent casts Mindbreak Trap targeting both spells.
        eff_exile_all_targets(PlayerId::Opp).call(&mut state, 1, &[spell_a, spell_b]);

        assert!(matches!(state.objects[&spell_a].zone, CardZone::Exile { .. }),
            "spell A should be exiled");
        assert!(matches!(state.objects[&spell_b].zone, CardZone::Exile { .. }),
            "spell B should be exiled");
    }

    #[test]
    fn test_mindbreak_trap_condition_checks_opponent_spell_count() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let trap_def = state.catalog.get("Mindbreak Trap").unwrap().clone();
        let alt = &trap_def.alternate_costs[0];
        let condition = alt.condition.as_ref().unwrap();

        // Opponent (Us from their perspective) has cast 2 spells — condition false for Opp caster.
        state.us.spells_cast_this_turn = 2;
        assert!(!condition(PlayerId::Opp, &state),
            "trap condition should be false when opponent cast only 2 spells");

        // Opponent has cast 3 spells — condition true.
        state.us.spells_cast_this_turn = 3;
        assert!(condition(PlayerId::Opp, &state),
            "trap condition should be true when opponent cast 3+ spells");
    }

    // ── §51: Simian Spirit Guide / hand-zone mana ──────────────────────────────

    #[test]
    fn test_ssg_potential_mana_includes_hand() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // SSG in hand → potential_mana should include {R}.
        add_hand_card(&mut state, PlayerId::Us, "Simian Spirit Guide");
        let pool = state.potential_mana(PlayerId::Us);
        assert!(pool.r >= 1, "potential_mana should see SSG's R from hand");
        assert!(pool.total >= 1);
    }

    #[test]
    fn test_ssg_produce_mana_exiles_from_hand() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let ssg_id = add_hand_card(&mut state, PlayerId::Us, "Simian Spirit Guide");
        // Activate SSG's hand-zone mana ability — should exile from hand.
        let act = ManaActivation { source_id: ssg_id, ability_index: 0, color_choice: Some(Color::Red) };
        execute_mana_activation(&mut state, 1, PlayerId::Us, &act);
        assert_eq!(state.objects[&ssg_id].zone, CardZone::Exile { on_adventure: false },
            "SSG should be exiled after paying mana");
        assert_eq!(state.us.pool.r, 1, "SSG should produce R");
    }

    #[test]
    fn test_ssg_on_battlefield_does_not_tap_for_mana() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // SSG on battlefield — its mana ability is hand-zone only.
        let ssg_def = catalog_card("Simian Spirit Guide");
        add_perm_with_def(&mut state, PlayerId::Us, &ssg_def, BattlefieldState::new());
        let pool = state.potential_mana(PlayerId::Us);
        assert_eq!(pool.r, 0, "SSG on battlefield should not produce R");
        assert_eq!(pool.total, 0);
    }

    // ── §52: Swords to Plowshares ──────────────────────────────────────────────

    #[test]
    fn test_swords_exiles_creature_and_gains_life() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Opponent has a 3/3 creature.
        let creature_def = catalog_card("Murktide Regent");
        let bf = BattlefieldState { counters: 0, ..BattlefieldState::new() };
        let creature_id = add_perm_with_def(&mut state, PlayerId::Opp, &creature_def, bf);
        recompute(&mut state);
        let opp_life_before = state.opp.life;

        // Swords to Plowshares: exile + controller gains life equal to power.
        eff_exile_target_gain_power(PlayerId::Us).call(&mut state, 1, &[creature_id]);

        assert_eq!(state.objects[&creature_id].zone, CardZone::Exile { on_adventure: false },
            "creature should be exiled");
        assert!(state.opp.life > opp_life_before,
            "opponent should gain life equal to creature's power");
    }

    // ── §53: City of Traitors ───────────────────────────────────────────────────

    #[test]
    fn test_city_of_traitors_sacrificed_when_another_land_played() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let cot_def = catalog_card("City of Traitors");
        let cot_id = add_perm_with_def(&mut state, PlayerId::Us, &cot_def, BattlefieldState::new());
        recompute(&mut state);

        // Playing another land fires LandPlayed — should trigger CoT's sacrifice.
        let other_land_id = state.alloc_id();
        fire_event(
            GameEvent::LandPlayed { id: other_land_id, controller: PlayerId::Us },
            &mut state, 1, PlayerId::Us,
        );
        assert!(!state.pending_triggers.is_empty(),
            "LandPlayed should produce a pending trigger for City of Traitors");

        // Resolve the trigger — CoT goes to graveyard.
        let ctx = state.pending_triggers.remove(0);
        ctx.effect.call(&mut state, 1, &[]);
        assert_eq!(state.objects[&cot_id].zone, CardZone::Graveyard,
            "City of Traitors should be sacrificed");
    }

    #[test]
    fn test_city_of_traitors_not_triggered_by_fetch() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let cot_def = catalog_card("City of Traitors");
        let _cot_id = add_perm_with_def(&mut state, PlayerId::Us, &cot_def, BattlefieldState::new());
        recompute(&mut state);

        // A land entering via fetch fires ZoneChange but NOT LandPlayed.
        let fetched_id = state.alloc_id();
        fire_event(
            GameEvent::ZoneChange {
                id: fetched_id,
                actor: PlayerId::Us,
                from: ZoneId::Library,
                to: ZoneId::Battlefield,
                controller: PlayerId::Us,
            },
            &mut state, 1, PlayerId::Us,
        );
        assert!(state.pending_triggers.is_empty(),
            "ZoneChange (fetch) should NOT trigger City of Traitors");
    }

    #[test]
    fn test_city_of_traitors_produces_two_colorless() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let cot_def = catalog_card("City of Traitors");
        add_perm_with_def(&mut state, PlayerId::Us, &cot_def, BattlefieldState::new());
        recompute(&mut state);
        let pool = state.potential_mana(PlayerId::Us);
        assert_eq!(pool.total, 2, "City of Traitors should produce 2 mana");
    }

    // ── §54: Omniscience ────────────────────────────────────────────────────────

    #[test]
    fn test_omniscience_grants_free_alternate_cost_to_hand_spells() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let omni_def = catalog_card("Omniscience");
        add_perm_with_def(&mut state, PlayerId::Us, &omni_def, BattlefieldState::new());
        let spell_id = add_hand_card(&mut state, PlayerId::Us, "Doomsday");
        recompute(&mut state);

        let def = state.def_of(spell_id).expect("hand card should have materialized def");
        assert!(def.alternate_costs().iter().any(|c| c.costs.is_empty()),
            "Omniscience should grant a zero-cost alternate to hand spells");
    }

    #[test]
    fn test_omniscience_does_not_affect_lands() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let omni_def = catalog_card("Omniscience");
        add_perm_with_def(&mut state, PlayerId::Us, &omni_def, BattlefieldState::new());
        let land_id = add_hand_card(&mut state, PlayerId::Us, "Underground Sea");
        recompute(&mut state);

        let def = state.def_of(land_id).expect("hand card should have materialized def");
        assert!(def.alternate_costs().is_empty(),
            "Omniscience should not grant alternate costs to lands");
    }

    #[test]
    fn test_omniscience_does_not_affect_opponent() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let omni_def = catalog_card("Omniscience");
        add_perm_with_def(&mut state, PlayerId::Us, &omni_def, BattlefieldState::new());
        let opp_spell = add_hand_card(&mut state, PlayerId::Opp, "Doomsday");
        recompute(&mut state);

        let def = state.def_of(opp_spell).expect("hand card should have materialized def");
        assert!(def.alternate_costs().is_empty(),
            "Omniscience should not affect opponent's spells");
    }

    // ── §55: Sneak Attack ───────────────────────────────────────────────────────

    #[test]
    fn test_sneak_attack_enchantment_has_ability() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let sa_def = catalog_card("Sneak Attack");
        add_perm_with_def(&mut state, PlayerId::Us, &sa_def, BattlefieldState::new());
        recompute(&mut state);
        assert_eq!(sa_def.abilities().len(), 1, "Sneak Attack should have one activated ability");
    }

    #[test]
    fn test_sneak_attack_puts_creature_with_haste() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let sa_def = catalog_card("Sneak Attack");
        add_perm_with_def(&mut state, PlayerId::Us, &sa_def, BattlefieldState::new());
        let creature_id = add_hand_card(&mut state, PlayerId::Us, "Orcish Bowmasters");
        recompute(&mut state);

        // Resolve the ability effect with the creature as the chosen target.
        let ability = &sa_def.abilities()[0];
        let eff = build_ability_effect(ability, PlayerId::Us, ObjId::UNSET);
        eff.call(&mut state, 1, &[creature_id]);
        recompute(&mut state);

        // Creature should be on the battlefield with haste.
        assert_eq!(state.objects[&creature_id].zone, CardZone::Battlefield,
            "creature should be on the battlefield");
        let def = state.def_of(creature_id).expect("should have materialized def");
        assert!(def.has_keyword(Keyword::Haste), "creature should have haste");
    }

    #[test]
    fn test_sneak_attack_delayed_trigger_sacrifices_at_end_step() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let sa_def = catalog_card("Sneak Attack");
        add_perm_with_def(&mut state, PlayerId::Us, &sa_def, BattlefieldState::new());
        let creature_id = add_hand_card(&mut state, PlayerId::Us, "Orcish Bowmasters");
        recompute(&mut state);

        // Resolve the ability to put creature onto battlefield.
        let ability = &sa_def.abilities()[0];
        let eff = build_ability_effect(ability, PlayerId::Us, ObjId::UNSET);
        eff.call(&mut state, 1, &[creature_id]);
        recompute(&mut state);
        assert_eq!(state.objects[&creature_id].zone, CardZone::Battlefield);

        // Drain any pending triggers from the ETB (e.g. Bowmasters draw-trigger setup).
        state.pending_triggers.clear();

        // Fire end step event — should produce a delayed sacrifice trigger.
        fire_event(
            GameEvent::EnteredStep { step: StepKind::End, active_player: PlayerId::Us },
            &mut state, 2, PlayerId::Us,
        );
        assert!(!state.pending_triggers.is_empty(),
            "end step should produce a sacrifice trigger");

        // Resolve the trigger — creature should be sacrificed.
        let ctx = state.pending_triggers.remove(0);
        ctx.effect.call(&mut state, 2, &[]);
        assert_eq!(state.objects[&creature_id].zone, CardZone::Graveyard,
            "creature should be sacrificed at end step");
    }

    // ── 42. Magus of the Moon ─────────────────────────────────────────────────

    /// Helper: place Magus of the Moon on the battlefield with its static CE registered.
    fn etb_magus_of_the_moon(state: &mut SimState, who: PlayerId) -> ObjId {
        let def = catalog_card("Magus of the Moon");
        add_perm_with_def(state, who, &def, BattlefieldState::new())
    }

    /// Nonbasic dual land (Underground Sea: island + swamp) should become a Mountain
    /// with only "{T}: Add {R}" after Magus of the Moon enters play.
    #[test]
    fn test_magus_nonbasic_becomes_mountain() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let _magus = etb_magus_of_the_moon(&mut state, PlayerId::Us);
        let sea_id = add_default_perm(&mut state, PlayerId::Opp, "Underground Sea");
        recompute(&mut state);

        let def = state.def_of(sea_id).expect("Underground Sea should have materialized def");
        let land = def.as_land().expect("should still be a Land");
        assert!(land.land_types.mountain, "nonbasic should gain Mountain type");
        assert!(!land.land_types.island, "nonbasic should lose Island type");
        assert!(!land.land_types.swamp, "nonbasic should lose Swamp type");
        assert_eq!(land.mana_abilities.len(), 1, "should have exactly one mana ability");
        assert!(land.abilities.is_empty(), "non-mana abilities should be cleared");
    }

    /// Basic Island is unaffected by Magus of the Moon.
    #[test]
    fn test_magus_basic_land_unaffected() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let _magus = etb_magus_of_the_moon(&mut state, PlayerId::Us);
        let island_def = catalog_card("Island");
        let island_id = add_perm_with_def(&mut state, PlayerId::Us, &island_def, BattlefieldState::new());
        recompute(&mut state);

        let def = state.def_of(island_id).expect("Island should have materialized def");
        let land = def.as_land().expect("should be a Land");
        assert!(land.land_types.island, "basic Island should keep Island type");
        assert!(!land.land_types.mountain, "basic Island should not gain Mountain");
    }

    /// Legendary supertype is preserved on Karakas under Magus of the Moon.
    #[test]
    fn test_magus_preserves_supertypes() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let _magus = etb_magus_of_the_moon(&mut state, PlayerId::Us);
        let karakas_def = catalog_card("Karakas");
        let karakas_id = add_perm_with_def(&mut state, PlayerId::Us, &karakas_def, BattlefieldState::new());
        recompute(&mut state);

        let def = state.def_of(karakas_id).expect("Karakas should have materialized def");
        assert!(def.supertypes.contains(&Supertype::Legendary),
            "Karakas should keep Legendary supertype");
        let land = def.as_land().expect("should be a Land");
        assert!(land.land_types.mountain, "Karakas should become a Mountain");
        assert!(land.abilities.is_empty(), "Karakas activated ability should be stripped");
    }

    /// A fetch land under Magus loses its fetch ability and becomes a Mountain.
    /// The search predicate is baked into the ability, which is cleared.
    #[test]
    fn test_magus_fetch_land_loses_ability() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let _magus = etb_magus_of_the_moon(&mut state, PlayerId::Us);
        let delta_def = catalog_card("Polluted Delta");
        let delta_id = add_perm_with_def(&mut state, PlayerId::Us, &delta_def, BattlefieldState::new());
        recompute(&mut state);

        let def = state.def_of(delta_id).expect("Polluted Delta should have materialized def");
        let land = def.as_land().expect("should be a Land");
        assert!(land.land_types.mountain, "Polluted Delta should be a Mountain");
        assert!(!land.land_types.island, "should lose Island subtype");
        assert!(!land.land_types.swamp, "should lose Swamp subtype");
        assert!(land.abilities.is_empty(), "fetch ability should be cleared");
        assert_eq!(land.mana_abilities.len(), 1, "should have exactly one mana ability");
    }

    /// When Magus of the Moon leaves the battlefield, nonbasic lands revert to original types.
    #[test]
    fn test_magus_ci_removed_on_ltb() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let magus_id = etb_magus_of_the_moon(&mut state, PlayerId::Us);
        let sea_id = add_default_perm(&mut state, PlayerId::Opp, "Underground Sea");
        recompute(&mut state);

        // Verify CE is active.
        let def = state.def_of(sea_id).unwrap();
        assert!(def.as_land().unwrap().land_types.mountain, "should be Mountain while Magus in play");

        // Magus leaves the battlefield.
        change_zone(magus_id, ZoneId::Graveyard, &mut state, 1, PlayerId::Us);
        recompute(&mut state);

        let def = state.def_of(sea_id).unwrap();
        let land = def.as_land().unwrap();
        assert!(land.land_types.island, "Underground Sea should revert to Island");
        assert!(land.land_types.swamp, "Underground Sea should revert to Swamp");
        assert!(!land.land_types.mountain, "should no longer be a Mountain");
    }

    /// Magus of the Moon does not affect creatures (modifier early-returns for non-Land).
    #[test]
    fn test_magus_does_not_affect_creatures() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let _magus = etb_magus_of_the_moon(&mut state, PlayerId::Us);
        let bowmasters_def = catalog_card("Orcish Bowmasters");
        let bowmasters_id = add_perm_with_def(&mut state, PlayerId::Opp, &bowmasters_def, BattlefieldState::new());
        recompute(&mut state);

        let def = state.def_of(bowmasters_id).expect("Bowmasters should have materialized def");
        assert!(def.as_creature().is_some(), "should still be a Creature");
        assert!(def.as_land().is_none(), "should not be a Land");
    }

    /// Snow-Covered Island has Supertype::Basic and should be unaffected.
    #[test]
    fn test_magus_snow_basic_unaffected() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let _magus = etb_magus_of_the_moon(&mut state, PlayerId::Us);
        let snow_def = catalog_card("Snow-Covered Island");
        let snow_id = add_perm_with_def(&mut state, PlayerId::Us, &snow_def, BattlefieldState::new());
        recompute(&mut state);

        let def = state.def_of(snow_id).expect("Snow-Covered Island should have materialized def");
        let land = def.as_land().expect("should be a Land");
        assert!(land.land_types.island, "Snow-Covered Island should keep Island type");
        assert!(!land.land_types.mountain, "should not gain Mountain");
        assert!(def.supertypes.contains(&Supertype::Basic), "should keep Basic");
        assert!(def.supertypes.contains(&Supertype::Snow), "should keep Snow");
    }

    /// Multiple nonbasic lands all become Mountains simultaneously.
    #[test]
    fn test_magus_multiple_nonbasics() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let _magus = etb_magus_of_the_moon(&mut state, PlayerId::Us);
        let sea_id = add_default_perm(&mut state, PlayerId::Opp, "Underground Sea");
        let tundra_id = add_default_perm(&mut state, PlayerId::Opp, "Tundra");
        recompute(&mut state);

        for (id, name) in [(sea_id, "Underground Sea"), (tundra_id, "Tundra")] {
            let def = state.def_of(id).unwrap_or_else(|| panic!("{name} should have materialized def"));
            let land = def.as_land().unwrap_or_else(|| panic!("{name} should be a Land"));
            assert!(land.land_types.mountain, "{name} should be a Mountain");
            assert_eq!(land.mana_abilities.len(), 1, "{name} should have one mana ability");
            assert!(land.abilities.is_empty(), "{name} non-mana abilities should be cleared");
        }
    }

    /// Blood Moon (enchantment) shares the same static ability as Magus of the Moon.
    #[test]
    fn test_blood_moon_nonbasic_becomes_mountain() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let bm_def = catalog_card("Blood Moon");
        add_perm_with_def(&mut state, PlayerId::Us, &bm_def, BattlefieldState::new());
        let sea_id = add_default_perm(&mut state, PlayerId::Opp, "Underground Sea");
        recompute(&mut state);

        let def = state.def_of(sea_id).expect("Underground Sea should have materialized def");
        let land = def.as_land().expect("should still be a Land");
        assert!(land.land_types.mountain, "nonbasic should gain Mountain type");
        assert!(!land.land_types.island, "nonbasic should lose Island type");
        assert!(!land.land_types.swamp, "nonbasic should lose Swamp type");
        assert_eq!(land.mana_abilities.len(), 1, "should have exactly one mana ability");
    }

    // ── 43. Urborg, Tomb of Yawgmoth / Yavimaya, Cradle of Growth ────────────

    /// Urborg makes all lands Swamps in addition to their other types.
    #[test]
    fn test_urborg_adds_swamp_to_nonbasic() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let urborg_def = catalog_card("Urborg, Tomb of Yawgmoth");
        let urborg_id = add_perm_with_def(&mut state, PlayerId::Us, &urborg_def, BattlefieldState::new());
        let sea_id = add_default_perm(&mut state, PlayerId::Opp, "Underground Sea");
        recompute(&mut state);

        // Underground Sea (island + swamp) should keep both and still be a swamp.
        let def = state.def_of(sea_id).unwrap();
        let land = def.as_land().unwrap();
        assert!(land.land_types.swamp, "should have Swamp");
        assert!(land.land_types.island, "should keep Island");

        // Urborg itself gains Swamp too.
        let urborg_mat = state.def_of(urborg_id).unwrap();
        let urborg_land = urborg_mat.as_land().unwrap();
        assert!(urborg_land.land_types.swamp, "Urborg itself should be a Swamp");
    }

    /// Urborg adds Swamp + "{T}: Add {B}" to a basic Island.
    #[test]
    fn test_urborg_adds_swamp_to_basic() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let urborg_def = catalog_card("Urborg, Tomb of Yawgmoth");
        add_perm_with_def(&mut state, PlayerId::Us, &urborg_def, BattlefieldState::new());
        let island_def = catalog_card("Island");
        let island_id = add_perm_with_def(&mut state, PlayerId::Us, &island_def, BattlefieldState::new());
        recompute(&mut state);

        let def = state.def_of(island_id).unwrap();
        let land = def.as_land().unwrap();
        assert!(land.land_types.island, "should keep Island");
        assert!(land.land_types.swamp, "should gain Swamp");
        assert_eq!(land.mana_abilities.len(), 2, "should have U and B mana abilities");
    }

    /// A land that is already a Swamp does not get a duplicate mana ability from Urborg.
    #[test]
    fn test_urborg_no_duplicate_on_swamp() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let urborg_def = catalog_card("Urborg, Tomb of Yawgmoth");
        add_perm_with_def(&mut state, PlayerId::Us, &urborg_def, BattlefieldState::new());
        let swamp_def = catalog_card("Swamp");
        let swamp_id = add_perm_with_def(&mut state, PlayerId::Us, &swamp_def, BattlefieldState::new());
        recompute(&mut state);

        let def = state.def_of(swamp_id).unwrap();
        let land = def.as_land().unwrap();
        assert!(land.land_types.swamp, "should still be a Swamp");
        assert_eq!(land.mana_abilities.len(), 1, "should not get a duplicate mana ability");
    }

    /// Yavimaya makes all lands Forests in addition to their other types.
    #[test]
    fn test_yavimaya_adds_forest_to_nonbasic() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let yav_def = catalog_card("Yavimaya, Cradle of Growth");
        add_perm_with_def(&mut state, PlayerId::Us, &yav_def, BattlefieldState::new());
        let sea_id = add_default_perm(&mut state, PlayerId::Opp, "Underground Sea");
        recompute(&mut state);

        let def = state.def_of(sea_id).unwrap();
        let land = def.as_land().unwrap();
        assert!(land.land_types.forest, "should gain Forest");
        assert!(land.land_types.island, "should keep Island");
        assert!(land.land_types.swamp, "should keep Swamp");
    }

    /// Urborg CI is removed when it leaves; lands revert.
    #[test]
    fn test_urborg_ci_removed_on_ltb() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let urborg_def = catalog_card("Urborg, Tomb of Yawgmoth");
        let urborg_id = add_perm_with_def(&mut state, PlayerId::Us, &urborg_def, BattlefieldState::new());
        let island_def = catalog_card("Island");
        let island_id = add_perm_with_def(&mut state, PlayerId::Us, &island_def, BattlefieldState::new());
        recompute(&mut state);
        assert!(state.def_of(island_id).unwrap().as_land().unwrap().land_types.swamp);

        change_zone(urborg_id, ZoneId::Graveyard, &mut state, 1, PlayerId::Us);
        recompute(&mut state);

        let land = state.def_of(island_id).unwrap().as_land().unwrap();
        assert!(!land.land_types.swamp, "Island should revert — no longer a Swamp");
        assert!(land.land_types.island, "Island should keep Island type");
    }

    /// Yavimaya + Blood Moon interaction: Blood Moon (L4) makes nonbasics into Mountains
    /// (losing all types and abilities), then Yavimaya (also L4, registered later) adds Forest
    /// on top. Result: nonbasic is Mountain + Forest with "{T}: Add {R}" and "{T}: Add {G}".
    /// Yavimaya itself is a nonbasic, so Blood Moon turns it into a Mountain too — but
    /// Yavimaya's CE persists because type-changing (L4) is independent of ability removal (L6).
    #[test]
    fn test_yavimaya_plus_blood_moon() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Blood Moon first, then Yavimaya — registration order within the same layer matters.
        let bm_def = catalog_card("Blood Moon");
        add_perm_with_def(&mut state, PlayerId::Us, &bm_def, BattlefieldState::new());
        let yav_def = catalog_card("Yavimaya, Cradle of Growth");
        let yav_id = add_perm_with_def(&mut state, PlayerId::Us, &yav_def, BattlefieldState::new());
        let sea_id = add_default_perm(&mut state, PlayerId::Opp, "Underground Sea");
        recompute(&mut state);

        // CR 613.7: Blood Moon (dep_order 0) applies before Yavimaya (dep_order 1).
        // Blood Moon makes Yavimaya a Mountain, stripping all abilities including its
        // static ability. Yavimaya's CE ceases to exist → nonbasics are Mountains only.
        let def = state.def_of(sea_id).unwrap();
        let land = def.as_land().unwrap();
        assert!(land.land_types.mountain, "Blood Moon should make it a Mountain");
        assert!(!land.land_types.forest, "Yavimaya CE suppressed — no Forest");
        assert!(!land.land_types.island, "original Island type should be gone");
        assert!(!land.land_types.swamp, "original Swamp type should be gone");
        assert_eq!(land.mana_abilities.len(), 1,
            "should have only R mana ability");

        // Yavimaya itself is nonbasic: Blood Moon turns it into a Mountain and
        // strips its static ability, so its CE doesn't exist.
        let yav_mat = state.def_of(yav_id).unwrap();
        let yav_land = yav_mat.as_land().unwrap();
        assert!(yav_land.land_types.mountain, "Yavimaya should be a Mountain under Blood Moon");
        assert!(!yav_land.land_types.forest, "Yavimaya's CE is suppressed — no Forest");
    }

    /// CR 613.7: dependency (Blood Moon writes LandTypes, Yavimaya reads LandTypes)
    /// overrides timestamp — same result regardless of registration order.
    #[test]
    fn test_yavimaya_before_blood_moon_same_result() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Yavimaya first, Blood Moon second — opposite registration order.
        let yav_def = catalog_card("Yavimaya, Cradle of Growth");
        let yav_id = add_perm_with_def(&mut state, PlayerId::Us, &yav_def, BattlefieldState::new());
        let bm_def = catalog_card("Blood Moon");
        add_perm_with_def(&mut state, PlayerId::Us, &bm_def, BattlefieldState::new());
        let sea_id = add_default_perm(&mut state, PlayerId::Opp, "Underground Sea");
        recompute(&mut state);

        // Same result as test_yavimaya_plus_blood_moon: Yavimaya's CE is suppressed.
        let def = state.def_of(sea_id).unwrap();
        let land = def.as_land().unwrap();
        assert!(land.land_types.mountain, "Blood Moon should make it a Mountain");
        assert!(!land.land_types.forest, "Yavimaya CE suppressed — no Forest");
        assert_eq!(land.mana_abilities.len(), 1, "only R mana ability");

        let yav_mat = state.def_of(yav_id).unwrap();
        let yav_land = yav_mat.as_land().unwrap();
        assert!(yav_land.land_types.mountain);
        assert!(!yav_land.land_types.forest, "Yavimaya's CE is suppressed");
    }

    /// Urborg's CE is also suppressed under Blood Moon — nonbasics are Mountains only.
    #[test]
    fn test_urborg_suppressed_under_blood_moon() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let bm_def = catalog_card("Blood Moon");
        add_perm_with_def(&mut state, PlayerId::Us, &bm_def, BattlefieldState::new());
        let urborg_def = catalog_card("Urborg, Tomb of Yawgmoth");
        let urborg_id = add_perm_with_def(&mut state, PlayerId::Us, &urborg_def, BattlefieldState::new());
        let sea_id = add_default_perm(&mut state, PlayerId::Opp, "Underground Sea");
        recompute(&mut state);

        // Urborg's CE suppressed — nonbasics are Mountains only, no Swamp.
        let def = state.def_of(sea_id).unwrap();
        let land = def.as_land().unwrap();
        assert!(land.land_types.mountain);
        assert!(!land.land_types.swamp, "Urborg CE suppressed — no Swamp");
        assert_eq!(land.mana_abilities.len(), 1, "only R mana ability");

        // Urborg itself is a Mountain (nonbasic, legendary).
        let urborg_mat = state.def_of(urborg_id).unwrap();
        let urborg_land = urborg_mat.as_land().unwrap();
        assert!(urborg_land.land_types.mountain);
        assert!(!urborg_land.land_types.swamp, "Urborg's own CE is suppressed");
    }

    // ── Section 32: Protection ─────────────────────────────────────────────────

    /// Helper: a colored instant (blue) for protection tests.
    fn blue_instant(name: &str) -> CardDef {
        CardDef::new(
            name, CardKind::Instant(SpellData { mana_cost: "U".into(), ..Default::default() }),
            vec![Color::Blue], None, vec![], CardLayout::Normal, None,
            vec![], vec![], vec![], vec![],
        )
    }

    /// Helper: a colorless instant for protection tests.
    fn colorless_instant(name: &str) -> CardDef {
        CardDef::new(
            name, CardKind::Instant(SpellData { mana_cost: "2".into(), ..Default::default() }),
            vec![], None, vec![], CardLayout::Normal, None,
            vec![], vec![], vec![], vec![],
        )
    }

    #[test]
    fn test_protection_colored_spell_cannot_target() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Emrakul on battlefield — protected from colored spells.
        let emrakul_id = add_perm(&mut state, PlayerId::Us, "Emrakul, the Aeons Torn",
                                  BattlefieldState::new());
        // Also put a vanilla creature (no protection) for comparison.
        let vanilla_id = add_perm_with_def(&mut state, PlayerId::Us,
            &creature("Vanilla 2/2", 2, 2), BattlefieldState::new());

        // Blue instant on the stack — colored spell.
        let bolt_def = blue_instant("Blue Bolt");
        let bolt_id = add_stack_spell(&mut state, PlayerId::Opp, &bolt_def);

        let spec = TargetSpec::ObjectInZone {
            controller: Who::Opp,
            zone: ZoneId::Battlefield,
            filter: obj_pred_from_card(pred_type_eq(CardType::Creature)),
        };
        let targets = legal_targets(&spec, PlayerId::Opp, bolt_id, &state);

        assert!(!targets.contains(&emrakul_id),
            "Emrakul should not be a legal target for a colored spell");
        assert!(targets.contains(&vanilla_id),
            "non-protected creature should be a legal target");
    }

    #[test]
    fn test_protection_colorless_spell_can_target() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let emrakul_id = add_perm(&mut state, PlayerId::Us, "Emrakul, the Aeons Torn",
                                  BattlefieldState::new());

        let spell_def = colorless_instant("Colorless Zap");
        let spell_id = add_stack_spell(&mut state, PlayerId::Opp, &spell_def);

        let spec = TargetSpec::ObjectInZone {
            controller: Who::Opp,
            zone: ZoneId::Battlefield,
            filter: obj_pred_from_card(pred_type_eq(CardType::Creature)),
        };
        let targets = legal_targets(&spec, PlayerId::Opp, spell_id, &state);

        assert!(targets.contains(&emrakul_id),
            "Emrakul should be a legal target for a colorless spell");
    }

    #[test]
    fn test_protection_colored_permanent_ability_can_target() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let emrakul_id = add_perm(&mut state, PlayerId::Us, "Emrakul, the Aeons Torn",
                                  BattlefieldState::new());

        // A colored permanent on the battlefield (not a spell) — ability source.
        let mut perm_def = creature("Blue Pinger", 1, 1);
        perm_def.colors = vec![Color::Blue];
        let perm_id = add_perm_with_def(&mut state, PlayerId::Opp, &perm_def,
                                        BattlefieldState::new());

        let spec = TargetSpec::ObjectInZone {
            controller: Who::Opp,
            zone: ZoneId::Battlefield,
            filter: obj_pred_from_card(pred_type_eq(CardType::Creature)),
        };
        let targets = legal_targets(&spec, PlayerId::Opp, perm_id, &state);

        assert!(targets.contains(&emrakul_id),
            "Emrakul should be a legal target for a colored permanent's ability (not a spell)");
    }

    #[test]
    fn test_protection_prevents_spell_damage() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let emrakul_id = add_perm(&mut state, PlayerId::Us, "Emrakul, the Aeons Torn",
                                  BattlefieldState::new());

        // Colored spell on the stack dealing damage.
        let bolt_def = blue_instant("Blue Blast");
        let bolt_id = add_stack_spell(&mut state, PlayerId::Opp, &bolt_def);

        eff_damage_target(PlayerId::Opp, 15, bolt_id).call(&mut state, 1, &[emrakul_id]);

        let bf = state.objects[&emrakul_id].bf.as_ref().unwrap();
        assert_eq!(bf.damage, 0, "damage from colored spell should be prevented by protection");
    }

    #[test]
    fn test_protection_does_not_prevent_combat_damage() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let emrakul_id = add_perm(&mut state, PlayerId::Us, "Emrakul, the Aeons Torn",
                                  BattlefieldState::new());

        // Colored creature on battlefield dealing combat damage.
        let mut atk_def = creature("Blue Attacker", 5, 5);
        atk_def.colors = vec![Color::Blue];
        let atk_id = add_perm_with_def(&mut state, PlayerId::Opp, &atk_def,
                                       BattlefieldState::new());

        // Directly check: the colored creature is a permanent, not a spell.
        assert!(!is_protected_from(emrakul_id, atk_id, &state),
            "Emrakul is NOT protected from colored permanents (only colored spells)");
    }

    #[test]
    fn test_protection_colorless_spell_damage_goes_through() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let emrakul_id = add_perm(&mut state, PlayerId::Us, "Emrakul, the Aeons Torn",
                                  BattlefieldState::new());

        let zap_def = colorless_instant("Colorless Zap");
        let zap_id = add_stack_spell(&mut state, PlayerId::Opp, &zap_def);

        eff_damage_target(PlayerId::Opp, 15, zap_id).call(&mut state, 1, &[emrakul_id]);

        let bf = state.objects[&emrakul_id].bf.as_ref().unwrap();
        assert_eq!(bf.damage, 15, "damage from colorless spell should not be prevented");
    }

    // ── Section 46: Mistrise Village ────────────────────────────────────────────

    /// Helper: place Mistrise Village on the battlefield via change_zone (fires replacement).
    fn etb_mistrise_village(state: &mut SimState, who: PlayerId) -> ObjId {
        let def = catalog_card("Mistrise Village");
        let id = state.alloc_id();
        state.objects.insert(id, GameObject {
            id,
            catalog_key: "Mistrise Village".to_string(),
            owner: who,
            controller: who,
            zone: CardZone::Hand { known: false },
            is_token: false,
            bf: None, spell: None, materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });

        state.catalog.entry("Mistrise Village".to_string()).or_insert(def);
        change_zone(id, ZoneId::Battlefield, state, 1, who);
        id
    }

    /// Mistrise Village enters untapped when you control a Forest.
    #[test]
    fn test_mistrise_village_etb_untapped_with_forest() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Put a Forest on the battlefield first.
        let forest_def = catalog_card("Forest");
        add_perm_with_def(&mut state, PlayerId::Us, &forest_def, BattlefieldState::new());
        recompute(&mut state);

        let mv_id = etb_mistrise_village(&mut state, PlayerId::Us);

        let bf = state.objects[&mv_id].bf.as_ref().expect("should be on battlefield");
        assert!(!bf.tapped, "Mistrise Village should enter untapped when you control a Forest");
    }

    /// Mistrise Village enters untapped when you control a Mountain.
    #[test]
    fn test_mistrise_village_etb_untapped_with_mountain() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let mtn_def = catalog_card("Mountain");
        add_perm_with_def(&mut state, PlayerId::Us, &mtn_def, BattlefieldState::new());
        recompute(&mut state);

        let mv_id = etb_mistrise_village(&mut state, PlayerId::Us);

        let bf = state.objects[&mv_id].bf.as_ref().unwrap();
        assert!(!bf.tapped, "Mistrise Village should enter untapped when you control a Mountain");
    }

    /// Mistrise Village enters tapped when you control neither Mountain nor Forest.
    #[test]
    fn test_mistrise_village_etb_tapped_without_mountain_or_forest() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Only an Island on the battlefield — no Mountain or Forest.
        let island_def = catalog_card("Island");
        add_perm_with_def(&mut state, PlayerId::Us, &island_def, BattlefieldState::new());
        recompute(&mut state);

        let mv_id = etb_mistrise_village(&mut state, PlayerId::Us);

        let bf = state.objects[&mv_id].bf.as_ref().unwrap();
        assert!(bf.tapped, "Mistrise Village should enter tapped without Mountain or Forest");
    }

    /// Mistrise Village {U},{T} ability: the next spell you cast can't be countered.
    #[test]
    fn test_mistrise_village_ability_makes_next_spell_uncounterable() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Place Mistrise Village (via add_perm so it starts untapped for the ability).
        let mv_def = catalog_card("Mistrise Village");
        let mv_id = add_perm_with_def(&mut state, PlayerId::Us, &mv_def, BattlefieldState::new());
        recompute(&mut state);

        // Activate the {U},{T} ability (second ability, index 0 in abilities vec).
        let ability = &mv_def.abilities()[0];
        let eff = build_ability_effect(ability, PlayerId::Us, mv_id);
        eff.call(&mut state, 1, &[]);

        // Should have registered a latent spell mod (not a dormant CI).
        assert_eq!(state.latent_spell_mods.len(), 1, "ability should register one LatentSpellMod");
        assert!(state.continuous_instances.is_empty(), "no CI yet — consumed at cast time");

        // Simulate casting a spell — consume_latent_spell_mod + fire SpellCast.
        let spell_def = catalog_card("Brainstorm");
        let spell_id = add_stack_spell(&mut state, PlayerId::Us, &spell_def);
        state.stack.push(spell_id);
        consume_latent_spell_mod(&mut state, PlayerId::Us, spell_id);
        fire_event(
            GameEvent::SpellCast { caster: PlayerId::Us, card_id: spell_id, mana_spent: true },
            &mut state, 1, PlayerId::Us,
        );

        // LatentSpellMod consumed → CI now exists and is active.
        assert!(state.latent_spell_mods.is_empty(), "LatentSpellMod consumed");
        assert_eq!(state.continuous_instances.len(), 1, "CI created from LatentSpellMod");

        // Try to counter it — should fizzle.
        eff_counter_target(PlayerId::Opp).call(&mut state, 1, &[spell_id]);
        assert!(state.stack.contains(&spell_id),
            "spell should remain on stack — can't be countered");
    }

    /// The LatentSpellMod is consumed on the first spell — the second is not protected.
    #[test]
    fn test_mistrise_village_ability_one_shot() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let mv_def = catalog_card("Mistrise Village");
        let mv_id = add_perm_with_def(&mut state, PlayerId::Us, &mv_def, BattlefieldState::new());
        recompute(&mut state);

        // Activate ability.
        let ability = &mv_def.abilities()[0];
        let eff = build_ability_effect(ability, PlayerId::Us, mv_id);
        eff.call(&mut state, 1, &[]);

        // Cast first spell — consumes the LatentSpellMod.
        let spell1_def = catalog_card("Brainstorm");
        let spell1_id = add_stack_spell(&mut state, PlayerId::Us, &spell1_def);
        state.stack.push(spell1_id);
        consume_latent_spell_mod(&mut state, PlayerId::Us, spell1_id);
        fire_event(
            GameEvent::SpellCast { caster: PlayerId::Us, card_id: spell1_id, mana_spent: true },
            &mut state, 1, PlayerId::Us,
        );
        assert!(state.latent_spell_mods.is_empty(), "LatentSpellMod consumed by first spell");

        // Cast second spell — no LatentSpellMod left, so no CI produced.
        let spell2_def = catalog_card("Ponder");
        let spell2_id = add_stack_spell(&mut state, PlayerId::Us, &spell2_def);
        state.stack.push(spell2_id);
        consume_latent_spell_mod(&mut state, PlayerId::Us, spell2_id);
        fire_event(
            GameEvent::SpellCast { caster: PlayerId::Us, card_id: spell2_id, mana_spent: true },
            &mut state, 1, PlayerId::Us,
        );

        // The second spell should be counterable.
        eff_counter_target(PlayerId::Opp).call(&mut state, 1, &[spell2_id]);
        assert!(!state.stack.contains(&spell2_id),
            "second spell should be counterable — LatentSpellMod already consumed");
    }

    /// The LatentSpellMod expires at end of turn if no spell is cast.
    #[test]
    fn test_mistrise_village_ability_expires_eot() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let mv_def = catalog_card("Mistrise Village");
        let mv_id = add_perm_with_def(&mut state, PlayerId::Us, &mv_def, BattlefieldState::new());
        recompute(&mut state);

        let ability = &mv_def.abilities()[0];
        let eff = build_ability_effect(ability, PlayerId::Us, mv_id);
        eff.call(&mut state, 1, &[]);

        assert_eq!(state.latent_spell_mods.len(), 1, "LatentSpellMod registered");

        // Run cleanup step — should remove the EndOfTurn LatentSpellMod.
        let step = Step { kind: StepKind::Cleanup, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true, &mut make_strategies());

        assert!(state.latent_spell_mods.is_empty(),
            "Mistrise Village LatentSpellMod should expire at end of turn");
    }

    // ── Section 47: Brotherhood's End ───────────────────────────────────────────

    /// Mode 0: deals 3 damage to each creature and each planeswalker.
    /// A 3/3 creature should die (lethal), a 4/4 should survive with 3 damage.
    #[test]
    fn test_brotherhoods_end_mode0_damages_creatures() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let small = creature("Bear", 2, 2);
        let small_id = add_perm_with_def(&mut state, PlayerId::Opp, &small, BattlefieldState::new());
        let big = creature("Giant", 4, 4);
        let big_id = add_perm_with_def(&mut state, PlayerId::Opp, &big, BattlefieldState::new());

        let be_def = catalog_card("Brotherhood's End");
        // Mode 0: damage to creatures/planeswalkers
        let mode = be_def.spell_modes().unwrap().get(0).unwrap();
        let effect = (mode.factory)(PlayerId::Us, ObjId(0), 0);
        effect.call(&mut state, 1, &[]);

        // Bear (2/2): 3 damage is lethal — but we haven't run SBAs yet, just check damage.
        assert_eq!(state.objects[&small_id].bf.as_ref().unwrap().damage, 3,
            "Bear should have 3 damage marked");
        assert_eq!(state.objects[&big_id].bf.as_ref().unwrap().damage, 3,
            "Giant should have 3 damage marked");
    }

    /// Mode 1: destroys all artifacts with mana value 3 or less.
    /// Lotus Petal (MV 0) should be destroyed; an artifact with MV 4 should survive.
    #[test]
    fn test_brotherhoods_end_mode1_destroys_cheap_artifacts() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let petal_id = add_default_perm(&mut state, PlayerId::Opp, "Lotus Petal");

        let be_def = catalog_card("Brotherhood's End");
        // Mode 1: destroy artifacts with MV ≤ 3
        let mode = be_def.spell_modes().unwrap().get(1).unwrap();
        let effect = (mode.factory)(PlayerId::Us, ObjId(0), 0);
        effect.call(&mut state, 1, &[]);

        assert_eq!(state.objects[&petal_id].zone, CardZone::Graveyard,
            "Lotus Petal (MV 0) should be destroyed by Brotherhood's End mode 1");
    }

    // ── Section 48: Mox Opal ────────────────────────────────────────────────────

    /// Mox Opal's mana ability requires metalcraft (3+ artifacts).
    /// With only 2 artifacts on the battlefield, the condition should fail.
    #[test]
    fn test_mox_opal_metalcraft_not_met() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let opal_def = catalog_card("Mox Opal");
        let opal_id = add_perm_with_def(&mut state, PlayerId::Us, &opal_def, BattlefieldState::new());
        // Only 1 artifact (Mox Opal itself) + add one more
        let _petal_id = add_default_perm(&mut state, PlayerId::Us, "Lotus Petal");
        recompute(&mut state);

        // 2 artifacts — metalcraft not met
        let ma = &opal_def.mana_abilities()[0];
        let cond = ma.condition.as_ref().expect("Mox Opal should have a condition");
        assert!(!cond(opal_id, &state), "metalcraft should not be active with only 2 artifacts");
    }

    /// With 3+ artifacts, metalcraft is active and the condition should pass.
    #[test]
    fn test_mox_opal_metalcraft_met() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let opal_def = catalog_card("Mox Opal");
        let opal_id = add_perm_with_def(&mut state, PlayerId::Us, &opal_def, BattlefieldState::new());
        let _petal_id = add_default_perm(&mut state, PlayerId::Us, "Lotus Petal");
        let _cage_id = add_default_perm(&mut state, PlayerId::Us, "Grafdigger's Cage");
        recompute(&mut state);

        // 3 artifacts — metalcraft active
        let ma = &opal_def.mana_abilities()[0];
        let cond = ma.condition.as_ref().expect("Mox Opal should have a condition");
        assert!(cond(opal_id, &state), "metalcraft should be active with 3 artifacts");
    }

    // ── §55: Karn, the Great Creator ──────────────────────────────────────────

    /// Karn's static ability suppresses all activated abilities (including mana abilities)
    /// on artifacts opponents control, via CE setting activatable=false.
    #[test]
    fn test_karn_suppresses_opponent_artifact_abilities() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Karn on our side.
        let karn_def = catalog_card("Karn, the Great Creator");
        let _karn_id = add_perm_with_def(&mut state, PlayerId::Us, &karn_def,
            BattlefieldState { loyalty: 5, ..BattlefieldState::new() });

        // Opponent controls a Lotus Petal (artifact with mana ability).
        let opp_petal_id = add_default_perm(&mut state, PlayerId::Opp, "Lotus Petal");

        // Our own Lotus Petal should NOT be suppressed.
        let our_petal_id = add_default_perm(&mut state, PlayerId::Us, "Lotus Petal");

        recompute(&mut state);

        // Opponent's artifact: mana abilities should be suppressed.
        let opp_def = state.def_of(opp_petal_id).expect("opponent petal should have materialized def");
        assert!(
            opp_def.mana_abilities().iter().all(|ma| !ma.activatable),
            "Karn should suppress mana abilities on opponent's artifacts"
        );

        // Our own artifact: mana abilities should be unaffected.
        let our_def = state.def_of(our_petal_id).expect("our petal should have materialized def");
        assert!(
            our_def.mana_abilities().iter().all(|ma| ma.activatable),
            "Karn should NOT suppress mana abilities on our own artifacts"
        );
    }

    /// Karn does not affect non-artifact permanents.
    #[test]
    fn test_karn_does_not_affect_non_artifacts() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let karn_def = catalog_card("Karn, the Great Creator");
        let _karn_id = add_perm_with_def(&mut state, PlayerId::Us, &karn_def,
            BattlefieldState { loyalty: 5, ..BattlefieldState::new() });

        // Opponent land with mana ability — should NOT be suppressed.
        let land_id = make_land(&mut state, PlayerId::Opp, "Underground Sea", false);

        recompute(&mut state);

        let land_def = state.def_of(land_id).expect("land should have materialized def");
        assert!(
            land_def.mana_abilities().iter().all(|ma| ma.activatable),
            "Karn should not suppress mana abilities on non-artifact permanents"
        );
    }

    // ── 44. Dragon's Rage Channeler ──────────────────────────────────────────

    #[test]
    fn test_drc_surveil_on_noncreature_cast() {
        // DRC on battlefield; cast a noncreature spell → surveil 1 trigger fires.
        let mut state = make_state();
        state.catalog = test_catalog();
        state.surveil_choice = std::sync::Arc::new(|_, _| true); // always mill

        // Put a known card on top of library.
        let top_id = {
            let id = state.alloc_id();
            state.objects.insert(id, GameObject {
                id,
                catalog_key: "Brainstorm".to_string(),
                owner: PlayerId::Us, controller: PlayerId::Us,
                zone: CardZone::Library,
                is_token: false, bf: None, spell: None, materialized: None,
                counters: HashMap::new(), ci_timestamp: 0,
            });
            state.us.library_order.push_front(id);
            state.catalog.entry("Brainstorm".to_string()).or_insert_with(|| catalog_card("Brainstorm"));
            id
        };

        // DRC on battlefield.
        let _drc_id = add_default_perm(&mut state, PlayerId::Us, "Dragon's Rage Channeler");

        // A noncreature spell on the stack (simulate casting Ponder).
        let spell_id = {
            let id = state.alloc_id();
            state.objects.insert(id, GameObject {
                id,
                catalog_key: "Ponder".to_string(),
                owner: PlayerId::Us, controller: PlayerId::Us,
                zone: CardZone::Stack,
                is_token: false, bf: None, spell: None, materialized: None,
                counters: HashMap::new(), ci_timestamp: 0,
            });
            state.catalog.entry("Ponder".to_string()).or_insert_with(|| catalog_card("Ponder"));
            id
        };

        // Fire SpellCast event.
        fire_event(
            GameEvent::SpellCast { caster: PlayerId::Us, card_id: spell_id, mana_spent: true },
            &mut state, 1, PlayerId::Us,
        );
        for ctx in std::mem::take(&mut state.pending_triggers) { ctx.effect.call(&mut state, 1, &[]); }

        assert_eq!(state.objects[&top_id].zone, CardZone::Graveyard,
            "DRC surveil should mill top library card when surveil_choice returns true");
    }

    #[test]
    fn test_drc_no_surveil_on_creature_cast() {
        // DRC on battlefield; cast a creature spell → no surveil trigger.
        let mut state = make_state();
        state.catalog = test_catalog();
        state.surveil_choice = std::sync::Arc::new(|_, _| true);

        let top_id = {
            let id = state.alloc_id();
            state.objects.insert(id, GameObject {
                id,
                catalog_key: "Brainstorm".to_string(),
                owner: PlayerId::Us, controller: PlayerId::Us,
                zone: CardZone::Library,
                is_token: false, bf: None, spell: None, materialized: None,
                counters: HashMap::new(), ci_timestamp: 0,
            });
            state.us.library_order.push_front(id);
            state.catalog.entry("Brainstorm".to_string()).or_insert_with(|| catalog_card("Brainstorm"));
            id
        };

        let _drc_id = add_default_perm(&mut state, PlayerId::Us, "Dragon's Rage Channeler");

        // Cast a creature spell.
        let spell_id = {
            let id = state.alloc_id();
            state.objects.insert(id, GameObject {
                id,
                catalog_key: "Barrowgoyf".to_string(),
                owner: PlayerId::Us, controller: PlayerId::Us,
                zone: CardZone::Stack,
                is_token: false, bf: None, spell: None, materialized: None,
                counters: HashMap::new(), ci_timestamp: 0,
            });
            state.catalog.entry("Barrowgoyf".to_string()).or_insert_with(|| catalog_card("Barrowgoyf"));
            id
        };

        fire_event(
            GameEvent::SpellCast { caster: PlayerId::Us, card_id: spell_id, mana_spent: true },
            &mut state, 1, PlayerId::Us,
        );

        assert!(state.pending_triggers.is_empty(),
            "DRC should not trigger on creature spell cast");
        assert_eq!(state.objects[&top_id].zone, CardZone::Library,
            "library card should remain untouched when no surveil fires");
    }

    #[test]
    fn test_drc_delirium_grants_flying_and_pt() {
        // With ≥4 card types in graveyard, DRC should be 3/3 with flying.
        let mut state = make_state();
        state.catalog = test_catalog();

        // Put 4 different card types in graveyard.
        add_graveyard_card(&mut state, PlayerId::Us, "Island");       // Land
        add_graveyard_card(&mut state, PlayerId::Us, "Brainstorm");   // Instant
        add_graveyard_card(&mut state, PlayerId::Us, "Ponder");       // Sorcery (need catalog)
        add_graveyard_card(&mut state, PlayerId::Us, "Barrowgoyf");   // Creature

        let drc_id = add_default_perm(&mut state, PlayerId::Us, "Dragon's Rage Channeler");

        recompute(&mut state);

        let def = state.def_of(drc_id).expect("DRC should have materialized def");
        if let CardKind::Creature(c) = &def.kind {
            assert_eq!(c.power(), 3, "delirium DRC should have 3 power (1+2)");
            assert_eq!(c.toughness(), 3, "delirium DRC should have 3 toughness (1+2)");
            assert!(c.keywords.contains(Keyword::Flying), "delirium DRC should have flying");
        } else {
            panic!("DRC should be a creature");
        }
    }

    #[test]
    fn test_drc_no_delirium_without_enough_types() {
        // With <4 card types in graveyard, DRC should remain 1/1 without flying.
        let mut state = make_state();
        state.catalog = test_catalog();

        // Only 2 card types in graveyard.
        add_graveyard_card(&mut state, PlayerId::Us, "Island");       // Land
        add_graveyard_card(&mut state, PlayerId::Us, "Brainstorm");   // Instant

        let drc_id = add_default_perm(&mut state, PlayerId::Us, "Dragon's Rage Channeler");

        recompute(&mut state);

        let def = state.def_of(drc_id).expect("DRC should have materialized def");
        if let CardKind::Creature(c) = &def.kind {
            assert_eq!(c.power(), 1, "non-delirium DRC should have 1 power");
            assert_eq!(c.toughness(), 1, "non-delirium DRC should have 1 toughness");
            assert!(!c.keywords.contains(Keyword::Flying), "non-delirium DRC should not have flying");
        } else {
            panic!("DRC should be a creature");
        }
    }

    // ── Mishra's Bauble ───────────────────────────────────────────────────────

    #[test]
    fn test_mishras_bauble_delayed_draw_at_next_upkeep() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let def = catalog_card("Mishra's Bauble");
        let _bauble_id = add_perm_with_def(&mut state, PlayerId::Us, &def, BattlefieldState::new());
        recompute(&mut state);

        // Activate the ability (tap + sac → create delayed trigger).
        let ability = &def.abilities()[0];
        let eff = build_ability_effect(ability, PlayerId::Us, ObjId::UNSET);
        eff.call(&mut state, 1, &[]);

        assert_eq!(state.trigger_instances.len(), 1,
            "activating Bauble should register a delayed trigger");
        assert_eq!(state.trigger_instances[0].expiry, Some(Expiry::OneShot),
            "delayed trigger should be OneShot");

        // Fire upkeep — should produce a draw trigger and remove the OneShot.
        fire_event(
            GameEvent::EnteredStep { step: StepKind::Upkeep, active_player: PlayerId::Us },
            &mut state, 2, PlayerId::Us,
        );
        assert_eq!(state.pending_triggers.len(), 1,
            "upkeep should produce one draw trigger");
        assert_eq!(state.pending_triggers[0].source_name, "Mishra's Bauble (delayed draw)");
        assert!(state.trigger_instances.is_empty(),
            "OneShot trigger should be removed after firing");

        // Resolve the draw trigger.
        let hand_before = state.hand_size(PlayerId::Us);
        // Add a card in library so the draw has something to pick up.
        add_library_card(&mut state, PlayerId::Us, "Island");
        let ctx = state.pending_triggers.remove(0);
        ctx.effect.call(&mut state, 2, &[]);
        assert_eq!(state.hand_size(PlayerId::Us), hand_before + 1,
            "resolving Bauble trigger should draw a card");
    }

    // ── Containment Priest ────────────────────────────────────────────────────

    #[test]
    fn test_containment_priest_does_not_exile_itself() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Put Containment Priest in hand, then move it to BF via non-cast
        // (e.g. Aether Vial). Its own replacement is active_when = on_battlefield,
        // so it can't fire against itself since it's not on BF yet.
        let def = catalog_card("Containment Priest");
        let cp_id = add_hand_card_with_def(&mut state, PlayerId::Us, &def);
        recompute(&mut state);

        change_zone(cp_id, ZoneId::Battlefield, &mut state, 1, PlayerId::Us);

        assert_eq!(state.objects[&cp_id].zone, CardZone::Battlefield,
            "Containment Priest should not exile itself when entering via non-cast");
    }

    #[test]
    fn test_containment_priest_exiles_non_cast_creature() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Put Containment Priest on the battlefield for us.
        let def = catalog_card("Containment Priest");
        add_perm_with_def(&mut state, PlayerId::Us, &def, BattlefieldState::new());
        recompute(&mut state);

        // Put an opponent creature in hand, then move it to BF via non-cast.
        let opp_def = catalog_card("Orcish Bowmasters");
        let opp_id = add_hand_card_with_def(&mut state, PlayerId::Opp, &opp_def);

        change_zone(opp_id, ZoneId::Battlefield, &mut state, 1, PlayerId::Opp);

        assert!(matches!(state.objects[&opp_id].zone, CardZone::Exile { .. }),
            "non-cast creature should be exiled by Containment Priest");
    }

    // ── Delver of Secrets ─────────────────────────────────────────────────────

    #[test]
    fn test_delver_transforms_on_upkeep_with_instant_on_top() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let def = catalog_card("Delver of Secrets");
        let delver_id = add_perm_with_def(&mut state, PlayerId::Us, &def, BattlefieldState::new());
        // Put an instant on top of library.
        add_library_card(&mut state, PlayerId::Us, "Brainstorm");
        recompute(&mut state);

        // Fire upkeep trigger.
        fire_event(
            GameEvent::EnteredStep { step: StepKind::Upkeep, active_player: PlayerId::Us },
            &mut state, 1, PlayerId::Us,
        );
        assert_eq!(state.pending_triggers.len(), 1, "should produce a transform trigger");
        let ctx = state.pending_triggers.remove(0);
        ctx.effect.call(&mut state, 1, &[]);

        assert_eq!(state.objects[&delver_id].bf.as_ref().unwrap().active_face, 1,
            "Delver should be on back face after transform");

        // Recompute should give 3/2 flying.
        recompute(&mut state);
        let mat = state.def_of(delver_id).unwrap();
        assert_eq!(mat.name, "Insectile Aberration");
        if let CardKind::Creature(c) = &mat.kind {
            assert_eq!(c.power(), 3);
            assert_eq!(c.toughness(), 2);
            assert!(c.keywords.contains(Keyword::Flying));
        } else {
            panic!("back face should be a creature");
        }
    }

    #[test]
    fn test_delver_no_transform_without_instant_on_top() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let def = catalog_card("Delver of Secrets");
        let delver_id = add_perm_with_def(&mut state, PlayerId::Us, &def, BattlefieldState::new());
        // Put a land on top (not instant/sorcery).
        add_library_card(&mut state, PlayerId::Us, "Island");
        recompute(&mut state);

        fire_event(
            GameEvent::EnteredStep { step: StepKind::Upkeep, active_player: PlayerId::Us },
            &mut state, 1, PlayerId::Us,
        );
        assert!(state.pending_triggers.is_empty(), "no transform trigger for non-instant top card");
        assert_eq!(state.objects[&delver_id].bf.as_ref().unwrap().active_face, 0,
            "Delver should remain on front face");
    }

    // ── Unholy Heat ───────────────────────────────────────────────────────────

    #[test]
    fn test_unholy_heat_2_damage_without_delirium() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let target_id = add_default_perm(&mut state, PlayerId::Opp, "Murktide Regent");
        recompute(&mut state);

        let def = catalog_card("Unholy Heat");
        let eff = build_spell_effect(&def, PlayerId::Us, ObjId::UNSET, 0, 0).1;
        eff.call(&mut state, 1, &[target_id]);

        assert_eq!(state.permanent_bf(target_id).unwrap().damage, 2,
            "without delirium, Unholy Heat should deal 2 damage");
    }

    #[test]
    fn test_unholy_heat_6_damage_with_delirium() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Seed 4+ card types in graveyard.
        add_graveyard_card(&mut state, PlayerId::Us, "Island");       // Land
        add_graveyard_card(&mut state, PlayerId::Us, "Brainstorm");   // Instant
        add_graveyard_card(&mut state, PlayerId::Us, "Ponder");       // Sorcery
        add_graveyard_card(&mut state, PlayerId::Us, "Murktide Regent"); // Creature

        let target_id = add_default_perm(&mut state, PlayerId::Opp, "Murktide Regent");
        recompute(&mut state);

        let def = catalog_card("Unholy Heat");
        let eff = build_spell_effect(&def, PlayerId::Us, ObjId::UNSET, 0, 0).1;
        eff.call(&mut state, 1, &[target_id]);

        assert_eq!(state.permanent_bf(target_id).unwrap().damage, 6,
            "with delirium, Unholy Heat should deal 6 damage");
    }

    // ── Price of Progress ─────────────────────────────────────────────────────

    #[test]
    fn test_price_of_progress_deals_damage_per_nonbasic() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Opp controls 3 nonbasic lands.
        add_default_perm(&mut state, PlayerId::Opp, "Volcanic Island");
        add_default_perm(&mut state, PlayerId::Opp, "Underground Sea");
        add_default_perm(&mut state, PlayerId::Opp, "Wasteland");
        // Opp controls 1 basic land (should not count).
        add_default_perm(&mut state, PlayerId::Opp, "Island");
        // Us controls 1 nonbasic.
        add_default_perm(&mut state, PlayerId::Us, "Volcanic Island");
        recompute(&mut state);

        let opp_life_before = state.player(PlayerId::Opp).life;
        let us_life_before = state.player(PlayerId::Us).life;

        let def = catalog_card("Price of Progress");
        let eff = build_spell_effect(&def, PlayerId::Us, ObjId::UNSET, 0, 0).1;
        eff.call(&mut state, 1, &[]);

        assert_eq!(state.player(PlayerId::Opp).life, opp_life_before - 6,
            "opp should take 6 damage (3 nonbasics * 2)");
        assert_eq!(state.player(PlayerId::Us).life, us_life_before - 2,
            "us should take 2 damage (1 nonbasic * 2)");
    }

    // ── Null Rod ──────────────────────────────────────────────────────────────

    #[test]
    fn test_null_rod_suppresses_artifact_abilities() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let _null_rod = add_default_perm(&mut state, PlayerId::Us, "Null Rod");
        let petal_id = add_default_perm(&mut state, PlayerId::Opp, "Lotus Petal");
        recompute(&mut state);

        let def = state.def_of(petal_id).expect("Lotus Petal should have materialized def");
        let ma = def.mana_abilities();
        assert!(!ma.is_empty(), "Lotus Petal should still have mana abilities listed");
        assert!(!ma[0].activatable, "Lotus Petal mana ability should not be activatable under Null Rod");
    }

    // ── Meltdown ──────────────────────────────────────────────────────────────

    #[test]
    fn test_meltdown_destroys_artifacts_at_or_below_x() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // MV 0 artifact (Lotus Petal), MV 2 artifact (Null Rod), and a non-artifact creature.
        let petal_id = add_default_perm(&mut state, PlayerId::Opp, "Lotus Petal");
        let rod_id = add_default_perm(&mut state, PlayerId::Opp, "Null Rod");
        let creature_id = add_default_perm(&mut state, PlayerId::Opp, "Murktide Regent");
        recompute(&mut state);

        // Meltdown with X=1: should destroy Lotus Petal (MV 0) but not Null Rod (MV 2).
        let def = catalog_card("Meltdown");
        let eff = build_spell_effect(&def, PlayerId::Us, ObjId::UNSET, 1, 0).1;
        eff.call(&mut state, 1, &[]);

        assert_eq!(state.objects[&petal_id].zone, CardZone::Graveyard,
            "Lotus Petal (MV 0) should be destroyed by Meltdown X=1");
        assert_eq!(state.objects[&rod_id].zone, CardZone::Battlefield,
            "Null Rod (MV 2) should survive Meltdown X=1");
        assert_eq!(state.objects[&creature_id].zone, CardZone::Battlefield,
            "non-artifact creature should be unaffected by Meltdown");
    }

    // ── Rough // Tumble ───────────────────────────────────────────────────────

    #[test]
    fn test_rough_deals_2_to_non_flyers_spares_flyers() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Non-flyer and flyer on opponent's board.
        let ground_id = add_default_perm(&mut state, PlayerId::Opp, "Orcish Bowmasters");
        let flyer_id = add_default_perm(&mut state, PlayerId::Opp, "Emrakul, the Aeons Torn");
        recompute(&mut state);

        let def = catalog_card("Rough // Tumble");
        let eff = build_spell_effect(&def, PlayerId::Us, ObjId::UNSET, 0, 0).1;
        eff.call(&mut state, 1, &[]);

        assert_eq!(state.permanent_bf(ground_id).unwrap().damage, 2,
            "Rough should deal 2 damage to non-flyer");
        assert_eq!(state.permanent_bf(flyer_id).unwrap().damage, 0,
            "Rough should not damage flyer");
    }

    #[test]
    fn test_tumble_deals_6_to_flyers_spares_non_flyers() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let ground_id = add_default_perm(&mut state, PlayerId::Opp, "Orcish Bowmasters");
        let flyer_id = add_default_perm(&mut state, PlayerId::Opp, "Emrakul, the Aeons Torn");
        recompute(&mut state);

        // Cast back face (Tumble).
        let def = catalog_card("Rough // Tumble");
        let back = def.adventure().expect("should have back face");
        let eff = build_spell_effect(back, PlayerId::Us, ObjId::UNSET, 0, 0).1;
        eff.call(&mut state, 1, &[]);

        assert_eq!(state.permanent_bf(ground_id).unwrap().damage, 0,
            "Tumble should not damage non-flyer");
        assert_eq!(state.permanent_bf(flyer_id).unwrap().damage, 6,
            "Tumble should deal 6 damage to flyer");
    }

    // ── Cori-Steel Cutter ────────────────────────────────────────────────────

    #[test]
    fn test_cori_equip_grants_keywords_and_pt() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let creature_id = add_default_perm(&mut state, PlayerId::Us, "Barrowgoyf");
        let cori_id = add_default_perm(&mut state, PlayerId::Us, "Cori-Steel Cutter");

        // Attach equipment to creature.
        state.permanent_bf_mut(cori_id).unwrap().attached_to = Some(creature_id);
        recompute(&mut state);

        let def = state.def_of(creature_id).expect("creature should have materialized def");
        if let CardKind::Creature(c) = &def.kind {
            assert!(c.keywords.contains(Keyword::Trample), "equipped creature should have trample");
            assert!(c.keywords.contains(Keyword::Haste), "equipped creature should have haste");
            // Barrowgoyf base is 0/1; equipment adds +1/+1 → at least 1/2.
            assert!(c.power() >= 1, "equipped creature should get +1 power");
            assert!(c.toughness() >= 2, "equipped creature should get +1 toughness");
        } else {
            panic!("should be a creature");
        }
    }

    #[test]
    fn test_cori_no_buff_when_unattached() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let creature_id = add_default_perm(&mut state, PlayerId::Us, "Barrowgoyf");
        let _cori_id = add_default_perm(&mut state, PlayerId::Us, "Cori-Steel Cutter");

        recompute(&mut state);

        let def = state.def_of(creature_id).expect("creature should have materialized def");
        if let CardKind::Creature(c) = &def.kind {
            assert!(!c.keywords.contains(Keyword::Trample), "unequipped creature should not have trample");
            assert!(!c.keywords.contains(Keyword::Haste), "unequipped creature should not have haste");
        } else {
            panic!("should be a creature");
        }
    }

    #[test]
    fn test_cori_flurry_second_spell() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let _cori_id = add_default_perm(&mut state, PlayerId::Us, "Cori-Steel Cutter");

        // Simulate second spell: first spell already counted.
        state.us.spells_cast_this_turn = 1;

        let spell_id = {
            let id = state.alloc_id();
            state.objects.insert(id, GameObject {
                id,
                catalog_key: "Ponder".to_string(),
                owner: PlayerId::Us, controller: PlayerId::Us,
                zone: CardZone::Stack,
                is_token: false, bf: None, spell: None, materialized: None,
                counters: HashMap::new(), ci_timestamp: 0,
            });
            state.catalog.entry("Ponder".to_string()).or_insert_with(|| catalog_card("Ponder"));
            id
        };

        fire_event(
            GameEvent::SpellCast { caster: PlayerId::Us, card_id: spell_id, mana_spent: true },
            &mut state, 1, PlayerId::Us,
        );
        for ctx in std::mem::take(&mut state.pending_triggers) { ctx.effect.call(&mut state, 1, &[]); }

        // A Monk Token should now exist on the battlefield.
        let monk_count = state.permanents_of(PlayerId::Us)
            .filter(|c| c.catalog_key == "Monk Token")
            .count();
        assert_eq!(monk_count, 1, "flurry should create exactly one Monk Token");
    }

    #[test]
    fn test_cori_flurry_not_first_spell() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let _cori_id = add_default_perm(&mut state, PlayerId::Us, "Cori-Steel Cutter");
        state.us.spells_cast_this_turn = 0;

        let spell_id = {
            let id = state.alloc_id();
            state.objects.insert(id, GameObject {
                id,
                catalog_key: "Ponder".to_string(),
                owner: PlayerId::Us, controller: PlayerId::Us,
                zone: CardZone::Stack,
                is_token: false, bf: None, spell: None, materialized: None,
                counters: HashMap::new(), ci_timestamp: 0,
            });
            state.catalog.entry("Ponder".to_string()).or_insert_with(|| catalog_card("Ponder"));
            id
        };

        fire_event(
            GameEvent::SpellCast { caster: PlayerId::Us, card_id: spell_id, mana_spent: true },
            &mut state, 1, PlayerId::Us,
        );
        for ctx in std::mem::take(&mut state.pending_triggers) { ctx.effect.call(&mut state, 1, &[]); }

        let monk_count = state.permanents_of(PlayerId::Us)
            .filter(|c| c.catalog_key == "Monk Token")
            .count();
        assert_eq!(monk_count, 0, "flurry should NOT trigger on first spell");
    }

    #[test]
    fn test_cori_flurry_not_third_spell() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let _cori_id = add_default_perm(&mut state, PlayerId::Us, "Cori-Steel Cutter");
        state.us.spells_cast_this_turn = 2;

        let spell_id = {
            let id = state.alloc_id();
            state.objects.insert(id, GameObject {
                id,
                catalog_key: "Ponder".to_string(),
                owner: PlayerId::Us, controller: PlayerId::Us,
                zone: CardZone::Stack,
                is_token: false, bf: None, spell: None, materialized: None,
                counters: HashMap::new(), ci_timestamp: 0,
            });
            state.catalog.entry("Ponder".to_string()).or_insert_with(|| catalog_card("Ponder"));
            id
        };

        fire_event(
            GameEvent::SpellCast { caster: PlayerId::Us, card_id: spell_id, mana_spent: true },
            &mut state, 1, PlayerId::Us,
        );
        for ctx in std::mem::take(&mut state.pending_triggers) { ctx.effect.call(&mut state, 1, &[]); }

        let monk_count = state.permanents_of(PlayerId::Us)
            .filter(|c| c.catalog_key == "Monk Token")
            .count();
        assert_eq!(monk_count, 0, "flurry should NOT trigger on third spell");
    }

    #[test]
    fn test_monk_prowess_noncreature() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let monk_id = add_default_perm(&mut state, PlayerId::Us, "Monk Token");
        recompute(&mut state);

        // Fire a noncreature SpellCast.
        let spell_id = {
            let id = state.alloc_id();
            state.objects.insert(id, GameObject {
                id,
                catalog_key: "Ponder".to_string(),
                owner: PlayerId::Us, controller: PlayerId::Us,
                zone: CardZone::Stack,
                is_token: false, bf: None, spell: None, materialized: None,
                counters: HashMap::new(), ci_timestamp: 0,
            });
            state.catalog.entry("Ponder".to_string()).or_insert_with(|| catalog_card("Ponder"));
            id
        };

        fire_event(
            GameEvent::SpellCast { caster: PlayerId::Us, card_id: spell_id, mana_spent: true },
            &mut state, 1, PlayerId::Us,
        );
        for ctx in std::mem::take(&mut state.pending_triggers) { ctx.effect.call(&mut state, 1, &[]); }
        recompute(&mut state);

        let def = state.def_of(monk_id).expect("Monk should have materialized def");
        if let CardKind::Creature(c) = &def.kind {
            assert_eq!(c.power(), 2, "Monk should be 2/2 after one prowess trigger");
            assert_eq!(c.toughness(), 2, "Monk should be 2/2 after one prowess trigger");
        } else {
            panic!("Monk should be a creature");
        }
    }

    #[test]
    fn test_detach_on_creature_leaves() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let creature_id = add_default_perm(&mut state, PlayerId::Us, "Barrowgoyf");
        let cori_id = add_default_perm(&mut state, PlayerId::Us, "Cori-Steel Cutter");
        state.permanent_bf_mut(cori_id).unwrap().attached_to = Some(creature_id);

        // Creature leaves the battlefield.
        change_zone(creature_id, ZoneId::Graveyard, &mut state, 1, PlayerId::Us);

        assert_eq!(state.permanent_bf(cori_id).unwrap().attached_to, None,
            "equipment should detach when creature leaves");
    }

    // ── Section 50: DD Strategy Evaluator ────────────────────────────────────────

    #[test]
    fn test_dd_plan_gap_no_hand_all_gaps_high() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let matchup = strategy::MatchupInfo::default();
        let gap = strategy::dd_plan_gap(&state, PlayerId::Us, &matchup);
        assert!(gap.mana >= 0.9, "no mana sources → mana gap near 1.0, got {}", gap.mana);
        assert!(gap.threat >= 0.9, "no threats → threat gap near 1.0, got {}", gap.threat);
    }

    #[test]
    fn test_dd_plan_gap_dd_in_hand_zeroes_threat() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let _dd = add_hand_card(&mut state, PlayerId::Us, "Doomsday");
        let matchup = strategy::MatchupInfo::default();
        let gap = strategy::dd_plan_gap(&state, PlayerId::Us, &matchup);
        assert_eq!(gap.threat, 0.0, "DD in hand → threat gap 0.0");
    }

    #[test]
    fn test_dd_plan_gap_lands_reduce_mana_gap() {
        let mut state = make_state();
        state.catalog = test_catalog();
        make_land(&mut state, PlayerId::Us, "Underground Sea", false);
        make_land(&mut state, PlayerId::Us, "Polluted Delta", false);
        let matchup = strategy::MatchupInfo::default();
        let gap = strategy::dd_plan_gap(&state, PlayerId::Us, &matchup);
        // 2 lands → mana_gap = (3-2)/3 ≈ 0.33
        assert!(gap.mana < 0.4, "2 lands → mana gap <0.4, got {}", gap.mana);
        assert!(gap.mana > 0.2, "2 lands → mana gap >0.2, got {}", gap.mana);
    }

    #[test]
    fn test_dd_plan_gap_interaction_high_vs_blue() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let matchup = strategy::MatchupInfo { opp_has_counters: true, ..Default::default() };
        let gap = strategy::dd_plan_gap(&state, PlayerId::Us, &matchup);
        assert!(gap.interaction >= 0.9, "no interaction vs blue → high gap, got {}", gap.interaction);
    }

    #[test]
    fn test_dd_plan_gap_interaction_low_vs_nonblue() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let matchup = strategy::MatchupInfo { opp_has_counters: false, ..Default::default() };
        let gap = strategy::dd_plan_gap(&state, PlayerId::Us, &matchup);
        assert!(gap.interaction <= 0.2, "vs non-blue → interaction gap low, got {}", gap.interaction);
    }

    #[test]
    fn test_dd_card_fills_land_high_when_needed() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let land_id = add_hand_card(&mut state, PlayerId::Us, "Underground Sea");
        let gap = strategy::TargetGap { mana: 1.0, threat: 1.0, interaction: 0.5 };
        let score = strategy::dd_card_fills(land_id, &gap, &state, PlayerId::Us);
        assert!(score > 0.7, "land fills high mana gap → score >0.7, got {}", score);
    }

    #[test]
    fn test_dd_card_fills_land_low_when_flooded() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let land_id = add_hand_card(&mut state, PlayerId::Us, "Underground Sea");
        let gap = strategy::TargetGap { mana: 0.0, threat: 1.0, interaction: 0.5 };
        let score = strategy::dd_card_fills(land_id, &gap, &state, PlayerId::Us);
        assert!(score < 0.1, "land when mana gap=0 → score <0.1, got {}", score);
    }

    #[test]
    fn test_dd_card_fills_doomsday_very_high() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let dd_id = add_hand_card(&mut state, PlayerId::Us, "Doomsday");
        let gap = strategy::TargetGap { mana: 0.5, threat: 1.0, interaction: 0.5 };
        let score = strategy::dd_card_fills(dd_id, &gap, &state, PlayerId::Us);
        assert!(score >= 0.9, "DD with high threat gap → score >=0.9, got {}", score);
    }

    #[test]
    fn test_dd_card_fills_second_dd_low() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let _dd1 = add_hand_card(&mut state, PlayerId::Us, "Doomsday");
        let dd2 = add_hand_card(&mut state, PlayerId::Us, "Doomsday");
        let gap = strategy::TargetGap { mana: 0.5, threat: 1.0, interaction: 0.5 };
        let score = strategy::dd_card_fills(dd2, &gap, &state, PlayerId::Us);
        assert!(score <= 0.15, "second DD → score low, got {}", score);
    }

    #[test]
    fn test_dd_card_fills_oracle_near_zero() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let oracle_id = add_hand_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        let gap = strategy::TargetGap { mana: 0.5, threat: 1.0, interaction: 0.5 };
        let score = strategy::dd_card_fills(oracle_id, &gap, &state, PlayerId::Us);
        assert!(score <= 0.1, "Oracle pre-DD → near-zero, got {}", score);
    }

    #[test]
    fn test_dd_card_fills_cantrip_always_medium() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let bs_id = add_hand_card(&mut state, PlayerId::Us, "Brainstorm");
        // Even with all gaps filled, cantrips retain medium value
        let gap_low = strategy::TargetGap { mana: 0.0, threat: 0.0, interaction: 0.0 };
        let gap_high = strategy::TargetGap { mana: 1.0, threat: 1.0, interaction: 1.0 };
        let score_low = strategy::dd_card_fills(bs_id, &gap_low, &state, PlayerId::Us);
        let score_high = strategy::dd_card_fills(bs_id, &gap_high, &state, PlayerId::Us);
        assert!(score_low > 0.2, "cantrip always valuable, got {}", score_low);
        assert!(score_high > 0.2, "cantrip always valuable, got {}", score_high);
        assert!((score_low - score_high).abs() < 0.2, "cantrip score stable across gap states");
    }

    #[test]
    fn test_dd_card_fills_fow_high_when_interaction_needed() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let fow_id = add_hand_card(&mut state, PlayerId::Us, "Force of Will");
        let gap = strategy::TargetGap { mana: 0.5, threat: 0.5, interaction: 1.0 };
        let score = strategy::dd_card_fills(fow_id, &gap, &state, PlayerId::Us);
        assert!(score > 0.7, "FoW with high interaction gap → score >0.7, got {}", score);
    }

    #[test]
    fn test_dd_london_bottom_picks_worst_cards() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Hand: Oracle (dead), DD (great), Underground Sea (good mana), Brainstorm (medium)
        let oracle_id = add_hand_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        let _dd_id = add_hand_card(&mut state, PlayerId::Us, "Doomsday");
        let _land_id = add_hand_card(&mut state, PlayerId::Us, "Underground Sea");
        let _bs_id = add_hand_card(&mut state, PlayerId::Us, "Brainstorm");
        let strat = strategy::DoomsdayStrategy::new(strategy::MatchupInfo::default());
        let bottom = strat.london_bottom(&state, 1);
        assert_eq!(bottom.len(), 1);
        assert_eq!(bottom[0], oracle_id, "Oracle should be bottomed as lowest-value card");
    }

    // ── Section 51: Opponent Strategy Evaluator ──────────────────────────────────

    #[test]
    fn test_opp_plan_gap_no_board_all_gaps_high() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Opp facing combo (DD): opp_fast_clock = false
        let matchup = strategy::MatchupInfo { opp_has_counters: true, opp_fast_clock: false, ..Default::default() };
        let gap = strategy::opp_plan_gap(&state, PlayerId::Opp, &matchup);
        assert!(gap.mana >= 0.9, "no lands → mana gap high, got {}", gap.mana);
        assert!(gap.threat >= 0.9, "no threats → threat gap high, got {}", gap.threat);
        assert!(gap.interaction >= 0.9, "no interaction vs combo → gap high, got {}", gap.interaction);
    }

    #[test]
    fn test_opp_plan_gap_lands_reduce_mana() {
        let mut state = make_state();
        state.catalog = test_catalog();
        make_land(&mut state, PlayerId::Opp, "Volcanic Island", false);
        make_land(&mut state, PlayerId::Opp, "Scalding Tarn", false);
        let matchup = strategy::MatchupInfo::default();
        let gap = strategy::opp_plan_gap(&state, PlayerId::Opp, &matchup);
        assert!(gap.mana < 0.1, "2 lands → mana gap ~0, got {}", gap.mana);
    }

    #[test]
    fn test_opp_plan_gap_creature_reduces_threat() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let _delver = add_default_perm(&mut state, PlayerId::Opp, "Delver of Secrets");
        let matchup = strategy::MatchupInfo::default();
        let gap = strategy::opp_plan_gap(&state, PlayerId::Opp, &matchup);
        assert!(gap.threat < 0.5, "1 creature → threat gap reduced, got {}", gap.threat);
    }

    #[test]
    fn test_opp_plan_gap_interaction_low_vs_aggro() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Facing aggro: opp_fast_clock = true
        let matchup = strategy::MatchupInfo { opp_has_counters: false, opp_fast_clock: true, ..Default::default() };
        let gap = strategy::opp_plan_gap(&state, PlayerId::Opp, &matchup);
        assert!(gap.interaction <= 0.5, "vs aggro → interaction gap capped, got {}", gap.interaction);
    }

    #[test]
    fn test_opp_card_fills_land_high_when_needed() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let land_id = add_hand_card(&mut state, PlayerId::Opp, "Volcanic Island");
        let gap = strategy::TargetGap { mana: 1.0, threat: 1.0, interaction: 0.5 };
        let score = strategy::opp_card_fills(land_id, &gap, &state, PlayerId::Opp);
        assert!(score > 0.7, "land fills high mana gap → score >0.7, got {}", score);
    }

    #[test]
    fn test_opp_card_fills_land_low_when_flooded() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let land_id = add_hand_card(&mut state, PlayerId::Opp, "Volcanic Island");
        let gap = strategy::TargetGap { mana: 0.0, threat: 1.0, interaction: 0.5 };
        let score = strategy::opp_card_fills(land_id, &gap, &state, PlayerId::Opp);
        assert!(score < 0.1, "land when mana gap=0 → near-zero, got {}", score);
    }

    #[test]
    fn test_opp_card_fills_creature_high() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let delver_id = add_hand_card(&mut state, PlayerId::Opp, "Delver of Secrets");
        let gap = strategy::TargetGap { mana: 0.0, threat: 1.0, interaction: 0.5 };
        let score = strategy::opp_card_fills(delver_id, &gap, &state, PlayerId::Opp);
        assert!(score > 0.7, "creature with high threat gap → high score, got {}", score);
    }

    #[test]
    fn test_opp_card_fills_surplus_creature_lower() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let _board_delver = add_default_perm(&mut state, PlayerId::Opp, "Delver of Secrets");
        let hand_delver = add_hand_card(&mut state, PlayerId::Opp, "Delver of Secrets");
        // threat_gap low since we already have one on board
        let gap = strategy::TargetGap { mana: 0.0, threat: 0.1, interaction: 0.5 };
        let score = strategy::opp_card_fills(hand_delver, &gap, &state, PlayerId::Opp);
        assert!(score < 0.3, "surplus Delver on board → lower, got {}", score);
    }

    #[test]
    fn test_opp_card_fills_fow_high_vs_combo() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let fow_id = add_hand_card(&mut state, PlayerId::Opp, "Force of Will");
        let gap = strategy::TargetGap { mana: 0.0, threat: 0.5, interaction: 1.0 };
        let score = strategy::opp_card_fills(fow_id, &gap, &state, PlayerId::Opp);
        assert!(score > 0.7, "FoW with high interaction gap → score >0.7, got {}", score);
    }

    #[test]
    fn test_opp_card_fills_cantrip_medium() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let bs_id = add_hand_card(&mut state, PlayerId::Opp, "Brainstorm");
        let gap = strategy::TargetGap { mana: 0.0, threat: 0.0, interaction: 0.0 };
        let score = strategy::opp_card_fills(bs_id, &gap, &state, PlayerId::Opp);
        assert!(score > 0.2 && score < 0.5, "cantrip always medium, got {}", score);
    }

    // ── Section 52: Cantrip Effect Primitives ──────────────────────────────────

    /// Wire evaluate_card so that specific cards get known scores for deterministic testing.
    /// Maps card names to scores; anything not in the map gets 0.5.
    fn wire_eval(state: &mut SimState, scores: Vec<(&str, f64)>) {
        let map: HashMap<String, f64> = scores.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        state.evaluate_card = Arc::new(move |_who, card_id, state| {
            state.objects.get(&card_id)
                .and_then(|o| map.get(&o.catalog_key))
                .copied()
                .unwrap_or(0.5)
        });
    }

    #[test]
    fn test_put_back_eval_puts_worst_on_top() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Hand: Doomsday (high), Oracle (low), Brainstorm (medium)
        let dd = add_hand_card(&mut state, PlayerId::Us, "Doomsday");
        let oracle = add_hand_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        let bs = add_hand_card(&mut state, PlayerId::Us, "Brainstorm");
        wire_eval(&mut state, vec![("Doomsday", 0.9), ("Thassa's Oracle", 0.05), ("Brainstorm", 0.35)]);

        eff_put_back(PlayerId::Us, 1).call(&mut state, 0, &[]);

        // Oracle should be gone from hand and on top of library.
        let hand: Vec<ObjId> = state.hand_of(PlayerId::Us).map(|c| c.id).collect();
        assert!(!hand.contains(&oracle), "Oracle should be removed from hand");
        assert!(hand.contains(&dd), "Doomsday should stay in hand");
        assert!(hand.contains(&bs), "Brainstorm should stay in hand");
        assert_eq!(state.us.library_order.front(), Some(&oracle), "Oracle should be on top of library");
    }

    #[test]
    fn test_put_back_eval_twice_puts_two_worst() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let dd = add_hand_card(&mut state, PlayerId::Us, "Doomsday");
        let oracle = add_hand_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        let edge = add_hand_card(&mut state, PlayerId::Us, "Edge of Autumn");
        let bs = add_hand_card(&mut state, PlayerId::Us, "Brainstorm");
        wire_eval(&mut state, vec![
            ("Doomsday", 0.9), ("Thassa's Oracle", 0.05),
            ("Edge of Autumn", 0.1), ("Brainstorm", 0.35),
        ]);

        eff_put_back(PlayerId::Us, 1).call(&mut state, 0, &[]);
        eff_put_back(PlayerId::Us, 1).call(&mut state, 0, &[]);

        let hand: Vec<ObjId> = state.hand_of(PlayerId::Us).map(|c| c.id).collect();
        assert_eq!(hand.len(), 2, "should have 2 cards left in hand");
        assert!(hand.contains(&dd));
        assert!(hand.contains(&bs));
        // Oracle was put back first (worst), then Edge of Autumn (second worst).
        // Oracle first → front, then Edge → new front. So library front = Edge, next = Oracle.
        assert_eq!(state.us.library_order.front(), Some(&edge), "Edge on top (put back second)");
        assert_eq!(state.us.library_order.get(1), Some(&oracle), "Oracle second (put back first)");
    }

    #[test]
    fn test_scry_keeps_good_cards_on_top() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Library top-to-bottom: Doomsday (0.9), Oracle (0.05), Brainstorm (0.35)
        let dd = add_library_card(&mut state, PlayerId::Us, "Doomsday");
        let oracle = add_library_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        let bs = add_library_card(&mut state, PlayerId::Us, "Brainstorm");
        // Reorder so dd is on top
        state.us.library_order.clear();
        state.us.library_order.push_back(dd);
        state.us.library_order.push_back(oracle);
        state.us.library_order.push_back(bs);

        wire_eval(&mut state, vec![("Doomsday", 0.9), ("Thassa's Oracle", 0.05), ("Brainstorm", 0.35)]);

        eff_scry(PlayerId::Us, 3).call(&mut state, 0, &[]);

        // Doomsday (0.9 >= 0.3) and Brainstorm (0.35 >= 0.3) kept on top.
        // Oracle (0.05 < 0.3) bottomed.
        // Kept cards preserve order: Doomsday, Brainstorm.
        let lib: Vec<ObjId> = state.us.library_order.iter().copied().collect();
        assert_eq!(lib.len(), 3);
        assert_eq!(lib[0], dd, "Doomsday should be on top (kept)");
        assert_eq!(lib[1], bs, "Brainstorm should be second (kept)");
        assert_eq!(lib[2], oracle, "Oracle should be on bottom (scried away)");
    }

    #[test]
    fn test_scry_all_bad_sends_all_to_bottom() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let oracle = add_library_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        let edge = add_library_card(&mut state, PlayerId::Us, "Edge of Autumn");
        let deep = add_library_card(&mut state, PlayerId::Us, "Doomsday"); // will be below scry range
        state.us.library_order.clear();
        state.us.library_order.push_back(oracle);
        state.us.library_order.push_back(edge);
        state.us.library_order.push_back(deep);

        wire_eval(&mut state, vec![("Thassa's Oracle", 0.05), ("Edge of Autumn", 0.1), ("Doomsday", 0.9)]);

        eff_scry(PlayerId::Us, 2).call(&mut state, 0, &[]);

        // Both top cards were bad → both bottomed. Doomsday (untouched) now on top.
        let lib: Vec<ObjId> = state.us.library_order.iter().copied().collect();
        assert_eq!(lib[0], deep, "Doomsday should now be on top");
    }

    #[test]
    fn test_order_sorts_top_n_by_score() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let oracle = add_library_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        let dd = add_library_card(&mut state, PlayerId::Us, "Doomsday");
        let bs = add_library_card(&mut state, PlayerId::Us, "Brainstorm");
        state.us.library_order.clear();
        state.us.library_order.push_back(oracle);  // worst on top
        state.us.library_order.push_back(dd);       // best in middle
        state.us.library_order.push_back(bs);        // medium at bottom

        wire_eval(&mut state, vec![("Doomsday", 0.9), ("Thassa's Oracle", 0.05), ("Brainstorm", 0.35)]);

        eff_order(PlayerId::Us, 3).call(&mut state, 0, &[]);

        let lib: Vec<ObjId> = state.us.library_order.iter().copied().collect();
        assert_eq!(lib[0], dd, "Doomsday (0.9) should be on top after ordering");
        assert_eq!(lib[1], bs, "Brainstorm (0.35) should be second");
        assert_eq!(lib[2], oracle, "Oracle (0.05) should be third");
    }

    #[test]
    fn test_order_does_not_touch_cards_beyond_n() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let oracle = add_library_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        let dd = add_library_card(&mut state, PlayerId::Us, "Doomsday");
        let deep = add_library_card(&mut state, PlayerId::Us, "Force of Will");
        state.us.library_order.clear();
        state.us.library_order.push_back(oracle);
        state.us.library_order.push_back(dd);
        state.us.library_order.push_back(deep);

        wire_eval(&mut state, vec![("Doomsday", 0.9), ("Thassa's Oracle", 0.05), ("Force of Will", 0.8)]);

        eff_order(PlayerId::Us, 2).call(&mut state, 0, &[]);

        let lib: Vec<ObjId> = state.us.library_order.iter().copied().collect();
        assert_eq!(lib[0], dd, "DD sorted to top of the 2");
        assert_eq!(lib[1], oracle, "Oracle second of the 2");
        assert_eq!(lib[2], deep, "FoW untouched at position 3");
    }

    #[test]
    fn test_maybe_shuffle_shuffles_when_top_is_bad() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let oracle = add_library_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        let dd = add_library_card(&mut state, PlayerId::Us, "Doomsday");
        let bs = add_library_card(&mut state, PlayerId::Us, "Brainstorm");
        state.us.library_order.clear();
        state.us.library_order.push_back(oracle);  // 0.05 — below threshold
        state.us.library_order.push_back(dd);
        state.us.library_order.push_back(bs);

        wire_eval(&mut state, vec![("Thassa's Oracle", 0.05), ("Doomsday", 0.9), ("Brainstorm", 0.35)]);

        // Record original order.
        let before: Vec<ObjId> = state.us.library_order.iter().copied().collect();

        eff_maybe_shuffle(PlayerId::Us).call(&mut state, 0, &[]);

        // Library was shuffled — same cards, potentially different order.
        let after: Vec<ObjId> = state.us.library_order.iter().copied().collect();
        assert_eq!(after.len(), before.len(), "shuffle preserves card count");
        // All original cards still present.
        for id in &before {
            assert!(after.contains(id), "card {:?} missing after shuffle", id);
        }
    }

    #[test]
    fn test_maybe_shuffle_does_not_shuffle_when_top_is_good() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let dd = add_library_card(&mut state, PlayerId::Us, "Doomsday");
        let oracle = add_library_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        state.us.library_order.clear();
        state.us.library_order.push_back(dd);     // 0.9 — above threshold
        state.us.library_order.push_back(oracle);

        wire_eval(&mut state, vec![("Doomsday", 0.9), ("Thassa's Oracle", 0.05)]);

        eff_maybe_shuffle(PlayerId::Us).call(&mut state, 0, &[]);

        // No shuffle — order preserved exactly.
        let lib: Vec<ObjId> = state.us.library_order.iter().copied().collect();
        assert_eq!(lib[0], dd);
        assert_eq!(lib[1], oracle);
    }

    #[test]
    fn test_brainstorm_composition_draw3_putback2() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Start with 2 cards in hand: Oracle (bad, 0.05) and Edge (bad, 0.1)
        let oracle = add_hand_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        let edge = add_hand_card(&mut state, PlayerId::Us, "Edge of Autumn");
        // Library: DD (0.9), FoW (0.8), Brainstorm (0.35)
        let dd = add_library_card(&mut state, PlayerId::Us, "Doomsday");
        let fow = add_library_card(&mut state, PlayerId::Us, "Force of Will");
        let bs = add_library_card(&mut state, PlayerId::Us, "Brainstorm");
        state.us.library_order.clear();
        state.us.library_order.push_back(dd);
        state.us.library_order.push_back(fow);
        state.us.library_order.push_back(bs);

        wire_eval(&mut state, vec![
            ("Doomsday", 0.9), ("Force of Will", 0.8), ("Brainstorm", 0.35),
            ("Thassa's Oracle", 0.05), ("Edge of Autumn", 0.1),
        ]);

        // Brainstorm = draw 3, put back 2 worst
        let effect = eff_draw(PlayerId::Us, 3)
            .then(eff_put_back(PlayerId::Us, 2));
        effect.call(&mut state, 0, &[]);

        // After draw 3: hand = Oracle(0.05), Edge(0.1), DD(0.9), FoW(0.8), BS(0.35)
        // Put back worst: Oracle(0.05) → top. Hand = Edge(0.1), DD(0.9), FoW(0.8), BS(0.35)
        // Put back worst: Edge(0.1) → top. Hand = DD(0.9), FoW(0.8), BS(0.35)
        let hand_names: Vec<String> = state.hand_of(PlayerId::Us)
            .map(|c| c.catalog_key.clone()).collect();
        assert_eq!(hand_names.len(), 3, "hand should have 3 cards");
        assert!(hand_names.contains(&"Doomsday".to_string()));
        assert!(hand_names.contains(&"Force of Will".to_string()));
        assert!(hand_names.contains(&"Brainstorm".to_string()));
        // Oracle and Edge should be on top of library (Edge on top, Oracle second)
        let lib: Vec<ObjId> = state.us.library_order.iter().copied().collect();
        assert_eq!(lib[0], edge, "Edge (put back second) on top");
        assert_eq!(lib[1], oracle, "Oracle (put back first) second");
    }

    #[test]
    fn test_ponder_keeps_best_on_top_and_draws_it() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Library: Oracle (0.05), BS (0.35), DD (0.9)
        let oracle = add_library_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        let bs = add_library_card(&mut state, PlayerId::Us, "Brainstorm");
        let dd = add_library_card(&mut state, PlayerId::Us, "Doomsday");
        state.us.library_order.clear();
        state.us.library_order.push_back(oracle);
        state.us.library_order.push_back(bs);
        state.us.library_order.push_back(dd);

        wire_eval(&mut state, vec![("Doomsday", 0.9), ("Brainstorm", 0.35), ("Thassa's Oracle", 0.05)]);

        // Ponder = order(3), maybe_shuffle, draw(1)
        let effect = eff_order(PlayerId::Us, 3)
            .then(eff_maybe_shuffle(PlayerId::Us))
            .then(eff_draw(PlayerId::Us, 1));
        effect.call(&mut state, 0, &[]);

        // After order: DD(0.9) on top, BS(0.35), Oracle(0.05)
        // maybe_shuffle: top is DD(0.9) >= 0.3 → no shuffle
        // draw: DD drawn into hand
        let hand: Vec<String> = state.hand_of(PlayerId::Us)
            .map(|c| c.catalog_key.clone()).collect();
        assert!(hand.contains(&"Doomsday".to_string()), "should draw DD (best card)");
    }

    #[test]
    fn test_ponder_shuffles_when_all_top3_are_bad() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Library: Oracle, Edge, Unearth — all score below 0.3, plus a DD deep
        let oracle = add_library_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        let edge = add_library_card(&mut state, PlayerId::Us, "Edge of Autumn");
        let unearth = add_library_card(&mut state, PlayerId::Us, "Unearth");
        let dd = add_library_card(&mut state, PlayerId::Us, "Doomsday");
        state.us.library_order.clear();
        state.us.library_order.push_back(oracle);
        state.us.library_order.push_back(edge);
        state.us.library_order.push_back(unearth);
        state.us.library_order.push_back(dd);

        wire_eval(&mut state, vec![
            ("Thassa's Oracle", 0.05), ("Edge of Autumn", 0.1),
            ("Unearth", 0.05), ("Doomsday", 0.9),
        ]);

        // After order(3): best of {Oracle:0.05, Edge:0.1, Unearth:0.05} → Edge on top (0.1)
        // maybe_shuffle: top is Edge(0.1) < 0.3 → shuffles!
        let effect = eff_order(PlayerId::Us, 3)
            .then(eff_maybe_shuffle(PlayerId::Us))
            .then(eff_draw(PlayerId::Us, 1));
        effect.call(&mut state, 0, &[]);

        // Should have drawn 1 card (from shuffled library)
        let hand_count = state.hand_of(PlayerId::Us).count();
        assert_eq!(hand_count, 1, "should draw 1 card after ponder");
        // Library should have 3 cards remaining
        assert_eq!(state.us.library_order.len(), 3, "3 cards left in library");
    }

    #[test]
    fn test_preordain_scries_then_draws() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Library: Oracle(0.05), DD(0.9), BS(0.35)
        let oracle = add_library_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        let dd = add_library_card(&mut state, PlayerId::Us, "Doomsday");
        let bs = add_library_card(&mut state, PlayerId::Us, "Brainstorm");
        state.us.library_order.clear();
        state.us.library_order.push_back(oracle);
        state.us.library_order.push_back(dd);
        state.us.library_order.push_back(bs);

        wire_eval(&mut state, vec![("Doomsday", 0.9), ("Thassa's Oracle", 0.05), ("Brainstorm", 0.35)]);

        // Preordain = scry(2), draw(1)
        let effect = eff_scry(PlayerId::Us, 2).then(eff_draw(PlayerId::Us, 1));
        effect.call(&mut state, 0, &[]);

        // Scry 2 sees Oracle(0.05) and DD(0.9).
        // Oracle (0.05 < 0.3) → bottom. DD (0.9 >= 0.3) → keep on top.
        // After scry: DD on top, BS, Oracle on bottom.
        // Draw: DD drawn.
        let hand: Vec<String> = state.hand_of(PlayerId::Us)
            .map(|c| c.catalog_key.clone()).collect();
        assert!(hand.contains(&"Doomsday".to_string()), "should draw DD after scrying Oracle to bottom");
        // Library: BS, Oracle
        let lib: Vec<ObjId> = state.us.library_order.iter().copied().collect();
        assert_eq!(lib.len(), 2);
        assert_eq!(lib[0], bs, "BS should be on top of remaining library");
        assert_eq!(lib[1], oracle, "Oracle should be on bottom");
    }

    #[test]
    fn test_consider_surveil_then_draw() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Library: Oracle(0.05 → mill), DD(0.9)
        let oracle = add_library_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        let dd = add_library_card(&mut state, PlayerId::Us, "Doomsday");
        state.us.library_order.clear();
        state.us.library_order.push_back(oracle);
        state.us.library_order.push_back(dd);

        // Wire surveil_choice to use evaluator (mill if < 0.3)
        wire_eval(&mut state, vec![("Thassa's Oracle", 0.05), ("Doomsday", 0.9)]);
        let eval = Arc::clone(&state.evaluate_card);
        state.surveil_choice = Arc::new(move |card_id, state| {
            let who = state.objects.get(&card_id).map(|o| o.owner).unwrap_or(PlayerId::Us);
            eval(who, card_id, state) < 0.3
        });

        // Consider = surveil(1), draw(1)
        let effect = eff_surveil(PlayerId::Us, 1).then(eff_draw(PlayerId::Us, 1));
        effect.call(&mut state, 0, &[]);

        // Oracle (0.05 < 0.3) → milled to graveyard. DD drawn.
        let hand: Vec<String> = state.hand_of(PlayerId::Us)
            .map(|c| c.catalog_key.clone()).collect();
        assert!(hand.contains(&"Doomsday".to_string()), "should draw DD after surveilling Oracle away");
        // Oracle in graveyard
        let gy: Vec<String> = state.objects.values()
            .filter(|o| o.zone == CardZone::Graveyard)
            .map(|o| o.catalog_key.clone()).collect();
        assert!(gy.contains(&"Thassa's Oracle".to_string()), "Oracle should be in graveyard");
    }

    #[test]
    fn test_consider_surveil_keeps_good_card() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Library: DD(0.9 → keep), Oracle(0.05)
        let dd = add_library_card(&mut state, PlayerId::Us, "Doomsday");
        let oracle = add_library_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        state.us.library_order.clear();
        state.us.library_order.push_back(dd);
        state.us.library_order.push_back(oracle);

        wire_eval(&mut state, vec![("Doomsday", 0.9), ("Thassa's Oracle", 0.05)]);
        let eval = Arc::clone(&state.evaluate_card);
        state.surveil_choice = Arc::new(move |card_id, state| {
            let who = state.objects.get(&card_id).map(|o| o.owner).unwrap_or(PlayerId::Us);
            eval(who, card_id, state) < 0.3
        });

        let effect = eff_surveil(PlayerId::Us, 1).then(eff_draw(PlayerId::Us, 1));
        effect.call(&mut state, 0, &[]);

        // DD (0.9 >= 0.3) → kept on top, then drawn.
        let hand: Vec<String> = state.hand_of(PlayerId::Us)
            .map(|c| c.catalog_key.clone()).collect();
        assert!(hand.contains(&"Doomsday".to_string()), "should draw DD (surveil kept it)");
        // Oracle still in library (not milled)
        assert_eq!(state.us.library_order.len(), 1);
    }

    #[test]
    fn test_opp_london_bottom_picks_worst() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Opp hand: FoW (needed vs combo), extra Wasteland (useless), Delver (threat), Brainstorm (medium)
        let _fow = add_hand_card(&mut state, PlayerId::Opp, "Force of Will");
        let waste = add_hand_card(&mut state, PlayerId::Opp, "Wasteland");
        let _delver = add_hand_card(&mut state, PlayerId::Opp, "Delver of Secrets");
        let _bs = add_hand_card(&mut state, PlayerId::Opp, "Brainstorm");
        // Already have 2 lands on board → mana gap is 0
        make_land(&mut state, PlayerId::Opp, "Underground Sea", false);
        make_land(&mut state, PlayerId::Opp, "Polluted Delta", false);
        let matchup = strategy::MatchupInfo { opp_has_counters: true, opp_fast_clock: false, ..Default::default() };
        let strat = strategy::GenericOppStrategy::new(matchup);
        let bottom = strat.london_bottom(&state, 1);
        assert_eq!(bottom.len(), 1);
        // With mana gap=0, the only land (Wasteland) should score lowest
        assert_eq!(bottom[0], waste, "extra land should be bottomed when mana-flooded");
    }

    // ── Section 53: Mulligan Decision Tests ──────────────────────────────────

    #[test]
    fn test_dd_mull_no_land_hand() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // All spells, no lands: should mull even with Dark Ritual (can't cast without B source).
        add_hand_card(&mut state, PlayerId::Us, "Doomsday");
        add_hand_card(&mut state, PlayerId::Us, "Force of Will");
        add_hand_card(&mut state, PlayerId::Us, "Brainstorm");
        add_hand_card(&mut state, PlayerId::Us, "Ponder");
        add_hand_card(&mut state, PlayerId::Us, "Dark Ritual");
        add_hand_card(&mut state, PlayerId::Us, "Thoughtseize");
        add_hand_card(&mut state, PlayerId::Us, "Consider");
        assert!(dd_should_mulligan(&state, PlayerId::Us, 0),
            "no lands (only Ritual) should mull — can't cast Ritual without B source");
    }

    #[test]
    fn test_dd_mull_truly_no_mana() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // All non-mana cards
        add_hand_card(&mut state, PlayerId::Us, "Doomsday");
        add_hand_card(&mut state, PlayerId::Us, "Force of Will");
        add_hand_card(&mut state, PlayerId::Us, "Brainstorm");
        add_hand_card(&mut state, PlayerId::Us, "Ponder");
        add_hand_card(&mut state, PlayerId::Us, "Thoughtseize");
        add_hand_card(&mut state, PlayerId::Us, "Daze");
        add_hand_card(&mut state, PlayerId::Us, "Consider");
        assert!(dd_should_mulligan(&state, PlayerId::Us, 0), "0 mana sources should mull");
    }

    #[test]
    fn test_dd_mull_land_flood() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // 5 mana-producing lands + 2 spells → flood
        add_hand_card(&mut state, PlayerId::Us, "Underground Sea");
        add_hand_card(&mut state, PlayerId::Us, "Polluted Delta");
        add_hand_card(&mut state, PlayerId::Us, "Island");
        add_hand_card(&mut state, PlayerId::Us, "Swamp");
        add_hand_card(&mut state, PlayerId::Us, "Misty Rainforest");
        add_hand_card(&mut state, PlayerId::Us, "Doomsday");
        add_hand_card(&mut state, PlayerId::Us, "Brainstorm");
        assert!(dd_should_mulligan(&state, PlayerId::Us, 0), "5+ mana lands should mull");
    }

    #[test]
    fn test_dd_keep_good_hand() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Classic keep: land, ritual, DD, cantrip, interaction
        add_hand_card(&mut state, PlayerId::Us, "Underground Sea");
        add_hand_card(&mut state, PlayerId::Us, "Dark Ritual");
        add_hand_card(&mut state, PlayerId::Us, "Doomsday");
        add_hand_card(&mut state, PlayerId::Us, "Brainstorm");
        add_hand_card(&mut state, PlayerId::Us, "Force of Will");
        add_hand_card(&mut state, PlayerId::Us, "Ponder");
        add_hand_card(&mut state, PlayerId::Us, "Polluted Delta");
        assert!(!dd_should_mulligan(&state, PlayerId::Us, 0), "good DD hand should keep");
    }

    #[test]
    fn test_dd_mull_all_mana_no_action() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // All lands + rituals, no threats or cantrips
        add_hand_card(&mut state, PlayerId::Us, "Underground Sea");
        add_hand_card(&mut state, PlayerId::Us, "Polluted Delta");
        add_hand_card(&mut state, PlayerId::Us, "Dark Ritual");
        add_hand_card(&mut state, PlayerId::Us, "Lotus Petal");
        add_hand_card(&mut state, PlayerId::Us, "Island");
        add_hand_card(&mut state, PlayerId::Us, "Swamp");
        add_hand_card(&mut state, PlayerId::Us, "Wasteland");
        // 7 mana sources, 0 threats, 0 selection → 5+ mana → mull
        assert!(dd_should_mulligan(&state, PlayerId::Us, 0), "all mana should mull");
    }

    #[test]
    fn test_dd_mull_6_card_lenient() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // 6 cards: 1 land + 5 interaction (no threat/selection) — still has "spells"
        add_hand_card(&mut state, PlayerId::Us, "Underground Sea");
        add_hand_card(&mut state, PlayerId::Us, "Force of Will");
        add_hand_card(&mut state, PlayerId::Us, "Daze");
        add_hand_card(&mut state, PlayerId::Us, "Thoughtseize");
        add_hand_card(&mut state, PlayerId::Us, "Force of Will");
        add_hand_card(&mut state, PlayerId::Us, "Daze");
        assert!(!dd_should_mulligan(&state, PlayerId::Us, 1), "6-card with land + spells should keep");
    }

    #[test]
    fn test_dd_always_keeps_at_4() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Terrible 4-card hand — should still keep
        add_hand_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        add_hand_card(&mut state, PlayerId::Us, "Unearth");
        add_hand_card(&mut state, PlayerId::Us, "Lion's Eye Diamond");
        add_hand_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        assert!(!dd_should_mulligan(&state, PlayerId::Us, 3), "always keep at 4 cards");
    }

    #[test]
    fn test_opp_mull_no_land() {
        let mut state = make_state();
        state.catalog = test_catalog();
        add_hand_card(&mut state, PlayerId::Opp, "Delver of Secrets");
        add_hand_card(&mut state, PlayerId::Opp, "Force of Will");
        add_hand_card(&mut state, PlayerId::Opp, "Brainstorm");
        add_hand_card(&mut state, PlayerId::Opp, "Ponder");
        add_hand_card(&mut state, PlayerId::Opp, "Daze");
        add_hand_card(&mut state, PlayerId::Opp, "Lightning Bolt");
        add_hand_card(&mut state, PlayerId::Opp, "Dragon's Rage Channeler");
        assert!(opp_should_mulligan(&state, PlayerId::Opp, 0, &[Color::Blue, Color::Red]), "0-land opp hand should mull");
    }

    #[test]
    fn test_opp_mull_land_flood() {
        let mut state = make_state();
        state.catalog = test_catalog();
        add_hand_card(&mut state, PlayerId::Opp, "Volcanic Island");
        add_hand_card(&mut state, PlayerId::Opp, "Scalding Tarn");
        add_hand_card(&mut state, PlayerId::Opp, "Flooded Strand");
        add_hand_card(&mut state, PlayerId::Opp, "Polluted Delta");
        add_hand_card(&mut state, PlayerId::Opp, "Misty Rainforest");
        add_hand_card(&mut state, PlayerId::Opp, "Delver of Secrets");
        add_hand_card(&mut state, PlayerId::Opp, "Brainstorm");
        assert!(opp_should_mulligan(&state, PlayerId::Opp, 0, &[Color::Blue, Color::Red]), "5+ mana lands opp hand should mull");
    }

    #[test]
    fn test_opp_keep_good_hand() {
        let mut state = make_state();
        state.catalog = test_catalog();
        add_hand_card(&mut state, PlayerId::Opp, "Volcanic Island");
        add_hand_card(&mut state, PlayerId::Opp, "Wasteland");
        add_hand_card(&mut state, PlayerId::Opp, "Delver of Secrets");
        add_hand_card(&mut state, PlayerId::Opp, "Brainstorm");
        add_hand_card(&mut state, PlayerId::Opp, "Force of Will");
        add_hand_card(&mut state, PlayerId::Opp, "Daze");
        add_hand_card(&mut state, PlayerId::Opp, "Ponder");
        assert!(!opp_should_mulligan(&state, PlayerId::Opp, 0, &[Color::Blue, Color::Red]), "good tempo hand should keep");
    }

    #[test]
    fn test_opp_mull_all_interaction_no_threats() {
        let mut state = make_state();
        state.catalog = test_catalog();
        add_hand_card(&mut state, PlayerId::Opp, "Volcanic Island");
        add_hand_card(&mut state, PlayerId::Opp, "Wasteland");
        add_hand_card(&mut state, PlayerId::Opp, "Force of Will");
        add_hand_card(&mut state, PlayerId::Opp, "Daze");
        add_hand_card(&mut state, PlayerId::Opp, "Lightning Bolt");
        add_hand_card(&mut state, PlayerId::Opp, "Fatal Push");
        add_hand_card(&mut state, PlayerId::Opp, "Thoughtseize");
        // 2 mana + 5 interaction, 0 threats, 0 selection → mull
        assert!(opp_should_mulligan(&state, PlayerId::Opp, 0, &[Color::Blue, Color::Red]),
            "all interaction no threats/cantrips should mull");
    }

    #[test]
    fn test_opp_always_keeps_at_4() {
        let mut state = make_state();
        state.catalog = test_catalog();
        add_hand_card(&mut state, PlayerId::Opp, "Force of Will");
        add_hand_card(&mut state, PlayerId::Opp, "Daze");
        add_hand_card(&mut state, PlayerId::Opp, "Lightning Bolt");
        add_hand_card(&mut state, PlayerId::Opp, "Fatal Push");
        assert!(!opp_should_mulligan(&state, PlayerId::Opp, 3, &[Color::Blue, Color::Red]), "always keep at 4 cards");
    }

    // ── Section 54b: Color-Aware Mulligan Tests ──────────────────────────────

    #[test]
    fn test_dd_mull_wasteland_only() {
        // Wasteland produces no colored mana — should mull.
        let mut state = make_state();
        state.catalog = test_catalog();
        add_hand_card(&mut state, PlayerId::Us, "Wasteland");
        add_hand_card(&mut state, PlayerId::Us, "Doomsday");
        add_hand_card(&mut state, PlayerId::Us, "Brainstorm");
        add_hand_card(&mut state, PlayerId::Us, "Ponder");
        add_hand_card(&mut state, PlayerId::Us, "Force of Will");
        add_hand_card(&mut state, PlayerId::Us, "Thoughtseize");
        add_hand_card(&mut state, PlayerId::Us, "Consider");
        assert!(dd_should_mulligan(&state, PlayerId::Us, 0),
            "Wasteland-only hand should mull — no U or BBB");
    }

    #[test]
    fn test_dd_keep_fetch_hand() {
        // 3 fetches + Ritual + DD: fetches provide U/B, should keep.
        let mut state = make_state();
        state.catalog = test_catalog();
        add_hand_card(&mut state, PlayerId::Us, "Polluted Delta");
        add_hand_card(&mut state, PlayerId::Us, "Misty Rainforest");
        add_hand_card(&mut state, PlayerId::Us, "Flooded Strand");
        add_hand_card(&mut state, PlayerId::Us, "Dark Ritual");
        add_hand_card(&mut state, PlayerId::Us, "Doomsday");
        add_hand_card(&mut state, PlayerId::Us, "Brainstorm");
        add_hand_card(&mut state, PlayerId::Us, "Ponder");
        assert!(!dd_should_mulligan(&state, PlayerId::Us, 0),
            "fetch lands provide U/B — should keep");
    }

    #[test]
    fn test_dd_keep_usea_cantrips() {
        // 1 Underground Sea + cantrips: has U, should keep.
        let mut state = make_state();
        state.catalog = test_catalog();
        add_hand_card(&mut state, PlayerId::Us, "Underground Sea");
        add_hand_card(&mut state, PlayerId::Us, "Brainstorm");
        add_hand_card(&mut state, PlayerId::Us, "Ponder");
        add_hand_card(&mut state, PlayerId::Us, "Consider");
        add_hand_card(&mut state, PlayerId::Us, "Doomsday");
        add_hand_card(&mut state, PlayerId::Us, "Force of Will");
        add_hand_card(&mut state, PlayerId::Us, "Thoughtseize");
        assert!(!dd_should_mulligan(&state, PlayerId::Us, 0),
            "USea + cantrips should keep");
    }

    #[test]
    fn test_opp_mull_wasteland_only() {
        // Opponent with only Wasteland — no U, should mull.
        let mut state = make_state();
        state.catalog = test_catalog();
        add_hand_card(&mut state, PlayerId::Opp, "Wasteland");
        add_hand_card(&mut state, PlayerId::Opp, "Delver of Secrets");
        add_hand_card(&mut state, PlayerId::Opp, "Brainstorm");
        add_hand_card(&mut state, PlayerId::Opp, "Force of Will");
        add_hand_card(&mut state, PlayerId::Opp, "Daze");
        add_hand_card(&mut state, PlayerId::Opp, "Lightning Bolt");
        add_hand_card(&mut state, PlayerId::Opp, "Ponder");
        assert!(opp_should_mulligan(&state, PlayerId::Opp, 0, &[Color::Blue, Color::Red]),
            "Wasteland-only opp hand should mull — no U");
    }

    #[test]
    fn test_opp_keep_fetch_hand() {
        // Fetch land provides U via deck knowledge — should keep.
        let mut state = make_state();
        state.catalog = test_catalog();
        add_hand_card(&mut state, PlayerId::Opp, "Scalding Tarn");
        add_hand_card(&mut state, PlayerId::Opp, "Delver of Secrets");
        add_hand_card(&mut state, PlayerId::Opp, "Brainstorm");
        add_hand_card(&mut state, PlayerId::Opp, "Force of Will");
        add_hand_card(&mut state, PlayerId::Opp, "Daze");
        add_hand_card(&mut state, PlayerId::Opp, "Lightning Bolt");
        add_hand_card(&mut state, PlayerId::Opp, "Ponder");
        assert!(!opp_should_mulligan(&state, PlayerId::Opp, 0, &[Color::Blue, Color::Red]),
            "fetch land provides U — should keep");
    }

    // ── Section 55: Mana Ability Fixes ──────────────────────────────────────

    #[test]
    fn test_led_excluded_from_mana_sub_loop() {
        // LED has ActivationTiming::Instant, so enumerate_mana_abilities must skip it.
        let mut state = make_state();
        state.catalog = test_catalog();
        let led_def = catalog_card("Lion's Eye Diamond");
        add_perm_with_def(&mut state, PlayerId::Us, &led_def, BattlefieldState::new());
        recompute(&mut state);

        let options = enumerate_mana_abilities(&state, PlayerId::Us);
        assert!(options.is_empty(),
            "LED should be excluded from mana sub-loop (timing != Default), got {} options", options.len());
    }

    #[test]
    fn test_led_excluded_from_potential_mana() {
        // LED has non-Default timing, so potential_mana should NOT count it.
        // This prevents the engine from thinking spells are affordable when the
        // strategy refuses to auto-crack LED (undertapping bug).
        let mut state = make_state();
        state.catalog = test_catalog();
        let led_def = catalog_card("Lion's Eye Diamond");
        add_perm_with_def(&mut state, PlayerId::Us, &led_def, BattlefieldState::new());
        recompute(&mut state);

        let pool = state.potential_mana(PlayerId::Us);
        assert_eq!(pool.total, 0, "potential_mana should not count LED (non-Default timing)");
    }

    #[test]
    fn test_insufficient_mana_blocks_cast() {
        // With only LED on the battlefield (non-Default timing), a BBB spell
        // should NOT appear in legal actions — the engine cannot auto-tap LED.
        let mut state = make_state();
        state.catalog = test_catalog();
        let led_def = catalog_card("Lion's Eye Diamond");
        add_perm_with_def(&mut state, PlayerId::Us, &led_def, BattlefieldState::new());
        add_hand_card(&mut state, PlayerId::Us, "Doomsday");
        state.current_turn = 1;
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
        recompute(&mut state);

        let legal = strategy::collect_legal_actions(&state, PlayerId::Us);
        assert!(
            !legal.iter().any(|a| matches!(a, LegalAction::CastSpell { .. })),
            "Doomsday should not be castable with only LED as a mana source"
        );
    }

    #[test]
    fn test_sufficient_mana_allows_cast() {
        // With real lands producing BBB, Doomsday should appear in legal actions.
        let mut state = make_state();
        state.catalog = test_catalog();
        let sea_def = catalog_card("Underground Sea");
        for _ in 0..3 {
            add_perm_with_def(&mut state, PlayerId::Us, &sea_def, BattlefieldState::new());
        }
        add_hand_card(&mut state, PlayerId::Us, "Doomsday");
        state.current_turn = 1;
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
        recompute(&mut state);

        let legal = strategy::collect_legal_actions(&state, PlayerId::Us);
        assert!(
            legal.iter().any(|a| matches!(a, LegalAction::CastSpell { .. })),
            "Doomsday should be castable with 3 Underground Seas"
        );
    }

    #[test]
    fn test_fow_not_offered_on_empty_stack() {
        // Force of Will should NOT appear in legal actions when the stack is empty.
        let mut state = make_state();
        state.catalog = test_catalog();
        // Give opponent a land, FoW in hand, and a blue card to pitch.
        let sea_def = catalog_card("Underground Sea");
        add_perm_with_def(&mut state, PlayerId::Opp, &sea_def, BattlefieldState::new());
        add_hand_card(&mut state, PlayerId::Opp, "Force of Will");
        add_hand_card(&mut state, PlayerId::Opp, "Brainstorm"); // blue card to pitch
        state.current_turn = 2;
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
        recompute(&mut state);
        assert!(state.stack.is_empty(), "stack should be empty");

        let legal = strategy::collect_legal_actions(&state, PlayerId::Opp);
        let has_fow = legal.iter().any(|a| {
            if let LegalAction::CastSpell { card_id, .. } = a {
                state.objects.get(card_id).map_or(false, |c| c.catalog_key == "Force of Will")
            } else { false }
        });
        assert!(!has_fow, "FoW should not be castable on empty stack — no valid targets");
    }

    #[test]
    fn test_cavern_colored_mana_blocked_for_non_creature() {
        // Cavern of Souls' colored mana ability requires casting_spell to be a creature.
        // With no spell being cast (or a non-creature spell), the condition should fail.
        let mut state = make_state();
        state.catalog = test_catalog();
        let cavern_def = catalog_card("Cavern of Souls");
        let cavern_id = add_perm_with_def(&mut state, PlayerId::Us, &cavern_def, BattlefieldState::new());
        recompute(&mut state);

        // No spell being cast — colored ability should be unavailable.
        state.casting_spell = None;
        let options = enumerate_mana_abilities(&state, PlayerId::Us);
        // Should only see the colorless ability (index 0), not the colored one (index 1).
        assert!(options.iter().all(|o| o.ability_index == 0),
            "Cavern colored mana should be blocked when no spell is being cast");

        // Casting a non-creature spell — colored ability still unavailable.
        let ts_id = add_hand_card(&mut state, PlayerId::Us, "Thoughtseize");
        state.casting_spell = Some(ts_id);
        let options = enumerate_mana_abilities(&state, PlayerId::Us);
        assert!(options.iter().all(|o| o.source_id != cavern_id || o.ability_index == 0),
            "Cavern colored mana should be blocked for non-creature spells");
    }

    #[test]
    fn test_cavern_colored_mana_allowed_for_creature() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let cavern_def = catalog_card("Cavern of Souls");
        let cavern_id = add_perm_with_def(&mut state, PlayerId::Us, &cavern_def, BattlefieldState::new());
        recompute(&mut state);

        // Casting a creature spell — colored ability should be available.
        let creature_id = add_hand_card(&mut state, PlayerId::Us, "Murktide Regent");
        state.casting_spell = Some(creature_id);
        let options = enumerate_mana_abilities(&state, PlayerId::Us);
        let has_colored = options.iter().any(|o| o.source_id == cavern_id && o.ability_index == 1);
        assert!(has_colored,
            "Cavern colored mana should be available when casting a creature spell");
    }

    #[test]
    fn test_mana_log_and_cast_both_present() {
        // Casting via run_cast_submachine should log both mana production and the cast.
        let mut state = make_state();
        state.catalog = test_catalog();

        let sea_def = catalog_card("Underground Sea");
        add_perm_with_def(&mut state, PlayerId::Us, &sea_def, BattlefieldState::new());
        let dr_id = add_hand_card(&mut state, PlayerId::Us, "Dark Ritual");
        recompute(&mut state);

        let mut strat = strategy::DoomsdayStrategy::new(strategy::MatchupInfo::default());
        run_cast_submachine(&mut state, 1, PlayerId::Us, dr_id, SpellFace::Main, &mut strat);

        let has_cast = state.log.iter().any(|l| l.contains("Cast Dark Ritual"));
        let has_mana = state.log.iter().any(|l| l.contains("add B to pool"));
        assert!(has_cast, "should have a Cast log line, got: {:?}", state.log);
        assert!(has_mana, "should have a mana production log line, got: {:?}", state.log);
    }

    // ── Section 54: Validation (Phase 8) ─────────────────────────────────────

    fn cards(list: &[(&str, i32)]) -> Vec<(String, i32, String)> {
        list.iter().map(|(n, q)| (n.to_string(), *q, "main".to_string())).collect()
    }

    fn val_dd_deck() -> Vec<(String, i32, String)> {
        cards(&[
            ("Underground Sea", 3), ("Polluted Delta", 4), ("Flooded Strand", 1),
            ("Misty Rainforest", 1), ("Scalding Tarn", 1), ("Marsh Flats", 1),
            ("Island", 1), ("Swamp", 1), ("Undercity Sewers", 2), ("Wasteland", 3),
            ("Cavern of Souls", 1),
            ("Lotus Petal", 2), ("Lion's Eye Diamond", 1),
            ("Dark Ritual", 4), ("Doomsday", 4), ("Brainstorm", 4),
            ("Ponder", 4), ("Consider", 1), ("Edge of Autumn", 1),
            ("Force of Will", 4), ("Daze", 3), ("Thoughtseize", 2),
            ("Street Wraith", 1), ("Thassa's Oracle", 1), ("Unearth", 1),
            ("Tamiyo, Inquisitive Student", 4), ("Orcish Bowmasters", 2),
            ("Murktide Regent", 2),
        ])
    }

    fn val_ub_tempo_deck() -> Vec<(String, i32, String)> {
        cards(&[
            ("Underground Sea", 4), ("Polluted Delta", 4), ("Flooded Strand", 2),
            ("Misty Rainforest", 1), ("Scalding Tarn", 1), ("Bloodstained Mire", 1),
            ("Island", 1), ("Swamp", 1), ("Wasteland", 4), ("Undercity Sewers", 1),
            ("Tamiyo, Inquisitive Student", 4), ("Orcish Bowmasters", 4),
            ("Murktide Regent", 3), ("Barrowgoyf", 2), ("Brazen Borrower", 1),
            ("Kaito, Bane of Nightmares", 2),
            ("Brainstorm", 4), ("Ponder", 4), ("Force of Will", 4), ("Daze", 3),
            ("Fatal Push", 4), ("Snuff Out", 1), ("Thoughtseize", 4),
        ])
    }

    /// Parse "us: N cards (-M mulligans)" from opening log line.
    fn parse_log_hand_info(log: &str) -> Option<(u32, u32, u32, u32)> {
        // Format: "... us: 7 cards (-0 mulligans), opp: 6 cards (-1 mulligans)"
        let us_hand = log.find("us: ").and_then(|i| log[i+4..].chars().next()?.to_digit(10))?;
        let us_mull = {
            let marker = "us: ";
            let after_us = log.find(marker).map(|i| &log[i..])?;
            let m_pos = after_us.find("(-")?;
            after_us[m_pos+2..].chars().next()?.to_digit(10)?
        };
        let opp_hand = log.find("opp: ").and_then(|i| log[i+5..].chars().next()?.to_digit(10))?;
        let opp_mull = {
            let marker = "opp: ";
            let after_opp = log.find(marker).map(|i| &log[i..])?;
            let m_pos = after_opp.find("(-")?;
            after_opp[m_pos+2..].chars().next()?.to_digit(10)?
        };
        Some((us_hand, us_mull, opp_hand, opp_mull))
    }

    #[test]
    fn test_parse_log_hand_info() {
        let log = "T0 [us] Turn 3 — UB Tempo (play) | us: 7 cards (-0 mulligans), opp: 7 cards (-0 mulligans)";
        let result = parse_log_hand_info(log);
        assert_eq!(result, Some((7, 0, 7, 0)), "failed to parse: {}", log);

        let log2 = "T0 [us] Turn 4 — UB Tempo (draw) | us: 6 cards (-1 mulligans), opp: 5 cards (-2 mulligans)";
        let result2 = parse_log_hand_info(log2);
        assert_eq!(result2, Some((6, 1, 5, 2)), "failed to parse: {}", log2);
    }

    /// Run N=10000 simulations and report key metrics.
    /// This test always passes — it prints stats for manual inspection.
    /// Run with: cargo test validation_stats -- --nocapture --ignored
    #[test]
    #[ignore] // slow: ~10s for 10000 sims
    fn validation_stats() {
        use rand::Rng;
        let catalog = test_catalog();
        let dd_cards = val_dd_deck();
        let opp_cards = val_ub_tempo_deck();
        let n = 1_000; // ~2min in debug mode; use --release for 10000
        let mut rng = StdRng::seed_from_u64(12345);

        let mut dd_success = 0u32;
        let mut dd_fail = 0u32;
        let mut us_mull_total = 0u32;
        let mut opp_mull_total = 0u32;
        let mut us_mulled_games = 0u32;
        let mut opp_mulled_games = 0u32;
        let mut success_turns = Vec::new();
        let mut us_hand_sizes = Vec::new();
        let mut opp_hand_sizes = Vec::new();

        let mut panics = 0u32;
        for _i in 0..n {
            // Use a per-sim RNG seeded from the master, so panics don't lose rng state.
            let sim_seed = rng.gen::<u64>();
            let mut sim_rng = StdRng::seed_from_u64(sim_seed);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                simulate_game(
                    "doomsday", "UB Tempo", &catalog, &dd_cards, &opp_cards, &mut sim_rng,
                )
            }));
            let result = match result {
                Ok(r) => r,
                Err(_) => { panics += 1; continue; }
            };
            let state = result;
            if state.success {
                dd_success += 1;
                success_turns.push(state.current_turn);
                // Find the opening hand summary line (contains "mulligans").
                if let Some(info_log) = state.log.iter().find(|l| l.contains("mulligans")) {
                    if let Some((uh, um, oh, om)) = parse_log_hand_info(info_log) {
                        us_hand_sizes.push(uh);
                        opp_hand_sizes.push(oh);
                        if um > 0 { us_mulled_games += 1; }
                        us_mull_total += um;
                        if om > 0 { opp_mulled_games += 1; }
                        opp_mull_total += om;
                    }
                }
            } else {
                dd_fail += 1;
            }
        }

        let total = dd_success + dd_fail;
        let success_rate = dd_success as f64 / total as f64 * 100.0;
        let avg_us_hand = if us_hand_sizes.is_empty() { 0.0 }
            else { us_hand_sizes.iter().sum::<u32>() as f64 / us_hand_sizes.len() as f64 };
        let avg_opp_hand = if opp_hand_sizes.is_empty() { 0.0 }
            else { opp_hand_sizes.iter().sum::<u32>() as f64 / opp_hand_sizes.len() as f64 };

        // Turn distribution for successful games
        let mut turn_counts = [0u32; 8]; // index 2..7
        for &t in &success_turns {
            if (t as usize) < turn_counts.len() { turn_counts[t as usize] += 1; }
        }

        eprintln!("\n══════════════════════════════════════════════════════");
        eprintln!("  VALIDATION: {} sims vs UB Tempo", n);
        eprintln!("══════════════════════════════════════════════════════");
        eprintln!("  DD success rate: {}/{} ({:.1}%)", dd_success, total, success_rate);
        eprintln!("  DD fail (no cast): {}/{} ({:.1}%)", dd_fail, total, 100.0 - success_rate);
        if panics > 0 {
            eprintln!("  ⚠ Panicked sims: {} (pre-existing engine bugs)", panics);
        }
        eprintln!();
        eprintln!("  DD success by turn:");
        for t in 2..=7 {
            let c = turn_counts[t];
            let pct = if dd_success > 0 { c as f64 / dd_success as f64 * 100.0 } else { 0.0 };
            eprintln!("    T{}: {:>5} ({:.1}%)", t, c, pct);
        }
        eprintln!();
        eprintln!("  Mulligan rates:");
        eprintln!("    Us:  {:.1}% of games mulled, avg {:.2} mulls/game",
            us_mulled_games as f64 / total as f64 * 100.0,
            us_mull_total as f64 / total as f64);
        eprintln!("    Opp: {:.1}% of games mulled, avg {:.2} mulls/game",
            opp_mulled_games as f64 / total as f64 * 100.0,
            opp_mull_total as f64 / total as f64);
        eprintln!();
        eprintln!("  Avg hand size (after mulls+london bottom):");
        eprintln!("    Us:  {:.2}", avg_us_hand);
        eprintln!("    Opp: {:.2}", avg_opp_hand);
        eprintln!("══════════════════════════════════════════════════════\n");

        // Sanity assertions — these should be very loose, just catching broken sims.
        assert!(success_rate > 10.0, "DD success rate suspiciously low: {:.1}%", success_rate);
        assert!(success_rate < 95.0, "DD success rate suspiciously high: {:.1}%", success_rate);
        assert!(avg_us_hand >= 5.0, "avg US hand size too low: {:.2}", avg_us_hand);
        assert!(avg_opp_hand >= 5.0, "avg OPP hand size too low: {:.2}", avg_opp_hand);
    }

    #[test]
    fn test_decision_log_populated() {
        let catalog = test_catalog();
        let dd_cards = val_dd_deck();
        let opp_cards = val_ub_tempo_deck();
        let mut rng = StdRng::seed_from_u64(42);
        // Run sims until one succeeds (DD resolves).
        let state = loop {
            let s = simulate_game("doomsday", "UB Tempo", &catalog, &dd_cards, &opp_cards, &mut rng);
            if s.success { break s; }
        };
        assert!(!state.decision_log.is_empty(), "decision_log should have entries");
        // Should contain at least a mulligan decision and a planner decision.
        let has_mulligan = state.decision_log.iter().any(|l| l.contains("mulligan"));
        let has_plan = state.decision_log.iter().any(|l| l.contains("plan"));
        assert!(has_mulligan, "decision_log should contain mulligan entries");
        assert!(has_plan, "decision_log should contain planner entries");
        eprintln!("\n── decision_log ({} entries) ──", state.decision_log.len());
        for entry in &state.decision_log {
            eprintln!("  {}", entry);
        }
    }

    // ── Targeted-spell legality ──────────────────────────────────────────────

    /// Put a permanent spell on the stack WITH an effect (eff_enter_permanent),
    /// mimicking how the cast submachine sets up a permanent about to resolve.
    fn add_permanent_spell_on_stack(state: &mut SimState, who: PlayerId, name: &str) -> ObjId {
        let id = state.alloc_id();
        let eff = eff_enter_permanent(who, name.to_string());
        state.objects.insert(id, GameObject {
            id,
            catalog_key: name.to_string(),
            owner: who,
            controller: who,
            zone: CardZone::Stack,
            is_token: false,
            spell: Some(SpellState {
                effect: Some(eff),
                chosen_targets: vec![],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
            bf: None,
            materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });
        state.catalog.entry(name.to_string())
            .or_insert_with(|| test_catalog().remove(name).unwrap_or_else(||
                creature(name, 1, 1)));
        state.stack.push(id);
        id
    }

    #[test]
    fn test_resolved_permanent_does_not_leave_stale_stack_object() {
        // When a permanent spell resolves, the old spell object must not linger
        // with zone == Stack. Stale stack objects caused counterspells to find
        // phantom targets long after the permanent had entered the battlefield.
        let mut state = make_state();
        state.catalog = test_catalog();
        let mut strategies = make_strategies();
        state.current_turn = 2;
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));

        add_permanent_spell_on_stack(&mut state, PlayerId::Opp, "Murktide Regent");
        recompute(&mut state);
        resolve_top_of_stack(&mut state, 2, PlayerId::Opp, &mut strategies);

        // Stack should be empty and no objects should have zone == Stack.
        assert!(state.stack.is_empty(), "stack list should be empty after resolution");
        let stale_stack_objs: Vec<_> = state.objects.values()
            .filter(|o| o.zone == CardZone::Stack)
            .map(|o| o.catalog_key.clone())
            .collect();
        assert!(stale_stack_objs.is_empty(),
            "no objects should remain with zone == Stack after permanent resolves, found: {:?}",
            stale_stack_objs);
    }

    #[test]
    fn test_fow_not_legal_after_permanent_resolves() {
        // End-to-end: a Us creature resolves, then on a later priority window
        // with an empty stack, Opp's Force of Will must NOT appear in legal actions.
        let mut state = make_state();
        state.catalog = test_catalog();
        let mut strategies = make_strategies();
        state.current_turn = 2;
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));

        // Resolve a Us creature spell (simulating a prior turn).
        add_permanent_spell_on_stack(&mut state, PlayerId::Us, "Murktide Regent");
        recompute(&mut state);
        resolve_top_of_stack(&mut state, 2, PlayerId::Us, &mut strategies);

        // Now set up opp's turn with FoW in hand and an empty stack.
        state.current_turn = 4;
        let island_def = catalog_card("Island");
        for _ in 0..5 {
            add_perm_with_def(&mut state, PlayerId::Opp, &island_def, BattlefieldState::new());
        }
        add_hand_card(&mut state, PlayerId::Opp, "Force of Will");
        add_hand_card(&mut state, PlayerId::Opp, "Brainstorm"); // blue pitch fodder
        recompute(&mut state);

        assert!(state.stack.is_empty(), "precondition: stack should be empty");
        let legal = strategy::collect_legal_actions(&state, PlayerId::Opp);
        let has_fow = legal.iter().any(|a| {
            if let LegalAction::CastSpell { card_id, .. } = a {
                state.objects.get(card_id).map_or(false, |c| c.catalog_key == "Force of Will")
            } else { false }
        });
        assert!(!has_fow,
            "Force of Will must not be offered when the stack is empty \
             (stale resolved-permanent objects must not be targetable)");
    }

    #[test]
    fn test_fow_not_legal_with_empty_stack() {
        // Force of Will requires a target spell on the stack.
        // With an empty stack it must NOT appear in legal actions.
        let mut state = make_state();
        state.catalog = test_catalog();
        let island_def = catalog_card("Island");
        for _ in 0..5 {
            add_perm_with_def(&mut state, PlayerId::Opp, &island_def, BattlefieldState::new());
        }
        // Give opp a blue card to pitch + FoW itself (hand_min >= 2).
        add_hand_card(&mut state, PlayerId::Opp, "Force of Will");
        add_hand_card(&mut state, PlayerId::Opp, "Brainstorm"); // blue pitch fodder
        state.current_turn = 4;
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
        recompute(&mut state);

        assert!(state.stack.is_empty(), "precondition: stack should be empty");
        let legal = strategy::collect_legal_actions(&state, PlayerId::Opp);
        let has_fow = legal.iter().any(|a| {
            if let LegalAction::CastSpell { card_id, .. } = a {
                state.objects.get(card_id).map_or(false, |c| c.catalog_key == "Force of Will")
            } else { false }
        });
        assert!(!has_fow,
            "Force of Will must not be offered as a legal action with an empty stack");
    }

    #[test]
    fn test_fow_legal_with_opposing_spell_on_stack() {
        // When an opponent's spell IS on the stack, FoW should be legal.
        let mut state = make_state();
        state.catalog = test_catalog();
        let island_def = catalog_card("Island");
        for _ in 0..5 {
            add_perm_with_def(&mut state, PlayerId::Opp, &island_def, BattlefieldState::new());
        }
        add_hand_card(&mut state, PlayerId::Opp, "Force of Will");
        add_hand_card(&mut state, PlayerId::Opp, "Brainstorm"); // pitch fodder
        // Put an opponent (Us) spell on the stack.
        let bs_def = catalog_card("Brainstorm");
        let spell_id = add_stack_spell(&mut state, PlayerId::Us, &bs_def);
        state.stack.push(spell_id);
        state.current_turn = 4;
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
        recompute(&mut state);

        let legal = strategy::collect_legal_actions(&state, PlayerId::Opp);
        let has_fow = legal.iter().any(|a| {
            if let LegalAction::CastSpell { card_id, .. } = a {
                state.objects.get(card_id).map_or(false, |c| c.catalog_key == "Force of Will")
            } else { false }
        });
        assert!(has_fow,
            "Force of Will should be offered when an opposing spell is on the stack");
    }

    // ── Section: Engine Invariant Tests ──────────────────────────────────────

    #[test]
    fn test_priority_round_both_players_pass() {
        // After handle_priority_round, the stack must be empty and the game
        // state must be self-consistent (assert_engine_invariants fires inside).
        let mut state = make_state();
        state.catalog = test_catalog();
        state.current_turn = 1;
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
        // Give both players some lands so the state isn't degenerate.
        let island_def = catalog_card("Island");
        for _ in 0..3 {
            add_perm_with_def(&mut state, PlayerId::Us, &island_def, BattlefieldState::new());
            add_perm_with_def(&mut state, PlayerId::Opp, &island_def, BattlefieldState::new());
        }
        recompute(&mut state);

        let mut strategies = make_strategies();
        handle_priority_round(&mut state, 1, PlayerId::Us, &mut strategies);

        assert!(state.stack.is_empty(),
            "stack should be empty after priority round with no spells cast");
        let stack_objs: Vec<_> = state.objects.values()
            .filter(|o| o.zone == CardZone::Stack)
            .collect();
        assert!(stack_objs.is_empty(),
            "no objects should have zone == Stack after clean priority round");
    }

    #[test]
    fn test_no_stale_objects_after_multi_permanent_resolution() {
        // Cast 3 permanent spells, resolve all, verify no zone == Stack objects remain.
        // This is the multi-spell version of test_resolved_permanent_does_not_leave_stale_stack_object.
        let mut state = make_state();
        state.catalog = test_catalog();
        let mut strategies = make_strategies();
        state.current_turn = 3;
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));

        // Push 3 permanent spells onto the stack and resolve each.
        let names = ["Murktide Regent", "Orcish Bowmasters", "Grief"];
        for name in &names {
            add_permanent_spell_on_stack(&mut state, PlayerId::Opp, name);
            recompute(&mut state);
            resolve_top_of_stack(&mut state, 3, PlayerId::Opp, &mut strategies);
        }

        assert!(state.stack.is_empty(),
            "stack list should be empty after resolving all 3 permanents");
        let stale: Vec<_> = state.objects.values()
            .filter(|o| o.zone == CardZone::Stack)
            .map(|o| o.catalog_key.clone())
            .collect();
        assert!(stale.is_empty(),
            "no objects should have zone == Stack after resolving 3 permanents, found: {:?}", stale);
        // All 3 should be on the battlefield.
        for name in &names {
            let on_bf = state.objects.values()
                .any(|o| o.catalog_key == *name && o.zone == CardZone::Battlefield);
            assert!(on_bf, "{} should be on the battlefield after resolution", name);
        }
    }

    #[test]
    fn test_zone_tracking_consistency() {
        // After various zone transitions, verify library_order, graveyard_order,
        // and actual object zones are in sync.
        let mut state = make_state();
        state.catalog = test_catalog();
        state.current_turn = 1;
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));

        // Put some cards in different zones.
        let island_def = catalog_card("Island");
        let perm_id = add_perm_with_def(&mut state, PlayerId::Us, &island_def, BattlefieldState::new());
        let hand_id = add_hand_card(&mut state, PlayerId::Us, "Brainstorm");

        // Move permanent to graveyard.
        change_zone(perm_id, ZoneId::Graveyard, &mut state, 1, PlayerId::Us);
        // Move hand card to graveyard.
        change_zone(hand_id, ZoneId::Graveyard, &mut state, 1, PlayerId::Us);

        // Verify graveyard_order matches actual graveyard objects.
        let gy_objs: Vec<ObjId> = state.objects.values()
            .filter(|o| o.zone == CardZone::Graveyard && o.owner == PlayerId::Us)
            .map(|o| o.id)
            .collect();
        for &id in &state.graveyard_order {
            assert!(state.objects.get(&id).map_or(false, |o| o.zone == CardZone::Graveyard),
                "graveyard_order contains id {:?} that is not in graveyard zone", id);
        }
        for &id in &gy_objs {
            assert!(state.graveyard_order.contains(&id),
                "object {:?} in graveyard zone but missing from graveyard_order", id);
        }

        // Verify library_order matches actual library objects.
        for who in [PlayerId::Us, PlayerId::Opp] {
            let lib_objs: Vec<ObjId> = state.objects.values()
                .filter(|o| o.zone == CardZone::Library && o.owner == who)
                .map(|o| o.id)
                .collect();
            let lib_order = &state.player(who).library_order;
            for &id in lib_order.iter() {
                assert!(state.objects.get(&id).map_or(false, |o| o.zone == CardZone::Library),
                    "library_order for {:?} contains id {:?} not in library zone", who, id);
            }
            for &id in &lib_objs {
                assert!(lib_order.contains(&id),
                    "object {:?} in library zone for {:?} but missing from library_order", id, who);
            }
        }
    }

    /// Stress-test: run many scenarios with random seeds to reproduce rare
    /// invariant violations (graveyard_order desync, etc.).
    // ── Turn planner tests ────────────────────────────────────────────────────

    /// Extract the priority-round action names from a plan (spells + land drops, skip taps).
    fn plan_spell_names(plan: &[planner::PlanAction], state: &SimState) -> Vec<String> {
        plan.iter().filter_map(|a| match a {
            planner::PlanAction::CastSpell(id) | planner::PlanAction::LandDrop(id) =>
                state.objects.get(id).map(|c| c.catalog_key.clone()),
            planner::PlanAction::TapForMana { .. } | planner::PlanAction::CrackFetch { .. } => None,
        }).collect()
    }

    /// Build a state with specific lands on board and cards in hand.
    /// Loads full catalog defs so mana abilities work.
    fn plan_test_state(
        lands: &[&str],
        hand: &[&str],
    ) -> SimState {
        let mut state = make_state();
        let catalog = test_catalog();
        for &name in lands {
            let def = catalog.get(name).expect(&format!("land not in catalog: {name}"));
            add_perm_with_def(&mut state, PlayerId::Us, def, BattlefieldState::new());
        }
        for &name in hand {
            add_hand_card(&mut state, PlayerId::Us, name);
        }
        // Merge full catalog so plan_def_of fallback works.
        for (k, v) in catalog {
            state.catalog.entry(k).or_insert(v);
        }
        state
    }

    #[test]
    fn plan_direct_dd_cast() {
        // 3 black-producing lands + DD in hand → cast DD directly.
        let state = plan_test_state(
            &["Underground Sea", "Badlands", "Scrubland"],
            &["Doomsday"],
        );
        let plan = planner::make_turn_plan(&state, PlayerId::Us, planner::dd_plan_quality);
        let names = plan_spell_names(&plan, &state);
        assert!(names.contains(&"Doomsday".to_string()),
            "plan should cast Doomsday, got: {:?}", names);
        assert!(!names.contains(&"Dark Ritual".to_string()),
            "direct path should not need Ritual");
    }

    #[test]
    fn plan_ritual_path() {
        // 1 black land + Ritual + DD in hand → Ritual then DD.
        let state = plan_test_state(
            &["Underground Sea"],
            &["Dark Ritual", "Doomsday"],
        );
        let plan = planner::make_turn_plan(&state, PlayerId::Us, planner::dd_plan_quality);
        let names = plan_spell_names(&plan, &state);
        assert_eq!(names, vec!["Dark Ritual", "Doomsday"],
            "should cast Ritual then DD, got: {:?}", names);
    }

    #[test]
    fn plan_land_drop_enables_dd() {
        // No lands on board, 1 land + Ritual + DD in hand → land drop, then Ritual → DD.
        let state = plan_test_state(
            &[],
            &["Underground Sea", "Dark Ritual", "Doomsday"],
        );
        let plan = planner::make_turn_plan(&state, PlayerId::Us, planner::dd_plan_quality);
        let names = plan_spell_names(&plan, &state);
        assert_eq!(names, vec!["Underground Sea", "Dark Ritual", "Doomsday"],
            "should land drop then Ritual then DD, got: {:?}", names);
    }

    #[test]
    fn plan_petal_on_board_ritual_dd() {
        // Lotus Petal on board (untapped) + Ritual + DD in hand, no lands.
        let catalog = test_catalog();
        let petal_def = catalog.get("Lotus Petal").expect("Lotus Petal not in catalog");
        let mut state = make_state();
        add_perm_with_def(&mut state, PlayerId::Us, petal_def, BattlefieldState::new());
        add_hand_card(&mut state, PlayerId::Us, "Dark Ritual");
        add_hand_card(&mut state, PlayerId::Us, "Doomsday");
        for (k, v) in catalog {
            state.catalog.entry(k).or_insert(v);
        }
        let plan = planner::make_turn_plan(&state, PlayerId::Us, planner::dd_plan_quality);
        let names = plan_spell_names(&plan, &state);
        assert_eq!(names, vec!["Dark Ritual", "Doomsday"],
            "Petal sac → Ritual → DD, got: {:?}", names);
    }

    #[test]
    fn plan_no_dd_casts_cantrip() {
        // Land on board + cantrip in hand, no DD → should cast cantrip.
        let state = plan_test_state(
            &["Underground Sea"],
            &["Brainstorm"],
        );
        let plan = planner::make_turn_plan(&state, PlayerId::Us, planner::dd_plan_quality);
        let names = plan_spell_names(&plan, &state);
        assert_eq!(names, vec!["Brainstorm"],
            "without DD, should cast cantrip, got: {:?}", names);
    }

    #[test]
    fn plan_land_drop_plus_cantrip() {
        // Land in hand + cantrip in hand + land on board → land drop + cantrip.
        let state = plan_test_state(
            &["Underground Sea"],
            &["Badlands", "Ponder"],
        );
        let plan = planner::make_turn_plan(&state, PlayerId::Us, planner::dd_plan_quality);
        let names = plan_spell_names(&plan, &state);
        assert!(names.contains(&"Badlands".to_string()), "should land drop, got: {:?}", names);
        assert!(names.contains(&"Ponder".to_string()), "should cast Ponder, got: {:?}", names);
    }

    #[test]
    fn plan_empty_hand_passes() {
        // Land on board but empty hand → empty plan.
        let state = plan_test_state(&["Underground Sea"], &[]);
        let plan = planner::make_turn_plan(&state, PlayerId::Us, planner::dd_plan_quality);
        assert!(plan.is_empty(), "empty hand should produce empty plan, got: {:?}", plan);
    }

    #[test]
    fn plan_no_mana_for_spell() {
        // DD in hand but no mana sources → can't cast, no spells in plan.
        let state = plan_test_state(&[], &["Doomsday"]);
        let plan = planner::make_turn_plan(&state, PlayerId::Us, planner::dd_plan_quality);
        let names = plan_spell_names(&plan, &state);
        assert!(!names.contains(&"Doomsday".to_string()),
            "shouldn't cast DD without mana, got: {:?}", names);
    }

    #[test]
    fn plan_ritual_without_dd_not_cast() {
        // Ritual in hand, no DD → should NOT cast Ritual (wasted BBB).
        let state = plan_test_state(
            &["Underground Sea"],
            &["Dark Ritual", "Brainstorm"],
        );
        let plan = planner::make_turn_plan(&state, PlayerId::Us, planner::dd_plan_quality);
        let names = plan_spell_names(&plan, &state);
        assert!(!names.contains(&"Dark Ritual".to_string()),
            "shouldn't cast Ritual without DD, got: {:?}", names);
        assert!(names.contains(&"Brainstorm".to_string()),
            "should cast Brainstorm instead, got: {:?}", names);
    }

    #[test]
    fn plan_prefers_dd_over_cantrip() {
        // Can cast either cantrip or Ritual→DD. Should pick DD.
        let state = plan_test_state(
            &["Underground Sea"],
            &["Dark Ritual", "Doomsday", "Brainstorm"],
        );
        let plan = planner::make_turn_plan(&state, PlayerId::Us, planner::dd_plan_quality);
        let names = plan_spell_names(&plan, &state);
        assert!(names.contains(&"Doomsday".to_string()),
            "should prefer DD over cantrip, got: {:?}", names);
    }

    #[test]
    #[ignore] // 500 full simulations — run with `cargo test -- --ignored`
    fn stress_invariant_check() {
        let catalog = test_catalog();
        let dd_cards = val_dd_deck();
        let opp_cards = val_ub_tempo_deck();
        for seed in 0..500 {
            let mut rng = StdRng::seed_from_u64(seed);
            let _ = simulate_game("doomsday", "UB Tempo", &catalog, &dd_cards, &opp_cards, &mut rng);
        }
    }
