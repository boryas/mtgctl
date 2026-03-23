use super::*;

// ── Mana cost ─────────────────────────────────────────────────────────────────

#[derive(Clone, Default, Debug)]
pub(crate) struct ManaCost {
    pub(crate) w: i32,
    pub(crate) u: i32,
    pub(crate) b: i32,
    pub(crate) r: i32,
    pub(crate) g: i32,
    pub(crate) c: i32,       // colorless pips {C}
    pub(crate) generic: i32, // any-color pips {1}, {2}, ...
}

impl ManaCost {
    pub(crate) fn total_specific(&self) -> i32 { self.w + self.u + self.b + self.r + self.g + self.c }
    pub(crate) fn mana_value(&self) -> i32 { self.total_specific() + self.generic }

    /// Reconstruct a compact display string (e.g. `ManaCost{generic:1,u:1}` → `"1U"`).
    pub(crate) fn display(&self) -> String {
        let mut s = String::new();
        if self.generic > 0 { s.push_str(&self.generic.to_string()); }
        for _ in 0..self.w { s.push('W'); }
        for _ in 0..self.u { s.push('U'); }
        for _ in 0..self.b { s.push('B'); }
        for _ in 0..self.r { s.push('R'); }
        for _ in 0..self.g { s.push('G'); }
        for _ in 0..self.c { s.push('C'); }
        s
    }
}

/// Parse a mana cost string into a ManaCost.
/// Leading digits → generic; W/U/B/R/G/C → specific color pips.
/// Empty string = no castable mana cost (alt-cost-only or uncostable cards like Daze/FoW).
/// "0" = genuinely free (Lotus Petal, LED).
pub(crate) fn parse_mana_cost(cost: &str) -> ManaCost {
    let mut mc = ManaCost::default();
    let mut chars = cost.trim().chars().peekable();
    let mut num = String::new();
    while chars.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) {
        num.push(chars.next().unwrap());
    }
    if !num.is_empty() {
        mc.generic = num.parse().unwrap_or(0);
    }
    for c in chars {
        match c {
            'W' => mc.w += 1,
            'U' => mc.u += 1,
            'B' => mc.b += 1,
            'R' => mc.r += 1,
            'G' => mc.g += 1,
            'C' => mc.c += 1,
            _ => mc.generic += 1,
        }
    }
    mc
}

/// Convert a mana-production string (e.g. "U", "WUBRG") to a Vec<Color>.
/// Each character maps to the corresponding Color; unknown chars are ignored.
pub(crate) fn produces_colors(s: &str) -> Vec<Color> {
    s.chars().filter_map(|c| match c {
        'W' => Some(Color::White),
        'U' => Some(Color::Blue),
        'B' => Some(Color::Black),
        'R' => Some(Color::Red),
        'G' => Some(Color::Green),
        _ => None,
    }).collect()
}

/// Total mana value (CMC) of a cost string.
pub(crate) fn mana_value(cost: &str) -> i32 {
    parse_mana_cost(cost).mana_value()
}

// ── Effect factories ──────────────────────────────────────────────────────────

// ── Cost types ────────────────────────────────────────────────────────────────

/// Where the source object must be located for an ability to be activated.
#[derive(Clone, Default)]
pub(crate) enum SourceZone {
    #[default]
    Battlefield,
    Hand,
}

/// A single component of an activation or spell cost.
///
/// `CostAnd` / `CostOr` are combinators that mirror CR 118 semantics:
/// - `CostAnd`: all sub-costs must be paid (CR 118.9 — multiple additional costs).
/// - `CostOr`: player chooses exactly one payable branch (CR 118.9 modal costs,
///   e.g. Bitter Triumph's "pay 3 life OR discard a card").
///   Strategy always picks the first affordable branch.
#[derive(Clone)]
#[allow(dead_code)]
pub(crate) enum CostComponent {
    Mana(ManaCost),
    TapSelf,
    SacSelf,                              // sacrifice the source from battlefield
    DiscardSelf,                          // discard the source from hand
    Life(i32),
    SacPermanent(ObjPredicate),          // sacrifice another permanent from battlefield
    DiscardCard(ObjPredicate),           // discard another card from hand
    ExileFromHand(ObjPredicate),         // exile another card from hand (pitch costs)
    ReturnFromBattlefield(ObjPredicate), // return another permanent from battlefield to hand
    TapPermanent(ObjPredicate),          // tap another permanent (Kappa Cannoneer et al.)
    LoyaltyAdjust(i32),                   // +/- loyalty; also marks pw_activated_this_turn
    /// All sub-costs must be paid (explicit AND).
    CostAnd(Vec<CostComponent>),
    /// Exactly one payable sub-cost branch is paid (OR — player's choice).
    CostOr(Vec<CostComponent>),
    /// Optional additional cost paid 0+ times at cast time; each payment creates a
    /// copy of the spell on the stack (CR 702.58). Tracked via `CostsPaidCtx::replicate_count`.
    /// `can_pay` is always true (0 payments is valid). Payment logic lives in `cast_spell`.
    Replicate(ManaCost),
    /// Variable life payment: "as an additional cost, pay X life" where X is strategy-chosen.
    /// `can_pay` checks `life >= chosen_x`; payment records `chosen_x` in `CostsPaidCtx`.
    XLife,
}

/// Factory for a spell effect: takes (controller, source_id, chosen_x) and returns the resolved `Effect`.
/// `chosen_x` is the strategy-chosen X value; 0 for spells without an X cost.
pub(super) type SpellFactory = std::sync::Arc<dyn Fn(PlayerId, ObjId, u32) -> Effect + Send + Sync>;

/// Factory for an activated ability effect: takes (controller, source_id), returns `Effect`.
pub(super) type AbilityFactory = std::sync::Arc<dyn Fn(PlayerId, ObjId) -> Effect + Send + Sync>;

/// One way to pay for a spell instead of (or in addition to) its mana cost.
/// Options are tried in order; the first affordable one is taken.
///
/// `hand_min` and `prob` are strategy metadata, not rules costs.
/// Per CR 118.9d, additional costs apply on top when an AlternateCost is chosen;
/// that is enforced at the cast-spell call site.
///
/// `condition` — optional rules restriction on when this cost may be chosen (CR 118.9b),
/// e.g. "If it's not your turn" on Force of Negation. `None` = always available.
#[derive(Clone, Default)]
pub(crate) struct AlternateCost {
    pub(crate) costs: Vec<CostComponent>,
    pub(crate) hand_min: i32,
    pub(crate) prob: Option<f64>,
    pub(crate) condition: Option<std::sync::Arc<dyn Fn(PlayerId, &SimState) -> bool + Send + Sync>>,
}

/// An activated ability a permanent or hand card can use.
///
/// Describes a choice made at ability-resolution time (CR: "choose" ≠ "target").
/// The engine enumerates valid objects and delegates the pick to `Strategy::choose_for_effect`.
#[derive(Clone)]
pub(crate) struct ChoiceSpec {
    /// Whose objects to enumerate (relative to the ability's controller).
    pub(crate) controller: Who,
    /// Zone the chosen object must be in.
    pub(crate) zone: ZoneId,
    /// Predicate the chosen object must satisfy.
    pub(crate) filter: ObjPredicate,
}

/// Enumerate valid choices for a `ChoiceSpec` from the perspective of `controller`.
pub(super) fn enumerate_choices(spec: &ChoiceSpec, controller: PlayerId, state: &SimState) -> Vec<ObjId> {
    let target_who = spec.controller.resolve(controller);
    state.objects.values()
        .filter(|o| {
            let zone_match = match spec.zone {
                ZoneId::Exile => matches!(o.zone, CardZone::Exile { .. }),
                ZoneId::Hand => matches!(o.zone, CardZone::Hand { .. }),
                ZoneId::Battlefield => o.zone == CardZone::Battlefield,
                ZoneId::Graveyard  => o.zone == CardZone::Graveyard,
                ZoneId::Stack      => o.zone == CardZone::Stack,
                ZoneId::Library    => o.zone == CardZone::Library,
            };
            zone_match && (o.owner == target_who || o.controller == target_who)
        })
        .filter(|o| (spec.filter)(o.id, state))
        .map(|o| o.id)
        .collect()
}

/// Preconditions are derived automatically: ability is available iff
/// every cost component can be paid and a valid target exists (if required).
#[derive(Clone)]
pub(crate) struct AbilityDef {
    /// Where the source must be located for this ability to be activatable.
    /// Default: Battlefield. Set to Hand for cycling, channel, ninjutsu, etc.
    pub(crate) source_zone: SourceZone,
    /// All costs to activate this ability, paid simultaneously.
    pub(crate) costs: Vec<CostComponent>,

    // ── Target (optional) ─────────────────────────────────────────────────────
    /// If not `TargetSpec::None`, a valid target must exist for the ability to be available.
    pub(crate) target_spec: TargetSpec,

    // ── Choice (optional, CR "choose" ≠ "target") ────────────────────────────
    /// If `Some`, the engine enumerates matching objects at resolution time and
    /// delegates the pick to `Strategy::choose_for_effect`. The chosen ObjId is
    /// passed to the effect closure via its `targets` slice.
    pub(crate) choice_spec: Option<ChoiceSpec>,

    // ── Effect ────────────────────────────────────────────────────────────────
    pub(crate) ability_factory: Option<AbilityFactory>,
}

impl Default for AbilityDef {
    fn default() -> Self {
        AbilityDef {
            source_zone: SourceZone::Battlefield,
            costs: Vec::new(),
            target_spec: TargetSpec::None,
            choice_spec: None,
            ability_factory: None,
        }
    }
}

impl AbilityDef {
    /// True if this is a loyalty ability (costs contain a `LoyaltyAdjust`).
    pub(crate) fn is_loyalty_ability(&self) -> bool {
        self.costs.iter().any(|c| matches!(c, CostComponent::LoyaltyAdjust(_)))
    }

    /// Returns the loyalty delta if this is a loyalty ability.
    pub(crate) fn loyalty_delta(&self) -> Option<i32> {
        self.costs.iter().find_map(|c| {
            if let CostComponent::LoyaltyAdjust(n) = c { Some(*n) } else { None }
        })
    }

    /// True if this looks like a fetch land activation (SacSelf + Life cost > 0).
    pub(crate) fn is_fetch_ability(&self) -> bool {
        self.costs.iter().any(|c| matches!(c, CostComponent::SacSelf))
            && self.costs.iter().any(|c| matches!(c, CostComponent::Life(n) if *n > 0))
    }
}

// ── Mana ability types ────────────────────────────────────────────────────────

/// Factory that builds the mana-production effect for one activation.
/// `who` — resolved controller; `color` — the specific color needed (`None` = generic slot).
/// Fixed-color sources (e.g. Islands) ignore `color`; any-color sources (e.g. Lotus Petal)
/// use `color` to produce exactly the requested pip.
pub(crate) type ManaEffectFactory =
    std::sync::Arc<dyn Fn(PlayerId, Option<Color>) -> Effect + Send + Sync>;

/// How a permanent produces mana.
/// `costs`         — what must be paid to activate (typically TapSelf or SacSelf).
/// `produces`      — colors produced; empty vec → colorless only. Used for affordability prediction.
/// `produces_count`— number of mana produced per activation (default 1). Used for prediction.
/// `make_effect`   — factory that builds the actual production effect (add mana + any side effects).
#[derive(Clone)]
pub(crate) struct ManaAbility {
    pub(crate) costs: Vec<CostComponent>,
    pub(crate) produces: Vec<Color>,
    pub(crate) produces_count: usize,
    pub(crate) make_effect: ManaEffectFactory,
}

impl Default for ManaAbility {
    fn default() -> Self {
        Self {
            costs: vec![],
            produces: vec![],
            produces_count: 1,
            make_effect: std::sync::Arc::new(|_, _| Effect(std::sync::Arc::new(|_, _, _| {}))),
        }
    }
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
            source_zone: SourceZone::Hand,
            costs: vec![
                CostComponent::Mana(parse_mana_cost(&self.mana_cost)),
                CostComponent::ReturnFromBattlefield(cost_pred_unblocked_attacker()),
            ],
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
    pub(crate) make_effect: std::sync::Arc<dyn Fn(ObjId, PlayerId) -> Effect + Send + Sync>,
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
    /// Costs that must always be paid in addition to the chosen base/alternative cost.
    /// Per CR 118.9d these apply regardless of which cost path is taken.
    pub(crate) additional_costs: Vec<CostComponent>,
    /// False iff this spell can't be countered (CR 608.2b). Checked at resolution by
    /// `eff_counter_target`; the spell is still a legal target for counterspells.
    /// Modifiable by continuous effects.
    pub(crate) counterable: bool,
    /// Generic-mana surcharge applied to this card's casting cost by a CE (e.g. Disruptor Flute).
    /// Added to `ManaCost.generic` during affordability checks. Reset to 0 on each `recompute`
    /// because materialized views start from a fresh catalog clone.
    pub(crate) casting_cost_modifier: i32,
    /// True when a CE (e.g. Disruptor Flute, Pithing Needle) suppresses this card's non-mana
    /// activated abilities. Reset to false on each `recompute`.
    ///
    /// NOTE: `AbilityDef`s (non-mana) and `ManaAbility`s are separate types checked by separate
    /// code paths, so this flag does NOT accidentally suppress mana abilities. Null Rod
    /// (suppress ALL activated abilities including mana) would need a companion
    /// `mana_abilities_suppressed: bool` field. Future work: a more principled
    /// `AbilitySuppression` model when adding Null Rod / Mycosynth Lattice interactions.
    pub(crate) non_mana_abilities_suppressed: bool,
    /// True when this permanent on the battlefield prevents free casts and creatures entering
    /// from graveyards or libraries (e.g. Grafdigger's Cage). Checked by `play_free_cast`.
    pub(crate) blocks_free_cast: bool,
}

/// Factory that creates a `ContinuousInstance` for a specific game object.
/// Called when the object enters the battlefield; `source_id` and `controller` are bound then.
pub(super) type StaticAbilityDef =
    std::sync::Arc<dyn Fn(ObjId, PlayerId) -> ContinuousInstance + Send + Sync>;

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

    /// False iff this spell can't be countered (CR 608.2b).
    pub(crate) fn counterable(&self) -> bool { self.counterable }
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
            additional_costs: vec![],
            counterable: true,
            casting_cost_modifier: 0,
            non_mana_abilities_suppressed: false,
            blocks_free_cast: false,
        }
    }
}

// ── ETB replacement / trigger helpers ────────────────────────────────────────

/// Build a `ReplacementDef` for self-ETB replacement effects.
///
/// Eliminates the repeated boilerplate (extract id, `current_zone_id`, `fire_event`) present in
/// every ETB replacement. `extra` is called **after** the zone-change event fires, so
/// `state.permanent_bf_mut(id)` is live by the time it runs.
///
/// Signature: `extra(source_id, id, controller, state, t)`
///
/// Cards that need pre-fire mutation (e.g. Murktide setting counters before entering) keep their
/// replacement inline with a custom check fn.
pub(super) fn etb_self_replacement<F>(extra: F) -> ReplacementDef
where
    F: Fn(ObjId, ObjId, PlayerId, &mut SimState, u8) + Send + Sync + 'static,
{
    let extra = std::sync::Arc::new(extra);
    ReplacementDef {
        check: std::sync::Arc::new(etb_self_check),
        make_effect: std::sync::Arc::new(move |source_id, controller: PlayerId| {
            let extra = std::sync::Arc::clone(&extra);
            Effect(std::sync::Arc::new(move |state, t, targets| {
                let Some(&id) = targets.first() else { return };
                let from = current_zone_id(id, state);
                fire_event(
                    GameEvent::ZoneChange { id, actor: controller, from, to: ZoneId::Battlefield, controller },
                    state, t, controller,
                );
                extra(source_id, id, controller, state, t);
            }))
        }),
    }
}

/// Build a `TriggerCheckFn` for simple self-ETB triggers.
///
/// Fires when this permanent enters the battlefield under its controller's control.
/// Pushes a `TriggerContext` with the given `source_name`, `target_spec`, and effect.
///
/// Cards with combined ETB+other triggers (e.g. Orcish Bowmasters) or effects that read state
/// at trigger-push time keep their trigger inline.
pub(super) fn etb_self_trigger<F>(
    source_name: &'static str,
    target_spec: TargetSpec,
    make_effect: F,
) -> TriggerCheckFn
where
    F: Fn(ObjId, PlayerId) -> Effect + Send + Sync + 'static,
{
    std::sync::Arc::new(move |event, source_id, controller, _state, pending| {
        if let GameEvent::ZoneChange { id, to: ZoneId::Battlefield, controller: ctlr, .. } = event {
            if *id == source_id && *ctlr == controller {
                pending.push(TriggerContext {
                    source_name: source_name.into(),
                    controller,
                    target_spec: target_spec.clone(),
                    effect: make_effect(source_id, controller),
                });
            }
        }
    })
}

/// Build a `ReplacementDef` for permanents that enter the battlefield tapped.
pub(super) fn replacement_enters_tapped() -> ReplacementDef {
    etb_self_replacement(|_, id, _, state, _| {
        if let Some(bf) = state.permanent_bf_mut(id) { bf.tapped = true; }
    })
}

/// Build a `ReplacementDef` that sets a planeswalker's loyalty on ETB.
pub(super) fn replacement_planeswalker_etb(base_loyalty: i32) -> ReplacementDef {
    etb_self_replacement(move |_, id, _, state, _| {
        if let Some(bf) = state.permanent_bf_mut(id) { bf.loyalty = base_loyalty; }
    })
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
fn bowmasters_trigger_ctx(_source_id: ObjId, controller: PlayerId, log_msg: &'static str) -> TriggerContext {
    TriggerContext {
        source_name: "Orcish Bowmasters".into(),
        controller,
        target_spec: TargetSpec::Union(vec![
            TargetSpec::ObjectInZone { controller: Who::Opp, zone: ZoneId::Battlefield, filter: obj_pred_from_card(pred_type_eq(CardType::Creature)) },
            TargetSpec::ObjectInZone { controller: Who::Opp, zone: ZoneId::Battlefield, filter: obj_pred_from_card(pred_type_eq(CardType::Planeswalker)) },
            TargetSpec::Player(Who::Opp),
        ]),
        effect: Effect(std::sync::Arc::new(move |state, t, targets| {
            // Apply 1 damage to the chosen target, then amass.
            // ObjIds are globally unique: try player first, then permanent.
            if let Some(&id) = targets.first() {
                if id == state.us.id || id == state.opp.id {
                    let player = state.who_pid(id);
                    state.player_mut(player).life -= 1;
                    state.log(t, controller, format!("Bowmasters: 1 damage to {player}"));
                } else {
                    let name = state.permanent_name(id);
                    if let Some(name) = name {
                        if let Some(bf) = state.permanent_bf_mut(id) {
                            bf.damage += 1;
                        }
                        state.log(t, controller, format!("Bowmasters: 1 damage to {name}"));
                    }
                }
            }
            // No target chosen (no legal targets) — do nothing.
            do_amass("Orc Army", controller, 1, state, t);
            state.log(t, controller, log_msg);
        })),
    }
}

pub(super) fn bowmasters_check(event: &GameEvent, source_id: ObjId, controller: PlayerId, _state: &SimState, pending: &mut Vec<TriggerContext>) {
    match event {
        // ETB: only fires for the entering Bowmasters itself.
        GameEvent::ZoneChange { id, to: ZoneId::Battlefield, controller: ctlr, .. }
            if *id == source_id && *ctlr == controller =>
        {
            pending.push(bowmasters_trigger_ctx(source_id, controller, "Bowmasters ETB: amass Orc 1"));
        }
        // Opponent draws any card that isn't their natural draw-step draw.
        GameEvent::Draw { controller: drawer, draw_index: _, is_natural }
            if *drawer != controller && !is_natural =>
        {
            pending.push(bowmasters_trigger_ctx(source_id, controller, "Bowmasters draw trigger: amass Orc 1"));
        }
        _ => {}
    }
}

/// ETB trigger for Recruiter of the Guard: search library for a creature with toughness ≤ 2,
/// put it into hand. CR 700.3 (search), CR 701.14 (reveal — not modeled; card goes to hand).
pub(super) fn recruiter_check(event: &GameEvent, source_id: ObjId, controller: PlayerId, _state: &SimState, pending: &mut Vec<TriggerContext>) {
    if let GameEvent::ZoneChange { id, to: ZoneId::Battlefield, controller: ctlr, .. } = event {
        if *id == source_id && *ctlr == controller {
            let pred = pred_and(pred_type_eq(CardType::Creature), pred_toughness_le(2));
            pending.push(TriggerContext {
                source_name: "Recruiter of the Guard".into(),
                controller,
                target_spec: TargetSpec::None,
                effect: eff_fetch_search(controller, pred, ZoneId::Hand),
            });
        }
    }
}

pub(super) fn murktide_check(event: &GameEvent, source_id: ObjId, controller: PlayerId, state: &SimState, pending: &mut Vec<TriggerContext>) {
    if let GameEvent::ZoneChange {
        id, from: ZoneId::Graveyard, to: ZoneId::Exile,
        controller: exiler, ..
    } = event {
        let is_instant_or_sorcery = state.objects.get(id)
            .and_then(|o| state.catalog.get(o.catalog_key.as_str()))
            .map_or(false, |d| d.is_instant() || d.is_sorcery());
        if is_instant_or_sorcery && *exiler == controller {
            pending.push(TriggerContext {
                source_name: "Murktide Regent".into(),
                controller,
                target_spec: TargetSpec::None,
                effect: Effect(std::sync::Arc::new(move |state, t, _targets| {
                    if let Some(bf) = state.permanent_bf_mut(source_id) {
                        bf.counters += 1;
                        state.log(t, controller, "Murktide: inst/sorc exiled → +1/+1 counter");
                    }
                })),
            });
        }
    }
}

pub(super) fn tamiyo_check(event: &GameEvent, source_id: ObjId, controller: PlayerId, _state: &SimState, pending: &mut Vec<TriggerContext>) {
    match event {
        // EnteredStep DeclareAttackers fires after attackers are marked, so p.attacking is set.
        GameEvent::EnteredStep { step: StepKind::DeclareAttackers, active_player }
            if *active_player == controller =>
        {
            pending.push(TriggerContext {
                source_name: "Tamiyo, Inquisitive Student".into(),
                controller,
                target_spec: TargetSpec::None,
                effect: Effect(std::sync::Arc::new(move |state, t, _targets| {
                    if state.permanent_bf(source_id).map_or(false, |bf| bf.attacking) {
                        do_create_token("Clue Token", controller, state, t);
                    }
                })),
            });
        }
        // Controller draws their 3rd card this turn.
        GameEvent::Draw { controller: drawer, draw_index: 3, .. }
            if *drawer == controller =>
        {
            pending.push(TriggerContext {
                source_name: "Tamiyo, Inquisitive Student".into(),
                controller,
                target_spec: TargetSpec::None,
                effect: Effect(std::sync::Arc::new(move |state, t, _targets| {
                    // Guard: only flip if still on front face (active_face == 0).
                    if state.permanent_bf(source_id).map_or(true, |bf| bf.active_face != 0) { return; }
                    do_flip_tamiyo(source_id, controller, state, t);
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
        (inst.check)(event, inst.source_id, inst.controller, state, &mut pending);
    }
    pending
}

/// Push a vec of `TriggerContext`s onto the stack as triggered ability items.
/// Target selection (choose_trigger_target) happens here — at push time, before the stack resolves.
pub(super) fn push_triggers(triggers: Vec<TriggerContext>, state: &mut SimState) {
    for ctx in triggers {
        let all_targets = legal_targets(&ctx.target_spec, ctx.controller, state);
        let chosen_targets = pick_target(&all_targets, state)
            .into_iter().collect::<Vec<_>>();
        let ab_id = state.alloc_id();
        let ab_owner = state.player_id(ctx.controller);
        let ab = StackAbility {
            id: ab_id,
            source_name: ctx.source_name.clone(),
            owner: ab_owner,
            effect: ctx.effect.clone(),
            chosen_targets,
            costs_paid_ctx: CostsPaidCtx::default(),
            is_triggered: true,
            counterable: true,
            choice_spec: None,
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
    controller: PlayerId,
    _state: &SimState,
    pending: &mut Vec<TriggerContext>,
) {
    if let GameEvent::CreatureAttacked { attacker_id, attacker_controller, .. } = event {
        if *attacker_controller != controller {
            let attacker_id = *attacker_id;
            let tamiyo_id = source_id;
            pending.push(TriggerContext {
                source_name: "Tamiyo, Seasoned Scholar".into(),
                controller,
                target_spec: TargetSpec::None,
                effect: Effect(std::sync::Arc::new(move |state, t, _targets| {
                    let atk_name = state.permanent_name(attacker_id).unwrap_or_default();
                    if state.permanent_bf(attacker_id).is_some() {
                        state.continuous_instances.push(ContinuousInstance {
                            source_id: tamiyo_id,
                            controller,
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
                    state.log(t, controller, format!("Tamiyo +2: {} gets -1/-0 until end of turn", atk_name));
                })),
            });
        }
    }
}

pub(super) fn build_tamiyo_plus_two(who: PlayerId, source_id: ObjId) -> Effect {
    Effect(std::sync::Arc::new(move |state, t, _targets| {
        let source_name = state.permanent_name(source_id).unwrap_or_default();
        // Register a floating trigger watcher that fires for each opposing attacker.
        // Expires at the start of our next turn (StartOfControllerNextTurn).
        state.trigger_instances.push(TriggerInstance {
            source_id,
            controller: who,
            check: std::sync::Arc::new(tamiyo_plus_two_check),
            expiry: Some(ContinuousExpiry::StartOfControllerNextTurn),
            active: true,
        });
        state.log(t, who, format!("{} +2: attackers get -1/-0 until your next turn", source_name));
    }))
}

/// Build an `Effect` closure for an activated ability at push time.
pub(super) fn build_ability_effect(
    ability: &AbilityDef,
    who: PlayerId,
    source_id: ObjId,
) -> Effect {
    if let Some(factory) = &ability.ability_factory {
        return factory(who, source_id);
    }
    // No factory — no-op (e.g. a loyalty ability that only adjusts loyalty counters).
    Effect(std::sync::Arc::new(|_state, _t, _targets| {}))
}

/// Build a `(TargetSpec, Effect)` for a spell at cast time.
///
/// For Rust-defined spells: uses `spell_factory` from `SpellData`.
/// For non-spell cards (permanents): returns `eff_enter_permanent`.
pub(super) fn build_spell_effect(
    def: &CardDef,
    who: PlayerId,
    source_id: ObjId,
    chosen_x: u32,
) -> (TargetSpec, Effect) {
    let target_spec = def.target_spec().clone();
    if let CardKind::Instant(s) | CardKind::Sorcery(s) = &def.kind {
        if let Some(factory) = &s.spell_factory {
            return (target_spec, factory(who, source_id, chosen_x));
        }
    }
    (TargetSpec::None, eff_enter_permanent(who, def.name.clone()))
}


/// Pre-register trigger and replacement instances for a card object at simulation init.
/// Reads directly from `card_def.trigger_defs` and `card_def.replacement_defs` — no table lookup.
/// Instances start with `active: false`; they are activated when the card enters the battlefield.
pub(super) fn preregister_instances(card_def: &CardDef, source_id: ObjId, controller: PlayerId, state: &mut SimState) {
    for check in card_def.trigger_defs.iter().cloned() {
        state.trigger_instances.push(TriggerInstance {
            source_id,
            controller,
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
            controller,
            check: repl.check.clone(),
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
    controller: PlayerId,
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

pub(super) fn leyline_check(event: &GameEvent, _source_id: ObjId, _controller: PlayerId) -> Option<Vec<ObjId>> {
    if let GameEvent::ZoneChange { id, to: ZoneId::Graveyard, .. } = event {
        Some(vec![*id])
    } else {
        None
    }
}

// ── Shared ETB-self check ─────────────────────────────────────────────────────

/// Matches any ZoneChange where this permanent is the object entering the battlefield.
pub(super) fn etb_self_check(event: &GameEvent, source_id: ObjId, _controller: PlayerId) -> Option<Vec<ObjId>> {
    if let GameEvent::ZoneChange { id, to: ZoneId::Battlefield, .. } = event {
        if *id == source_id {
            return Some(vec![*id]);
        }
    }
    None
}

/// Read the card's current zone as a ZoneId. Used to supply the `from` field when re-firing
/// an ETB event from inside a replacement (the card has not yet moved when the replacement fires).
pub(super) fn current_zone_id(id: ObjId, state: &SimState) -> ZoneId {
    state.objects.get(&id).map(|c| card_zone_to_id(&c.zone)).unwrap_or(ZoneId::Hand)
}

// ── Murktide Regent ETB ───────────────────────────────────────────────────────

pub(super) fn murktide_etb_check(event: &GameEvent, source_id: ObjId, controller: PlayerId) -> Option<Vec<ObjId>> {
    if let GameEvent::ZoneChange { id, to: ZoneId::Battlefield, controller: ctlr, .. } = event {
        if *id == source_id && *ctlr == controller {
            return Some(vec![*id]);
        }
    }
    None
}

// ── Dauthi Voidwalker replacement check ──────────────────────────────────────

/// Replacement check for Dauthi Voidwalker: fires when any card controlled by
/// the opponent (not DV's controller) would be put into the graveyard from anywhere.
pub(super) fn dv_replacement_check(event: &GameEvent, _source_id: ObjId, controller: PlayerId) -> Option<Vec<ObjId>> {
    if let GameEvent::ZoneChange { id, to: ZoneId::Graveyard, controller: card_controller, .. } = event {
        if *card_controller != controller {
            return Some(vec![*id]);
        }
    }
    None
}
