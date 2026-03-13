# Pilegen Improvement Plan

Iterative steps to improve `src/pilegen.rs` against the principles in `CLAUDE.md`.
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

**Status: DONE** (commit to follow)

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

**Known remaining issue:** Resolution in `handle_priority_round` detects counters
with an `is_counter` check — this violates target opacity (Step 4b below).

**Verification:** 113 tests pass. `counter_target` and `matches_counter_target` deleted.

---

## Step 4b — Move `stack` to `SimState`; implement `eff_counter_target()`

**Status: DONE**

**Why:** `stack` is a local variable in `handle_priority_round`, which prevents
`Effect` closures from accessing it. This forces the `is_counter` hack in
resolution. Once `stack` is on `SimState`, counterspells can use a proper
`eff_counter_target()` closure — resolution becomes truly opaque.

**What to do:**

1. Add `pub(crate) stack: Vec<StackItem>` to `SimState`; initialize to `vec![]`.
2. Replace all `stack` local uses in `handle_priority_round` with `state.stack`.
3. Remove `stack: &[StackItem]` parameters from `has_valid_target`,
   `choose_trigger_target`, `choose_spell_target`, `cast_spell`, `push_triggers`
   — they all read `state.stack` directly.
4. Implement `eff_counter_target()` in `effects.rs`:
   - finds `targets[0]` (an `ObjId`) in `state.stack`
   - removes it and moves it to the owner's graveyard
   - if not found, logs a fizzle and does nothing (same as `eff_destroy_target` on
     a dead permanent — no special-case needed)
5. In `spell_effect()` catch-all in `catalog.rs`, when `def.target()` starts with
   `"stack:"`, return `(TargetSpec::StackEntry { filter }, eff_counter_target())`.
6. Delete the `is_counter` block in resolution — just:
   ```rust
   } else if let Some(ref eff) = top.effect {
       // New Effect path: all non-adventure, non-ability spells including counterspells.
       ...
       eff.call(state, t, &top.chosen_targets, catalog_map, rng_dyn);
   }
   ```

**Verification:** `cargo test` passes. No `is_counter` in codebase.

---

## Step 5 — `StackEntry` enum

**Status: TODO**

**Why:** `StackItem` has 17 fields, many mutually exclusive. The flat struct makes
it hard to reason about what fields are valid for spells vs abilities vs triggers.

**What to do:**

Replace `StackItem` with:

```rust
enum StackEntry {
    Spell {
        id: ObjId,
        card_id: ObjId,
        name: String,
        owner: ObjId,
        controller: ObjId,
        chosen_targets: Vec<Target>,
        effect: Option<Effect>,
        is_adventure_face: bool,
    },
    Ability {
        id: ObjId,
        source_id: ObjId,
        source_name: String,
        owner: ObjId,
        controller: ObjId,
        ability_def: Option<AbilityDef>,
        chosen_targets: Vec<Target>,
        effect: Option<Effect>,
    },
    Trigger {
        id: ObjId,
        source_id: ObjId,
        controller: ObjId,
        context: TriggerContext,
        chosen_targets: Vec<Target>,
        effect: Option<Effect>,
    },
}
```

Adventure-specific fields live only on `Spell`. Trigger context lives only on
`Trigger`. No special `ninjutsu_attack_target` — ninjutsu targets are in
`chosen_targets` on the `Ability` variant.

**Verification:** `cargo test` passes. No `unwrap_or_default()` workarounds for
dead fields.

---

## Step 6 — DFC / adventure / ninjutsu: state on the object

**Status: TODO**

**Why:** These three mechanics all hard-code their state tracking rather than
reading generic game state. The principle: state belongs on the object it
describes; mechanics read it from there.

### Adventure

- Remove `PlayerState.on_adventure: Vec<String>`.
- Add `on_adventure: bool` to the card object when in exile.
- `collect_hand_actions` reads exile objects with `on_adventure == true` to find
  castable adventure cards. No special `Vec<String>` needed.

### Tamiyo / DFC transform

- Tamiyo flip is a two step play->exile(id); exile->play-flipped(id);
- Add `active_face: u8` field to permanents (0 = front, 1 = back).
- DFC card definitions carry both faces. Transform = flip `active_face` in place.
  Same `ObjId`, same damage counters, same "entered this turn" status.
- `do_amass_orc()` and `do_flip_tamiyo()` find targets by `ObjId`, not by name.

### Ninjutsu

- Remove `ninjutsu_attack_target` from `StackItem` (done after Step 5).
- Ninjutsu is an activated ability from hand with combat timing. The attacker
  to return is chosen as part of paying the cost of the ability.
  The costs are: (usually) mana + return an unblocked attacking creature to your hand.
  whether "there exists" an unblocked attacking creature (and equally, but less interestingly, the mana) determines whether we can ninjutsu.
  The "enters attacking" state is generic: attackers track an attack target, so a ninjutsu effect reads that off (we can track costs paid as an input to effects, this works for murktide too)
  the entering creature inherits the returned attacker's `attack_target` which it reads from having access to costs paid.

**Verification:** `cargo test` passes. `do_flip_tamiyo`, `on_adventure: Vec<String>`,
and `ninjutsu_attack_target` are deleted.

---

## Step 7 — Ability effects as closures from TOML

**Status: TODO**

**Why:** `ability.effect.starts_with("draw:")`, `"tamiyo_plus_two"`, and the other
string-dispatched effects in `apply_ability_effect()` are not composable and require
engine changes to add new effects. The `Effect` closure system already exists and
works correctly for spells.

**What to do:**

- Extend `AbilityDef` to carry an `Effect` closure built at TOML deserialization.
- Remove the `ability.effect` string dispatch block in `apply_ability_effect()`.
- `"tamiyo_plus_two"` becomes a registered closure on Tamiyo's `CardDef`, built
  the same way `bowmasters_check` is registered in `CARD_TRIGGERS`.
- `search:`, `draw:`, and other effect strings are replaced by `eff_*` primitives.

**Verification:** `cargo test` passes. `apply_ability_effect` string dispatch
(`strip_prefix`, `starts_with`, `==`) is deleted. `"tamiyo_plus_two"` string is gone.

---

## Notes

- The `decide_action()` boundary (engine ↔ strategy) is correct. After Step 1,
  `strategy.rs` formalizes this as a module boundary. A future step can make it a
  trait to support multiple player archetypes.
- Probability constants (`75%` on-board roll, `30%` second-spell, `35%` ninjutsu)
  should eventually move to `PilegenConfig`. Not blocking anything above.
- `eprintln!()` decision logging in strategy bypasses `state.log`. Should be
  replaced with `state.log.push(...)` at some point.
