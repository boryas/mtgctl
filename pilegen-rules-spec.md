# Pilegen Rules Engine Spec

A description of the MTG rules subset the engine implements (or intends to
implement), written as a spec for the engine rather than as a player-facing
rulebook. Gaps and approximations are noted explicitly.

---

## Objects

Every entity the game tracks is an **object** with a stable `ObjId`. Objects are:

- **Players** — exactly two, `us` and `opp`. Have life total, mana pool,
  hand size, library, graveyard, exile, and a set of in-play permanents.
- **Cards** — a card object tracks its current zone and any zone-dependent
  state (battlefield state, stack state). One physical card = one object with
  one ID that persists across zone changes.
- **Tokens** — created by effects, never in a library. Otherwise treated as
  cards on the battlefield.
- **Stack abilities** — activated or triggered abilities on the stack. Not
  cards; have no zone besides "stack."

Objects have **generic properties**: color identity, card type, mana value,
toughness, power, loyalty, controller, owner. Predicates over these properties
are the basis for targeting, trigger conditions, and legality checks.

---

## Zones

Every card object is in exactly one zone at all times:

| Zone | Contents |
|------|----------|
| Library | Undrawn cards. Ordered (top known, rest unknown). |
| Hand | Drawn cards. Count is public; identities are hidden (modelled probabilistically). |
| Stack | Spells being cast or abilities being activated/triggered. |
| Battlefield | Permanents in play. |
| Graveyard | Discarded, destroyed, or resolved cards. Ordered; public. |
| Exile | Exiled cards. May carry flags (e.g. `on_adventure`). |

**Zone changes** always fire a `ZoneChange` game event. Zone changes are the
primary trigger source for ETB, death, and exile effects.

---

## Turn Structure

Turns proceed in a fixed sequence of **phases**, each containing one or more
**steps**. The active player (AP) is the player whose turn it is; the
non-active player (NAP) is the other.

```
Beginning Phase
  Untap step       — AP untaps all permanents. No priority.
  Upkeep step      — Priority round.
  Draw step        — AP draws a card. Priority round.

Pre-combat Main Phase
  (no named steps) — AP may play a land. Priority round.

Combat Phase
  Begin Combat step      — Priority round.
  Declare Attackers step — AP declares attackers. Priority round.
  Declare Blockers step  — NAP declares blockers. Priority round.
  Combat Damage step     — Damage is assigned and dealt simultaneously. Priority round.
  End Combat step        — Priority round.

Post-combat Main Phase
  (no named steps) — AP may play a land (if not used pre-combat). Priority round.

Ending Phase
  End step     — Priority round. "Until end of turn" effects expire here.
  Cleanup step — AP discards to hand size (7). Damage is removed. No priority
                 (unless a triggered ability fires during cleanup).
```

Each step that has a priority round fires an `EnteredStep` event, then runs the
priority round. Steps without priority (Untap, Cleanup) execute their action
and proceed.

**Approximation:** The engine currently simulates only the turns leading up to
a pre-selected "target turn" rather than a full game loop. Combat is simulated
once per relevant turn.

---

## Priority

The **priority system** is the core mechanic. A priority round proceeds as:

1. The player with priority takes an **action** or **passes**.
2. If a player takes an action that places something on the stack, priority
   passes to the other player.
3. If both players pass consecutively with something on the stack, the top
   item resolves and priority returns to AP.
4. If both players pass consecutively with an empty stack, the step ends.

**Special actions** (playing a land) do not use the stack and do not cause
priority to pass.

**Mana abilities** (tapping a land, cracking a fetch) do not use the stack.
They may be activated whenever a player has priority (or is paying a cost).

---

## Actions

When a player has priority they may take one action:

### Cast a Spell
1. Announce the spell and move the card to the stack.
2. Choose targets (if any) using the spell's `TargetSpec`.
3. Pay the cost (mana, life, exile a card, bounce a land, etc.).
4. The spell is now on the stack. Priority passes to the other player.

**Alternate costs** (e.g. Force of Will's pitch cost) replace the mana cost
entirely when used. Only one cost option is paid.

**Additional costs** (e.g. Snuff Out's life payment alongside its mana cost)
are paid on top of the mana cost.

**Casting conditions** must be satisfied before casting is legal:
- Sorceries require: AP, main phase, empty stack.
- Instants: any time a player has priority.
- Creatures, planeswalkers, artifacts, enchantments: sorcery speed (as above).
- Lands: special action, AP, main phase, empty stack, one per turn.

### Activate an Ability
1. Announce the ability and its source permanent.
2. Choose targets (if any).
3. Pay the cost.
4. The ability goes on the stack. Priority passes.

Activated abilities may be used any time a player has priority unless they have
a timing restriction (e.g. "activate only as a sorcery").

Mana abilities are an exception: they do not go on the stack and may be
activated mid-cost-payment.

### Pass
Priority passes to the other player. If both players pass consecutively the
step advances or the top stack item resolves.

---

## The Stack

The stack is an ordered list of **stack entries**, LIFO. Each entry is one of:

- **Spell** — a cast card. Has: source card ObjId, controller, chosen targets,
  effect closure, cost record.
- **Activated ability** — generated by a permanent's activated ability. Has:
  source permanent ObjId, controller, chosen targets, effect closure.
- **Triggered ability** — generated by a trigger firing. Has: source ObjId,
  controller, chosen targets, trigger context, effect closure.

When both players pass with the stack non-empty, the **top entry resolves**:

1. (For spells and abilities with targets) Validate targets are still legal.
2. Execute the effect closure.
3. Move the source card to its destination zone:
   - Instants and sorceries → graveyard (or exile if adventure).
   - Permanents → battlefield.
   - Abilities → removed from stack (no zone).

**Counterspells** target another stack entry by ObjId. When a counterspell
resolves, the targeted entry is removed from the stack without resolving and
its source card goes to the graveyard.

---

## Costs

A cost is a structured set of one or more of the following components:

| Component | Description |
|-----------|-------------|
| Mana | Pay N mana matching a `ManaCost` spec. |
| Life | Pay N life. |
| Tap self | Tap the source permanent. |
| Sacrifice self | Sacrifice the source permanent. |
| Sacrifice a land | Sacrifice any land you control. |
| Discard self | Discard this card (from hand). |
| Bounce island | Return a blue-producing land you control to hand. |
| Exile blue from hand | Exile another blue card from your hand. |

Costs are checked for affordability before an action is legal. All components
of a cost are paid simultaneously.

**Alternate costs** are complete cost replacements. A card may offer several
alternate cost options; the player chooses one at cast time. The default mana
cost is always an option if the card has one.

---

## Effects

An effect is a closure `(state, turn, targets, catalog) → ()`. Effects are
**composable**: `eff_a.then(eff_b)` produces an effect that applies `eff_a`
then `eff_b`.

### Effect primitives

| Primitive | Description |
|-----------|-------------|
| `eff_draw(n)` | Active player draws N cards (fires Draw events). |
| `eff_put_back(n)` | Active player puts N cards from hand on top of library. |
| `eff_mana(spec)` | Add mana to controller's pool. |
| `eff_destroy_target()` | Destroy the targeted permanent. |
| `eff_bounce_target()` | Return the targeted permanent to its owner's hand. |
| `eff_discard_random(n)` | Opponent discards N cards at random. |
| `eff_discard_choice(n)` | Opponent discards N cards of their choice. |
| `eff_enter_permanent(name)` | Put the card onto the battlefield. |
| `eff_reanimate(filter)` | Return a creature from graveyard to battlefield. |
| `eff_counter_target()` | Counter the targeted stack entry. |

Effects that require targets receive them as a `&[Target]` slice; the effect
closure reads the slice to determine what to act on.

### Continuous effects

Some effects persist: "until end of turn," "until your next turn," or
permanently. A continuous effect registers itself on `state.active_effects` with
an **expiry** condition and optionally an **on_event** hook that fires additional
triggers when game events occur during its lifetime.

Continuous effects are removed at the appropriate cleanup point.

---

## Targets

Every targeted spell or ability has a `TargetSpec` that describes what objects
are legal targets. A `TargetSpec` is a predicate over game objects.

### Target categories

- **Permanents** — objects on the battlefield, filtered by type, color, mana
  value, or other properties.
- **Players** — either player as an object.
- **Stack entries** — spells or abilities on the stack (for counterspells).
- **Cards in a zone** — e.g. "creature card in your graveyard."

### Target selection

At cast/activation time:
1. Find all legal targets matching the `TargetSpec`.
2. If no legal targets exist, the spell or ability cannot be cast/activated.
3. The controlling player (or the AI strategy for that player) selects a target
   from the legal set.
4. The chosen target is stored as `Target::Object(ObjId)` or
   `Target::Player(ObjId)`.

At resolution time:
1. Recheck that each chosen target is still a legal target.
2. If a target is no longer legal (died, left the zone), the spell or ability
   is **countered on resolution** (fizzles) if all its targets are illegal. If an ability is composed of several effects chained by then(), the order matters. If the first one fizzles, so does the rest of the chain.

---

## Triggered Abilities

A triggered ability watches for a specific **game event** and fires when the
event satisfies its condition predicate.

### Game events

Every meaningful state change fires a `GameEvent`:

| Event | Fired when |
|-------|-----------|
| `Draw { controller, draw_index, is_natural }` | A card is drawn. |
| `ZoneChange { card, from, to, controller }` | A card moves between zones. |
| `CreatureAttacked { attacker_id, attacker, attacker_controller, attack_target }` | A creature is declared as an attacker. |
| `EnteredPhase { phase, active_player }` | A phase with priority begins. |
| `EnteredStep { step, active_player }` | A step with priority begins. |

*More events will be added as cards require them.*

### Trigger pipeline

1. A game event fires.
2. `fire_triggers(event, state)` checks every permanent in play against the
   `CARD_TRIGGERS` registry (and any registered `active_effects` with
   `on_event` hooks).
3. Matching triggers produce a `TriggerContext` (source ObjId, controller,
   target spec, effect closure).
4. Each `TriggerContext` is pushed onto the **pending triggers** queue.
5. At the next opportunity to place things on the stack, pending triggers are
   moved onto the stack as triggered ability entries.
6. Targets are chosen at push time (step 5), not at fire time (step 2).
7. The triggered ability resolves normally via the priority/stack system.

**Invariant:** Game events are the only path to triggered behavior. If
something happens that should cause a trigger, a game event must be fired.

---

## State-Based Actions (SBAs)

SBAs are checked before every priority round and applied immediately (no stack,
no priority):

- A creature with damage ≥ toughness is destroyed.
- A player at 0 or less life loses the game.
- A planeswalker with 0 or less loyalty is put into the graveyard.
- A legend with a copy of itself causes one copy to be put into the graveyard
  (controller's choice).

SBAs are checked repeatedly until none apply.

**Current gap:** SBAs are partially implemented. Lethal damage and legend rule
are checked; the 0-loyalty planeswalker rule is not enforced as an SBA.

---

## Combat

Combat follows the Combat Phase step structure above.

### Declare Attackers
AP selects which untapped, non-summoning-sick creatures attack. Each attacker
may attack either the defending player or a planeswalker that player controls.
Attacking taps the creature (unless it has vigilance).

### Declare Blockers
NAP assigns blockers. Each blocker may block exactly one attacker. Multiple
blockers may block the same attacker (the engine currently models one-blocker-
per-attacker for simplicity).

### Combat Damage
Damage is assigned simultaneously:
- An unblocked attacker deals its power in damage to its target (player or
  planeswalker).
- A blocked attacker and its blocker deal damage to each other simultaneously.

Damage uses the **lethal damage** SBA: a creature is destroyed when accumulated
damage ≥ toughness.

### Keywords affecting combat
- **Flying** — can only be blocked by creatures with flying or reach.
- **First strike** — deals damage before non-first-strike creatures.
  *(Not yet modelled as two damage steps.)*
- **Trample** — excess damage from a blocked creature carries through to the
  player. *(Not yet implemented.)*
- **Vigilance** — does not tap when attacking.
- **Haste** — may attack the turn it enters play.
- **Menace** — must be blocked by two or more creatures.
- **Deathtouch** — any damage dealt is lethal.
- **Hexproof** — cannot be targeted by opponent's spells/abilities.

---

## Mana

Each player has a **mana pool** that resets to empty at the end of each step.
Mana is produced by mana abilities (land taps, etc.) and consumed when paying
costs.

The pool tracks each color separately: W, U, B, R, G, and C (colorless).

A `ManaCost` is satisfied if the pool contains at least the required pips of
each color plus enough total mana for any generic component.

**Potential mana** — for planning purposes, the engine computes how much mana a
player could produce by tapping all untapped lands (and other mana sources).
This is used to check whether a cost is affordable before attempting to pay it.

---

## Approximations and Known Gaps

The engine is a **Monte Carlo scenario generator**, not a complete MTG rules
engine. These approximations are intentional:

- **Hidden information** is modelled probabilistically. The opponent's hand is
  represented as a count; card identities are rolled using a hypergeometric
  distribution when a decision requires them (e.g. "does the opponent have a
  Force of Will?").
- **Mulligans** are modelled by starting with a reduced hand size.
- **Spell sequencing** on a given turn is simplified: each player takes one
  action per priority window rather than exhaustively exploring all lines.
- **Multi-blocker assignment** is not yet implemented.
- **Trample and first strike** combat keywords are not yet implemented.
- **Replacement effects** are not yet implemented.
- **The legend rule** is enforced but only during the resolution pass, not as
  a continuous SBA.
- **Mana burn** does not exist in current Magic rules and is not modelled.
