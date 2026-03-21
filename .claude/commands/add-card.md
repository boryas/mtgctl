# add-card

Wire a new card into the pilegen simulator.

**Invocation:** `/add-card <card name>`

---

## Steps

### 1. Look up the card

Fetch the card's exact Oracle text from Scryfall:

```
https://scryfall.com/search?q="$ARGUMENTS"
```

Do not guess at rules text — principle 7 in `src/pilegen/CLAUDE.md` requires exact rules.

### 2. Map cost structure onto engine primitives

Read `src/pilegen/catalog.rs` for `CostComponent` variants, `AlternateCost`, and `SpellData`.
Read `src/pilegen/card_defs.rs` for existing patterns to follow.

Per the CR (enforced by engine separation):
- Additional costs (CR 118.9d, paid on top of whatever path) → `SpellData.additional_costs: Vec<CostComponent>`
- Alternative costs (CR 118.9, replace mana cost) → `SpellData.alternate_costs: Vec<AlternateCost>`
- Modal cost (pay X OR Y) within either → `CostComponent::CostOr(vec![...])`

### 3. Add the CardDef in card_defs.rs

Find the right section (creatures, instants, sorceries, artifacts, lands, tokens).
Use `simple(...)` for plain spells; build a full `CardDef { kind: CardKind::... }` for complex ones.

Rules from `src/pilegen/CLAUDE.md` to follow:
- No card-specific logic in the engine; compose effects and predicates
- No string dispatch inside closures
- All state changes via typed effects (eff_draw, eff_bounce_target, etc.)

### 4. Register in all_cards()

Add the card to `all_cards()` at the bottom of `card_defs.rs`, in the correct section.

### 5. Build and test

Run `cargo build` and fix any errors.
Run `cargo test -q` — all 147+ tests must still pass.

### 6. Write a test (if non-trivial)

If the card has costs or effects not covered by existing tests, add a test in
`src/pilegen/tests.rs` following the existing section numbering. The test should verify
the card's most important property (effect fires, cost gate is enforced, etc.).
