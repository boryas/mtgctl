use std::sync::Arc;
use super::*;

// ── Trigger check functions (one per trigger-having card) ─────────────────────

/// Build a Bowmasters trigger context. Target is chosen at resolution time via TargetSpec::AnyTarget.
fn bowmasters_trigger_ctx(controller: &str, kind: &'static str, log_msg: &'static str) -> TriggerContext {
    let ctl = controller.to_string();
    TriggerContext {
        source: "Orcish Bowmasters".into(),
        controller: ctl.clone(),
        kind,
        target_spec: TargetSpec::AnyTarget,
        effect: std::sync::Arc::new(move |state, t, targets, _catalog| {
            // Apply 1 damage to the chosen target, then amass.
            match targets.first() {
                Some(Target::Player(id)) => {
                    let player = state.who_str(*id).to_string();
                    state.player_mut(&player).life -= 1;
                    state.log(t, &ctl, format!("Bowmasters: 1 damage to {player}"));
                }
                Some(Target::Object(id)) => {
                    let id = *id;
                    let tgt_ctl = state.permanent_controller(id).map(|s| s.to_string());
                    let name = state.permanent_name(id);
                    if let (Some(tgt_ctl), Some(name)) = (tgt_ctl, name) {
                        if let Some(p) = state.player_mut(&tgt_ctl).permanents
                            .iter_mut().find(|p| p.id == id)
                        {
                            p.damage += 1;
                        }
                        state.log(t, &ctl, format!("Bowmasters: 1 damage to {name}"));
                    }
                }
                _ => {
                    // No target chosen (no legal targets) — do nothing.
                }
            }
            do_amass_orc(&ctl, 1, state, t);
            state.log(t, &ctl, log_msg);
        }),
    }
}

pub(super) fn bowmasters_check(event: &GameEvent, controller: &str, pending: &mut Vec<TriggerContext>) {
    match event {
        // ETB: only fires for the entering Bowmasters itself.
        GameEvent::ZoneChange { card, to: ZoneId::Battlefield, controller: ctlr, .. }
            if card == "Orcish Bowmasters" && ctlr == controller =>
        {
            pending.push(bowmasters_trigger_ctx(controller, "BowmastersEtb", "Bowmasters ETB: amass Orc 1"));
        }
        // Opponent draws any card that isn't their natural draw-step draw.
        GameEvent::Draw { controller: drawer, draw_index: _, is_natural }
            if drawer != controller && !is_natural =>
        {
            pending.push(bowmasters_trigger_ctx(controller, "BowmastersDrawTrigger", "Bowmasters draw trigger: amass Orc 1"));
        }
        _ => {}
    }
}

fn murktide_check(event: &GameEvent, controller: &str, pending: &mut Vec<TriggerContext>) {
    if let GameEvent::ZoneChange {
        from: ZoneId::Graveyard, to: ZoneId::Exile,
        card_type, controller: exiler, ..
    } = event {
        if (card_type == "instant" || card_type == "sorcery") && exiler == controller {
            let ctl = controller.to_string();
            pending.push(TriggerContext {
                source: "Murktide Regent".into(),
                controller: ctl.clone(),
                kind: "MurktideExile",
                target_spec: TargetSpec::None,
                effect: std::sync::Arc::new(move |state, t, _targets, _catalog| {
                    if let Some(p) = state.player_mut(&ctl).permanents
                        .iter_mut().find(|p| p.name == "Murktide Regent")
                    {
                        p.counters += 1;
                        state.log(t, &ctl, "Murktide: inst/sorc exiled → +1/+1 counter");
                    }
                }),
            });
        }
    }
}

fn tamiyo_check(event: &GameEvent, controller: &str, pending: &mut Vec<TriggerContext>) {
    match event {
        // EnteredStep DeclareAttackers fires after attackers are marked, so p.attacking is set.
        GameEvent::EnteredStep { step: StepKind::DeclareAttackers, active_player }
            if active_player == controller =>
        {
            let ctl = controller.to_string();
            pending.push(TriggerContext {
                source: "Tamiyo, Inquisitive Student".into(),
                controller: ctl.clone(),
                kind: "TamiyoClue",
                target_spec: TargetSpec::None,
                effect: std::sync::Arc::new(move |state, t, _targets, _catalog| {
                    if state.player(&ctl).permanents.iter()
                        .any(|p| p.name == "Tamiyo, Inquisitive Student" && p.attacking)
                    {
                        do_create_clue(&ctl, state, t);
                    }
                }),
            });
        }
        // Controller draws their 3rd card this turn.
        GameEvent::Draw { controller: drawer, draw_index: 3, .. }
            if drawer == controller =>
        {
            let ctl = controller.to_string();
            pending.push(TriggerContext {
                source: "Tamiyo, Inquisitive Student".into(),
                controller: ctl.clone(),
                kind: "TamiyoFlip",
                target_spec: TargetSpec::None,
                effect: std::sync::Arc::new(move |state, t, _targets, catalog| {
                    do_flip_tamiyo(&ctl, state, t, catalog);
                }),
            });
        }
        _ => {}
    }
}

/// Signature for a per-card trigger check function.
/// Inspects the event, and if a trigger fires, appends a `TriggerContext` to `pending`.
/// Does NOT modify state — triggers are queued and pushed onto the stack by the caller.
type TriggerCheckFn = fn(&GameEvent, &str, &mut Vec<TriggerContext>);

static CARD_TRIGGERS: &[(&str, TriggerCheckFn)] = &[
    ("Orcish Bowmasters",         bowmasters_check),
    ("Murktide Regent",           murktide_check),
    ("Tamiyo, Inquisitive Student", tamiyo_check),
];

/// Check every in-play permanent against `CARD_TRIGGERS` for the given event,
/// then check `state.active_effects` for registered effect-based triggers.
/// Returns any triggered ability contexts that should be pushed onto the stack.
pub(super) fn fire_triggers(event: &GameEvent, state: &SimState) -> Vec<TriggerContext> {
    let mut pending: Vec<TriggerContext> = Vec::new();

    // Static card-based triggers.
    for &(card_name, check_fn) in CARD_TRIGGERS {
        for owner in ["us", "opp"] {
            if state.player(owner).permanents.iter().any(|p| p.name == card_name) {
                check_fn(event, owner, &mut pending);
            }
        }
    }

    // Effect-based triggers registered in active_effects.
    for effect in &state.active_effects {
        if let Some(on_event) = &effect.on_event {
            if let Some(ctx) = on_event(event, &effect.controller) {
                pending.push(ctx);
            }
        }
    }

    pending
}

/// Push a vec of `TriggerContext`s onto the stack as uncounterable triggered ability items.
/// Target selection (choose_trigger_target) happens here — at push time, before the stack resolves.
pub(super) fn push_triggers(triggers: Vec<TriggerContext>, stack: &mut Vec<StackItem>, state: &SimState, catalog_map: &HashMap<&str, &CardDef>) {
    for ctx in triggers {
        let chosen_targets = choose_trigger_target(&ctx.target_spec, &ctx.controller, state, catalog_map)
            .into_iter().collect();
        stack.push(StackItem {
            id: ObjId::UNSET,
            name: format!("{} trigger", ctx.source),
            owner: state.player_id(&ctx.controller),
            card_id: ObjId::UNSET,
            is_ability: true,       // NAP skips countering triggered abilities
            ability_def: None,
            counters: None,

            annotation: None,
            adventure_exile: false,
            adventure_card_name: None,
            adventure_face: None,
            trigger_context: Some(ctx),
            chosen_targets,
            ninjutsu_attack_target: None, // sentinel to avoid replace_all collision
            effect: None,
        });
    }
}

/// Apply the resolution effect of a triggered ability.
pub(super) fn apply_trigger(ctx: &TriggerContext, targets: &[Target], state: &mut SimState, t: u8, catalog_map: &HashMap<&str, &CardDef>) {
    (ctx.effect)(state, t, targets, catalog_map);
}

/// Build a TriggerContext for the Tamiyo +2 per-attacker trigger.
/// Extracted to keep the on_event closure in `tamiyo_plus_two_effect` readable.
fn tamiyo_plus_two_fire_ctx(tamiyo_ctl: String, attacker_id: ObjId, attacker_ctl: String) -> TriggerContext {
    let ctl = tamiyo_ctl.clone();
    let atk_ctl = attacker_ctl.clone();
    TriggerContext {
        source: "Tamiyo, Seasoned Scholar".into(),
        controller: tamiyo_ctl,
        kind: "TamiyoPlusTwoFire",
        target_spec: TargetSpec::None,
        effect: std::sync::Arc::new(move |state, t, _targets, _catalog| {
            let atk_name = state.permanent_name(attacker_id).unwrap_or_default();
            let still_in_play = state.player(&atk_ctl).permanents.iter().any(|p| p.id == attacker_id);
            if still_in_play {
                if let Some(p) = state.player_mut(&atk_ctl).permanents
                    .iter_mut().find(|p| p.id == attacker_id)
                {
                    p.power_mod -= 1;
                }
                state.active_effects.push(ContinuousEffect {
                    controller: ctl.clone(),
                    expires: EffectExpiry::EndOfTurn,
                    on_event: None,
                    stat_mod: Some(StatModData {
                        target_id: attacker_id,
                        power_delta: -1,
                        toughness_delta: 0,
                    }),
                });
            }
            state.log(t, &ctl, format!("Tamiyo +2: {} gets -1/-0 until end of turn", atk_name));
        }),
    }
}

/// Build a ContinuousEffect for Tamiyo's +2 loyalty ability.
pub(super) fn tamiyo_plus_two_effect(controller: &str) -> ContinuousEffect {
    ContinuousEffect {
        controller: controller.to_string(),
        expires: EffectExpiry::StartOfControllerNextTurn,
        on_event: Some(std::sync::Arc::new(|event, effect_controller| {
            if let GameEvent::CreatureAttacked { attacker_id, attacker_controller, .. } = event {
                if attacker_controller != effect_controller {
                    return Some(tamiyo_plus_two_fire_ctx(
                        effect_controller.to_string(),
                        *attacker_id,
                        attacker_controller.clone(),
                    ));
                }
            }
            None
        })),
        stat_mod: None,
    }
}
