# Pilegen Architecture Review

Assessment of `src/pilegen.rs` against the principles in `CLAUDE.md`.

---

## Overall

The engine has correct MTG priority/stack mechanics and a partial Effect
composition system that is moving in the right direction. The strategy border at
`decide_action()` is correctly placed. The main gaps are: card-name checks
inside the strategy layer that should be card-property checks, name-based object
lookups surviving the ID migration, and card-specific behavior that has not yet
been lifted into composable primitives.

---

## 1. Priority / Action System

**State: Sound structure, clean border at `decide_action()`.**

The `PriorityAction` enum, the AP/NAP alternation loop, and the LIFO stack
resolution are all correctly modelled. Priority correctly requires both players
to pass consecutively before the stack resolves.

`decide_action()` is the current and intentional border between the game engine
and player strategy. Everything inside it — `ap_proactive()`, `ap_react()`,
`nap_action()` — is strategy, and belongs there.

**Problems:**

- `ap_react()` (~3707) is hardcoded to protect Doomsday specifically
  (`s.name == "Doomsday" && s.owner == state.us.id`, ~3725). This names a
  specific card inside strategy code; acceptable for now but will need to
  evolve as the strategy layer matures.
- The probability constants (75% on-board action roll, 30% second-spell roll,
  35% ninjutsu roll) are inline magic numbers. They should live in
  `PilegenConfig` so scenarios can tune them without code changes.

**Direction:** The border is right. The next step is making `decide_action()` a
more sharply defined interface — a trait or function pointer — so it can be
swapped out for different player archetypes and tested independently of the
engine. Move probability constants to config.

---

## 2. Object Identity

**State: ID migration incomplete — ~60 string name lookups remain.**

`ObjId` is used correctly for permanents, stack items, and players. Most combat
and targeting logic now uses IDs.

**Remaining violations:**

- `TriggerContext.source: String` — a `TODO(ids)` comment acknowledges this.
  Should be `ObjId`.
- `do_amass_orc()` (~3500) finds the Orc Army token by `p.name == "Orc Army"`.
- `do_flip_tamiyo()` (~3529) removes/creates permanents by matching
  `"Tamiyo, Inquisitive Student"` and `"Tamiyo, Seasoned Scholar"` by name.
- `murktide_check()` closure (~3293) finds Murktide by `p.name == "Murktide
  Regent"` to add counters.
- Library lookup in `cast_spell()` (~2919): `position(|(_, n, _)| n == name)` —
  string equality on library cards that already have `ObjId`s assigned.
- `adventure_card_name: Option<String>` on `StackItem` (~643) instead of ObjId.

**Direction:** Finish the migration. `TriggerContext.source` → `ObjId` is the
most important. Card-specific effects (Tamiyo flip, Murktide counter) should
find their target permanent by the `ObjId` captured when the trigger was queued,
not by searching by name at resolution time.

---

## 3. Effects, Costs, Predicates

**State: Good foundation, old string-dispatch path still dominant.**

The `Effect` closure system with `.then()` composition and `eff_*` primitives is
correct and clean. Brainstorm as
`eff_draw(3).then(eff_put_back(2))` is exactly the right pattern.

**Problems:**

- Ability effects are string-dispatched (~2673–2749):
  `ability.effect.strip_prefix("draw:")`, `ability.effect.starts_with("search:")`,
  `ability.effect == "tamiyo_plus_two"`. These are not composable and require
  engine code changes to add new effects.
- The `spell_effect()` match (~3012–3062) maps card names to `(TargetSpec,
  Effect)` pairs. It works but every new spell requires a code change.
- `matches_target_type()`, `matches_counter_target()`, and
  `matches_search_filter()` are three separate predicate systems that could be
  one.
- `counter_target` in `SpellData` is a `String` that is matched with a custom
  function. It should be a `TargetSpec`.

**Direction:** Extend `AbilityDef` to carry an `Effect` (built from TOML at
deserialization time) rather than an effect string. Replace `ability.effect`
string dispatch with direct closure invocation. Unify predicate matching.

---

## 4. Card Implementations (Outer Ring)

**State: Mostly correct in principle, several leaks.**

The `CARD_TRIGGERS` static dispatch table is principled — it is a registry, not
engine code. The individual `bowmasters_check`, `murktide_check`, and
`tamiyo_check` functions are card implementations and their reference to their
own card name is acceptable.

**Leaks:**

- Ninjutsu is special-cased in `apply_ability_effect()` (~2632) with a
  hardcoded `ability.ninjutsu` boolean, manual permanent construction, and a
  `ninjutsu_attack_target` field threaded through `StackItem`. Ninjutsu is an
  activated ability from hand (like cycling) with combat timing restrictions and
  a cost that includes returning an unblocked attacker. The "entering attacking"
  target is already on the returned attacker in combat state — no special
  tracking needed.
- Adventure state lives on `PlayerState.on_adventure: Vec<String>` rather than
  on the card object itself. The card object should carry the `on_adventure`
  flag; the engine reads it from there.
- Tamiyo's transform destroys the student permanent and creates a new scholar
  permanent (`do_flip_tamiyo()`). A transforming permanent should flip its
  `active_face` field in place — same ObjId, same damage, same turn entered.
- Tamiyo's `+2` effect is dispatched by the string `"tamiyo_plus_two"` in
  `apply_ability_effect()` (~2738) rather than being a registered closure on
  her `CardDef`.

**Direction:** The unifying principle is: track state on the object generically,
let mechanics read it — don't hard-code each mechanic. DFCs (adventure,
transform) are a major todo. Both require: one card object, one ObjId
throughout, face/adventure state as a field on the card object, zone changes as
mutations not replacements.

---

## 5. Target System

**State: Works for permanents; incomplete for stack and graveyard.**

`TargetSpec` with `has_valid_target()` checks are correctly wired into
`collect_hand_actions()` and `respond_with_counter()`. The `counter_target`
field now correctly blocks proactive casting when the stack is empty.

**Gaps:**

- `counter_target` is a `String` that goes through its own matching function
  rather than being a `TargetSpec` variant. Should be unified.
- Targets chosen at cast time are not revalidated at resolution. If the target
  dies in response, the effect still applies.
- Land search uses a separate `choose_land_name()` function (~2139) rather than
  the standard target system.
- No targeting for players as objects (direct damage, discard).

**Direction:** Make `counter_target` a `TargetSpec`. Add resolution-time target
validation. Extend `TargetSpec` to cover player targets.

---

## 6. Triggers and Game Events

**State: Correct path exists; not all triggered behavior uses it.**

`GameEvent`, `fire_triggers()`, `queue_triggers()`, and `push_triggers()` form a
correct pipeline. Triggers correctly go onto the stack as `is_ability: true`
items and resolve via the priority loop.

**Bypasses:**

- Most step transitions do not fire `EnteredStep` events. Only
  `DeclareAttackers` does (~4390). Upkeep, Draw, BeginCombat, DeclareBlockers,
  EndCombat, and Cleanup produce no events — meaning triggered abilities cannot
  respond to them.
- Adventure resolution (~4186–4203) manually pushes to `on_adventure` rather
  than firing a zone-change event.
- Planeswalker loyalty restriction ("activate once per turn") is enforced by a
  flag in decision logic, not by a triggered/replacement effect.
- Ninjutsu's "return attacker to hand" is executed in the priority loop, not via
  a zone-change event.

**Direction:** Fire `EnteredStep` for all steps. Zone changes (including
adventure exile, ninjutsu return) should always fire `ZoneChange` events so
triggered abilities can respond. This is load-bearing for the combinatorial
future.

---

## 7. Stack

**State: Correct mechanics; representation too flat.**

The stack correctly models LIFO resolution, counterspell targeting via the
`counters: Option<ObjId>` field, and ability items alongside spell items.

**Problem:**

`StackItem` has 17 fields, many mutually exclusive by use case:
`adventure_exile`, `adventure_card_name`, `adventure_face` are only relevant for
adventure spells; `ninjutsu_attack_target` is only relevant for ninjutsu;
`trigger_context` is only relevant for triggered abilities. The flat struct makes
it hard to reason about what fields are valid in what state.

**Direction:** Replace with an enum:
```
enum StackEntry {
    Spell     { card_id, owner, chosen_targets, effect, … },
    Ability   { source_id, owner, ability_def, chosen_targets, effect, … },
    Trigger   { source_id, controller, context, chosen_targets, … },
}
```
Variant-specific data lives only on the relevant variant.

---

## 8. Strategy / AI

**State: Boundary exists at `decide_action()`; interface not yet formal.**

Strategy is correctly contained inside `decide_action()` and its callees:
`ap_proactive()`, `nap_action()`, `ap_react()`, `try_ninjutsu()`, and
`collect_on_board_actions()`. The engine calls `decide_action()` and does not
look inside it.

**Specific issues:**

- All probability constants are inline magic numbers. They should be in
  `PilegenConfig` so scenarios can vary strategy without code changes.
- There is no concept of the opponent's game plan — it defaults to generic
  "counter threats and attack." A real archetype model would let the opponent
  play its own combo, control, or aggro game plan.
- `eprintln!()` decision logging (~3687, 3692, 3835) bypasses `state.log` and
  cannot be captured, suppressed, or structured.

**Direction:** `decide_action()` is the right seam. Making it a formal
interface — a trait or a passed-in function — would allow different strategy
implementations per player, independent testing of strategy logic, and
eventually opponent archetypes. Move probability constants to config.

---

## Priority Order

| # | Area | Why first |
|---|------|-----------|
| 1 | Fire `EnteredStep` for all steps | Unblocks all future triggered abilities |
| 2 | `TriggerContext.source` → `ObjId` | Completes the ID migration |
| 3 | `counter_target` → `TargetSpec` | Unifies the predicate system |
| 5 | `StackEntry` enum | Cleans up the most complex data structure |
| 6 | `decide_action()` → formal trait | Formalizes the existing boundary; enables archetype testing |
| 7 | Ability effects → closures from TOML | Removes string dispatch entirely |
