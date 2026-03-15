# Pilegen Improvement Plan

Iterative steps to improve `src/pilegen/` against the principles in `CLAUDE.md`.
Each step must leave `cargo test` green and behavior unchanged (unless the step is
explicitly a behavior fix).

Steps are ordered by dependency and impact. Complete them in order.

---

## Step 1 — Split into `src/pilegen/` module

**Status: DONE** (commit 3e4b077)

**Why first:** With 6k+ lines, every subsequent step is harder than it needs to be.
Pulling strategy into its own file makes the engine/strategy boundary physically
visible and enforced by Rust's module system.

**What to do:**

Create `src/sim/` and split `pilegen.rs` along these seams:

| File | Contents |
|------|----------|
| `src/sim/mod.rs` | `SimState`, `ObjId`, `PlayerState`, game loop (`do_turn`, `do_phase`, `do_step`, `handle_priority_round`) |
| `src/sim/catalog.rs` | `CardDef`, `AbilityDef`, `CARD_TRIGGERS` registry, TOML loading |
| `src/sim/effects.rs` | `Effect` type, `eff_*` primitives, `.then()` composition |
| `src/sim/targets.rs` | `TargetSpec`, `has_valid_target()`, `matches_*()` |
| `src/sim/combat.rs` | `declare_attackers`, `declare_blockers`, `apply_combat_damage` |
| `src/sim/strategy.rs` | `decide_action()`, `ap_proactive()`, `nap_action()`, `ap_react()`, `try_ninjutsu()`, `collect_on_board_actions()` |
| `src/sim/display.rs` | `display_player_board()`, `display_exile()`, `display_stack()` |

**No logic changes.** This is a pure structural refactor.

**Verification:** `cargo test` passes with identical output.

---

## Step 2 — Fire `EnteredStep` for all steps

**Status: DONE** (commit 209b7cc)

**Why:** Currently only `DeclareAttackers` fires `EnteredStep`. Every step with a
priority round must fire it before the priority loop. This is load-bearing for any
triggered ability that needs to react to upkeep, draw step, end step, etc.

**What to do:**

Fire `EnteredStep` at the start of each priority-bearing step:
- Upkeep
- Draw
- Main (pre-combat)
- BeginCombat
- DeclareAttackers *(already done)*
- DeclareBlockers
- CombatDamage
- EndCombat
- Main (post-combat)
- End step

Untap and Cleanup correctly have no priority and should not fire it.

**Verification:** Add a test that registers a trigger on `EnteredStep { step: Upkeep }`
and confirms it fires.

---

## Step 3 — `TriggerContext.source` → `ObjId`

**Status: DONE**

**Why:** The last string-identity field in the trigger pipeline. Required before DFC
work (same card, two names, one `ObjId`). Acknowledged by `TODO(ids)` in the code.

**What to do:**

- Change `TriggerContext.source: String` → `source_id: ObjId`.
- Update all sites that create `TriggerContext` to pass the source `ObjId`.
- Update all sites that read `source` to look up the permanent by `source_id`.

**Verification:** `cargo test` passes. No `TODO(ids)` comments remain.

---

## Step 4 — `counter_target` → unified targeting (`TargetSpec::StackEntry`)

**Status: DONE** (commit 772256c)

**Why:** `counter_target` was a parallel, non-composable field that bypassed the
targeting system entirely. Counterspells are spells that target something on the
stack — the same concept as any targeted spell, just with a stack-zone predicate.

**What was done:**

- Removed `counter_target` from `SpellData` / `RawCardDef` / TOML entirely.
- Added `TargetSpec::StackEntry { filter: String }` to `predicates.rs`.
- Added `stack_filter_matches(filter: &str, kind: &CardKind) -> bool` (replaces
  `matches_counter_target`).
- `has_valid_target`, `choose_trigger_target`, `choose_spell_target`, `cast_spell`,
  and `push_triggers` all take `stack: &[StackItem]` so stack-targeting works.
- `Force of Will` and `Daze` in TOML now use `target = "stack:any"`.
- `respond_with_counter` in `strategy.rs` reads `def.target()` starting with
  `"stack:"` instead of `counter_target()`.
- `dd_countered` in `strategy.rs` uses `top.chosen_targets` instead of `top.counters`.

**Verification:** 113 tests pass. `counter_target` and `matches_counter_target` deleted.

---

## Step 4b — Move `stack` to `SimState`; implement `eff_counter_target()`

**Status: DONE**

**Why:** `stack` is a local variable in `handle_priority_round`, which prevents
`Effect` closures from accessing it. This forces the `is_counter` hack in
resolution. Once `stack` is on `SimState`, counterspells can use a proper
`eff_counter_target()` closure — resolution becomes truly opaque.

**Verification:** `cargo test` passes. No `is_counter` in codebase.

---

## Step 5 — `StackEntry` enum

**Status: DONE**

**Why:** `StackItem` has 17 fields, many mutually exclusive. The flat struct makes
it hard to reason about what fields are valid for spells vs abilities vs triggers.

**What was done:**

Replaced `StackItem` with a typed `StackEntry` enum: `Spell`, `Ability`, `Trigger`.
Adventure-specific fields live only on `Spell`. Ninjutsu targets are in
`chosen_targets` on `Ability`. No `ninjutsu_attack_target` field.

**Verification:** `cargo test` passes. No `unwrap_or_default()` workarounds for
dead fields.

---

## Step 6 — DFC / adventure / ninjutsu: state on the object

**Status: DONE**

**Why:** These three mechanics all hard-code their state tracking rather than
reading generic game state. The principle: state belongs on the object it
describes; mechanics read it from there.

**What was done:**

- `PlayerState.on_adventure: Vec<String>` deleted; replaced by `Exile { on_adventure: bool }` on `CardZone`.
- `active_face: u8` on `SimPermanent` for DFC transform in-place.
- `do_flip_tamiyo` and `do_amass_orc` find targets by `ObjId`, not by name.
- Ninjutsu attack target read from combat state generically via `chosen_targets`.

**Verification:** `cargo test` passes.

---

## Step 7 — Ability effects as closures from TOML

**Status: DONE**

**What was done:**

- `apply_ability_effect` and its string dispatch deleted. Replaced by
  `build_ability_effect(ability, who, source_id) -> Effect` in `catalog.rs`.
- `eff_fetch_search(who, source_id, filter, dest)` primitive added to `effects.rs`.
- `apply_trigger` deleted. Trigger resolution now uses `context.effect.call(...)`.
- All `StackEntry` variants (Spell, Ability, Trigger) resolve via a single
  `eff.call(state, t, &chosen_targets, catalog_map, rng_dyn)` path.
- `TriggerContext.kind` field deleted; assertions migrated to `source_name`.
- `EffectFn` type alias and `no_effect()` deleted from `mod.rs`.
- `"tamiyo_plus_two"` inline branch replaced by `NAMED_ABILITY_EFFECTS` static
  registry (same pattern as `CARD_TRIGGERS`).
- `change_zone(id, to, state, t, actor)` helper added; replaces `apply_effect_to`.

**Verification:** `cargo test` passes (113 tests). Single closure-call resolution
path for all stack entries.

---

## Step 8 — Flatten zone storage: `state.cards` as source of truth

**Status: DONE**

**Why:** `PlayerState` held six separate zone containers (`library`, `hand`, `lands`,
`permanents`, `graveyard`, `exile`), requiring every zone transition to keep two
representations in sync. `state.cards: HashMap<ObjId, CardObject>` with `zone:
CardZone` on each object is now the single canonical store.

**What was done:**

- **8a**: `SimLand` deleted. Lands are permanents with a land subtype. All mana
  production, potential-mana, display, and strategy sites filter `permanents_of` by
  `catalog.is_land()`. `PlayerState.lands` deleted.
- **8b**: `SimPermanent` deleted. `PlayerState.permanents` deleted. All battlefield
  objects live in `state.cards` with `zone: Battlefield` and a populated `bf`.
  Mana methods (`potential_mana`, `produce_mana`, `pay_mana`, `has_black_mana`) moved
  from `PlayerState` to `SimState`. Helpers added: `permanents_of`, `permanent_bf`,
  `permanent_bf_mut`.
- **8c**: `Zone` struct deleted. `PlayerState.hand`, `.graveyard`, `.exile`,
  `.library` deleted. All off-board cards live in `state.cards` with the appropriate
  `CardZone`. Helpers added: `hand_of`, `library_of`, `graveyard_of`, `exile_of`,
  `on_adventure_of`, `hand_size`, `set_card_zone`. Library threading (`us_lib`,
  `opp_lib`) removed from all function signatures.
- **8d**: `change_zone(id, to, state, t, actor, catalog_map)` now derives `from` from
  the card's current zone, fires `GameEvent::ZoneChange`, clears `bf` on Battlefield
  exit and `stack` on Stack exit, and logs semantically by `(from, to)` pair. Manual
  `GameEvent::ZoneChange` fire sites (lethal-damage deaths, delve GY→Exile) collapsed
  into `change_zone` calls. `eff_enter_permanent` still constructs new `CardObject`
  entries directly (entering the battlefield from resolution is a creation, not a
  zone-change in the same sense).

**Verification:** 113 tests pass. `SimLand`, `SimPermanent`, `Zone` deleted.
`state.cards` is the only container. `change_zone` has no source-zone special-casing.

---

## Step 9 — Spell effects as data; no engine dispatch on card names

**Why:** `spell_effect()` in `mod.rs` dispatches on card names to build `(TargetSpec,
Effect)` pairs at cast time. This violates the core principle: "no card should have
engine dispatch logic when used." `AbilityDef` already carries an `effect: String`
field that `build_ability_effect` parses into a closure — spells should work the same
way. Additionally, hardcoded `TargetSpec` variants (`OpponentCreatureMvLt4`,
`AnyOpponentNonlandPermanent`, etc.) collapse generic concepts into named constants
that must be extended per-card.

**Principle:** Effects and targets are data on the card definition. They are parsed
from TOML strings once at cast time by `build_spell_effect`. Resolution is fully
opaque: call `eff.call(state, t, &targets, catalog, rng)` — no names, no dispatch.

### 9a — `SpellData` gets `effects: Vec<String>`; TOML updated

Add `effects: Vec<String>` to `SpellData` and `RawCardDef`. Map it through
`From<RawCardDef>` for `Instant` and `Sorcery`. Add `effects()` accessor to `CardDef`
that returns the vec for spells and `&["enter"]` for permanents
(Creature/Artifact/Planeswalker/Enchantment).

Add `effects` to pilegen.toml for every instant/sorcery:

| Card | effects |
|------|---------|
| Brainstorm | `["draw:3", "put_back:2"]` |
| Ponder / Consider / Preordain | `["draw:1"]` |
| Dark Ritual | `["mana:BBB"]` |
| Doomsday | `["win"]` |
| Fatal Push / Snuff Out | `["destroy"]` |
| Thoughtseize | `["discard:1:nonland", "life_loss:2"]` |
| Hymn to Tourach | `["discard:2"]` |
| Unearth | `["reanimate"]` + `target = "self:gy:creature"` |
| Force of Will / Daze | `["counter"]` |

### 9b — Generic `TargetSpec`; delete hardcoded variants

Replace named variants with composable ones:

```rust
enum TargetSpec {
    None,
    AnyTarget,                                        // Bowmasters ping
    Player,
    Permanent { controller: Who, filter: String },   // replaces 3 named variants
    CardInZone { controller: Who, zone: ZoneId, filter: String }, // replaces CardInOwnGraveyard
    StackEntry { filter: String },
}
```

`target_spec_from_str(target: Option<&str>) -> TargetSpec` parses TOML target strings
into these variants:
- `"opp:creature_mv_lt4"` → `Permanent { controller: Opp, filter: "creature_mv_lt4" }`
- `"self:gy:creature"` → `CardInZone { controller: Actor, zone: Graveyard, filter: "creature" }`
- `"stack:any"` → `StackEntry { filter: "any" }`

`choose_trigger_target`, `choose_spell_target`, and `has_valid_target` updated to use
the new variants (no named-variant dispatch).

### 9c — `build_spell_effect` replaces `spell_effect`

`pub(super) fn build_spell_effect(def: &CardDef, who: &str, annotation: Option<String>) -> (TargetSpec, Effect)` in `catalog.rs`:
- Calls `target_spec_from_str(def.target())`
- Iterates `def.effects()`, building each into an `Effect` via `build_single_effect`,
  chaining with `.then()`
- For `"enter"`, builds `eff_enter_permanent(who, def.name.clone(), annotation)`
- For `"counter"`, builds `eff_counter_target(who)` (which pops the targeted stack item)

`build_single_effect(effect: &str, who: &str) -> Effect` handles the vocabulary;
shared with `build_ability_effect` (which becomes a thin wrapper).

`spell_effect()` in `mod.rs` deleted. `cast_spell` calls `build_spell_effect(def, who, annotation)`.

**Verification:** `cargo test` passes. `spell_effect` deleted. No card names in
resolution or cast path. All spells/permanents go through `build_spell_effect`.

---

---

## Step 10 — Replacement effects + unified event pipeline

**Status: DONE** (commit eb96e5a)

**What was done:**

- `fire_event(event, state, t, actor, catalog_map, rng)` — central clearinghouse for all elemental game events. Pipeline: `check_replacement → do_effect → check_triggers`.
- `do_effect` applies state mutation per event type (ZoneChange mutates zone, initialises/tears down `BattlefieldState`; Draw moves Library→Hand; notification events are no-ops).
- `log_event` logs semantically by (from, to) zone pair.
- `RegisteredReplacement` struct with `id: ObjId`, `source_id`, `controller`, `check: ReplacementCheckFn`, `effect: Effect`.
- Loop prevention: `repl_depth: u32` + `repl_applied: HashSet<ObjId>` on `SimState`. Depth 1 = fresh chain (clears applied set); depth > 1 = inside replacement chain (keeps applied set).
- `change_zone` becomes a thin caller of `fire_event`. All zone mutation logic lives in `do_effect`.
- `sim_draw` converted from `&mut self` method to free function, calls `fire_event`.
- `queue_triggers` deleted; all event sites go through `fire_event`.
- Leyline of the Void: ongoing replacement, `ZoneChange{to: Graveyard}` → `ZoneChange{to: Exile}`.
- Murktide ETB: self-ETB replacement, sets exile-count counters then re-fires ETB event (skipped by `repl_applied`).

---

## Step 11 — Pre-registered trigger/replacement instances + behavior on CardDef

**Status: DONE** (commit eb96e5a)

**What was done:**

- `TriggerInstance` and `ReplacementInstance` structs (replacing `RegisteredReplacement`), each with `source_id`, `controller`, `check` fn, pre-built `effect`, and `active: bool`.
- All instances pre-registered at sim init with `active: false`; `activate_instances` / `deactivate_instances` flip the flag on battlefield entry/exit.
- `change_zone` activates/deactivates instances **before** calling `fire_event` so ETB replacements are visible to `check_replacement`.
- `SimState` gains `trigger_instances: Vec<TriggerInstance>` and `replacement_instances: Vec<ReplacementInstance>`.
- `fire_triggers` iterates `state.trigger_instances` (skips inactive); no longer scans permanents by name.
- `CARD_TRIGGERS`, `CARD_REPLACEMENTS`, `TriggerPrototype`, `ReplacementPrototype` all removed.
- `CardDef` gains `trigger_defs: Vec<TriggerCheckFn>` and `replacement_defs: Vec<ReplacementDef>`, populated in `From<RawCardDef>` by card name. No runtime table lookups; behavior lives on the card definition.
- `preregister_instances(card_def, source_id, controller, state)` reads from `card_def` directly.

---

## Step 12 — Compact per-player state display

**Status: DONE** (commit 2509928)

**What was done:**

- One line per zone (was one line per card).
- Permanents split into **Lands** and **Permanents** lines; tapped first within each group, then alphabetical.
- Graveyard shown in entry order (oldest first) — rules-relevant for Doomsday piles. `SimState` gains `graveyard_order: Vec<ObjId>` updated in `do_effect`.
- Hand shows known card names + hidden count on one line.
- Exile shows cards with `(adv)` annotation inline.

---

## Next Steps (ideas, not yet scheduled)

### Unified predicate layer

Targeting legality, search filters, strategy queries ("what lands do I have?"), and
trigger guards are all doing the same thing: "does this card satisfy these
constraints?" They're currently scattered across `predicates.rs`, `catalog.rs`, and
`strategy.rs` with slightly different signatures.

Once Step 8 lands and all cards are `CardObject`, a single predicate type makes sense:

- `CardFilter` — composable, data-driven; works on `(&CardObject, &CardDef)`.
- `card_matches(filter: &CardFilter, card: &CardObject, catalog) -> bool` — one evaluation path.
- Everything goes through it: targeting, search, strategy queries, trigger guards.

The TOML vocabulary (`type`, `controller`, `color`, `zone`, etc.) naturally extends
to filter expressions. A `[filter]` block in TOML that parses into a `CardFilter` at
load time would let abilities declare targeting predicates without Rust — the same way
effects are already declared.


### Replacement effects

**DONE** — see Steps 10 and 11.

### Characteristic changing abilities
once we have truly rich characteristics
tag a card with "has a characteristic changing ability"
and then when you look at a characteristic of that card, you have to run the ability on it first
e.g. barrowgoyf p/t, kaito hexproof creature on owner turn

### State-based actions
SBAs (creatures with lethal damage, players at 0 life, pw with 0 loyalty, two of a legend, etc.) are currently checked inline at specific points. A proper `check_state_based_actions` pass before every priority invocation would make the engine more correct and remove special-case checks.

### Searching
kind of like targeting but in the library. reuse predicates but a diff zone.
useful tests:
fetches

### Strategy as a trait
`decide_action()` and friends could become a `PlayerStrategy` trait, making it
possible to swap in different archetypes (e.g. a reactive opponent, a goldfish) or
test the engine against a fixed script.

### ninjutsu cost
reasonable and tricky
more generic way is to "capture the cost" in general. And if the cost is bouncing an attacking creature we should snapshot the state of the attacking creature including what it was attacking.

this cuts across to murktide "capturing the cost" to include the spells delved.

### unify shared bits of activation and spell costs

### `handle_priority_round`
Function is massive and needs cleaning up.

Comment on `handle_priority_round` is out of date.

Is `sim_play_land` needed with generic zone logic?

Library threading (`us_lib`/`them_lib`) is already gone. The remaining work: use
player `ObjId` (`AP_id`/`NAP_id`) instead of `"us"`/`"opp"` strings for active
player tracking.

Factor `resolve_top_of_stack` out of `handle_priority_round`.

### protection
good test for targeting
with protection from X, X cannot:
damage
enchant
block
target (if instant gives protection like veil of summer, target becomes invalid at resolution and fizzles)

### `do_step`
match enum to string is silly. `state.current_phase` should also be an enum and this should be a function. Also why is `state.current_phase` even wrong now?
pull out functions for the steps? No benefit to epic mono-function I can see.

attacking / blocking strategy is in `do_step` for DeclareAttackers/DeclareBlockers.

share 'unblocked' state between ninjutsu and damage? or derive it naturally from attacking/blocked/step?

### `activate_planeswalkers`
"pending actions" type logic should all be in strategy, not main engine.

## review / cleanups

These should be relatively easy things to tick off, just from code review.

``eff_enter_permanent` encodes a lot of logic that feels overweight.

`clue` and `Orc Army` is a token not a card

tamiyo flip is exile -> bf-flipped

