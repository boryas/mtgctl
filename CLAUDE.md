## Architecture Principles

### Pilegen specific
**Behavior and implementation flow from the rules engine**
Magic has infinite complexity built into the cards, so we must rely on the engine of the game to ensure regularity and to avoid a combinatorial explosion. This is the most important principle. The rest are reminders of it or special cases of it.

Try to document or leave yourself notes/docs about rules as you implement/improve them so that they ratchet upwards in fidelity.

**Before implementing a card check which fundamental rule it relies on**
DO NOT HARDCODE CARD BEHAVIORS AROUND THE RULES
DO NOT HARDCODE CARD BEHAVIORS AROUND THE RULES
DO NOT HARDCODE CARD BEHAVIORS AROUND THE RULES

only implement cards faithfully via the rules. better to under-implement than to shortcut.
re-check rules specs if necessary

**No strings or dispatch inside the engine**
All cards should have their abilities stored on or associated to them without dispatching by string matching (nor enum matching ideally). This ensures cards flow from rules, not random code.

**Self-referentiality**
The rules of magic are highly self referential
effects grant abilities to themselves and other cards
effects remove abilities from themselves and other cards
effects modify every observable characteristic of every card
replacement effects replace every observable effect
triggers notice every observable effect
etc.

We must build the system to use single points of action and self-referntiality to be able to handle this property of the game. This is the reason uniformity and opacity are important.

**Uniformity**
Only one way to do an effect, make an event, etc..
uniformity in identifying objects by IDs. uniformity in effect definition between spells and abilities. uniformity in cost application between spells and abilities, uniformity in checking state-based-actions before every priority invocation, etc.

**Opacity**
store behavior in opaque operations that are blindly applied when necessary
no special logic in the game engine to resolve targets or effects, all separated into the definitions/implementations of the target specs / effects themselves. Resolution simply applies effects (to optional targets)

**Hard separation game engine and strategy**
The game simulation is a hard simulation of the rules of mtg with some creative extension for allowing randomization

The decisions players make when faced with:
* priority
* a trigger they own
* combat

Should be modular, replacable, can depend on cards, etc. But should not in any way intertwine with the game logic.

**Card implementation is an "outer ring" of the rules**
The cards interplay with the rules intimately, but are not buried in the engine. The engine is generic and exists without the cards, the cards use the engine to carry out their implementation. "Deal 1 damage to any target; Amass Orcs 1" can be an effect defined by a triggered ability of a creature we define as a composed effect for Orcish Bowmasters, but we should not have the engine saying "if card == orcish bowmasters"

**Always build costs effects and predicates out of generic, reusable, composable elements**
e.g. bowmasters ability effect = Amass Orcs 1 + 1 Damage to any target
lightning bolt effect = 3 damage to any target

costs: mana, sacrifice, tap, bounce, discard, etc
effects: destroy, bounce, tap, discard, draw, etc
predicates: color is blue, mana value is less than 4, etc

examples:
non-creature, non-land, instant-or-sorcery: these are bad. They are arbitrary filters over the same core data. They should be implemented as: type != creature, type != land, type == instant OR type == sorcery.
AnyOpponentNonlandPermanent: this collapses multiple separate concepts into a single fragile target spec. It should be: Controller = opponent and type is permanent and type != land

#### Key MTG rules
Do not invent behaviors that are not in the rules. Implement behaviors via the rules. Adding to the rules is a major change and should be done with high intention.

Full rules:
https://yawgatog.com/resources/magic-rules/#R100
