## MTG Simulator Principles

Before implementing a card, answer these questions:

**1. Can this card be directly and trivially expressed using existing primitives in `DESIGN.md`?**
If yes — compose and move on.
If no — identify what's missing. The engine's core invariant is **writes are visible to reads; reads see all writes.** Cards fall into four categories:

- **React to the game** (triggers, replacements, prohibitions) — must use events, never hook into specific code
- **Read object state** (card, stack, player) — must see materialized effects-applied state
- **Modify object state once** (one-shot effects) — must generate events
- **Modify object state continuously** (continuous effects) — must be visible via materialized state

Define the missing primitive at the right level of generality before writing any code.

**2. Is your implementation at the same level of generality as the wording of the card?**
If your implementation is more specific than the oracle text, you've modeled the current simulation, not the rule.

**3. Is the player making a decision? If so, is there a strategy callback for it?**
If a player would be asked at the table, the engine must not answer for them.

Oracle text signal words — each implies a player choice:
- **choose** — active player selects a mode, color, card name, etc.
- **target** — active player selects a legal target
- **sacrifice** — the player told to sacrifice selects which permanent
- **discard** — the player told to discard selects which card (unless "at random")
- **may** — the affected player decides whether the effect applies
- **up to** — active player selects a quantity within a bound

Oracle text signal words — each implies a triggered ability (React to the game):
- **when** — triggers once on a specific event
- **whenever** — triggers every time the event occurs
- **at** — triggers at a defined game step or phase

Oracle text signal words — each implies a replacement effect:
- **skip** — replaces an event with nothing
- **as [event happens]** — modifies how an event occurs as it happens
- **if [event] instead [replacement]** — redirects an event to a different outcome

Oracle text signal words — each implies a prohibition effect:
- **can't** — permanently forbids an action or event
- **prevent** — stops damage or an effect from occurring
