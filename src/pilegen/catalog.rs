use serde::Deserialize;
use super::*;
// ── Config deserialization ────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
pub(crate) struct PilegenConfig {
    #[serde(default)]
    pub(crate) cards: Vec<CardDef>,
}

/// One way to pay for a counterspell. Each option is tried in order; the first
/// affordable one is taken.
///
/// Components (all optional, combined additively):
///   mana_cost           — standard mana (e.g. "3UU" for FoW hard cost)
///   exile_blue_from_hand — exile another blue card from hand as pitch cost
///   life_cost           — life paid alongside (e.g. 1 for FoW alternate)
///   bounce_island       — return any blue-producing land from play to hand
///   hand_min            — minimum hand size required (inclusive of this spell)
///   prob                — probability this option is even attempted (default 1.0)
#[derive(Deserialize, Clone, Default)]
pub(crate) struct AlternateCost {
    #[serde(default)]
    pub(crate) mana_cost: String,
    #[serde(default)]
    pub(crate) exile_blue_from_hand: bool,
    #[serde(default)]
    pub(crate) life_cost: i32,
    #[serde(default)]
    pub(crate) bounce_island: bool,
    #[serde(default)]
    pub(crate) hand_min: i32,
    #[serde(default)]
    pub(crate) prob: Option<f64>,
}

/// An activated ability a permanent can use during its controller's turn.
///
/// Preconditions are derived automatically: ability is available iff
/// the cost can be paid and a valid target exists (if one is required).
///
/// target syntax: "<who>:<type>"
///   who  ∈ { opp, us }
///   type ∈ { nonbasic_land, land, creature, planeswalker, artifact, any }
///
/// effect ∈ { destroy, bounce, exile, fetch_land }
#[derive(Deserialize, Clone, Default)]
pub(crate) struct AbilityDef {
    // ── Cost ──────────────────────────────────────────────────────────────────
    /// Mana cost to activate (empty = no mana required).
    #[serde(default)]
    pub(crate) mana_cost: String,
    /// Whether the source is tapped as part of the cost.
    #[serde(default)]
    pub(crate) tap_self: bool,
    /// Whether the source is sacrificed as part of the cost.
    #[serde(default)]
    pub(crate) sacrifice_self: bool,
    /// Life paid as part of the cost (e.g. 1 for fetchlands).
    #[serde(default)]
    pub(crate) life_cost: i32,

    // ── Target (optional) ─────────────────────────────────────────────────────
    /// If set, a valid target must exist for the ability to be available,
    /// and the effect is applied to a randomly chosen valid target.
    #[serde(default)]
    pub(crate) target: Option<String>,

    // ── Zone ──────────────────────────────────────────────────────────────────
    /// Zone the card must be in for this ability. Default "" / "play" = in play.
    /// Use "hand" for cycling/channel abilities.
    #[serde(default)]
    pub(crate) zone: String,
    /// Discard this card as part of the cost (zone="hand" abilities).
    #[serde(default)]
    pub(crate) discard_self: bool,
    /// Sacrifice a land you control as part of the cost (e.g. Edge of Autumn cycling).
    #[serde(default)]
    pub(crate) sacrifice_land: bool,

    // ── Effect ────────────────────────────────────────────────────────────────
    #[serde(default)]
    pub(crate) effect: String,
    /// If true, this is a ninjutsu activation: cost includes returning an unblocked attacker to
    /// hand, and the effect puts the ninja into play tapped and attacking.
    #[serde(default)]
    pub(crate) ninjutsu: bool,
    /// If Some, this is a loyalty ability with the given loyalty adjustment
    /// (positive = gain loyalty, negative = spend loyalty, 0 = 0-loyalty ability).
    /// Loyalty abilities are sorcery-speed and can only be activated once per turn per planeswalker.
    #[serde(default)]
    pub(crate) loyalty_cost: Option<i32>,
}

// ── Mana ability types ────────────────────────────────────────────────────────

/// How a permanent produces mana. `produces` is a string of color chars (e.g. "B", "U", "BU").
/// Empty produces → contributes to generic total only (e.g. Cavern of Souls).
#[derive(Deserialize, Clone, Default)]
pub(crate) struct ManaAbility {
    #[serde(default)] pub(crate) tap_self: bool,
    #[serde(default)] pub(crate) sacrifice_self: bool,
    #[serde(default)] pub(crate) produces: String,
}

/// The five basic land subtypes.
#[derive(Deserialize, Clone, Default)]
pub(crate) struct LandTypes {
    #[serde(default)] pub(crate) plains: bool,
    #[serde(default)] pub(crate) island: bool,
    #[serde(default)] pub(crate) swamp: bool,
    #[serde(default)] pub(crate) mountain: bool,
    #[serde(default)] pub(crate) forest: bool,
}

// ── Per-variant data structs ──────────────────────────────────────────────────

#[derive(Deserialize, Clone, Default)]
pub(crate) struct LandData {
    #[serde(default)] pub(crate) basic: bool,
    #[serde(default)] pub(crate) land_types: LandTypes,
    #[serde(default)] pub(crate) enters_tapped: bool,
    #[serde(default)] pub(crate) annotation_options: Vec<String>,
    #[serde(default)] pub(crate) mana_abilities: Vec<ManaAbility>,
    #[serde(default)] pub(crate) abilities: Vec<AbilityDef>,
}

#[derive(Deserialize, Clone)]
pub(crate) struct CreatureData {
    #[serde(default)] pub(crate) mana_cost: String,
    pub(crate) power: i32,
    pub(crate) toughness: i32,
    #[serde(default)] pub(crate) black: bool,
    #[serde(default)] pub(crate) blue: bool,
    #[allow(dead_code)]
    #[serde(default)] pub(crate) exileable: bool,
    #[serde(default)] pub(crate) legendary: bool,
    #[serde(default)] pub(crate) delve: bool,
    #[serde(default)] pub(crate) abilities: Vec<AbilityDef>,
    #[serde(default)] pub(crate) mana_abilities: Vec<ManaAbility>,
    #[serde(default)] pub(crate) adventure: Option<AdventureFace>,
    #[serde(default)] pub(crate) ninjutsu: Option<NinjutsuAbility>,
    #[serde(default)] pub(crate) keywords: Vec<String>,
}

#[derive(Deserialize, Clone, Default)]
pub(crate) struct ArtifactData {
    #[serde(default)] pub(crate) mana_cost: String,
    #[serde(default)] pub(crate) abilities: Vec<AbilityDef>,
    #[serde(default)] pub(crate) mana_abilities: Vec<ManaAbility>,
}

/// The ninjutsu ability on a ninja creature.
#[derive(Deserialize, Clone)]
pub(crate) struct NinjutsuAbility {
    pub(crate) mana_cost: String,
}

impl NinjutsuAbility {
    pub(crate) fn as_ability_def(&self) -> AbilityDef {
        AbilityDef {
            zone: "hand".to_string(),
            mana_cost: self.mana_cost.clone(),
            ninjutsu: true,
            ..Default::default()
        }
    }
}

/// The adventure face of an adventure card (the instant/sorcery half).
#[derive(Deserialize, Clone)]
pub(crate) struct AdventureFace {
    pub(crate) name: String,
    #[serde(default)] pub(crate) card_type: String,  // "instant" or "sorcery"
    #[serde(default)] pub(crate) mana_cost: String,
    #[serde(default)] pub(crate) target: Option<String>,
    #[serde(default)] pub(crate) effects: Vec<String>,
}

/// Spell data shared by Instant and Sorcery variants.
#[derive(Deserialize, Clone, Default)]
pub(crate) struct SpellData {
    #[serde(default)] pub(crate) mana_cost: String,
    #[serde(default)] pub(crate) blue: bool,
    #[serde(default)] pub(crate) black: bool,
    #[allow(dead_code)]
    #[serde(default)] pub(crate) exileable: bool,
    #[serde(default)] pub(crate) target: Option<String>,
    #[serde(default)] pub(crate) requires: Vec<String>,
    #[serde(default)] pub(crate) alternate_costs: Vec<AlternateCost>,
    #[serde(default)] pub(crate) delve: bool,
    #[serde(default)] pub(crate) effects: Vec<String>,
}

#[derive(Deserialize, Clone)]
pub(crate) struct PlaneswalkerData {
    #[serde(default)] pub(crate) mana_cost: String,
    #[serde(default)] pub(crate) loyalty: i32,
    #[serde(default)] pub(crate) abilities: Vec<AbilityDef>,
}

#[derive(Clone)]
pub(crate) enum CardKind {
    Land(LandData),
    Creature(CreatureData),
    Artifact(ArtifactData),
    Instant(SpellData),
    Sorcery(SpellData),
    Planeswalker(PlaneswalkerData),
    Enchantment,
}

// ── CardDef wrapper ───────────────────────────────────────────────────────────

/// A card the generator knows about. Cards not in the catalog are treated as
/// generic non-land spells: hand-eligible, not permanent candidates, not exileable.
///
/// Wrapper struct preserving direct `.name` access and stable HashMap keys while
/// holding a typed `kind` that enforces card-category invariants.
#[derive(Clone)]
pub(crate) struct CardDef {
    pub(crate) name: String,
    /// Relative likelihood of appearing as a permanent in play (default 100).
    #[allow(dead_code)]
    pub(crate) play_weight: Option<u32>,
    pub(crate) kind: CardKind,
}

impl CardDef {
    pub(crate) fn is_land(&self) -> bool { matches!(self.kind, CardKind::Land(_)) }
    pub(crate) fn is_creature(&self) -> bool { matches!(self.kind, CardKind::Creature(_)) }
    pub(crate) fn is_instant(&self) -> bool { matches!(self.kind, CardKind::Instant(_)) }
    #[allow(dead_code)]
    pub(crate) fn is_sorcery(&self) -> bool { matches!(self.kind, CardKind::Sorcery(_)) }

    pub(crate) fn mana_cost(&self) -> &str {
        match &self.kind {
            CardKind::Land(_) | CardKind::Enchantment => "",
            CardKind::Creature(c) => &c.mana_cost,
            CardKind::Artifact(a) => &a.mana_cost,
            CardKind::Instant(s) | CardKind::Sorcery(s) => &s.mana_cost,
            CardKind::Planeswalker(p) => &p.mana_cost,
        }
    }

    pub(crate) fn abilities(&self) -> &[AbilityDef] {
        match &self.kind {
            CardKind::Land(l) => &l.abilities,
            CardKind::Creature(c) => &c.abilities,
            CardKind::Artifact(a) => &a.abilities,
            CardKind::Planeswalker(p) => &p.abilities,
            CardKind::Instant(_) | CardKind::Sorcery(_) | CardKind::Enchantment => &[],
        }
    }

    pub(crate) fn alternate_costs(&self) -> &[AlternateCost] {
        match &self.kind {
            CardKind::Instant(s) | CardKind::Sorcery(s) => &s.alternate_costs,
            _ => &[],
        }
    }

    pub(crate) fn target(&self) -> Option<&str> {
        match &self.kind {
            CardKind::Instant(s) | CardKind::Sorcery(s) => s.target.as_deref(),
            _ => None,
        }
    }

    pub(crate) fn effects(&self) -> &[String] {
        match &self.kind {
            CardKind::Instant(s) | CardKind::Sorcery(s) => &s.effects,
            _ => &[],
        }
    }

    pub(crate) fn requires(&self) -> &[String] {
        match &self.kind {
            CardKind::Instant(s) | CardKind::Sorcery(s) => &s.requires,
            _ => &[],
        }
    }

    pub(crate) fn delve(&self) -> bool {
        match &self.kind {
            CardKind::Creature(c) => c.delve,
            CardKind::Instant(s) | CardKind::Sorcery(s) => s.delve,
            _ => false,
        }
    }

    pub(crate) fn legendary(&self) -> bool {
        match &self.kind {
            CardKind::Creature(c) => c.legendary,
            _ => false,
        }
    }

    pub(crate) fn annotation_options(&self) -> &[String] {
        match &self.kind {
            CardKind::Land(l) => &l.annotation_options,
            _ => &[],
        }
    }

    pub(crate) fn is_blue(&self) -> bool {
        self.mana_cost().contains('U')
            || match &self.kind {
                CardKind::Creature(c) => c.blue,
                CardKind::Instant(s) | CardKind::Sorcery(s) => s.blue,
                _ => false,
            }
    }

    pub(crate) fn is_black(&self) -> bool {
        self.mana_cost().contains('B')
            || match &self.kind {
                CardKind::Creature(c) => c.black,
                CardKind::Instant(s) | CardKind::Sorcery(s) => s.black,
                _ => false,
            }
    }

    pub(crate) fn mana_abilities(&self) -> &[ManaAbility] {
        match &self.kind {
            CardKind::Land(l) => &l.mana_abilities,
            CardKind::Creature(c) => &c.mana_abilities,
            CardKind::Artifact(a) => &a.mana_abilities,
            _ => &[],
        }
    }

    pub(crate) fn as_land(&self) -> Option<&LandData> {
        match &self.kind { CardKind::Land(l) => Some(l), _ => None }
    }

    pub(crate) fn as_creature(&self) -> Option<&CreatureData> {
        match &self.kind { CardKind::Creature(c) => Some(c), _ => None }
    }

    pub(crate) fn as_spell(&self) -> Option<&SpellData> {
        match &self.kind {
            CardKind::Instant(s) | CardKind::Sorcery(s) => Some(s),
            _ => None,
        }
    }

    pub(crate) fn adventure(&self) -> Option<&AdventureFace> {
        match &self.kind {
            CardKind::Creature(c) => c.adventure.as_ref(),
            _ => None,
        }
    }

    pub(crate) fn ninjutsu(&self) -> Option<&NinjutsuAbility> {
        match &self.kind {
            CardKind::Creature(c) => c.ninjutsu.as_ref(),
            _ => None,
        }
    }

    pub(crate) fn keywords(&self) -> &[String] {
        match &self.kind {
            CardKind::Creature(c) => &c.keywords,
            _ => &[],
        }
    }

    pub(crate) fn has_keyword(&self, kw: &str) -> bool {
        self.keywords().iter().any(|k| k == kw)
    }
}

// ── TOML deserialization: two-step via RawCardDef ─────────────────────────────

/// Typed card category used only during TOML deserialization.
#[derive(Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CardType {
    Land,
    Creature,
    Planeswalker,
    Artifact,
    #[default]
    Instant,
    Sorcery,
    Enchantment,
}

/// Flat deserialization target. Converted to `CardDef` by `From<RawCardDef>`.
#[derive(Deserialize, Clone, Default)]
pub(crate) struct RawCardDef {
    pub(crate) name: String,
    pub(crate) card_type: CardType,
    #[serde(default)] pub(crate) enters_tapped: bool,
    #[serde(default)] pub(crate) basic: bool,
    #[serde(default)] pub(crate) land_types: LandTypes,
    #[serde(default)] pub(crate) mana_abilities: Vec<ManaAbility>,
    #[serde(default)] pub(crate) annotation_options: Vec<String>,
    #[serde(default)] pub(crate) mana_cost: String,
    #[serde(default)] pub(crate) power: Option<i32>,
    #[serde(default)] pub(crate) toughness: Option<i32>,
    #[serde(default)] pub(crate) loyalty: Option<i32>,
    #[serde(default)] pub(crate) legendary: bool,
    #[serde(default)] pub(crate) blue: bool,
    #[serde(default)] pub(crate) black: bool,
    #[serde(default)] pub(crate) target: Option<String>,
    #[serde(default)] pub(crate) exileable: bool,
    #[serde(default)] pub(crate) play_weight: Option<u32>,
    #[serde(default)] pub(crate) requires: Vec<String>,
    #[serde(default)] pub(crate) abilities: Vec<AbilityDef>,
    #[serde(default)] pub(crate) delve: bool,
    #[serde(default)] pub(crate) alternate_costs: Vec<AlternateCost>,
    #[serde(default)] pub(crate) adventure: Option<AdventureFace>,
    #[serde(default)] pub(crate) ninjutsu: Option<NinjutsuAbility>,
    #[serde(default)] pub(crate) keywords: Vec<String>,
    #[serde(default)] pub(crate) effects: Vec<String>,
}

impl From<RawCardDef> for CardDef {
    fn from(r: RawCardDef) -> Self {
        let kind = match r.card_type {
            CardType::Land => CardKind::Land(LandData {
                basic: r.basic,
                land_types: r.land_types,
                enters_tapped: r.enters_tapped,
                annotation_options: r.annotation_options,
                mana_abilities: r.mana_abilities.clone(),
                abilities: r.abilities,
            }),
            CardType::Creature => CardKind::Creature(CreatureData {
                mana_cost: r.mana_cost,
                power: r.power.unwrap_or(1),
                toughness: r.toughness.unwrap_or(1),
                black: r.black,
                blue: r.blue,
                exileable: r.exileable,
                legendary: r.legendary,
                delve: r.delve,
                abilities: r.abilities,
                mana_abilities: r.mana_abilities.clone(),
                adventure: r.adventure,
                ninjutsu: r.ninjutsu,
                keywords: r.keywords,
            }),
            CardType::Instant => CardKind::Instant(SpellData {
                mana_cost: r.mana_cost,
                blue: r.blue,
                black: r.black,
                exileable: r.exileable,
                target: r.target,
                requires: r.requires,
                alternate_costs: r.alternate_costs,
                delve: r.delve,
                effects: r.effects,
            }),
            CardType::Sorcery => CardKind::Sorcery(SpellData {
                mana_cost: r.mana_cost,
                blue: r.blue,
                black: r.black,
                exileable: r.exileable,
                target: r.target,
                requires: r.requires,
                alternate_costs: r.alternate_costs,
                delve: r.delve,
                effects: r.effects,
            }),
            CardType::Artifact => CardKind::Artifact(ArtifactData {
                mana_cost: r.mana_cost,
                abilities: r.abilities,
                mana_abilities: r.mana_abilities.clone(),
            }),
            CardType::Planeswalker => CardKind::Planeswalker(PlaneswalkerData {
                mana_cost: r.mana_cost,
                loyalty: r.loyalty.unwrap_or(0),
                abilities: r.abilities,
            }),
            CardType::Enchantment => CardKind::Enchantment,
        };
        CardDef { name: r.name, play_weight: r.play_weight, kind }
    }
}

impl<'de> Deserialize<'de> for CardDef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        RawCardDef::deserialize(d).map(CardDef::from)
    }
}

// ── Trigger check functions (one per trigger-having card) ─────────────────────

/// Build a Bowmasters trigger context. Target is "any_target" = creature | planeswalker | player.
fn bowmasters_trigger_ctx(_source_id: ObjId, controller: &str, log_msg: &'static str) -> TriggerContext {
    let ctl = controller.to_string();
    TriggerContext {
        source_name: "Orcish Bowmasters".into(),
        controller: ctl.clone(),
        target_spec: target_spec_from_str(Some("any_target")),
        effect: Effect(std::sync::Arc::new(move |state, t, targets, _catalog, _rng| {
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
                    if let (Some(_tgt_ctl), Some(name)) = (tgt_ctl, name) {
                        if let Some(bf) = state.permanent_bf_mut(id) {
                            bf.damage += 1;
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
        })),
    }
}

pub(super) fn bowmasters_check(event: &GameEvent, source_id: ObjId, controller: &str, pending: &mut Vec<TriggerContext>) {
    match event {
        // ETB: only fires for the entering Bowmasters itself.
        GameEvent::ZoneChange { card, to: ZoneId::Battlefield, controller: ctlr, .. }
            if card == "Orcish Bowmasters" && ctlr == controller =>
        {
            pending.push(bowmasters_trigger_ctx(source_id, controller, "Bowmasters ETB: amass Orc 1"));
        }
        // Opponent draws any card that isn't their natural draw-step draw.
        GameEvent::Draw { controller: drawer, draw_index: _, is_natural }
            if drawer != controller && !is_natural =>
        {
            pending.push(bowmasters_trigger_ctx(source_id, controller, "Bowmasters draw trigger: amass Orc 1"));
        }
        _ => {}
    }
}

fn murktide_check(event: &GameEvent, source_id: ObjId, controller: &str, pending: &mut Vec<TriggerContext>) {
    if let GameEvent::ZoneChange {
        from: ZoneId::Graveyard, to: ZoneId::Exile,
        card_type, controller: exiler, ..
    } = event {
        if (card_type == "instant" || card_type == "sorcery") && exiler == controller {
            let ctl = controller.to_string();
            pending.push(TriggerContext {
                source_name: "Murktide Regent".into(),
                controller: ctl.clone(),
                target_spec: TargetSpec::None,
                effect: Effect(std::sync::Arc::new(move |state, t, _targets, _catalog, _rng| {
                    if let Some(bf) = state.permanent_bf_mut(source_id) {
                        bf.counters += 1;
                        state.log(t, &ctl, "Murktide: inst/sorc exiled → +1/+1 counter");
                    }
                })),
            });
        }
    }
}

fn tamiyo_check(event: &GameEvent, source_id: ObjId, controller: &str, pending: &mut Vec<TriggerContext>) {
    match event {
        // EnteredStep DeclareAttackers fires after attackers are marked, so p.attacking is set.
        GameEvent::EnteredStep { step: StepKind::DeclareAttackers, active_player }
            if active_player == controller =>
        {
            let ctl = controller.to_string();
            pending.push(TriggerContext {
                source_name: "Tamiyo, Inquisitive Student".into(),
                controller: ctl.clone(),
                target_spec: TargetSpec::None,
                effect: Effect(std::sync::Arc::new(move |state, t, _targets, _catalog, _rng| {
                    if state.permanent_bf(source_id).map_or(false, |bf| bf.attacking) {
                        do_create_clue(&ctl, state, t);
                    }
                })),
            });
        }
        // Controller draws their 3rd card this turn.
        GameEvent::Draw { controller: drawer, draw_index: 3, .. }
            if drawer == controller =>
        {
            let ctl = controller.to_string();
            pending.push(TriggerContext {
                source_name: "Tamiyo, Inquisitive Student".into(),
                controller: ctl.clone(),
                target_spec: TargetSpec::None,
                effect: Effect(std::sync::Arc::new(move |state, t, _targets, catalog, _rng| {
                    do_flip_tamiyo(source_id, &ctl, state, t, catalog);
                })),
            });
        }
        _ => {}
    }
}

/// Signature for a per-card trigger check function.
/// Receives the source permanent's `ObjId` so it can be captured in the effect closure.
/// Inspects the event, and if a trigger fires, appends a `TriggerContext` to `pending`.
/// Does NOT modify state — triggers are queued and pushed onto the stack by the caller.
type TriggerCheckFn = fn(&GameEvent, ObjId, &str, &mut Vec<TriggerContext>);

static CARD_TRIGGERS: &[(&str, TriggerCheckFn)] = &[
    ("Orcish Bowmasters",           bowmasters_check),
    ("Murktide Regent",             murktide_check),
    ("Tamiyo, Inquisitive Student", tamiyo_check),
];

/// Check every in-play permanent against `CARD_TRIGGERS` for the given event,
/// then check `state.active_effects` for registered effect-based triggers.
/// Returns any triggered ability contexts that should be pushed onto the stack.
pub(super) fn fire_triggers(event: &GameEvent, state: &SimState) -> Vec<TriggerContext> {
    let mut pending: Vec<TriggerContext> = Vec::new();

    // Static card-based triggers: pass the source permanent's ObjId to each check function.
    for &(card_name, check_fn) in CARD_TRIGGERS {
        for owner in ["us", "opp"] {
            if let Some(p) = state.find_permanent_by_name(card_name, owner) {
                check_fn(event, p.id, owner, &mut pending);
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
pub(super) fn push_triggers(triggers: Vec<TriggerContext>, state: &mut SimState, catalog_map: &HashMap<&str, &CardDef>) {
    for ctx in triggers {
        let chosen_targets = choose_trigger_target(&ctx.target_spec, &ctx.controller, state, catalog_map)
            .into_iter().collect();
        let ab_id = state.alloc_id();
        let ab_owner = state.player_id(&ctx.controller);
        let ab = StackAbility {
            id: ab_id,
            source_name: ctx.source_name.clone(),
            owner: ab_owner,
            effect: ctx.effect.clone(),
            chosen_targets,
        };
        state.abilities.insert(ab_id, ab);
        state.stack.push(ab_id);
    }
}

/// Build a TriggerContext for the Tamiyo +2 per-attacker trigger.
/// Extracted to keep the on_event closure in `tamiyo_plus_two_effect` readable.
fn tamiyo_plus_two_fire_ctx(_tamiyo_id: ObjId, tamiyo_ctl: String, attacker_id: ObjId, _attacker_ctl: String) -> TriggerContext {
    let ctl = tamiyo_ctl.clone();
    TriggerContext {
        source_name: "Tamiyo, Seasoned Scholar".into(),
        controller: tamiyo_ctl,
        target_spec: TargetSpec::None,
        effect: Effect(std::sync::Arc::new(move |state, t, _targets, _catalog, _rng| {
            let atk_name = state.permanent_name(attacker_id).unwrap_or_default();
            let still_in_play = state.permanent_bf(attacker_id).is_some();
            if still_in_play {
                if let Some(bf) = state.permanent_bf_mut(attacker_id) {
                    bf.power_mod -= 1;
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
        })),
    }
}

/// Build a ContinuousEffect for Tamiyo's +2 loyalty ability.
/// `tamiyo_id` is the ObjId of the Tamiyo permanent activating the ability.
pub(super) fn tamiyo_plus_two_effect(controller: &str, tamiyo_id: ObjId) -> ContinuousEffect {
    ContinuousEffect {
        controller: controller.to_string(),
        expires: EffectExpiry::StartOfControllerNextTurn,
        on_event: Some(std::sync::Arc::new(move |event, effect_controller| {
            if let GameEvent::CreatureAttacked { attacker_id, attacker_controller, .. } = event {
                if attacker_controller != effect_controller {
                    return Some(tamiyo_plus_two_fire_ctx(
                        tamiyo_id,
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

/// A named effect builder: given `(who, source_id)`, produces an `Effect` closure.
/// Used by `NAMED_ABILITY_EFFECTS` to register card-specific ability effects by their
/// TOML effect tag, the same way `CARD_TRIGGERS` registers card-specific trigger logic.
type NamedEffectBuilder = fn(&str, ObjId) -> Effect;

fn build_tamiyo_plus_two(who: &str, source_id: ObjId) -> Effect {
    let who = who.to_string();
    Effect(std::sync::Arc::new(move |state, t, _targets, _catalog, _rng| {
        state.active_effects.push(tamiyo_plus_two_effect(&who, source_id));
        let source_name = state.permanent_name(source_id).unwrap_or_default();
        state.log(t, &who, format!("{} +2: attackers get -1/-0 until your next turn", source_name));
    }))
}

/// Registry of named ability effects. Each entry maps a TOML `effect` tag to a builder
/// function. New card-specific effects are added here rather than as inline branches.
static NAMED_ABILITY_EFFECTS: &[(&str, NamedEffectBuilder)] = &[
    ("tamiyo_plus_two", build_tamiyo_plus_two),
];

/// Build an `Effect` closure for an activated ability at push time.
/// Replaces the old string-dispatch path in `apply_ability_effect`.
pub(super) fn build_ability_effect(
    ability: &AbilityDef,
    who: &str,
    source_id: ObjId,
) -> Effect {
    let who = who.to_string();

    if let Some(rest) = ability.effect.strip_prefix("draw:") {
        let n: usize = rest.parse().unwrap_or(1);
        return eff_draw(who, n);
    }

    if ability.effect.starts_with("search:") {
        let mut parts = ability.effect.splitn(3, ':');
        parts.next(); // "search"
        let filter = parts.next().unwrap_or("").to_string();
        let dest = parts.next().unwrap_or("play").to_string();
        return eff_fetch_search(who, source_id, filter, dest);
    }

    for &(tag, builder) in NAMED_ABILITY_EFFECTS {
        if ability.effect == tag {
            return builder(&who, source_id);
        }
    }

    // Targeted effect (destroy/bounce/exile): target was chosen at push time, stored in chosen_targets.
    if !ability.effect.is_empty() {
        let to = match ability.effect.as_str() {
            "exile"  => ZoneId::Exile,
            "bounce" => ZoneId::Hand,
            _        => ZoneId::Graveyard,  // "destroy" and anything else
        };
        let who_c = who.clone();
        return Effect(std::sync::Arc::new(move |state, t, targets, catalog, _rng| {
            if let Some(Target::Object(id)) = targets.first() {
                change_zone(*id, to, state, t, &who_c, catalog);
            }
        }));
    }

    // No-op (ability with no effect string — e.g. loyalty ability that just adjusts loyalty).
    Effect(std::sync::Arc::new(|_state, _t, _targets, _catalog, _rng| {}))
}

/// Build an `Effect` for a single effect string (e.g. `"draw:3"`, `"destroy"`, `"win"`).
fn build_single_effect(effect: &str, who: &str, _def: &CardDef) -> Effect {
    if let Some(rest) = effect.strip_prefix("draw:") {
        let n: usize = rest.parse().unwrap_or(1);
        return eff_draw(who.to_string(), n);
    }
    if let Some(rest) = effect.strip_prefix("put_back:") {
        let n: usize = rest.parse().unwrap_or(0);
        return eff_put_back(who.to_string(), n);
    }
    if let Some(spec) = effect.strip_prefix("mana:") {
        return eff_mana(who.to_string(), spec.to_string());
    }
    if let Some(rest) = effect.strip_prefix("life_loss:") {
        let n: i32 = rest.parse().unwrap_or(0);
        return eff_life_loss(who.to_string(), n);
    }
    if let Some(rest) = effect.strip_prefix("discard:") {
        let mut parts = rest.splitn(2, ':');
        let n: usize = parts.next().unwrap_or("1").parse().unwrap_or(1);
        let filter = parts.next().unwrap_or("").to_string();
        return eff_discard(who.to_string(), Who::Opp, n, filter);
    }
    if effect.starts_with("search:") {
        let mut parts = effect.splitn(3, ':');
        parts.next();
        let filter = parts.next().unwrap_or("").to_string();
        let dest = parts.next().unwrap_or("play").to_string();
        return eff_fetch_search(who.to_string(), ObjId::UNSET, filter, dest);
    }
    match effect {
        "win"       => eff_doomsday(),
        "destroy"   => eff_destroy_target(who.to_string()),
        "bounce"    => eff_bounce_target(who.to_string()),
        "counter"   => eff_counter_target(who.to_string()),
        "reanimate" => eff_reanimate(who.to_string()),
        "cantrip"   => eff_draw(who.to_string(), 1),
        _           => Effect(std::sync::Arc::new(|_,_,_,_,_| {})),
    }
}

/// Build a `(TargetSpec, Effect)` for a spell at cast time.
/// Replaces the old `spell_effect` dispatch function in `mod.rs`.
///
/// - Parses `def.target()` into a `TargetSpec` via `target_spec_from_str`.
/// - Chains `def.effects()` into a single `Effect` via `build_single_effect`.
/// - If `effects` is empty, the card is a permanent: returns `eff_enter_permanent`.
pub(super) fn build_spell_effect(
    def: &CardDef,
    who: &str,
    annotation: Option<String>,
) -> (TargetSpec, Effect) {
    let target_spec = target_spec_from_str(def.target());
    let effects = def.effects();
    if effects.is_empty() {
        // Permanent (or unrecognized spell): enters the battlefield.
        return (TargetSpec::None, eff_enter_permanent(who.to_string(), def.name.clone(), annotation));
    }
    let mut eff = build_single_effect(effects[0].as_str(), who, def);
    for e in &effects[1..] {
        eff = eff.then(build_single_effect(e.as_str(), who, def));
    }
    (target_spec, eff)
}

/// Build a `(TargetSpec, Effect)` for an adventure face at cast time.
pub(super) fn build_adventure_effect(face: &AdventureFace, who: &str) -> (TargetSpec, Effect) {
    let target_spec = target_spec_from_str(face.target.as_deref());
    let effects = &face.effects;
    if effects.is_empty() {
        return (TargetSpec::None, Effect(std::sync::Arc::new(|_,_,_,_,_| {})));
    }
    // Use a dummy CardDef for build_single_effect (effects don't need def info currently).
    let dummy_def = CardDef {
        name: face.name.clone(),
        play_weight: None,
        kind: CardKind::Instant(SpellData::default()),
    };
    let mut eff = build_single_effect(effects[0].as_str(), who, &dummy_def);
    for e in &effects[1..] {
        eff = eff.then(build_single_effect(e.as_str(), who, &dummy_def));
    }
    (target_spec, eff)
}
