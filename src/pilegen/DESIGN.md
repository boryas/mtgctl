## Pilegen Engine Design

### Central state: `SimState`

Everything lives in `SimState`. Key fields:

- `objects: HashMap<ObjId, GameObject>` — every card in every zone, keyed by stable id
- `catalog: HashMap<String, CardDef>` — base card definitions; populated once at init, never mutated
- `stack: Vec<ObjId>` — spell/ability stack (LIFO)
- `abilities: HashMap<ObjId, StackAbility>` — triggered/activated abilities on the stack
- `trigger_instances`, `replacement_instances`, `continuous_instances` — live ability registrations
- `us: PlayerState`, `opp: PlayerState` — player state (life, mana pool, draw count, etc.)

### Identity: `ObjId`

A single `u64` newtype. Every object in the game — permanents, spells on stack, triggered
abilities, players — is identified by an `ObjId`. `ObjId::UNSET` (0) is the null sentinel.
`state.alloc_id()` is the only way to mint a new one.

### Objects and zones

`GameObject` carries `catalog_key` (card name), `owner`, `controller`, `zone: CardZone`,
and optional `bf: BattlefieldState`, `spell: SpellState`, `materialized: Option<CardDef>`.

`CardZone` is the stored zone (has fine variants like `Hand { known }`, `Exile { on_adventure }`).
`ZoneId` is the coarse enum used in events and effects. `card_zone_to_id` converts between them.

All zone transitions go through `change_zone(id, to, state, t, actor, rng)`, which fires
the event pipeline. Never move a card by writing to `obj.zone` directly.

### Two views of a card's characteristics

`state.catalog.get(name)` — the base `CardDef` as defined at load time. Use for
bootstrap lookups (e.g. cards in library/graveyard that have no in-play state).

`state.def_of(id)` — the materialized `CardDef` for an object currently in play,
with all continuous effects applied. Use this for any characteristic read of an
object on the battlefield. Returns `None` if the object has no materialized state.

`recompute(state)` rebuilds all materialized views by folding continuous instances
over the base catalog entry for each object. Call it before any decision that
reads P/T, types, or abilities of permanents.

### The event pipeline: `fire_event`

Every observable game action fires a `GameEvent` through `fire_event`, which runs:

1. **Replacement check** — first matching active `ReplacementInstance` intercepts;
   its effect runs instead and may re-fire a modified event. Loop prevention via
   `repl_applied` (cleared at depth 0).
2. **State mutation** — the event's own effect runs (zone change, draw, etc.).
3. **Trigger check** — `fire_triggers` walks all active `TriggerInstance`s;
   matching ones append `TriggerContext`s to `pending_triggers`.
4. **Log** — `log_event` derives display info from `state.objects` via `id`.

`GameEvent` variants: `ZoneChange { id, actor, from, to, controller }`, `Draw`,
`EnteredStep`, `EnteredPhase`, `CreatureAttacked`.

### The three instance types

**`TriggerInstance`** — a card's triggered ability registration.
- `check: TriggerCheckFn` — `fn(&GameEvent, source_id, controller, &SimState, &mut Vec<TriggerContext>)`
- Appends a `TriggerContext` to `pending` if the event matches.
- `TriggerContext` carries the effect closure and target spec; pushed to the stack
  as a `StackAbility` at the next priority window.

**`ReplacementInstance`** — a replacement effect registration.
- `check: ReplacementCheckFn` — `Arc<dyn Fn(&GameEvent, source_id, controller) -> Option<Vec<ObjId>>>` (Arc allows captures)
- Returns `Some(targets)` if it applies, `None` otherwise.
- `effect: Effect` — runs instead of the original event.

**`ContinuousInstance`** — a static or temporary ability affecting characteristics.
- `layer: ContinuousLayer` — determines application order (L6 = type-changing, L7 = P/T).
- `filter: ContinuousFilterFn` — which objects it applies to.
- `modifier: ContinuousModFn` — mutates a `CardDef` in place.
- `expiry: ContinuousExpiry` — `EndOfTurn`, `WhileSourceOnBattlefield`,
  `StartOfControllerNextTurn`.

### Instance lifecycle

All three instance types are pre-registered for every card at simulation init
(`preregister_instances`), starting with `active: false`.

`activate_instances(source_id, ...)` — called on ETB; marks trigger and replacement
instances active and registers any `StaticAbilityDef` ContinuousInstances.

`deactivate_instances(source_id, ...)` — called on LTB; marks instances inactive
and removes `WhileSourceOnBattlefield` ContinuousInstances.

### Effects and factories

`Effect` wraps `Arc<dyn Fn(&mut SimState, u8, &[ObjId])>`.
Built from composable primitives (`eff_draw`, `eff_destroy_target`, `eff_fetch_search`, etc.)
and chained with `.then()`. Never inspect card names or types inside an effect —
use predicates and `ObjId`s captured at factory time.

`SpellFactory = Arc<dyn Fn(PlayerId, ObjId, u32) -> Effect>` — built at card load time, called at cast time.
The `u32` is `chosen_x` (strategy-chosen X value; 0 for non-X spells). `SpellData.has_x_cost: bool`
marks X spells; the engine pays `Life(chosen_x)` and `Strategy::choose_x_for_spell` (default: 3) picks X.
`AbilityFactory = Arc<dyn Fn(PlayerId, ObjId) -> Effect>` — same pattern.
`StaticAbilityDef = Arc<dyn Fn(ObjId, PlayerId) -> ContinuousInstance>` — called on ETB.
`ReplacementDef.make_effect = Arc<dyn Fn(ObjId, PlayerId) -> Effect>` — called at pre-registration.

### Card definitions

`CardDef` holds `kind: CardKind` (Land/Creature/Artifact/Instant/Sorcery/Planeswalker/Enchantment),
`colors`, `types`, `supertypes`, `trigger_defs`, `replacement_defs`, `static_ability_defs`.

Cards are defined in `card_defs.rs` using `CardDef::new(...)`. The engine never references
specific card names — it only calls the closures stored on the def.

### Composition primitives

Before adding a new variant or primitive, check whether the concept decomposes into
existing ones. The available layers:

**`CardPredicate`** (`predicates.rs`) — `Arc<dyn Fn(&CardDef) -> bool>`.
Combinators: `pred_and`, `pred_or`, `pred_not`, `pred_any`.
Atomics: `pred_type_eq`, `pred_has_color`, `pred_has_supertype`, `pred_land_subtype`,
`pred_mana_value_le`, `pred_toughness_le`, `pred_no_colored_pips`.
Used for: targeting filters, fetch search, continuous effect applicability.

**`ObjPredicate`** (`predicates.rs`) — `Arc<dyn Fn(ObjId, &SimState) -> bool + Send + Sync>`.
State-aware predicate over any game object. Renamed from `CostPredicate`; now also used for
`ObjectInZone.filter` (targeting) and `ChoiceSpec.filter` (resolution-time choice enumeration).
Combinators: `cost_pred_and`, `cost_pred_or`, `cost_pred_not`.
Atomics: `obj_pred_from_card` (lifts `CardPredicate`), `cost_pred_blue_nonland`, `cost_pred_blue_producing`,
`cost_pred_unblocked_attacker`, `cost_pred_land`, `pred_has_counter(ct: CounterType)`.
Used for: cost component predicates (DiscardCard, ExileFromHand, etc.), ObjectInZone targeting,
and ChoiceSpec choice enumeration.

**`TargetSpec`** (`predicates.rs`) — declarative target description.
- `None` — no target required.
- `Player(Who)` — a player.
- `ObjectInZone { controller, zone, filter: ObjPredicate }` — cards/spells in a zone.
  For `zone=Stack`, only reaches spell objects (not abilities — see Known Seams).
- `AbilityOnStack { controller, ability_type: AbilityType }` — triggered/activated abilities.
  `AbilityType`: `Any | Triggered | Activated`.
- `Union(Vec<TargetSpec>)` — logical OR; use to express "A or B" targets.

**`ChoiceSpec`** (`catalog.rs`) — resolution-time "choose" (CR: not targeting).
Stored on `AbilityDef.choice_spec: Option<ChoiceSpec>` and carried into `StackAbility`.
At resolution, `enumerate_choices(spec, controller, state)` builds the candidate list;
`strategy.choose_for_effect(effect_id, &choices, state)` picks one; the chosen `ObjId`
is prepended to the effect's `targets` slice. `effect_id` is the `StackAbility`'s ObjId.

**`CounterType`** (`mod.rs`) — typed enum of counter kinds (currently: `Void`).
Stored in `GameObject.counters: HashMap<CounterType, u32>` — survives zone changes.

**`FreeCastPermission`** (`mod.rs`) — a deferred free-cast grant (`controller`, `target_id`).
Stored in `SimState.free_cast_permissions: Vec<FreeCastPermission>`; cleared at end of turn.
Strategy returns `PriorityAction::PlayFreeCast(id)`; engine calls `play_free_cast(id, ...)`.

**`ManaAbility`** (`catalog.rs`) — how a permanent produces mana.
Fields: `costs: Vec<CostComponent>`, `produces: Vec<Color>` (for affordability prediction),
`produces_count: usize` (default 1; 2 for Ancient Tomb), `make_effect: ManaEffectFactory`.
`ManaEffectFactory = Arc<dyn Fn(PlayerId, Option<Color>) -> Effect + Send + Sync>` — factory
called at activation time. `Option<Color>` is the specific pip being drawn in the color loop
(`None` for generic-slot activation). Fixed-color sources ignore it; any-color sources
(Lotus Petal, Cavern) use it to produce the right pip. `eff_mana` is the standard primitive.
Side effects (e.g. Ancient Tomb: "deal 2 damage") are chained via `.then()`.

`GameEvent::ManaProduced { who: PlayerId, spec: String }` — fired by `eff_mana` through the
event pipeline so replacement effects can intercept (e.g. Damping Sphere). State mutation
(`do_effect`) parses `spec` and adds to `ManaPool`. Mana abilities bypass the stack per
CR 605.3b; `produce_mana` remains a synchronous call path.

**`CostComponent`** (`catalog.rs`) — a single payable cost.
Combinators: `CostAnd(Vec<CostComponent>)`, `CostOr(Vec<CostComponent>)`.
Atomics: `Mana`, `TapSelf`, `SacSelf`, `DiscardSelf`, `Life`, `SacPermanent`,
`DiscardCard`, `ExileFromHand`, `ReturnFromBattlefield`, `TapPermanent`, `LoyaltyAdjust`,
`Replicate(ManaCost)` (optional, repeatable — see `cast_spell` for copy logic),
`XLife` (variable life payment; X is strategy-chosen — see below).

**`ChoiceRequest` / `ChoiceResult`** (`mod.rs`) — typed strategy choices inside effects.
Used when an effect needs a decision that is not a target (`TargetSpec`) and not an object
selection (`ChoiceSpec`): specifically, choices over abstract typed values.
Variants: `ChoiceRequest::Color`, `CreatureType`, `CardName`.
`SimState.resolve_choice: Arc<dyn Fn(ObjId, &ChoiceRequest, &SimState) -> ChoiceResult + ...>`
Effects call it via `let f = Arc::clone(&state.resolve_choice); f(source_id, &req, state)` —
clone the Arc before passing `&*state` to avoid double-borrow.
Default (in `SimState::new`): Blue / "Wizard" / "". Override in tests for specific choices.
Current users: Painter's Servant ETB (Color), Cavern of Souls ETB (CreatureType), Disruptor Flute ETB (CardName).

**`SimState.surveil_choice: Arc<dyn Fn(ObjId, &SimState) -> bool + Send + Sync>`** (`mod.rs`) —
Strategy callback for surveil. Given the `ObjId` of the card being surveiled, returns `true`
to put it in the graveyard, `false` to keep it on top. Effects clone the Arc before calling
with `&*state` (same double-borrow idiom as `resolve_choice`).
Default: `rand::thread_rng().gen_bool(0.5)` (coin flip). Override in tests for deterministic
outcomes. Current user: `eff_surveil` in `effects.rs`.

**`BattlefieldState.etb_choice: Option<ChoiceResult>`** (`mod.rs`) — uniform storage for the
choice a permanent made as it entered. Written by ETB replacement closures that call
`resolve_choice`; cleared automatically on LTB when `bf` is dropped. Convention: any ETB
replacement that calls `resolve_choice` MUST write the result here. Most cards also capture
the value in their CE closure; `etb_choice` is the side-channel for abilities needing to
inspect "what was named" without a captured copy.

**`CardDef.casting_cost_modifier: i32`** (`catalog.rs`, default 0) — generic-mana surcharge
applied by CE (e.g. Disruptor Flute). Added to `ManaCost.generic` during affordability checks
in `spell_is_affordable`. Reset to 0 each `recompute` (materialized starts from catalog clone).

**`CardDef.non_mana_abilities_suppressed: bool`** (`catalog.rs`, default false) — set by CE
to suppress non-mana activated abilities (e.g. Disruptor Flute, Pithing Needle). Checked by
`ability_available` in `strategy.rs`. Does NOT affect `ManaAbility`s — those are a separate
type on a separate code path. Null Rod (suppress ALL activated abilities including mana) would
need a companion `mana_abilities_suppressed` field.

**`etb_self_replacement<F>(extra) -> ReplacementDef`** (`catalog.rs`) — builds an ETB
self-replacement that handles the boilerplate (extract id, `current_zone_id`, `fire_event`) and
calls `extra(source_id, id, controller, state, t)` after the zone-change fires.

**`etb_self_trigger<F>(source_name, target_spec, make_effect) -> TriggerCheckFn`** (`catalog.rs`)
— builds a trigger check fn that fires when this permanent enters under its controller's control
and pushes a `TriggerContext` with the given spec and effect.

**X spells and `XLife`.** Cards with "as an additional cost, pay X life" (e.g. Toxic Deluge)
use `CardDef.additional_costs = vec![CostComponent::XLife]`. The X value flows as:
- `Strategy::choose_x_for_spell(card_id, state) -> u32` — trait method, default 3.
- `PriorityAction::CastSpell { chosen_x: u32 }` — strategy communicates the choice.
- `can_pay_costs(..., chosen_x: u32)` / `pay_costs(..., chosen_x: u32)` — both accept X.
  `XLife` checks `life >= chosen_x` and deducts on payment, recording in `CostsPaidCtx.chosen_x`.
- `SpellFactory = Arc<dyn Fn(PlayerId, ObjId, u32) -> Effect>` — third arg is `chosen_x`;
  non-X spells receive 0 and ignore it (`_x`).
- `CardDef.additional_costs` lives on the `CardDef` wrapper, not on `SpellData`.

### Known seams

**Split stack representation.** Spells live in `state.objects` (zone = Stack); triggered
and activated abilities live in `state.abilities`. These are separate maps with different
predicate languages: `CardPredicate` operates on `CardDef` and can only reach `objects`,
so `ObjectInZone { zone: Stack }` cannot target abilities. This is why `AbilityOnStack`
exists as a distinct `TargetSpec` variant rather than falling out of `ObjectInZone`.

Consequence: any card that counters "any spell or ability" (e.g. Disallow) requires
`Union([ObjectInZone { Stack, pred_any() }, AbilityOnStack { Any }])` — it cannot be
expressed as a single `ObjectInZone`.

Future fix: unify the stack so both spells and abilities are `GameObject`s in `objects`,
distinguished by a field (e.g. `stack_kind: StackKind::Spell | StackKind::Ability { is_triggered }`).
`ObjectInZone { zone: Stack }` would then reach everything, and `AbilityOnStack` becomes
expressible as `ObjectInZone` with an `is_ability` predicate.
