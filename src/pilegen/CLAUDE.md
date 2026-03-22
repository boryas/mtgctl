## MTG Simulator Principles

> Before any non-trivial implementation — new card mechanic, new engine primitive, new
> targeting or cost pattern — read `DESIGN.md` first. It catalogs the available composition
> primitives (effects, predicates, cost components, target specs) and documents known seams
> where the architecture has gaps. Check whether your need is already expressible before
> adding something new.



1. Cards use the engine; the engine doesn't know about cards.
   No card-specific logic in the engine. Cards compose the engine's primitives
   (effects, predicates, triggers, replacements) to express their behavior.
   The engine operates on opaque closures — it never branches on a card name.
   Compose larger effects from smaller elemental ones.

2. No strings inside the engine.
   The only strings allowed are card names (identity) and mana costs.
   No named-effect dispatch, no string-keyed behavior.

3. All state changes go through a uniform elemental effect interface.
   Required for the replacement → state mutation → trigger → log pipeline
   to intercept every change.

4. All reads of a card's characteristics go through the materialized view.
   Continuous effects are applied on every state change. Reading from the
   base catalog bypasses them — use state.def_of(id) for any object in play.

5. Hard separation of game engine and strategy.
   The engine calls out to strategy at rules-defined decision points.
   Strategy returns a choice; no strategy logic lives in the engine.

   A decision point is any moment the rules grant a player agency — choosing a color or
   creature type, deciding whether to put a card in the graveyard (surveil/scry), picking
   which permanent to sacrifice, etc. The engine must invoke a strategy callback for each
   such decision; it must never make a concrete selection itself.
   The test: if a player would be asked to make this decision at the table, there must be
   a strategy callback for it. A hardcoded value or a direct rng call at the effect site is
   a selection made by the engine, not the strategy — even if the strategy's default is a
   coin flip or a fixed answer. Defaults belong in the callback's default implementation
   (SimState::new or the Strategy trait), not inside the effect.

6. Strategy is isolated and replaceable.
   Strategy may dispatch on card names, use heuristics, or be hardcoded.
   The constraint is the interface, not the implementation.

7. Don't fake it with the rules. Full rules found in a Comprehensive Rules
   file in the project repo. Look up cards exact text via https://scryfall.com/search?q="<CARD>"

8. Build with composition — effects and predicates are the layers.
   Predicates express: target legality, cost satisfiability, and effect applicability.
   Effects express: all state changes.
   Before adding a new opaque variant or primitive, ask whether it decomposes into
   more-elemental ones. The symptom is a name that encodes multiple orthogonal concepts:
     Bad:  `TriggeredAbilityOnStack` — Zone + ObjectKind + AbilityKind baked into one name.
     Good: `AbilityOnStack { ability_type: AbilityType::Triggered }` — three independent axes,
           each a separate field, composable with `Union` and future predicates.
   New elemental primitives are fine; premature aggregation is not.
