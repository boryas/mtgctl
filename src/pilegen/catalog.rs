use super::*;

// ── Effect factories ──────────────────────────────────────────────────────────

/// Factory for a spell effect: takes controller name, returns the resolved `Effect`.
/// `TargetSpec` is derived from `SpellData.target` via `target_spec_from_str`.
pub(super) type SpellFactory = std::sync::Arc<dyn Fn(&str) -> Effect + Send + Sync>;

/// Factory for an activated ability effect: takes (controller, source_id), returns `Effect`.
pub(super) type AbilityFactory = std::sync::Arc<dyn Fn(&str, ObjId) -> Effect + Send + Sync>;

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
#[derive(Clone, Default)]
pub(crate) struct AlternateCost {
    pub(crate) mana_cost: String,
    pub(crate) exile_blue_from_hand: bool,
    pub(crate) life_cost: i32,
    pub(crate) bounce_island: bool,
    pub(crate) hand_min: i32,
    pub(crate) prob: Option<f64>,
}

/// An activated ability a permanent can use during its controller's turn.
///
/// Preconditions are derived automatically: ability is available iff
/// the cost can be paid and a valid target exists (if one is required).
#[derive(Clone)]
pub(crate) struct AbilityDef {
    // ── Cost ──────────────────────────────────────────────────────────────────
    /// Mana cost to activate (empty = no mana required).
    pub(crate) mana_cost: String,
    /// Whether the source is tapped as part of the cost.
    pub(crate) tap_self: bool,
    /// Whether the source is sacrificed as part of the cost.
    pub(crate) sacrifice_self: bool,
    /// Life paid as part of the cost (e.g. 1 for fetchlands).
    pub(crate) life_cost: i32,

    // ── Target (optional) ─────────────────────────────────────────────────────
    /// If not `TargetSpec::None`, a valid target must exist for the ability to be available,
    /// and the effect is applied to a randomly chosen valid target.
    pub(crate) target_spec: TargetSpec,

    // ── Zone ──────────────────────────────────────────────────────────────────
    /// Zone the card must be in for this ability. Default "" / "play" = in play.
    /// Use "hand" for cycling/channel abilities.
    pub(crate) zone: String,
    /// Discard this card as part of the cost (zone="hand" abilities).
    pub(crate) discard_self: bool,
    /// Sacrifice a land you control as part of the cost (e.g. Edge of Autumn cycling).
    pub(crate) sacrifice_land: bool,

    // ── Effect ────────────────────────────────────────────────────────────────
    pub(crate) effect: String,
    /// Pre-built effect factory for Rust-defined abilities. Supersedes `effect` when `Some`.
    pub(crate) ability_factory: Option<AbilityFactory>,
    /// If true, this is a ninjutsu activation: cost includes returning an unblocked attacker to
    /// hand, and the effect puts the ninja into play tapped and attacking.
    pub(crate) ninjutsu: bool,
    /// If Some, this is a loyalty ability with the given loyalty adjustment
    /// (positive = gain loyalty, negative = spend loyalty, 0 = 0-loyalty ability).
    /// Loyalty abilities are sorcery-speed and can only be activated once per turn per planeswalker.
    pub(crate) loyalty_cost: Option<i32>,
}

impl Default for AbilityDef {
    fn default() -> Self {
        AbilityDef {
            mana_cost: String::new(),
            tap_self: false,
            sacrifice_self: false,
            life_cost: 0,
            target_spec: TargetSpec::None,
            zone: String::new(),
            discard_self: false,
            sacrifice_land: false,
            effect: String::new(),
            ability_factory: None,
            ninjutsu: false,
            loyalty_cost: None,
        }
    }
}

// ── Mana ability types ────────────────────────────────────────────────────────

/// How a permanent produces mana. `produces` is a string of color chars (e.g. "B", "U", "BU").
/// Empty produces → contributes to generic total only (e.g. Cavern of Souls).
#[derive(Clone, Default)]
pub(crate) struct ManaAbility {
    pub(crate) tap_self: bool,
    pub(crate) sacrifice_self: bool,
    pub(crate) produces: String,
}

/// The five basic land subtypes.
#[derive(Clone, Default)]
pub(crate) struct LandTypes {
    pub(crate) plains: bool,
    pub(crate) island: bool,
    pub(crate) swamp: bool,
    pub(crate) mountain: bool,
    pub(crate) forest: bool,
}

// ── Per-variant data structs ──────────────────────────────────────────────────

#[derive(Clone, Default)]
pub(crate) struct LandData {
    pub(crate) land_types: LandTypes,
    pub(crate) mana_abilities: Vec<ManaAbility>,
    pub(crate) abilities: Vec<AbilityDef>,
}

#[derive(Clone)]
pub(crate) struct CreatureData {
    pub(crate) mana_cost: String,
    // `power` and `toughness` are private — always read through the materialized `CardDef`
    // (which folds in counters and CE modifiers). Write only via `adjust_pt`.
    power: i32,
    toughness: i32,
    #[allow(dead_code)]
    pub(crate) exileable: bool,
    pub(crate) legendary: bool,
    pub(crate) delve: bool,
    pub(crate) abilities: Vec<AbilityDef>,
    pub(crate) mana_abilities: Vec<ManaAbility>,
    pub(crate) ninjutsu: Option<NinjutsuAbility>,
    pub(crate) keywords: Vec<String>,
}

#[derive(Clone, Default)]
pub(crate) struct ArtifactData {
    pub(crate) mana_cost: String,
    pub(crate) abilities: Vec<AbilityDef>,
    pub(crate) mana_abilities: Vec<ManaAbility>,
}

/// The ninjutsu ability on a ninja creature.
#[derive(Clone)]
pub(crate) struct NinjutsuAbility {
    pub(crate) mana_cost: String,
}

impl CreatureData {
    /// Read effective power — call only on a value from `MaterializedState.defs`, never
    /// directly from `catalog_map`, so continuous effects are always reflected.
    pub(crate) fn power(&self) -> i32 { self.power }

    /// Read effective toughness — same rule as `power()`.
    pub(crate) fn toughness(&self) -> i32 { self.toughness }

    /// Apply a power/toughness delta. Used exclusively by `fold_game_state_into_def`
    /// (counters + temporary mods) and `ContinuousModFn` closures in CE machinery.
    pub(super) fn adjust_pt(&mut self, delta_power: i32, delta_toughness: i32) {
        self.power     += delta_power;
        self.toughness += delta_toughness;
    }

    /// Construct a `CreatureData` with the mandatory fields.
    /// All optional fields (exileable, legendary, delve, abilities, ninjutsu, keywords)
    /// default to false/empty and can be set on the returned value.
    pub(super) fn new(mana_cost: impl Into<String>, power: i32, toughness: i32) -> Self {
        CreatureData {
            mana_cost: mana_cost.into(),
            power,
            toughness,
            exileable: false,
            legendary: false,
            delve: false,
            abilities: vec![],
            mana_abilities: vec![],
            ninjutsu: None,
            keywords: vec![],
        }
    }
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

/// Spell data shared by Instant and Sorcery variants.
#[derive(Clone)]
pub(crate) struct SpellData {
    pub(crate) mana_cost: String,
    #[allow(dead_code)]
    pub(crate) exileable: bool,
    /// Declarative target specification. `TargetSpec::None` means no target required.
    pub(crate) target_spec: TargetSpec,
    pub(crate) requires: Vec<String>,
    pub(crate) alternate_costs: Vec<AlternateCost>,
    pub(crate) delve: bool,
    /// Card subtypes (e.g. `["adventure"]` for the adventure face of a split card).
    pub(crate) subtypes: Vec<String>,
    /// Pre-built effect factory for Rust-defined spells.
    pub(crate) spell_factory: Option<SpellFactory>,
}

impl Default for SpellData {
    fn default() -> Self {
        SpellData {
            mana_cost: String::new(),
            exileable: false,
            target_spec: TargetSpec::None,
            requires: Vec::new(),
            alternate_costs: Vec::new(),
            delve: false,
            subtypes: Vec::new(),
            spell_factory: None,
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct PlaneswalkerData {
    pub(crate) mana_cost: String,
    pub(crate) loyalty: i32,
    pub(crate) abilities: Vec<AbilityDef>,
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

/// Layout of a multi-face card. Determines how `back` is interpreted at cast/flip time.
/// `Normal` = single-faced (default). `DoubleFaced` = transform DFC (e.g. Tamiyo).
/// `Split` = two castable halves (split cards, adventures).
#[derive(Clone, Default, Debug, PartialEq)]
pub(crate) enum CardLayout {
    #[default]
    Normal,
    DoubleFaced,
    Split,
}

// ── CardDef wrapper ───────────────────────────────────────────────────────────

/// A card the generator knows about. Cards not in the catalog are treated as
/// generic non-land spells: hand-eligible, not permanent candidates, not exileable.
///
/// Wrapper struct preserving direct `.catalog_key` access and stable HashMap keys while
/// holding a typed `kind` that enforces card-category invariants.
/// A replacement effect definition stored directly on a `CardDef`.
/// `check` returns `Some(targets)` if the replacement applies to an event; `build_effect` builds
/// the closure that runs instead of the original event.
#[derive(Clone)]
pub(crate) struct ReplacementDef {
    pub(crate) check: ReplacementCheckFn,
    /// Factory: called at instance-registration time with `(source_id, controller)`.
    /// CardDef-specific data is captured inside the factory at card-load time.
    pub(crate) make_effect: std::sync::Arc<dyn Fn(ObjId, &str) -> Effect + Send + Sync>,
}

#[derive(Clone)]
pub(crate) struct CardDef {
    pub(crate) name: String,
    /// Relative likelihood of appearing as a permanent in play (default 100).
    #[allow(dead_code)]
    pub(crate) play_weight: Option<u32>,
    pub(super) kind: CardKind,
    /// Colors of this card, derived from mana cost and explicit color flags at load time.
    pub(crate) colors: Vec<Color>,
    /// Card types (Land, Creature, Instant, etc.) — mirrors the `kind` discriminant but
    /// allows multi-type and is accessible without pattern-matching on `kind`.
    pub(crate) types: Vec<CardType>,
    /// Supertypes (Legendary, Basic, Snow).
    pub(crate) supertypes: Vec<Supertype>,
    /// Layout of a multi-face card (Normal / DoubleFaced / Split).
    pub(crate) layout: CardLayout,
    /// Back/second face for DFCs and split/adventure cards.
    /// For DoubleFaced cards, this is the transformed face.
    /// For Split cards (including adventures), this is the second castable half.
    pub(crate) back: Option<Box<CardDef>>,
    /// Trigger check functions for this card (set at card definition time).
    pub(super) trigger_defs: Vec<TriggerCheckFn>,
    /// Replacement effect definitions for this card (set at card definition time).
    pub(super) replacement_defs: Vec<ReplacementDef>,
    /// Static ability factories. Called at ETB to register a `ContinuousInstance` for this
    /// object. The CI has `expiry: WhileSourceOnBattlefield` and is removed on LTB.
    pub(super) static_ability_defs: Vec<StaticAbilityDef>,
}

/// Factory that creates a `ContinuousInstance` for a specific game object.
/// Called when the object enters the battlefield; `source_id` and `controller` are bound then.
pub(super) type StaticAbilityDef =
    std::sync::Arc<dyn Fn(ObjId, &str) -> ContinuousInstance + Send + Sync>;

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

    pub(crate) fn target_spec(&self) -> &TargetSpec {
        match &self.kind {
            CardKind::Instant(s) | CardKind::Sorcery(s) => &s.target_spec,
            _ => &TargetSpec::None,
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
            CardKind::Planeswalker(_) => true,  // all PWs are legendary since 2013
            _ => false,
        }
    }

    pub(crate) fn is_blue(&self) -> bool { self.colors.contains(&Color::Blue) }

    #[allow(dead_code)]
    pub(crate) fn is_black(&self) -> bool { self.colors.contains(&Color::Black) }

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

    #[allow(dead_code)]
    pub(crate) fn as_spell(&self) -> Option<&SpellData> {
        match &self.kind {
            CardKind::Instant(s) | CardKind::Sorcery(s) => Some(s),
            _ => None,
        }
    }

    /// Returns the adventure/split second face if this is a `Split`-layout card.
    pub(crate) fn adventure(&self) -> Option<&CardDef> {
        if self.layout == CardLayout::Split { self.back.as_deref() } else { None }
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

    /// Returns true if this card has the given subtype (e.g. `"adventure"`).
    pub(crate) fn has_subtype(&self, st: &str) -> bool {
        match &self.kind {
            CardKind::Instant(s) | CardKind::Sorcery(s) => s.subtypes.iter().any(|t| t == st),
            _ => false,
        }
    }
}

// ── CardDef constructor + card type helpers ───────────────────────────────────

/// Map a `CardKind` to the corresponding `CardType` enum value.
pub(crate) fn card_type_of(kind: &CardKind) -> CardType {
    match kind {
        CardKind::Land(_)         => CardType::Land,
        CardKind::Creature(_)     => CardType::Creature,
        CardKind::Artifact(_)     => CardType::Artifact,
        CardKind::Instant(_)      => CardType::Instant,
        CardKind::Sorcery(_)      => CardType::Sorcery,
        CardKind::Planeswalker(_) => CardType::Planeswalker,
        CardKind::Enchantment     => CardType::Enchantment,
    }
}

/// Return the lowercase card type string used in `GameEvent::ZoneChange.card_type`.
fn card_type_event_str(ct: CardType) -> &'static str {
    match ct {
        CardType::Land        => "land",
        CardType::Creature    => "creature",
        CardType::Artifact    => "artifact",
        CardType::Instant     => "instant",
        CardType::Sorcery     => "sorcery",
        CardType::Planeswalker => "planeswalker",
        CardType::Enchantment => "enchantment",
    }
}

impl CardDef {
    /// Construct a `CardDef` from its parts. Used by `cards.rs` to define cards in Rust.
    /// `colors` must be pre-computed (use `parse_colors`).
    pub(crate) fn new(
        name: impl Into<String>,
        kind: CardKind,
        colors: Vec<Color>,
        play_weight: Option<u32>,
        supertypes: Vec<Supertype>,
        layout: CardLayout,
        back: Option<Box<CardDef>>,
        trigger_defs: Vec<TriggerCheckFn>,
        replacement_defs: Vec<ReplacementDef>,
        static_ability_defs: Vec<StaticAbilityDef>,
    ) -> Self {
        let types = vec![card_type_of(&kind)];
        CardDef {
            name: name.into(),
            play_weight,
            kind,
            colors,
            types,
            supertypes,
            layout,
            back,
            trigger_defs,
            replacement_defs,
            static_ability_defs,
        }
    }
}

/// Build a `ReplacementDef` for permanents that enter the battlefield tapped.
/// The replacement re-fires the `ZoneChange` event and sets `bf.tapped = true`.
pub(super) fn replacement_enters_tapped(card_type: CardType) -> ReplacementDef {
    let ct = card_type_event_str(card_type).to_string();
    ReplacementDef {
        check: etb_self_check,
        make_effect: std::sync::Arc::new(move |_source_id, controller: &str| {
            let ctl = controller.to_string();
            let ct = ct.clone();
            Effect(std::sync::Arc::new(move |state, t, targets, rng| {
                let Some(Target::Object(id)) = targets.first() else { return; };
                let id = *id;
                let from = current_zone_id(id, state);
                let card_name = state.objects.get(&id)
                    .map(|c| c.catalog_key.clone())
                    .unwrap_or_default();
                fire_event(
                    GameEvent::ZoneChange {
                        id, actor: ctl.clone(), card: card_name,
                        card_type: ct.clone(), from,
                        to: ZoneId::Battlefield, controller: ctl.clone(),
                    },
                    state, t, &ctl, rng,
                );
                if let Some(bf) = state.permanent_bf_mut(id) {
                    bf.tapped = true;
                }
            }))
        }),
    }
}

/// Build a `ReplacementDef` that sets a planeswalker's loyalty on ETB.
pub(super) fn replacement_planeswalker_etb(base_loyalty: i32) -> ReplacementDef {
    let ct = card_type_event_str(CardType::Planeswalker).to_string();
    ReplacementDef {
        check: etb_self_check,
        make_effect: std::sync::Arc::new(move |_source_id, controller: &str| {
            let ctl = controller.to_string();
            let ct = ct.clone();
            Effect(std::sync::Arc::new(move |state, t, targets, rng| {
                let Some(Target::Object(id)) = targets.first() else { return; };
                let id = *id;
                let from = current_zone_id(id, state);
                let card_name = state.objects.get(&id)
                    .map(|c| c.catalog_key.clone())
                    .unwrap_or_default();
                fire_event(
                    GameEvent::ZoneChange {
                        id, actor: ctl.clone(), card: card_name,
                        card_type: ct.clone(), from,
                        to: ZoneId::Battlefield, controller: ctl.clone(),
                    },
                    state, t, &ctl, rng,
                );
                if let Some(bf) = state.permanent_bf_mut(id) {
                    bf.loyalty = base_loyalty;
                }
            }))
        }),
    }
}

// ── Card type enum ─────────────────────────────────────────────────────────────

/// Card category used by the engine and predicates.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
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


/// Derive the color identity of a card from its mana cost string and explicit
/// per-color override flags (used for cards whose cost doesn't reflect their color,
/// e.g. Force of Will alternative-cost pitch cards).
pub(crate) fn parse_colors(mana_cost: &str, blue: bool, black: bool) -> Vec<Color> {
    let mut colors = Vec::new();
    if mana_cost.contains('W') { colors.push(Color::White); }
    if mana_cost.contains('U') || blue { colors.push(Color::Blue); }
    if mana_cost.contains('B') || black { colors.push(Color::Black); }
    if mana_cost.contains('R') { colors.push(Color::Red); }
    if mana_cost.contains('G') { colors.push(Color::Green); }
    colors
}

// ── Trigger check functions (one per trigger-having card) ─────────────────────

/// Build a Bowmasters trigger context. Target is "any_target" = creature | planeswalker | player.
fn bowmasters_trigger_ctx(_source_id: ObjId, controller: &str, log_msg: &'static str) -> TriggerContext {
    let ctl = controller.to_string();
    TriggerContext {
        source_name: "Orcish Bowmasters".into(),
        controller: ctl.clone(),
        target_spec: target_spec_from_str(Some("any_target")),
        effect: Effect(std::sync::Arc::new(move |state, t, targets, _rng| {
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

/// ETB trigger for Recruiter of the Guard: search library for a creature with toughness ≤ 2,
/// put it into hand. CR 700.3 (search), CR 701.14 (reveal — not modeled; card goes to hand).
pub(super) fn recruiter_check(event: &GameEvent, _source_id: ObjId, controller: &str, pending: &mut Vec<TriggerContext>) {
    if let GameEvent::ZoneChange { card, to: ZoneId::Battlefield, controller: ctlr, .. } = event {
        if card == "Recruiter of the Guard" && ctlr == controller {
            let ctl = controller.to_string();
            let pred = pred_and(pred_type_eq(CardType::Creature), pred_toughness_le(2));
            pending.push(TriggerContext {
                source_name: "Recruiter of the Guard".into(),
                controller: ctl.clone(),
                target_spec: TargetSpec::None,
                effect: eff_fetch_search(ctl, pred, ZoneId::Hand),
            });
        }
    }
}

pub(super) fn murktide_check(event: &GameEvent, source_id: ObjId, controller: &str, pending: &mut Vec<TriggerContext>) {
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
                effect: Effect(std::sync::Arc::new(move |state, t, _targets, _rng| {
                    if let Some(bf) = state.permanent_bf_mut(source_id) {
                        bf.counters += 1;
                        state.log(t, &ctl, "Murktide: inst/sorc exiled → +1/+1 counter");
                    }
                })),
            });
        }
    }
}

pub(super) fn tamiyo_check(event: &GameEvent, source_id: ObjId, controller: &str, pending: &mut Vec<TriggerContext>) {
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
                effect: Effect(std::sync::Arc::new(move |state, t, _targets, _rng| {
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
                effect: Effect(std::sync::Arc::new(move |state, t, _targets, _rng| {
                    // Guard: only flip if still on front face (active_face == 0).
                    if state.permanent_bf(source_id).map_or(true, |bf| bf.active_face != 0) { return; }
                    do_flip_tamiyo(source_id, &ctl, state, t);
                })),
            });
        }
        _ => {}
    }
}


/// Check every active trigger instance for the given event.
/// Returns any triggered ability contexts that should be pushed onto the stack.
pub(super) fn fire_triggers(event: &GameEvent, state: &SimState) -> Vec<TriggerContext> {
    let mut pending: Vec<TriggerContext> = Vec::new();
    for inst in &state.trigger_instances {
        if !inst.active { continue; }
        (inst.check)(event, inst.source_id, &inst.controller, &mut pending);
    }
    pending
}

/// Push a vec of `TriggerContext`s onto the stack as uncounterable triggered ability items.
/// Target selection (choose_trigger_target) happens here — at push time, before the stack resolves.
pub(super) fn push_triggers(triggers: Vec<TriggerContext>, state: &mut SimState) {
    for ctx in triggers {
        let all_targets = legal_targets(&ctx.target_spec, &ctx.controller, state);
        let chosen_targets = pick_target(&all_targets, state)
            .into_iter().collect::<Vec<_>>();
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

/// Trigger check for Tamiyo +2: fires for each opposing creature that attacks.
/// Produces a trigger whose effect registers a -1/0 ContinuousInstance (L7) for that attacker.
pub(super) fn tamiyo_plus_two_check(
    event: &GameEvent,
    source_id: ObjId,
    controller: &str,
    pending: &mut Vec<TriggerContext>,
) {
    if let GameEvent::CreatureAttacked { attacker_id, attacker_controller, .. } = event {
        if attacker_controller != controller {
            let attacker_id = *attacker_id;
            let tamiyo_id = source_id;
            let ctl = controller.to_string();
            pending.push(TriggerContext {
                source_name: "Tamiyo, Seasoned Scholar".into(),
                controller: ctl.clone(),
                target_spec: TargetSpec::None,
                effect: Effect(std::sync::Arc::new(move |state, t, _targets, _rng| {
                    let atk_name = state.permanent_name(attacker_id).unwrap_or_default();
                    if state.permanent_bf(attacker_id).is_some() {
                        state.continuous_instances.push(ContinuousInstance {
                            source_id: tamiyo_id,
                            controller: ctl.clone(),
                            layer: ContinuousLayer::L7PowerToughness,
                            filter: std::sync::Arc::new(move |id, _| id == attacker_id),
                            modifier: std::sync::Arc::new(|def, _state| {
                                if let CardKind::Creature(c) = &mut def.kind {
                                    c.adjust_pt(-1, 0);
                                }
                            }),
                            expiry: ContinuousExpiry::EndOfTurn,
                        });
                    }
                    state.log(t, &ctl, format!("Tamiyo +2: {} gets -1/-0 until end of turn", atk_name));
                })),
            });
        }
    }
}

/// A named effect builder: given `(who, source_id)`, produces an `Effect` closure.
/// Used by `NAMED_ABILITY_EFFECTS` to register card-specific ability effects by name.
type NamedEffectBuilder = fn(&str, ObjId) -> Effect;

pub(super) fn build_tamiyo_plus_two(who: &str, source_id: ObjId) -> Effect {
    let who = who.to_string();
    Effect(std::sync::Arc::new(move |state, t, _targets, _rng| {
        let source_name = state.permanent_name(source_id).unwrap_or_default();
        // Register a floating trigger watcher that fires for each opposing attacker.
        // Expires at the start of our next turn (StartOfControllerNextTurn).
        state.trigger_instances.push(TriggerInstance {
            source_id,
            controller: who.clone(),
            check: std::sync::Arc::new(tamiyo_plus_two_check),
            expiry: Some(ContinuousExpiry::StartOfControllerNextTurn),
            active: true,
        });
        state.log(t, &who, format!("{} +2: attackers get -1/-0 until your next turn", source_name));
    }))
}

/// Registry of named ability effects. Each entry maps an effect tag to a builder
/// function. New card-specific effects are added here rather than as inline branches.
static NAMED_ABILITY_EFFECTS: &[(&str, NamedEffectBuilder)] = &[
    ("tamiyo_plus_two", build_tamiyo_plus_two),
];

/// Map the destination token from a `"search:filter:dest"` effect string to a `ZoneId`.
/// Called at load time only — no string lookup at simulation time.
fn search_dest_from_str(dest: &str) -> ZoneId {
    match dest {
        "play"    => ZoneId::Battlefield,
        "hand"    => ZoneId::Hand,
        "library" => ZoneId::Library,
        _         => ZoneId::Battlefield,
    }
}

/// Build an `Effect` closure for an activated ability at push time.
///
/// For Rust-defined abilities: uses `ability_factory` from `AbilityDef`.
/// Fallback: string dispatch on `ability.effect` for hand-constructed `AbilityDef`s in tests.
pub(super) fn build_ability_effect(
    ability: &AbilityDef,
    who: &str,
    source_id: ObjId,
) -> Effect {
    // Rust-defined abilities: use the pre-built factory.
    if let Some(factory) = &ability.ability_factory {
        return factory(who, source_id);
    }

    let who = who.to_string();

    if let Some(rest) = ability.effect.strip_prefix("draw:") {
        let n: usize = rest.parse().unwrap_or(1);
        return eff_draw(who, n);
    }

    if ability.effect.starts_with("search:") {
        let mut parts = ability.effect.splitn(3, ':');
        parts.next(); // "search"
        let filter = parts.next().unwrap_or("land");
        let dest = search_dest_from_str(parts.next().unwrap_or("play"));
        return eff_fetch_search(who, search_filter_pred(filter), dest);
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
        return Effect(std::sync::Arc::new(move |state, t, targets, rng| {
            if let Some(Target::Object(id)) = targets.first() {
                change_zone(*id, to, state, t, &who_c, rng);
            }
        }));
    }

    // No-op (ability with no effect string — e.g. loyalty ability that just adjusts loyalty).
    Effect(std::sync::Arc::new(|_state, _t, _targets, _rng| {}))
}

/// Build a `(TargetSpec, Effect)` for a spell at cast time.
///
/// For Rust-defined spells: uses `spell_factory` from `SpellData`.
/// For non-spell cards (permanents): returns `eff_enter_permanent`.
pub(super) fn build_spell_effect(
    def: &CardDef,
    who: &str,
) -> (TargetSpec, Effect) {
    let target_spec = def.target_spec().clone();
    if let CardKind::Instant(s) | CardKind::Sorcery(s) = &def.kind {
        if let Some(factory) = &s.spell_factory {
            return (target_spec, factory(who));
        }
    }
    (TargetSpec::None, eff_enter_permanent(who.to_string(), def.name.clone()))
}


/// Pre-register trigger and replacement instances for a card object at simulation init.
/// Reads directly from `card_def.trigger_defs` and `card_def.replacement_defs` — no table lookup.
/// Instances start with `active: false`; they are activated when the card enters the battlefield.
pub(super) fn preregister_instances(card_def: &CardDef, source_id: ObjId, controller: &str, state: &mut SimState) {
    for check in card_def.trigger_defs.iter().cloned() {
        state.trigger_instances.push(TriggerInstance {
            source_id,
            controller: controller.to_string(),
            check,
            expiry: None,
            active: false,
        });
    }
    for repl in &card_def.replacement_defs {
        let id = state.alloc_id();
        state.replacement_instances.push(ReplacementInstance {
            id,
            source_id,
            controller: controller.to_string(),
            check: repl.check,
            effect: (repl.make_effect)(source_id, controller),
            active: false,
        });
    }
}

/// Activate all trigger and replacement instances for a card entering the battlefield.
/// Also registers any static-ability `ContinuousInstance`s from `def.static_ability_defs`.
/// `def` is `None` only in test helpers that bypass the catalog — static ability CIs won't fire.
pub(super) fn activate_instances(
    source_id: ObjId,
    controller: &str,
    def: Option<&CardDef>,
    state: &mut SimState,
) {
    for inst in &mut state.trigger_instances {
        if inst.source_id == source_id { inst.active = true; }
    }
    for inst in &mut state.replacement_instances {
        if inst.source_id == source_id { inst.active = true; }
    }
    if let Some(card_def) = def {
        for factory in &card_def.static_ability_defs {
            state.continuous_instances.push(factory(source_id, controller));
        }
    }
}

/// Deactivate all trigger and replacement instances for a card leaving the battlefield.
/// Also removes static-ability ContinuousInstances (WhileSourceOnBattlefield) for this object.
pub(super) fn deactivate_instances(source_id: ObjId, state: &mut SimState) {
    for inst in &mut state.trigger_instances {
        if inst.source_id == source_id { inst.active = false; }
    }
    for inst in &mut state.replacement_instances {
        if inst.source_id == source_id { inst.active = false; }
    }
    state.continuous_instances.retain(|ci| {
        !(ci.source_id == source_id && ci.expiry == ContinuousExpiry::WhileSourceOnBattlefield)
    });
}

// ── Leyline of the Void ───────────────────────────────────────────────────────

pub(super) fn leyline_check(event: &GameEvent, _source_id: ObjId, _controller: &str) -> Option<Vec<Target>> {
    if let GameEvent::ZoneChange { id, to: ZoneId::Graveyard, .. } = event {
        Some(vec![Target::Object(*id)])
    } else {
        None
    }
}

// ── Shared ETB-self check ─────────────────────────────────────────────────────

/// Matches any ZoneChange where this permanent is the object entering the battlefield.
fn etb_self_check(event: &GameEvent, source_id: ObjId, _controller: &str) -> Option<Vec<Target>> {
    if let GameEvent::ZoneChange { id, to: ZoneId::Battlefield, .. } = event {
        if *id == source_id {
            return Some(vec![Target::Object(*id)]);
        }
    }
    None
}

/// Read the card's current zone as a ZoneId. Used to supply the `from` field when re-firing
/// an ETB event from inside a replacement (the card has not yet moved when the replacement fires).
fn current_zone_id(id: ObjId, state: &SimState) -> ZoneId {
    state.objects.get(&id).map(|c| card_zone_to_id(&c.zone)).unwrap_or(ZoneId::Hand)
}

// ── Murktide Regent ETB ───────────────────────────────────────────────────────

pub(super) fn murktide_etb_check(event: &GameEvent, source_id: ObjId, controller: &str) -> Option<Vec<Target>> {
    if let GameEvent::ZoneChange { id, to: ZoneId::Battlefield, controller: ctlr, .. } = event {
        if *id == source_id && ctlr == controller {
            return Some(vec![Target::Object(*id)]);
        }
    }
    None
}

