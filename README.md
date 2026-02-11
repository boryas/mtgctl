# mtgctl

A CLI tool for tracking and analyzing Magic: The Gathering match data, built in Rust. Designed for detailed game-level tracking with archetype-aware prompts, league management, and statistical analysis.

## Commands

### `mtgctl game`

Match and game data entry and analysis.

- `add-match` — Interactively record a best-of-3 match with per-game details (play/draw, mulligans, opening hand plan, win condition/loss reason, turn count)
- `list-matches` — Show recent matches
- `match-details <ID>` — Display all games and data for a match
- `edit-match <ID>` / `edit-game <ID> <GAME#>` — Modify recorded data
- `remove-match <ID>` — Delete a match
- `add-deck` — Register a new deck
- `board-plan` — Show sideboard strategy for an opponent archetype
- `stats` — Interactive statistics with multi-level filtering and grouping
- `league-stats` — League-specific win/loss tracking
- `graph` — Visualize trends (win rate, mulligans, game length) as ASCII or HTML charts
- `html-stats` — Generate a comprehensive HTML statistics report
- `reconcile-deck` — Normalize opponent deck names for an archetype

### `mtgctl deck`

Deck list management and probability calculations.

- `import` — Import a deck list (mainboard/sideboard)
- `list` / `view` / `delete` — Browse and manage decks
- `probability` — Calculate opening hand probability for specific cards
- `sequential` — Sequential opening hand probability analysis

### `mtgctl league`

MTGO League trophy probability calculator. Given a win rate and drop strategy, calculates trophy probability, expected cost, and time to trophy.

## Archetype Definitions

The `definitions/` directory contains TOML files defining opponent archetypes. Each file specifies:

- Category (Blue, Combo, Non-Blue, Other)
- Game plans, win conditions, and loss reasons
- Subtypes with variant-specific options
- Sideboard plan descriptions

These drive the interactive prompts during match entry, suggesting context-appropriate options for each matchup.

### Doomsday-Specific Tracking

When playing Doomsday, additional data is collected: pile type (Cyclers, Brainstorm+LED, etc.), whether Doomsday resolved, pile quality assessment, and sideboard juke plans.

## Configuration

`config.toml` supports default deck/archetype/era selections for game entry and pre-configured stat filters and groupings.

## Building

```
cargo build --release
```

Requires SQLite development libraries.
