# Unified Archetype Definition System

The archetype system uses a three-level hierarchy for deck selection:

1. **Archetype** - The main deck category (e.g., Lands, Doomsday, Storm)
2. **Subtype** - A variant of the archetype (e.g., Mono Green, Tempo, ANT)
3. **List** - A specific decklist version (e.g., sprouts-2.1, tempo-doomsday-wasteland-1.0)

Each level can define game plans, win conditions, and board plans. Lists are linked to Moxfield decklists for version tracking.

You can configure default preferences for each selection level via `config.toml`.

## Directory Structure

All archetype definitions are stored in `definitions/` as TOML files:
- `definitions/lands.toml` - Lands archetype with subtypes
- `definitions/doomsday.toml` - Doomsday archetype with Tempo and Combo subtypes
- `definitions/storm.toml` - Storm archetype with TES, ANT, Ruby, and Black Saga subtypes
- etc.

## Three-Step Deck Selection

When adding a match, you'll be prompted three times:

1. **Select Archetype** - Choose from all archetype files (e.g., "Doomsday", "Lands", "Storm")
2. **Select Subtype** - Choose a variant (e.g., "Tempo", "Mono Green", "ANT")
3. **Select List** - Choose a specific decklist version (e.g., "tempo-doomsday-wasteland-1.0", "sprouts-2.1")

Each prompt uses fuzzy search (type to filter options). Config defaults pre-select your preferences at each step.

## File Structure

### Example: Doomsday with Multiple Subtypes and Lists

```toml
# Doomsday archetype
name = "Doomsday"
category = "Combo"

# Tempo variant
[subtypes.Tempo]
game_plans = ["combo", "cantrips", "control", "creatures"]
win_conditions = ["doomsday", "beatdown", "other"]

[subtypes.Tempo.lists]
"tempo-doomsday-wasteland-1.0" = "https://moxfield.com/decks/XQ8cIaH-Q0-yjdNzdN3JkQ"
"tempo-doomsday-v2" = "https://moxfield.com/..."

# Combo variant
[subtypes.Combo]
game_plans = ["combo", "cantrips", "control"]
win_conditions = ["doomsday", "other"]

[subtypes.Combo.lists]
"combo-doomsday-1.0" = "https://moxfield.com/..."

[board_plan]
description = "Flute Doomsday, disrupt pile"
```

### Example: Lands with Single Subtype

```toml
# Lands archetype
name = "Lands"
category = "Non-Blue"

[subtypes."Mono Green"]
game_plans = ["combo", "saga", "mana denial", "hate", "value"]
win_conditions = ["marit lage", "constructs", "concede"]

[subtypes."Mono Green".lists]
"sprouts-2.1" = "https://moxfield.com/..."
"sprouts-2.2" = "https://moxfield.com/..."

[board_plan]
description = "Counter Life from the Loam, pressure life total"
```

### How It Works

When you select **"Doomsday" → "Tempo" → "tempo-doomsday-wasteland-1.0"**:
- Game plans and win conditions come from `[subtypes.Tempo]`
- The deck name stored in database: `"Doomsday: Tempo (tempo-doomsday-wasteland-1.0)"`
- Board plan comes from the top-level `[board_plan]` (applies to all subtypes)

Each subtype must have:
- `game_plans` - Array of strategies for this variant
- `win_conditions` - Array of how this variant wins
- `lists` - Table of decklist names → Moxfield URLs

Optional per-subtype:
- `board_plan` - Subtype-specific sideboarding guide (overrides archetype-level board_plan)

## Configuration (config.toml)

You can customize default preferences for deck selection and statistics via `config.toml` in the root directory:

```toml
# mtgctl Configuration File

[game_entry]
# Default archetype to pre-select during match entry
# Example: "Doomsday", "Lands"
default_archetype = "Doomsday"

# Default subtype to pre-select (only used if default_archetype is set)
# Example: "Tempo", "Mono Green"
default_subtype = "Tempo"

# Default list/version to pre-select (only used if archetype and subtype are set)
# Example: "tempo-doomsday-wasteland-1.0", "sprouts-2.1"
default_list = "tempo-doomsday-wasteland-1.0"

# Default era to assign to new matches
# Use an integer without quotes (e.g., default_era = 2)
# Leave empty or comment out to auto-detect current era from database
default_era = 2

[stats]
# Default slicing dimensions to show
# Options: "era", "my-deck", "opponent-deck", "my-deck-archetype", "opponent-deck-archetype"
default_slices = ["era", "my-deck-archetype"]

# Minimum games threshold for showing a slice
# Only slices with this many or more games will be displayed
min_games = 0
```

### Game Entry Defaults

When creating a new match with defaults set:
- **default_archetype**: Pre-selects this archetype in Step 1 (e.g., "Doomsday")
- **default_subtype**: Pre-selects this subtype in Step 2 (e.g., "Tempo")
- **default_list**: Pre-selects this list in Step 3 (e.g., "tempo-doomsday-wasteland-1.0")
- **default_era**: Assigns this era to new matches (if not set, uses current era from database)

All three defaults must be set to skip prompts entirely. If only some are set, you'll be prompted starting at the first missing level.

### Stats Defaults

When running `game stats`:
- **default_slices**: Automatically shows these slicing dimensions when no flags are specified
  - Example: `default_slices = ["era", "my-deck-archetype"]` shows both era and archetype breakdowns by default
- **min_games**: Filters out slices with fewer than this many games
  - Useful for hiding statistical noise from small sample sizes
  - Set to 0 to show all slices

## Adding New Decks

### Adding a New List to Existing Subtype

Edit the archetype file and add to the `[subtypes.X.lists]` section:

```toml
[subtypes.Tempo.lists]
"tempo-doomsday-wasteland-1.0" = "https://moxfield.com/decks/XQ8cIaH-Q0-yjdNzdN3JkQ"
"tempo-doomsday-v2" = "https://moxfield.com/decks/abc123"  # Add this line
```

### Adding a New Subtype

Add a new `[subtypes.X]` section to the archetype file:

```toml
[subtypes."New Variant"]
game_plans = ["your", "strategies"]
win_conditions = ["your", "wincons"]

[subtypes."New Variant".lists]
"new-variant-1.0" = "https://moxfield.com/..."
```

### Adding a New Archetype

Create a new file in `definitions/`:

```toml
name = "Your New Archetype"
category = "Combo"

[subtypes.Default]
game_plans = ["your", "plans"]
win_conditions = ["your", "wincons"]

[subtypes.Default.lists]
"version-1.0" = "https://moxfield.com/..."

[board_plan]
description = "How to beat this archetype"
```

## Board Plans

Board plans show what to do when playing **against** a specific deck:

```bash
./target/debug/mtgctl game board-plan "Doomsday: Tempo"
# Shows: Flute Doomsday, disrupt pile
```

Board plans can be defined at:
1. Archetype level (applies to all subtypes) - `[board_plan]`
2. Subtype level (overrides archetype-level) - `[subtypes.X.board_plan]`

## Legacy Files

The following files are kept for backward compatibility:
- `definitions.md` - Original deck list (used as final fallback)
- `board_plans.txt` - Original board plans (used as fallback)
- `definitions_old/` - Old 63-file structure (backup)

The system will use the unified archetype files first, then fall back to these if needed.
