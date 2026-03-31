# add-deck

Add a new deck to the deck database and set up a todo list for wiring the cards into the pilegen simulator.

**Invocation:** `/add-deck <URL>`

---

## Step 1: Fetch the deck list

Determine the source from the URL and fetch accordingly:

**Moxfield** (`moxfield.com/decks/...`):
- `WebFetch` the URL and parse the card list. Store the URL in `moxfield_url`.

**MTGGoldfish specific deck** (`mtggoldfish.com/deck/<id>`):
- `WebFetch` the page and scrape the card list table. Store URL in `moxfield_url` (it holds any source URL).

**MTGGoldfish archetype page** (`mtggoldfish.com/archetype/...`):
- `WebFetch` the page. You may need two fetches with different prompts: one focused on extracting the complete "Most Played Cards" list exhaustively (all cards, all categories), and one for the first concrete decklist.
- Store the archetype URL.
- The "Most Played Cards" list is the card universe for the archetype. The first decklist is the representative example.

Parse the result into: mainboard `[(qty, card_name)]` and sideboard `[(qty, card_name)]`.

---

## Step 2: Collect metadata interactively

Ask the user for the following, in order:

1. **Archetype** — e.g. "Doomsday", "Sneak and Show". Check if it already exists in `deck_types` first.
2. **Subtype** — e.g. "Tempo", "Turbo". `None` if it's an archetype-only row.
3. **Category** — one of: `Combo`, `Control`, `Aggro`, `Midrange`, `Other`. Check existing rows for convention.
4. **List name slug** — the short identifier used as `list_name` in `decks`, e.g. `sneak-and-show-aug24`. This is also the part that appears in parentheses in the display name (e.g. "Sneak and Show (sneak-and-show-aug24)"). Suggest a slug derived from archetype + date or player.
5. **Era** — integer era identifier. Run `SELECT MAX(era) FROM decks;` on `mtgctl.db` to find the current top era and use that as the default. Confirm with the user if unsure.
6. **Flow type** — `"doomsday"` for Doomsday variants, `NULL` otherwise (usually `None`).

---

## Step 3: Insert into the database

Use the Diesel models and patterns from `src/game.rs` (`backfill_deck_types`, `insert_into(decks::table)`).

**If the archetype/subtype doesn't exist** in `deck_types`:
```
NewDeckType { category, archetype, subtype, flow_type }
→ insert_into(deck_types::table)
```
Retrieve the resulting `type_id`.

**Insert the deck**:
```
NewDeck { list_name, moxfield_url, era, type_id }
→ insert_into(decks::table)
```
Retrieve the resulting `deck_id`.

**Insert the card list** into `cards`:
```
NewCard { deck_id, card_name, quantity, board: "main" | "side" }
→ insert_into(cards::table)  (one row per card)
```

---

## Step 4: Cross-reference against implemented cards

Read `src/pilegen/card_defs.rs` and collect the set of card names in `all_cards()` (each is the string passed to `CardDef::new` as the first arg, or the `name` field in the returned struct).

Diff the deck list against that set to produce:
- **Implemented** — already in the engine
- **Not implemented** — need `/add-card` work

---

## Step 5: Write the org todo file

Write `~/org/projects/mtgctl/<archetype-kebab>.org` (e.g. `sneak-and-show.org`, not the full slug) with:
- A header with the deck name, source URL, and date.
- A `TODO` entry for each **unimplemented** card (mainboard first, sideboard after with `:side:` tag).
- Cards that are already implemented can be listed as `DONE` for completeness, or omitted.
- Cards that appear in the archetype "Most Played" universe but not the specific decklist get an extra `:meta:` tag, so the user can filter by tag to see just the specific list vs. the full universe.

Example structure:
```org
#+TITLE: Sneak and Show (sneak-and-show-aug24)
#+DATE: 2026-03-21
#+SOURCE: https://mtggoldfish.com/deck/7684674

* Cards to implement
** TODO Show and Tell                                           :main:
** TODO Sneak Attack                                           :main:
** TODO Griselbrand                                            :main:
** DONE Force of Will                                          :main:
** TODO Emrakul, the Aeons Torn                               :main:
** TODO Blood Moon                                             :side:
```

---

## Notes

- The `list_name` slug is what `lookup_my_deck_id` uses to find a deck. It must match the "(slug)" suffix in match-entry deck names exactly.
- Check for duplicates before inserting: query `decks` by `list_name` first.
- If the archetype row already exists in `deck_types`, just look up its `type_id` — don't re-insert.
