//! Stage-1 smoke tests. Confirms the IR types compile and construct.
//! Real evaluator / executor tests land in Stage 2.

use super::ability::*;
use super::action::*;
use super::ce::*;
use super::context::*;
use super::event_log::*;
use super::executor::*;
use super::expr::*;

#[test]
fn expr_construction_smoke() {
    // "Controller of source" — a trivial expr tree, confirms enum variants compose.
    let _ = Expr::Controller(Box::new(Expr::Ctx(Ctx::Source)));

    // "Source has power >= 3"
    let _ = Expr::Ge(
        Box::new(Expr::Power(Box::new(Expr::Ctx(Ctx::Source)))),
        Box::new(Expr::Num(3)),
    );
}

#[test]
fn scoped_binder_smoke() {
    // "All creatures you control" — scoped AllObjects with a bind + filter.
    let _ = Expr::AllObjects {
        zone: ZoneSel::Global(ZoneKindSel::Battlefield),
        bind: "c",
        filter: Box::new(Expr::And(
            Box::new(Expr::Contains(
                Box::new(Expr::TypeLit(crate::CardType::Creature)),
                Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::Var("c"))))),
            )),
            Box::new(Expr::Eq(
                Box::new(Expr::Controller(Box::new(Expr::Ctx(Ctx::Var("c"))))),
                Box::new(Expr::Ctx(Ctx::Controller)),
            )),
        )),
    };
}

#[test]
fn action_construction_smoke() {
    let _ = Action::Draw {
        who: Who::You,
        n: Expr::Num(3),
    };

    let _ = Action::DealDamage {
        source: Expr::Ctx(Ctx::Source),
        target: Expr::Ctx(Ctx::Var("target")),
        amount: Expr::Num(3),
    };
}

#[test]
fn ability_construction_smoke() {
    // Trivial triggered ability: "When ~ enters, draw a card."
    let _ = Ability {
        text: Some("When ~ enters, draw a card."),
        kind: AbilityKind::Triggered {
            spec: TriggerSpec::When {
                pattern: EventPattern::EntersZone {
                    obj_filter: Filter(Expr::Eq(
                        Box::new(Expr::Ctx(Ctx::It)),
                        Box::new(Expr::Ctx(Ctx::Source)),
                    )),
                    zone_kind: ZoneKindSel::Battlefield,
                },
                condition: None,
            },
            target_spec: crate::TargetSpec::None,
            body: Action::Draw {
                who: Who::You,
                n: Expr::Num(1),
            },
            active_zone: ZoneKindSel::Battlefield,
        },
    };
}

#[test]
fn event_log_smoke() {
    let log = EventLog::new();
    assert_eq!(log.count(Window::ThisTurn, |_| true), 0);
    assert_eq!(log.count(Window::ThisGame, |_| true), 0);
    assert!(!log.any(Window::ThisTurn, |_| true));
}

#[test]
fn bind_env_smoke() {
    let env = BindEnv::new()
        .with_var("x", Value::Num(5))
        .with_subj(Value::Num(10));
    assert!(matches!(env.get("x"), Some(Value::Num(5))));
    assert!(env.get("missing").is_none());
}

#[test]
fn ce_mod_smoke() {
    let _ = CEMod::PumpPT(Expr::Num(2), Expr::Num(2));
    let _ = CEMod::GrantFlash;
    let _ = CEMod::AltCost(CostSpec::Free);
}

#[test]
fn game_ctx_smoke() {
    let _ = Expr::GameCtx(GameCtx::Monarch);
    let _ = Ctx::Triggering(EventField::DamageAmount);
    let _ = Ctx::ThisCast(EventField::DelvedExiled);
}

#[test]
fn axis_smoke() {
    // Sanity-check the shared dep vocabulary: reads and writes share one enum.
    let reads = vec![Axis::PT, Axis::Type];
    let writes = vec![Axis::PT];
    assert!(reads.contains(&Axis::PT) && writes.contains(&Axis::PT));
}

// ── Dependency-axis tests ───────────────────────────────────────────────────
//
// `deps_of(Expr)` walks the tree and returns the axes the expression reads.
// `writes_of(CEMod)` is hard-coded per variant. Together they let the engine
// derive CR 613 CE ordering without per-card annotations.

mod deps {
    use super::*;
    use crate::ir::ce::{CEMod, CostSpec};
    use crate::{CardType, Color, CounterType, Keyword, Supertype};
    use std::collections::HashSet;

    fn reads_of(e: &Expr) -> HashSet<Axis> {
        deps_of(e).reads.into_iter().collect()
    }

    #[test]
    fn power_reads_pt() {
        let e = Expr::Power(Box::new(Expr::Ctx(Ctx::It)));
        assert_eq!(reads_of(&e), HashSet::from([Axis::PT]));
    }

    #[test]
    fn types_reads_type() {
        let e = Expr::Types(Box::new(Expr::Ctx(Ctx::It)));
        assert_eq!(reads_of(&e), HashSet::from([Axis::Type]));
    }

    #[test]
    fn supertypes_and_subtypes_both_read_type_layer() {
        for e in [
            Expr::Supertypes(Box::new(Expr::Ctx(Ctx::It))),
            Expr::Subtypes(Box::new(Expr::Ctx(Ctx::It))),
        ] {
            assert_eq!(reads_of(&e), HashSet::from([Axis::Type]));
        }
    }

    #[test]
    fn colors_reads_color() {
        let e = Expr::Colors(Box::new(Expr::Ctx(Ctx::It)));
        assert_eq!(reads_of(&e), HashSet::from([Axis::Color]));
    }

    #[test]
    fn keywords_reads_abilities() {
        let e = Expr::Keywords(Box::new(Expr::Ctx(Ctx::It)));
        assert_eq!(reads_of(&e), HashSet::from([Axis::Abilities]));
    }

    #[test]
    fn mv_and_name_read_copy_layer() {
        // MV derives from the printed mana cost; only layer-1 copy rewrites it.
        for e in [
            Expr::Mv(Box::new(Expr::Ctx(Ctx::It))),
            Expr::Name(Box::new(Expr::Ctx(Ctx::It))),
        ] {
            assert_eq!(reads_of(&e), HashSet::from([Axis::Copy]));
        }
    }

    #[test]
    fn controller_reads_control_but_owner_is_free() {
        let ctrl = Expr::Controller(Box::new(Expr::Ctx(Ctx::It)));
        assert_eq!(reads_of(&ctrl), HashSet::from([Axis::Control]));
        // Owner never changes at runtime — no axis.
        let owner = Expr::Owner(Box::new(Expr::Ctx(Ctx::It)));
        assert_eq!(reads_of(&owner), HashSet::new());
    }

    #[test]
    fn zone_and_counters_and_life() {
        let z = Expr::ZoneOf(Box::new(Expr::Ctx(Ctx::It)));
        assert_eq!(reads_of(&z), HashSet::from([Axis::Zone]));
        let c = Expr::CountersOn(Box::new(Expr::Ctx(Ctx::It)), CounterType::Void);
        assert_eq!(reads_of(&c), HashSet::from([Axis::Counters]));
        let l = Expr::Life(Box::new(Expr::Ctx(Ctx::Controller)));
        assert_eq!(reads_of(&l), HashSet::from([Axis::Life]));
        let h = Expr::HandSize(Box::new(Expr::Ctx(Ctx::Controller)));
        assert_eq!(reads_of(&h), HashSet::from([Axis::HandSize]));
    }

    #[test]
    fn game_ctx_and_triggering_event() {
        assert_eq!(
            reads_of(&Expr::GameCtx(GameCtx::Monarch)),
            HashSet::from([Axis::GameCtx]),
        );
        assert_eq!(
            reads_of(&Expr::Ctx(Ctx::Triggering(EventField::DamageAmount))),
            HashSet::from([Axis::EventLog]),
        );
    }

    #[test]
    fn composed_predicate_unions_axes() {
        // "creature with power >= 3" — reads Type and PT.
        let e = Expr::And(
            Box::new(Expr::Contains(
                Box::new(Expr::TypeLit(CardType::Creature)),
                Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::It)))),
            )),
            Box::new(Expr::Ge(
                Box::new(Expr::Power(Box::new(Expr::Ctx(Ctx::It)))),
                Box::new(Expr::Num(3)),
            )),
        );
        assert_eq!(reads_of(&e), HashSet::from([Axis::Type, Axis::PT]));
    }

    #[test]
    fn all_objects_reads_zone_and_filter_axes() {
        // "Count creatures with flying on the battlefield" — Zone + Type + Abilities.
        let filter = Expr::And(
            Box::new(Expr::Contains(
                Box::new(Expr::TypeLit(CardType::Creature)),
                Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::Var("c"))))),
            )),
            Box::new(Expr::Contains(
                Box::new(Expr::KeywordLit(Keyword::Flying)),
                Box::new(Expr::Keywords(Box::new(Expr::Ctx(Ctx::Var("c"))))),
            )),
        );
        let e = Expr::Count(Box::new(Expr::AllObjects {
            zone: ZoneSel::Global(ZoneKindSel::Battlefield),
            bind: "c",
            filter: Box::new(filter),
        }));
        assert_eq!(
            reads_of(&e),
            HashSet::from([Axis::Zone, Axis::Type, Axis::Abilities]),
        );
    }

    #[test]
    fn dedup_removes_repeated_axes() {
        // Two power reads should collapse to one PT entry.
        let e = Expr::Add(
            Box::new(Expr::Power(Box::new(Expr::Ctx(Ctx::Var("a"))))),
            Box::new(Expr::Power(Box::new(Expr::Ctx(Ctx::Var("b"))))),
        );
        let d = deps_of(&e);
        assert_eq!(d.reads, vec![Axis::PT]);
    }

    // ── writes_of for each CEMod variant ────────────────────────────────────

    #[test]
    fn pt_writers() {
        for m in [
            CEMod::PumpPT(Expr::Num(1), Expr::Num(1)),
            CEMod::SetPT(Expr::Num(2), Expr::Num(2)),
            CEMod::SetPower(Expr::Num(0)),
            CEMod::SetToughness(Expr::Num(0)),
        ] {
            assert_eq!(writes_of(&m), vec![Axis::PT]);
        }
    }

    #[test]
    fn type_writers() {
        for m in [
            CEMod::OverrideTypes(vec![CardType::Creature]),
            CEMod::AddType(CardType::Artifact),
            CEMod::AddSubtype("Bear".into()),
            CEMod::RemoveSubtype("Bear".into()),
        ] {
            assert_eq!(writes_of(&m), vec![Axis::Type]);
        }
    }

    #[test]
    fn color_writers() {
        assert_eq!(writes_of(&CEMod::SetColors(vec![Color::Red])), vec![Axis::Color]);
        assert_eq!(writes_of(&CEMod::AddColor(Color::Blue)), vec![Axis::Color]);
    }

    #[test]
    fn abilities_writers() {
        for m in [
            CEMod::AddKeyword(Keyword::Flying),
            CEMod::RemoveKeyword(Keyword::Flying),
            CEMod::CantAttack,
            CEMod::CantBlock,
            CEMod::SetProtection(Expr::ColorLit(Color::White)),
        ] {
            assert_eq!(writes_of(&m), vec![Axis::Abilities]);
        }
    }

    #[test]
    fn copy_writer_covers_all_characteristic_layers() {
        // Layer 1 copy rewrites everything downstream — any characteristic
        // reader must order after copy.
        let w: HashSet<Axis> = writes_of(&CEMod::CopyOf(Expr::Ctx(Ctx::Source)))
            .into_iter()
            .collect();
        assert!(w.contains(&Axis::Copy));
        assert!(w.contains(&Axis::Type));
        assert!(w.contains(&Axis::Color));
        assert!(w.contains(&Axis::Abilities));
        assert!(w.contains(&Axis::PT));
    }

    #[test]
    fn rule_mod_writers() {
        for m in [
            CEMod::AllowLoss(Expr::Bool(true)),
            CEMod::MaxHandSize(Expr::Num(7)),
            CEMod::ExtraLandDrops(Expr::Num(1)),
            CEMod::SkipStep(crate::StepKind::Untap),
        ] {
            assert_eq!(writes_of(&m), vec![Axis::RuleMod]);
        }
    }

    #[test]
    fn cast_permission_writers() {
        for m in [
            CEMod::CastableFrom(ZoneKindSel::Graveyard),
            CEMod::AltCost(CostSpec::Free),
            CEMod::AnyColorMana,
            CEMod::GrantFlash,
            CEMod::OnResolveExile,
        ] {
            assert_eq!(writes_of(&m), vec![Axis::CastPermission]);
        }
    }

    #[test]
    fn cost_mod_writers() {
        for m in [
            CEMod::CastingCostPlus(Expr::Num(2)),
            CEMod::SpellsCostMore {
                filter: Filter(Expr::Bool(true)),
                amount: Expr::Num(1),
            },
            CEMod::SpellsCostLess {
                filter: Filter(Expr::Bool(true)),
                amount: Expr::Num(1),
            },
        ] {
            assert_eq!(writes_of(&m), vec![Axis::CostMod]);
        }
    }

    // ── End-to-end dep edge ─────────────────────────────────────────────────

    #[test]
    fn ce_edge_pump_then_read_power() {
        // A CE pumps P/T; another reads power. There must be a PT edge.
        let writer = CEMod::PumpPT(Expr::Num(1), Expr::Num(1));
        let reader = Expr::Power(Box::new(Expr::Ctx(Ctx::It)));
        let w: HashSet<Axis> = writes_of(&writer).into_iter().collect();
        let r: HashSet<Axis> = deps_of(&reader).reads.into_iter().collect();
        assert!(
            !w.is_disjoint(&r),
            "expected overlap between writer {:?} and reader {:?}", w, r
        );
    }

    #[test]
    fn ce_edge_copy_then_read_type() {
        // Copy writes to Type; a reader of Types must order after copy.
        let writer = CEMod::CopyOf(Expr::Ctx(Ctx::Source));
        let reader = Expr::Types(Box::new(Expr::Ctx(Ctx::It)));
        let w: HashSet<Axis> = writes_of(&writer).into_iter().collect();
        let r: HashSet<Axis> = deps_of(&reader).reads.into_iter().collect();
        assert!(w.contains(&Axis::Type));
        assert!(!w.is_disjoint(&r));
    }

    #[test]
    fn ce_edge_no_overlap_is_independent() {
        // Life reader is independent of a PT writer — no ordering edge.
        let writer = CEMod::PumpPT(Expr::Num(1), Expr::Num(1));
        let reader = Expr::Life(Box::new(Expr::Ctx(Ctx::Controller)));
        let w: HashSet<Axis> = writes_of(&writer).into_iter().collect();
        let r: HashSet<Axis> = deps_of(&reader).reads.into_iter().collect();
        assert!(
            w.is_disjoint(&r),
            "expected no overlap; got w={:?} r={:?}", w, r
        );
    }

    // Silence unused import warnings for the supertype import (kept for parity
    // with sibling modules using the same import pattern).
    #[allow(dead_code)]
    fn _touch(_: Supertype) {}
}

// ── Parity tests: IR filter vs closure predicate ────────────────────────────
//
// For each predicate in `predicates.rs`, build an equivalent IR Filter and
// verify they agree on a small fixture. Every new predicate variant added to
// the closure API should ship with a matching IR case here.

mod parity {
    use super::*;
    use crate::catalog::{ArtifactData, CreatureData, LandData, LandTypes, BasicLandType};
    use crate::{
        predicates as P, CardDef, CardKind, CardLayout, CardType, CardZone, Color,
        CounterType, GameObject, Keyword, ObjId, PlayerId, PlayerState, SimState,
        Supertype,
    };
    use std::collections::HashMap;

    fn make_empty_state() -> SimState {
        let us = PlayerState::new("us_deck");
        let opp = PlayerState::new("opp_deck");
        SimState::new(us, opp)
    }

    /// Insert an object with a materialized def — query-side needs `def_of` to work.
    fn insert_with_def(state: &mut SimState, owner: PlayerId, def: CardDef) -> ObjId {
        let id = state.alloc_id();
        state.objects.insert(
            id,
            GameObject {
                id,
                catalog_key: def.name.clone(),
                owner,
                controller: owner,
                zone: CardZone::Battlefield,
                is_token: false,
                spell: None,
                bf: None,
                materialized: Some(def.clone()),
                counters: HashMap::new(),
                ci_timestamp: 0,
            },
        );
        state.catalog.entry(def.name.clone()).or_insert(def);
        id
    }

    fn make_creature(name: &str, mana: &str, colors: Vec<Color>, power: i32, toughness: i32,
                     legendary: bool, keywords: &[Keyword], subtypes: &[&str]) -> CardDef {
        let mut c = CreatureData::new(mana, power, toughness);
        c.keywords = crate::catalog::Keywords::from_slice(keywords);
        c.creature_subtypes = subtypes.iter().map(|s| (*s).into()).collect();
        c.legendary = legendary;
        let supers = if legendary { vec![Supertype::Legendary] } else { vec![] };
        CardDef::new(name, CardKind::Creature(c), colors, None, supers,
                     CardLayout::Normal, None, vec![], vec![], vec![], vec![])
    }

    fn make_island(name: &str) -> CardDef {
        let land = LandData {
            land_types: LandTypes::from_types(&[BasicLandType::Island]),
            abilities: vec![],
            mana_abilities: vec![],
        };
        CardDef::new(name, CardKind::Land(land), vec![], None, vec![Supertype::Basic],
                     CardLayout::Normal, None, vec![], vec![], vec![], vec![])
    }

    fn make_equipment(name: &str, mana: &str) -> CardDef {
        let a = ArtifactData {
            mana_cost: mana.into(),
            abilities: vec![],
            mana_abilities: vec![],
            subtypes: vec!["Equipment".into()],
        };
        CardDef::new(name, CardKind::Artifact(a), vec![], None, vec![],
                     CardLayout::Normal, None, vec![], vec![], vec![], vec![])
    }

    /// Build the IR equivalent of `obj_pred_from_card(pred_type_eq(t))`.
    fn ir_type_is(t: CardType) -> Filter {
        Filter(Expr::Contains(
            Box::new(Expr::TypeLit(t)),
            Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::It)))),
        ))
    }

    fn ir_has_supertype(s: Supertype) -> Filter {
        Filter(Expr::Contains(
            Box::new(Expr::SupertypeLit(s)),
            Box::new(Expr::Supertypes(Box::new(Expr::Ctx(Ctx::It)))),
        ))
    }

    fn ir_has_color(c: Color) -> Filter {
        Filter(Expr::Contains(
            Box::new(Expr::ColorLit(c)),
            Box::new(Expr::Colors(Box::new(Expr::Ctx(Ctx::It)))),
        ))
    }

    fn ir_mana_value_le(n: i32) -> Filter {
        Filter(Expr::Le(
            Box::new(Expr::Mv(Box::new(Expr::Ctx(Ctx::It)))),
            Box::new(Expr::Num(n as i64)),
        ))
    }

    fn ir_mana_value_eq(n: i32) -> Filter {
        Filter(Expr::Eq(
            Box::new(Expr::Mv(Box::new(Expr::Ctx(Ctx::It)))),
            Box::new(Expr::Num(n as i64)),
        ))
    }

    fn ir_toughness_le(n: i32) -> Filter {
        Filter(Expr::And(
            Box::new(ir_type_is(CardType::Creature).0),
            Box::new(Expr::Le(
                Box::new(Expr::Toughness(Box::new(Expr::Ctx(Ctx::It)))),
                Box::new(Expr::Num(n as i64)),
            )),
        ))
    }

    fn ir_has_keyword(kw: Keyword) -> Filter {
        Filter(Expr::Contains(
            Box::new(Expr::KeywordLit(kw)),
            Box::new(Expr::Keywords(Box::new(Expr::Ctx(Ctx::It)))),
        ))
    }

    fn ir_has_subtype(st: &str) -> Filter {
        Filter(Expr::Contains(
            Box::new(Expr::SubtypeLit(st.into())),
            Box::new(Expr::Subtypes(Box::new(Expr::Ctx(Ctx::It)))),
        ))
    }

    fn ir_land_subtype(st: &str) -> Filter {
        // "is a land AND has the land subtype"
        Filter(Expr::And(
            Box::new(ir_type_is(CardType::Land).0),
            Box::new(Expr::Contains(
                Box::new(Expr::SubtypeLit(st.into())),
                Box::new(Expr::Subtypes(Box::new(Expr::Ctx(Ctx::It)))),
            )),
        ))
    }

    fn ir_has_counter(ct: CounterType) -> Filter {
        Filter(Expr::Gt(
            Box::new(Expr::CountersOn(Box::new(Expr::Ctx(Ctx::It)), ct)),
            Box::new(Expr::Num(0)),
        ))
    }

    fn assert_parity(
        label: &str,
        state: &SimState,
        id: ObjId,
        closure_pred: impl Fn(ObjId, &SimState) -> bool,
        ir_filter: &Filter,
    ) {
        let env = BindEnv::new();
        let closure_ans = closure_pred(id, state);
        let ir_ans = matches(ir_filter, id, state, &env);
        assert_eq!(
            closure_ans, ir_ans,
            "parity mismatch for {label} on obj {:?}: closure={}, ir={}",
            id, closure_ans, ir_ans
        );
    }

    #[test]
    fn type_predicates() {
        let mut s = make_empty_state();
        let cid = insert_with_def(&mut s, PlayerId::Us,
            make_creature("Grizzly Bears", "{1}{G}", vec![Color::Green], 2, 2,
                          false, &[], &["Bear"]));
        let iid = insert_with_def(&mut s, PlayerId::Us, make_island("Island"));
        let eid = insert_with_def(&mut s, PlayerId::Us, make_equipment("Sword", "{2}"));

        let closure_creature = P::obj_pred_from_card(P::pred_type_eq(CardType::Creature));
        let ir = ir_type_is(CardType::Creature);
        for (label, id) in [("bear", cid), ("island", iid), ("sword", eid)] {
            assert_parity(label, &s, id, |i, st| closure_creature(i, st), &ir);
        }
    }

    #[test]
    fn supertype_color_subtype_predicates() {
        let mut s = make_empty_state();
        let bear = insert_with_def(&mut s, PlayerId::Us,
            make_creature("Grizzly Bears", "{1}{G}", vec![Color::Green], 2, 2,
                          false, &[], &["Bear"]));
        let dragon = insert_with_def(&mut s, PlayerId::Us,
            make_creature("Thundermaw Hellkite", "{3}{R}{R}", vec![Color::Red], 5, 5,
                          true, &[Keyword::Flying, Keyword::Haste], &["Dragon"]));
        let sword = insert_with_def(&mut s, PlayerId::Us, make_equipment("Sword", "{2}"));

        // Legendary
        let closure = P::obj_pred_from_card(P::pred_has_supertype(Supertype::Legendary));
        let ir = ir_has_supertype(Supertype::Legendary);
        for (l, id) in [("bear", bear), ("dragon", dragon)] {
            assert_parity(l, &s, id, |i, st| closure(i, st), &ir);
        }

        // Red
        let closure = P::obj_pred_from_card(P::pred_has_color(Color::Red));
        let ir = ir_has_color(Color::Red);
        for (l, id) in [("bear", bear), ("dragon", dragon)] {
            assert_parity(l, &s, id, |i, st| closure(i, st), &ir);
        }

        // subtype "Equipment"
        let closure = P::obj_pred_from_card(P::pred_has_subtype("Equipment"));
        let ir = ir_has_subtype("Equipment");
        for (l, id) in [("bear", bear), ("sword", sword), ("dragon", dragon)] {
            assert_parity(l, &s, id, |i, st| closure(i, st), &ir);
        }

        // subtype "Dragon"
        let closure = P::obj_pred_from_card(P::pred_has_subtype("Dragon"));
        let ir = ir_has_subtype("Dragon");
        for (l, id) in [("bear", bear), ("dragon", dragon)] {
            assert_parity(l, &s, id, |i, st| closure(i, st), &ir);
        }
    }

    #[test]
    fn mana_value_and_toughness_predicates() {
        let mut s = make_empty_state();
        let bear = insert_with_def(&mut s, PlayerId::Us,
            make_creature("Bear", "{1}{G}", vec![Color::Green], 2, 2, false, &[], &[]));
        let dragon = insert_with_def(&mut s, PlayerId::Us,
            make_creature("Dragon", "{3}{R}{R}", vec![Color::Red], 5, 5, false, &[], &[]));

        for n in [2, 3, 5] {
            let closure = P::obj_pred_from_card(P::pred_mana_value_le(n));
            let ir = ir_mana_value_le(n);
            for (l, id) in [("bear", bear), ("dragon", dragon)] {
                assert_parity(&format!("mv<={n} {l}"), &s, id, |i, st| closure(i, st), &ir);
            }

            let closure = P::obj_pred_from_card(P::pred_mana_value_eq(n));
            let ir = ir_mana_value_eq(n);
            for (l, id) in [("bear", bear), ("dragon", dragon)] {
                assert_parity(&format!("mv=={n} {l}"), &s, id, |i, st| closure(i, st), &ir);
            }

            let closure = P::obj_pred_from_card(P::pred_toughness_le(n));
            let ir = ir_toughness_le(n);
            for (l, id) in [("bear", bear), ("dragon", dragon)] {
                assert_parity(&format!("tough<={n} {l}"), &s, id, |i, st| closure(i, st), &ir);
            }
        }
    }

    #[test]
    fn keyword_predicate() {
        let mut s = make_empty_state();
        let bear = insert_with_def(&mut s, PlayerId::Us,
            make_creature("Bear", "{1}{G}", vec![Color::Green], 2, 2, false, &[], &[]));
        let dragon = insert_with_def(&mut s, PlayerId::Us,
            make_creature("Dragon", "{3}{R}{R}", vec![Color::Red], 5, 5, false,
                          &[Keyword::Flying, Keyword::Haste], &[]));

        for kw in [Keyword::Flying, Keyword::Haste, Keyword::Trample] {
            let closure = P::obj_pred_from_card(P::pred_has_keyword(kw));
            let ir = ir_has_keyword(kw);
            for (l, id) in [("bear", bear), ("dragon", dragon)] {
                assert_parity(&format!("{:?} {l}", kw), &s, id, |i, st| closure(i, st), &ir);
            }
        }
    }

    #[test]
    fn land_subtype_predicate() {
        let mut s = make_empty_state();
        let island = insert_with_def(&mut s, PlayerId::Us, make_island("Island"));
        let bear = insert_with_def(&mut s, PlayerId::Us,
            make_creature("Bear", "{1}{G}", vec![Color::Green], 2, 2, false, &[], &[]));

        let closure = P::obj_pred_from_card(P::pred_land_subtype("island"));
        let ir = ir_land_subtype("island");
        for (l, id) in [("island", island), ("bear", bear)] {
            assert_parity(l, &s, id, |i, st| closure(i, st), &ir);
        }
    }

    #[test]
    fn counter_predicate() {
        let mut s = make_empty_state();
        let bear = insert_with_def(&mut s, PlayerId::Us,
            make_creature("Bear", "{1}{G}", vec![Color::Green], 2, 2, false, &[], &[]));
        // stamp a Void counter on bear
        s.objects.get_mut(&bear).unwrap().counters.insert(CounterType::Void, 1);
        let dragon = insert_with_def(&mut s, PlayerId::Us,
            make_creature("Dragon", "{3}{R}{R}", vec![Color::Red], 5, 5, false, &[], &[]));

        let closure = P::pred_has_counter(CounterType::Void);
        let ir = ir_has_counter(CounterType::Void);
        for (l, id) in [("bear", bear), ("dragon", dragon)] {
            assert_parity(l, &s, id, |i, st| closure(i, st), &ir);
        }
    }

    #[test]
    fn predicate_composition() {
        // AND / OR / NOT parity: build a compound predicate both ways.
        let mut s = make_empty_state();
        let bear = insert_with_def(&mut s, PlayerId::Us,
            make_creature("Bear", "{1}{G}", vec![Color::Green], 2, 2, false, &[], &[]));
        let dragon = insert_with_def(&mut s, PlayerId::Us,
            make_creature("Dragon", "{3}{R}{R}", vec![Color::Red], 5, 5, false,
                          &[Keyword::Flying], &[]));

        // (creature AND mv<=3)
        let closure = P::obj_pred_from_card(P::pred_and(
            P::pred_type_eq(CardType::Creature),
            P::pred_mana_value_le(3),
        ));
        let ir = Filter(Expr::And(
            Box::new(ir_type_is(CardType::Creature).0),
            Box::new(ir_mana_value_le(3).0),
        ));
        for (l, id) in [("bear", bear), ("dragon", dragon)] {
            assert_parity(&format!("and {l}"), &s, id, |i, st| closure(i, st), &ir);
        }

        // NOT flying
        let closure = P::obj_pred_from_card(P::pred_not(P::pred_has_keyword(Keyword::Flying)));
        let ir = Filter(Expr::Not(Box::new(ir_has_keyword(Keyword::Flying).0)));
        for (l, id) in [("bear", bear), ("dragon", dragon)] {
            assert_parity(&format!("!flying {l}"), &s, id, |i, st| closure(i, st), &ir);
        }
    }

    #[test]
    fn scoped_binder_enumeration() {
        // "All creatures on the battlefield" — AllObjects walks both players' battlefields.
        let mut s = make_empty_state();
        let _ = insert_with_def(&mut s, PlayerId::Us,
            make_creature("A", "{G}", vec![Color::Green], 1, 1, false, &[], &[]));
        let _ = insert_with_def(&mut s, PlayerId::Opp,
            make_creature("B", "{G}", vec![Color::Green], 1, 1, false, &[], &[]));
        let _ = insert_with_def(&mut s, PlayerId::Us, make_island("Island"));

        let all_creatures = Expr::AllObjects {
            zone: ZoneSel::Global(ZoneKindSel::Battlefield),
            bind: "c",
            filter: Box::new(ir_type_is(CardType::Creature).0),
        };
        let env = BindEnv::new();
        match eval_expr(&all_creatures, &s, &env) {
            Value::ObjSet(v) => assert_eq!(v.len(), 2, "expected 2 creatures, got {:?}", v),
            other => panic!("expected ObjSet, got {:?}", other),
        }

        // Count(AllObjects(creatures)) — should be 2
        let count_expr = Expr::Count(Box::new(all_creatures));
        let v = eval_expr(&count_expr, &s, &env);
        assert!(matches!(v, Value::Num(2)), "expected 2, got {:?}", v);
    }
}

// ── Parity tests: IR Action vs closure Effect ───────────────────────────────
//
// For each primitive in `effects.rs`, run the closure path and the IR path on
// independent clones of the same SimState; assert the game-relevant state
// (life, hand, zones, counters, battlefield damage) converges. Logs diverge by
// design — IR does not emit `state.log` yet — so we compare structural fields.

mod execute_parity {
    use super::*;
    use crate::catalog::{ArtifactData, CreatureData, LandData, LandTypes, BasicLandType};
    use crate::{
        effects as E, CardDef, CardKind, CardLayout, CardZone, Color, CounterType,
        GameObject, Keyword, ObjId, PlayerId, PlayerState, SimState, Supertype,
    };
    use std::collections::HashMap;

    fn make_state() -> SimState {
        let us = PlayerState::new("us");
        let opp = PlayerState::new("opp");
        SimState::new(us, opp)
    }

    fn make_creature(name: &str, mana: &str, power: i32, toughness: i32) -> CardDef {
        let c = CreatureData::new(mana, power, toughness);
        CardDef::new(
            name,
            CardKind::Creature(c),
            vec![Color::Green],
            None,
            vec![],
            CardLayout::Normal,
            None,
            vec![],
            vec![],
            vec![],
            vec![],
        )
    }

    fn make_artifact(name: &str) -> CardDef {
        let a = ArtifactData {
            mana_cost: "{2}".into(),
            abilities: vec![],
            mana_abilities: vec![],
            subtypes: vec![],
        };
        CardDef::new(
            name,
            CardKind::Artifact(a),
            vec![],
            None,
            vec![],
            CardLayout::Normal,
            None,
            vec![],
            vec![],
            vec![],
            vec![],
        )
    }

    fn make_land(name: &str) -> CardDef {
        let land = LandData {
            land_types: LandTypes::from_types(&[BasicLandType::Island]),
            abilities: vec![],
            mana_abilities: vec![],
        };
        CardDef::new(
            name,
            CardKind::Land(land),
            vec![],
            None,
            vec![Supertype::Basic],
            CardLayout::Normal,
            None,
            vec![],
            vec![],
            vec![],
            vec![],
        )
    }

    /// Insert an object without a zone yet; caller sets the zone via `set_card_zone`.
    fn insert_obj(state: &mut SimState, owner: PlayerId, def: CardDef) -> ObjId {
        let id = state.alloc_id();
        state.objects.insert(
            id,
            GameObject {
                id,
                catalog_key: def.name.clone(),
                owner,
                controller: owner,
                zone: CardZone::Library,
                is_token: false,
                spell: None,
                bf: None,
                materialized: Some(def.clone()),
                counters: HashMap::new(),
                ci_timestamp: 0,
            },
        );
        state.catalog.entry(def.name.clone()).or_insert(def);
        // default: freshly-allocated objects land in library_order for the player
        state.player_mut(owner).library_order.push_back(id);
        id
    }

    fn put_on_bf(state: &mut SimState, id: ObjId) {
        state.set_card_zone(id, CardZone::Battlefield);
        if let Some(obj) = state.objects.get_mut(&id) {
            obj.bf = Some(crate::BattlefieldState {
                tapped: false,
                damage: 0,
                entered_this_turn: false,
                counters: 0,
                power_mod: 0,
                toughness_mod: 0,
                loyalty: 0,
                pw_activated_this_turn: false,
                attacking: false,
                unblocked: false,
                attack_target: None,
                active_face: 0,
                etb_choice: None,
                attached_to: None,
                stun_counters: 0,
            });
        }
    }

    // ── Draw ─────────────────────────────────────────────────────────────────
    // SimState is not Clone, so each parity test defines a `setup` closure
    // producing a fresh state with deterministic object IDs (alloc_id just
    // increments a counter), then runs the closure path and the IR path on
    // two independently-constructed states.

    #[test]
    fn draw_parity() {
        let setup = || {
            let mut s = make_state();
            insert_obj(&mut s, PlayerId::Us, make_creature("A", "{G}", 1, 1));
            insert_obj(&mut s, PlayerId::Us, make_creature("B", "{G}", 1, 1));
            insert_obj(&mut s, PlayerId::Us, make_creature("C", "{G}", 1, 1));
            s
        };

        let mut closure_state = setup();
        let mut ir_state = setup();

        E::eff_draw(PlayerId::Us, 2).call(&mut closure_state, 0, &[]);
        execute(
            &Action::Draw { who: Who::You, n: Expr::Num(2) },
            &mut ir_state,
            &BindEnv::new().with_controller(PlayerId::Us),
        );

        let closure_hand: std::collections::HashSet<ObjId> =
            closure_state.hand_of(PlayerId::Us).map(|o| o.id).collect();
        let ir_hand: std::collections::HashSet<ObjId> =
            ir_state.hand_of(PlayerId::Us).map(|o| o.id).collect();
        assert_eq!(closure_hand, ir_hand, "hand diverged after Draw");

        let closure_lib: Vec<ObjId> =
            closure_state.player(PlayerId::Us).library_order.iter().copied().collect();
        let ir_lib: Vec<ObjId> =
            ir_state.player(PlayerId::Us).library_order.iter().copied().collect();
        assert_eq!(closure_lib, ir_lib, "library diverged after Draw");
    }

    // ── PayLife / GainLife ───────────────────────────────────────────────────

    #[test]
    fn pay_life_parity() {
        let setup = || make_state();

        let mut closure_state = setup();
        let mut ir_state = setup();
        let start = closure_state.life_of(PlayerId::Us);

        E::eff_life_loss(PlayerId::Us, 3).call(&mut closure_state, 0, &[]);
        execute(
            &Action::PayLife { who: Who::You, amount: Expr::Num(3) },
            &mut ir_state,
            &BindEnv::new().with_controller(PlayerId::Us),
        );

        assert_eq!(closure_state.life_of(PlayerId::Us), start - 3);
        assert_eq!(ir_state.life_of(PlayerId::Us), start - 3);
    }

    // ── DealDamage to a player ───────────────────────────────────────────────

    #[test]
    fn damage_player_parity() {
        let setup = || make_state();

        let mut closure_state = setup();
        let mut ir_state = setup();
        let opp_id = closure_state.player(PlayerId::Opp).id;
        let start = closure_state.life_of(PlayerId::Opp);

        // Closure path: zero source id avoids protection checks and models
        // "generic damage source, player target".
        E::eff_damage_target(PlayerId::Us, 3, ObjId::UNSET).call(
            &mut closure_state,
            0,
            &[opp_id],
        );
        execute(
            &Action::DealDamage {
                source: Expr::Ctx(Ctx::Source),
                target: Expr::Ctx(Ctx::Var("tgt")),
                amount: Expr::Num(3),
            },
            &mut ir_state,
            &BindEnv::new()
                .with_controller(PlayerId::Us)
                .with_var("tgt", Value::Player(PlayerId::Opp)),
        );

        assert_eq!(closure_state.life_of(PlayerId::Opp), start - 3);
        assert_eq!(ir_state.life_of(PlayerId::Opp), start - 3);
    }

    // ── DealDamage to a permanent ────────────────────────────────────────────

    #[test]
    fn damage_permanent_parity() {
        let setup = || {
            let mut s = make_state();
            let cid = insert_obj(&mut s, PlayerId::Opp, make_creature("Bear", "{1}{G}", 2, 3));
            put_on_bf(&mut s, cid);
            (s, cid)
        };

        let (mut closure_state, c_id) = setup();
        let (mut ir_state, i_id) = setup();
        assert_eq!(c_id, i_id, "setup must be deterministic across runs");

        E::eff_damage_target(PlayerId::Us, 2, ObjId::UNSET).call(
            &mut closure_state,
            0,
            &[c_id],
        );
        execute(
            &Action::DealDamage {
                source: Expr::Ctx(Ctx::Source),
                target: Expr::Ctx(Ctx::Var("tgt")),
                amount: Expr::Num(2),
            },
            &mut ir_state,
            &BindEnv::new()
                .with_controller(PlayerId::Us)
                .with_var("tgt", Value::Obj(i_id)),
        );

        let c_dmg = closure_state.objects.get(&c_id).and_then(|o| o.bf.as_ref()).map(|bf| bf.damage);
        let i_dmg = ir_state.objects.get(&i_id).and_then(|o| o.bf.as_ref()).map(|bf| bf.damage);
        assert_eq!(c_dmg, Some(2));
        assert_eq!(i_dmg, Some(2));
    }

    // ── Destroy ──────────────────────────────────────────────────────────────

    #[test]
    fn destroy_parity() {
        let setup = || {
            let mut s = make_state();
            let cid = insert_obj(&mut s, PlayerId::Us, make_creature("Bear", "{1}{G}", 2, 2));
            put_on_bf(&mut s, cid);
            (s, cid)
        };

        let (mut closure_state, c_id) = setup();
        let (mut ir_state, i_id) = setup();

        E::eff_destroy_target(PlayerId::Us).call(&mut closure_state, 0, &[c_id]);
        execute(
            &Action::Destroy { target: Expr::Ctx(Ctx::Var("t")) },
            &mut ir_state,
            &BindEnv::new()
                .with_controller(PlayerId::Us)
                .with_var("t", Value::Obj(i_id)),
        );

        let c_zone = closure_state.objects.get(&c_id).map(|o| o.zone);
        let i_zone = ir_state.objects.get(&i_id).map(|o| o.zone);
        assert!(matches!(c_zone, Some(CardZone::Graveyard)));
        assert_eq!(c_zone, i_zone);
    }

    // ── Exile ────────────────────────────────────────────────────────────────

    #[test]
    fn exile_parity() {
        let setup = || {
            let mut s = make_state();
            let cid = insert_obj(&mut s, PlayerId::Us, make_artifact("Relic"));
            put_on_bf(&mut s, cid);
            (s, cid)
        };

        let (mut closure_state, c_id) = setup();
        let (mut ir_state, i_id) = setup();

        E::eff_exile_target(PlayerId::Us).call(&mut closure_state, 0, &[c_id]);
        execute(
            &Action::Exile { target: Expr::Ctx(Ctx::Var("t")), bind_as: None },
            &mut ir_state,
            &BindEnv::new()
                .with_controller(PlayerId::Us)
                .with_var("t", Value::Obj(i_id)),
        );

        let c_zone = closure_state.objects.get(&c_id).map(|o| o.zone);
        let i_zone = ir_state.objects.get(&i_id).map(|o| o.zone);
        assert!(matches!(c_zone, Some(CardZone::Exile { .. })));
        assert!(matches!(i_zone, Some(CardZone::Exile { .. })));
    }

    // ── Return (bounce to hand) ─────────────────────────────────────────────

    #[test]
    fn bounce_parity() {
        let setup = || {
            let mut s = make_state();
            let cid = insert_obj(&mut s, PlayerId::Us, make_creature("Bear", "{G}", 2, 2));
            put_on_bf(&mut s, cid);
            (s, cid)
        };

        let (mut closure_state, c_id) = setup();
        let (mut ir_state, i_id) = setup();

        E::eff_bounce_target(PlayerId::Us).call(&mut closure_state, 0, &[c_id]);
        execute(
            &Action::Return {
                what: Expr::Ctx(Ctx::Var("t")),
                to: ZoneKindSel::Hand,
                bind_as: None,
            },
            &mut ir_state,
            &BindEnv::new()
                .with_controller(PlayerId::Us)
                .with_var("t", Value::Obj(i_id)),
        );

        let c_zone = closure_state.objects.get(&c_id).map(|o| o.zone);
        let i_zone = ir_state.objects.get(&i_id).map(|o| o.zone);
        assert!(matches!(c_zone, Some(CardZone::Hand { .. })));
        assert!(matches!(i_zone, Some(CardZone::Hand { .. })));
    }

    // ── Counters ─────────────────────────────────────────────────────────────
    //
    // `effects.rs` has no counter primitive; the baseline is a direct
    // `obj.counters.entry(...).or_insert(0) += n`, matching how card code
    // manipulates counters today.

    #[test]
    fn put_counters_parity() {
        let setup = || {
            let mut s = make_state();
            let cid = insert_obj(&mut s, PlayerId::Us, make_creature("Bear", "{G}", 2, 2));
            put_on_bf(&mut s, cid);
            (s, cid)
        };

        let (mut baseline, b_id) = setup();
        let (mut ir_state, i_id) = setup();

        *baseline.objects.get_mut(&b_id).unwrap().counters.entry(CounterType::Void).or_insert(0) += 2;
        execute(
            &Action::PutCounters {
                on: Expr::Ctx(Ctx::Var("t")),
                kind: CounterType::Void,
                n: Expr::Num(2),
            },
            &mut ir_state,
            &BindEnv::new()
                .with_controller(PlayerId::Us)
                .with_var("t", Value::Obj(i_id)),
        );

        let b = baseline.objects.get(&b_id).unwrap().counters.get(&CounterType::Void).copied();
        let i = ir_state.objects.get(&i_id).unwrap().counters.get(&CounterType::Void).copied();
        assert_eq!(b, Some(2));
        assert_eq!(i, Some(2));
    }

    #[test]
    fn remove_counters_saturates_at_zero() {
        let mut s = make_state();
        let cid = insert_obj(&mut s, PlayerId::Us, make_creature("Bear", "{G}", 2, 2));
        put_on_bf(&mut s, cid);
        s.objects.get_mut(&cid).unwrap().counters.insert(CounterType::Void, 1);

        execute(
            &Action::RemoveCounters {
                from: Expr::Ctx(Ctx::Var("t")),
                kind: CounterType::Void,
                n: Expr::Num(5),
            },
            &mut s,
            &BindEnv::new()
                .with_controller(PlayerId::Us)
                .with_var("t", Value::Obj(cid)),
        );

        let n = s.objects.get(&cid).unwrap().counters.get(&CounterType::Void).copied();
        assert_eq!(n, Some(0), "remove should saturate at 0, got {:?}", n);
    }

    // ── Mill ─────────────────────────────────────────────────────────────────
    //
    // No closure equivalent; baseline is manual change_zone calls matching
    // what `execute` should do.

    #[test]
    fn mill_parity_against_manual_baseline() {
        let setup = || {
            let mut s = make_state();
            let a = insert_obj(&mut s, PlayerId::Us, make_creature("A", "{G}", 1, 1));
            let b = insert_obj(&mut s, PlayerId::Us, make_creature("B", "{G}", 1, 1));
            let _c = insert_obj(&mut s, PlayerId::Us, make_creature("C", "{G}", 1, 1));
            (s, a, b)
        };

        let (mut baseline, ba, bb) = setup();
        let (mut ir_state, _ia, _ib) = setup();

        let top: Vec<ObjId> = baseline.library_of(PlayerId::Us).take(2).map(|o| o.id).collect();
        assert_eq!(top, vec![ba, bb]);
        for id in top {
            crate::change_zone(id, crate::ZoneId::Graveyard, &mut baseline, 0, PlayerId::Us);
        }

        execute(
            &Action::Mill { who: Who::You, count: Expr::Num(2) },
            &mut ir_state,
            &BindEnv::new().with_controller(PlayerId::Us),
        );

        let baseline_gy: std::collections::HashSet<ObjId> =
            baseline.graveyard_of(PlayerId::Us).map(|o| o.id).collect();
        let ir_gy: std::collections::HashSet<ObjId> =
            ir_state.graveyard_of(PlayerId::Us).map(|o| o.id).collect();
        assert_eq!(baseline_gy, ir_gy);

        let baseline_lib: Vec<ObjId> =
            baseline.player(PlayerId::Us).library_order.iter().copied().collect();
        let ir_lib: Vec<ObjId> =
            ir_state.player(PlayerId::Us).library_order.iter().copied().collect();
        assert_eq!(baseline_lib, ir_lib);
    }

    // ── Control flow ─────────────────────────────────────────────────────────

    #[test]
    fn sequence_runs_in_order() {
        let mut s = make_state();
        let start = s.life_of(PlayerId::Us);
        execute(
            &Action::Sequence(vec![
                Action::PayLife { who: Who::You, amount: Expr::Num(2) },
                Action::PayLife { who: Who::You, amount: Expr::Num(3) },
            ]),
            &mut s,
            &BindEnv::new().with_controller(PlayerId::Us),
        );
        assert_eq!(s.life_of(PlayerId::Us), start - 5);
    }

    #[test]
    fn if_then_branches_correctly() {
        // true branch
        let mut s = make_state();
        let start = s.life_of(PlayerId::Us);
        execute(
            &Action::IfThen {
                cond: Expr::Bool(true),
                then: Box::new(Action::PayLife { who: Who::You, amount: Expr::Num(1) }),
                else_: Some(Box::new(Action::PayLife { who: Who::You, amount: Expr::Num(10) })),
            },
            &mut s,
            &BindEnv::new().with_controller(PlayerId::Us),
        );
        assert_eq!(s.life_of(PlayerId::Us), start - 1);

        // false branch
        let mut s = make_state();
        let start = s.life_of(PlayerId::Us);
        execute(
            &Action::IfThen {
                cond: Expr::Bool(false),
                then: Box::new(Action::PayLife { who: Who::You, amount: Expr::Num(1) }),
                else_: Some(Box::new(Action::PayLife { who: Who::You, amount: Expr::Num(10) })),
            },
            &mut s,
            &BindEnv::new().with_controller(PlayerId::Us),
        );
        assert_eq!(s.life_of(PlayerId::Us), start - 10);
    }

    #[test]
    fn for_each_damages_all_creatures() {
        // Deal 1 damage to each creature on the battlefield.
        let mut s = make_state();
        let a = insert_obj(&mut s, PlayerId::Us, make_creature("A", "{G}", 1, 2));
        put_on_bf(&mut s, a);
        let b = insert_obj(&mut s, PlayerId::Opp, make_creature("B", "{G}", 1, 2));
        put_on_bf(&mut s, b);
        let land = insert_obj(&mut s, PlayerId::Us, make_land("Island"));
        put_on_bf(&mut s, land);

        execute(
            &Action::ForEach {
                over: Expr::AllObjects {
                    zone: ZoneSel::Global(ZoneKindSel::Battlefield),
                    bind: "c",
                    filter: Box::new(Expr::Contains(
                        Box::new(Expr::TypeLit(crate::CardType::Creature)),
                        Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::Var("c"))))),
                    )),
                },
                bind: "c",
                body: Box::new(Action::DealDamage {
                    source: Expr::Ctx(Ctx::Source),
                    target: Expr::Ctx(Ctx::Var("c")),
                    amount: Expr::Num(1),
                }),
            },
            &mut s,
            &BindEnv::new().with_controller(PlayerId::Us),
        );

        assert_eq!(s.objects.get(&a).unwrap().bf.as_ref().unwrap().damage, 1);
        assert_eq!(s.objects.get(&b).unwrap().bf.as_ref().unwrap().damage, 1);
        assert_eq!(s.objects.get(&land).unwrap().bf.as_ref().unwrap().damage, 0);
    }

    // ── Agency actions ───────────────────────────────────────────────────────

    use crate::ir::expr::Filter;
    use crate::{ChoiceRequest, ChoiceResult};
    use rand::SeedableRng;

    fn seeded_rng(seed: u64) -> Box<dyn rand::RngCore + Send> {
        Box::new(rand::rngs::StdRng::seed_from_u64(seed))
    }

    #[test]
    fn sacrifice_parity() {
        // Strategy picks the candidate with the smallest ObjId — deterministic
        // regardless of HashMap iteration order in `permanents_of`.
        let setup = || {
            let mut s = make_state();
            let a = insert_obj(&mut s, PlayerId::Us, make_creature("A", "{G}", 1, 1));
            let b = insert_obj(&mut s, PlayerId::Us, make_creature("B", "{G}", 2, 2));
            put_on_bf(&mut s, a);
            put_on_bf(&mut s, b);
            s.sacrifice_choice = std::sync::Arc::new(|_who, cands, _state| {
                cands.iter().min_by_key(|id| id.0).copied()
            });
            (s, a, b)
        };

        let (mut closure_state, a1, _b1) = setup();
        let (mut ir_state, a2, _b2) = setup();

        E::eff_sacrifice(
            PlayerId::Us,
            crate::effects::Who::Actor,
            std::sync::Arc::new(|_id, _s| true),
        )
        .call(&mut closure_state, 0, &[]);

        let creature_filter = Filter(Expr::Contains(
            Box::new(Expr::TypeLit(crate::CardType::Creature)),
            Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::It)))),
        ));
        execute(
            &Action::Sacrifice {
                who: Who::You,
                filter: creature_filter,
                count: Expr::Num(1),
                bind_as: None,
            },
            &mut ir_state,
            &BindEnv::new().with_controller(PlayerId::Us),
        );

        // Both paths should have sacrificed the smallest-id candidate (a).
        assert!(matches!(
            closure_state.objects.get(&a1).unwrap().zone,
            CardZone::Graveyard
        ));
        assert!(matches!(
            ir_state.objects.get(&a2).unwrap().zone,
            CardZone::Graveyard
        ));
    }

    #[test]
    fn surveil_parity() {
        // surveil_choice always returns true → top card always goes to graveyard.
        let setup = || {
            let mut s = make_state();
            insert_obj(&mut s, PlayerId::Us, make_creature("A", "{G}", 1, 1));
            insert_obj(&mut s, PlayerId::Us, make_creature("B", "{G}", 1, 1));
            insert_obj(&mut s, PlayerId::Us, make_creature("C", "{G}", 1, 1));
            s.surveil_choice = std::sync::Arc::new(|_id, _s| true);
            s
        };

        let mut closure_state = setup();
        let mut ir_state = setup();

        E::eff_surveil(PlayerId::Us, 2).call(&mut closure_state, 0, &[]);
        execute(
            &Action::Surveil { who: Who::You, n: Expr::Num(2) },
            &mut ir_state,
            &BindEnv::new().with_controller(PlayerId::Us),
        );

        let closure_gy: usize = closure_state.graveyard_of(PlayerId::Us).count();
        let ir_gy: usize = ir_state.graveyard_of(PlayerId::Us).count();
        assert_eq!(closure_gy, ir_gy, "graveyard size diverged after Surveil");
        assert_eq!(closure_gy, 2);
    }

    #[test]
    fn scry_parity() {
        // evaluate_card: card at index 0 scores 0.9 (keep), others score 0.1 (bottom).
        let setup = || {
            let mut s = make_state();
            let a = insert_obj(&mut s, PlayerId::Us, make_creature("A", "{G}", 1, 1));
            let b = insert_obj(&mut s, PlayerId::Us, make_creature("B", "{G}", 1, 1));
            let c = insert_obj(&mut s, PlayerId::Us, make_creature("C", "{G}", 1, 1));
            let keep = a; // captured into evaluator
            s.evaluate_card = std::sync::Arc::new(move |_who, id, _s| {
                if id == keep { 0.9 } else { 0.1 }
            });
            (s, a, b, c)
        };

        let (mut closure_state, _, _, _) = setup();
        let (mut ir_state, _, _, _) = setup();

        E::eff_scry(PlayerId::Us, 3).call(&mut closure_state, 0, &[]);
        execute(
            &Action::Scry { who: Who::You, n: Expr::Num(3) },
            &mut ir_state,
            &BindEnv::new().with_controller(PlayerId::Us),
        );

        let closure_lib: Vec<ObjId> =
            closure_state.player(PlayerId::Us).library_order.iter().copied().collect();
        let ir_lib: Vec<ObjId> =
            ir_state.player(PlayerId::Us).library_order.iter().copied().collect();
        assert_eq!(closure_lib, ir_lib, "library order diverged after Scry");
    }

    #[test]
    fn counter_parity() {
        // Put a spell on the stack via direct manipulation; compare counter paths.
        let setup = || {
            let mut s = make_state();
            let spell = insert_obj(&mut s, PlayerId::Opp, make_creature("X", "{2}", 2, 2));
            // Move onto stack.
            s.set_card_zone(spell, CardZone::Stack);
            s.stack.push(spell);
            (s, spell)
        };

        let (mut closure_state, spell1) = setup();
        let (mut ir_state, spell2) = setup();

        crate::effects::counter_one(spell1, &mut closure_state, 0, PlayerId::Us);
        execute(
            &Action::Counter { target: Expr::Num(spell2.0 as i64) },
            &mut ir_state,
            &BindEnv::new().with_controller(PlayerId::Us),
        );
        // The above uses Num for target; but IR expects an Obj value. Rerun
        // with the right path: bind the spell into the env instead.
        let (mut ir_state, spell3) = setup();
        let env = BindEnv::new()
            .with_controller(PlayerId::Us)
            .with_var("tgt", crate::ir::expr::Value::Obj(spell3));
        execute(
            &Action::Counter { target: Expr::Ctx(Ctx::Var("tgt")) },
            &mut ir_state,
            &env,
        );

        // Closure path: stack empty, spell in graveyard.
        assert!(closure_state.stack.is_empty());
        assert!(matches!(
            closure_state.objects.get(&spell1).unwrap().zone,
            CardZone::Graveyard
        ));
        // IR path: same.
        assert!(ir_state.stack.is_empty());
        assert!(matches!(
            ir_state.objects.get(&spell3).unwrap().zone,
            CardZone::Graveyard
        ));
    }

    #[test]
    fn may_do_respects_strategy_yes() {
        let mut s = make_state();
        let start = s.life_of(PlayerId::Us);
        s.resolve_choice = std::sync::Arc::new(|_src, req, _s| match req {
            ChoiceRequest::Mode(_) => ChoiceResult::Mode(1),
            _ => ChoiceResult::Mode(0),
        });
        execute(
            &Action::MayDo {
                who: Who::You,
                action: Box::new(Action::PayLife { who: Who::You, amount: Expr::Num(2) }),
            },
            &mut s,
            &BindEnv::new().with_controller(PlayerId::Us),
        );
        assert_eq!(s.life_of(PlayerId::Us), start - 2);
    }

    #[test]
    fn may_do_respects_strategy_no() {
        let mut s = make_state();
        let start = s.life_of(PlayerId::Us);
        // Default resolve_choice returns Mode(0) → "no"
        execute(
            &Action::MayDo {
                who: Who::You,
                action: Box::new(Action::PayLife { who: Who::You, amount: Expr::Num(2) }),
            },
            &mut s,
            &BindEnv::new().with_controller(PlayerId::Us),
        );
        assert_eq!(s.life_of(PlayerId::Us), start);
    }

    #[test]
    fn choose_picks_strategy_index() {
        let mut s = make_state();
        let start = s.life_of(PlayerId::Us);
        s.resolve_choice = std::sync::Arc::new(|_src, req, _s| match req {
            ChoiceRequest::Mode(_) => ChoiceResult::Mode(1),
            _ => ChoiceResult::Mode(0),
        });
        execute(
            &Action::Choose {
                who: Who::You,
                prompt: "test",
                options: vec![
                    crate::ir::action::ChoiceOption {
                        label: "pay 1",
                        cost: None,
                        action: Box::new(Action::PayLife {
                            who: Who::You,
                            amount: Expr::Num(1),
                        }),
                    },
                    crate::ir::action::ChoiceOption {
                        label: "pay 5",
                        cost: None,
                        action: Box::new(Action::PayLife {
                            who: Who::You,
                            amount: Expr::Num(5),
                        }),
                    },
                ],
            },
            &mut s,
            &BindEnv::new().with_controller(PlayerId::Us),
        );
        assert_eq!(s.life_of(PlayerId::Us), start - 5);
    }

    #[test]
    fn tap_and_untap_flip_battlefield_state() {
        let mut s = make_state();
        let id = insert_obj(&mut s, PlayerId::Us, make_creature("A", "{G}", 1, 1));
        put_on_bf(&mut s, id);
        let env = BindEnv::new()
            .with_controller(PlayerId::Us)
            .with_var("t", crate::ir::expr::Value::Obj(id));

        assert!(!s.permanent_bf(id).unwrap().tapped);
        execute(
            &Action::Tap { target: Expr::Ctx(Ctx::Var("t")) },
            &mut s,
            &env,
        );
        assert!(s.permanent_bf(id).unwrap().tapped, "Action::Tap sets tapped=true");
        execute(
            &Action::Untap { target: Expr::Ctx(Ctx::Var("t")) },
            &mut s,
            &env,
        );
        assert!(!s.permanent_bf(id).unwrap().tapped, "Action::Untap clears tapped");
    }

    #[test]
    fn move_changes_zone() {
        let mut s = make_state();
        let id = insert_obj(&mut s, PlayerId::Us, make_creature("A", "{G}", 1, 1));
        put_on_bf(&mut s, id);
        let env = BindEnv::new()
            .with_controller(PlayerId::Us)
            .with_var("t", crate::ir::expr::Value::Obj(id));

        execute(
            &Action::Move {
                what: Expr::Ctx(Ctx::Var("t")),
                to: ZoneKindSel::Hand,
                to_owner: None,
                bind_as: None,
            },
            &mut s,
            &env,
        );
        assert!(matches!(
            s.objects.get(&id).unwrap().zone,
            CardZone::Hand { .. }
        ));
    }

    #[test]
    fn reveal_marks_hand_known() {
        let mut s = make_state();
        let id = insert_obj(&mut s, PlayerId::Us, make_creature("A", "{G}", 1, 1));
        s.set_card_zone(id, CardZone::Hand { known: false });

        let env = BindEnv::new()
            .with_controller(PlayerId::Us)
            .with_var("t", crate::ir::expr::Value::Obj(id));

        execute(
            &Action::Reveal {
                who: Who::You,
                what: Expr::Ctx(Ctx::Var("t")),
            },
            &mut s,
            &env,
        );
        assert_eq!(
            s.objects.get(&id).unwrap().zone,
            CardZone::Hand { known: true }
        );
    }

    #[test]
    fn discard_sends_random_hand_card_to_graveyard() {
        let mut s = make_state();
        s.rng = seeded_rng(42);
        let a = insert_obj(&mut s, PlayerId::Us, make_creature("A", "{G}", 1, 1));
        let b = insert_obj(&mut s, PlayerId::Us, make_creature("B", "{G}", 1, 1));
        s.set_card_zone(a, CardZone::Hand { known: false });
        s.set_card_zone(b, CardZone::Hand { known: false });

        execute(
            &Action::Discard {
                who: Who::You,
                count: Expr::Num(1),
                at_random: true,
                filter: None,
            },
            &mut s,
            &BindEnv::new().with_controller(PlayerId::Us),
        );

        // One of the two should now be in the graveyard.
        let gy: usize = s.graveyard_of(PlayerId::Us).count();
        assert_eq!(gy, 1);
        let hand: usize = s.hand_of(PlayerId::Us).count();
        assert_eq!(hand, 1);
    }

    #[test]
    fn search_picks_matching_card() {
        // Seed rng so the pick is reproducible; three candidates in library.
        let mut s = make_state();
        s.rng = seeded_rng(7);
        let a = insert_obj(&mut s, PlayerId::Us, make_creature("A", "{G}", 1, 1));
        let b = insert_obj(&mut s, PlayerId::Us, make_creature("B", "{G}", 1, 1));
        let land = insert_obj(&mut s, PlayerId::Us, make_land("Island"));

        // Search for a creature — should land in hand.
        let creature_filter = Filter(Expr::Contains(
            Box::new(Expr::TypeLit(crate::CardType::Creature)),
            Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::It)))),
        ));
        execute(
            &Action::Search {
                who: Who::You,
                zone: ZoneKindSel::Library,
                filter: creature_filter,
                count: Expr::Num(1),
                dest: ZoneKindSel::Hand,
                shuffle: true,
                bind_as: None,
            },
            &mut s,
            &BindEnv::new().with_controller(PlayerId::Us),
        );

        // Exactly one of {a, b} ends up in hand; land stays in library.
        let in_hand: Vec<ObjId> = s.hand_of(PlayerId::Us).map(|o| o.id).collect();
        assert_eq!(in_hand.len(), 1);
        assert!(in_hand[0] == a || in_hand[0] == b);
        assert!(!matches!(
            s.objects.get(&land).unwrap().zone,
            CardZone::Hand { .. }
        ));
    }

    // Silence unused imports (Keyword/Supertype appear only in sibling module).
    #[allow(dead_code)]
    fn _touch_unused_imports(_: Keyword, _: Supertype) {}
}
