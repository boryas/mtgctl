cna# MTG CLI Tool - Project Plan
/c
## Overview
A comprehensive command-line interface tool for Magic: The Gathering players to manage decklists, track game results, calculate probabilities, and analyze tournament performance.

## Core Features

### 1. Decklist Management
- **Store and organize decklists**
  - Import from various formats (MTG Arena, MTGO, plain text)
  - Export to different formats
  - Categorize by format (Standard, Modern, Legacy, etc.)
  - Tag system for custom organization
  - Version control for deck iterations

### 2. Game Result Tracking
- **Match and game recording**
  - Record wins/losses with detailed information
  - Track opponents, decks played against
  - Note game conditions (play/draw, mulligans)
  - Session tracking for events/leagues
  - Statistical analysis of performance

### 3. Probability Calculator
- **In-game probability calculations**
  - Hypergeometric distribution for draw probabilities
  - Mana curve analysis
  - Mulligan decision support
  - Turn-by-turn probability calculations
  - Land drop probabilities

### 4. Tournament Analysis
- **MTGO League and Tournament Tools**
  - League EV calculations
  - Prize pool analysis
  - Win rate requirements for profitability
  - Bankroll management recommendations
  - Tournament ROI tracking

## Technical Architecture

### Command Structure
```
mtgctl <command> [subcommand] [options]

Commands:
- deck     - Decklist management
- game     - Game result tracking
- calc     - Probability calculations
- league   - Tournament/league analysis
- config   - Configuration management
```

### Data Storage
- **Local SQLite database** for persistent storage
- **JSON configuration files** for user preferences
- **Backup/sync options** for data portability

### External Integrations
- **Scryfall API** for card data and pricing
- **MTG Arena/MTGO export parsing**
- **Tournament site APIs** (if available)

## Implementation Phases

### Phase 1: Core Infrastructure
1. CLI framework setup (using clap or similar)
2. Database schema design and setup
3. Basic configuration management
4. Card data integration with Scryfall

### Phase 2: Decklist Management
1. Deck storage and retrieval
2. Import/export functionality
3. Basic deck analysis tools
4. Deck comparison features

### Phase 3: Game Tracking
1. Match recording system
2. Statistical analysis engine
3. Performance reporting
4. Historical data visualization

### Phase 4: Probability Tools
1. Core probability calculations
2. Interactive probability calculator
3. Deck analysis probabilities
4. Scenario-based calculations

### Phase 5: Tournament Analysis
1. League tracking system
2. EV calculation tools
3. Bankroll management features
4. Performance projections

## Technology Stack
- **Language**: Rust (for performance and reliability)
- **CLI Framework**: clap
- **Database**: SQLite with diesel ORM
- **HTTP Client**: reqwest (for API calls)
- **Serialization**: serde
- **Configuration**: config crate

## File Structure
```
mtgctl/
├── src/
│   ├── main.rs
│   ├── cli/
│   │   ├── mod.rs
│   │   ├── deck.rs
│   │   ├── game.rs
│   │   ├── calc.rs
│   │   └── league.rs
│   ├── db/
│   │   ├── mod.rs
│   │   ├── models.rs
│   │   └── schema.rs
│   ├── api/
│   │   ├── mod.rs
│   │   └── scryfall.rs
│   ├── utils/
│   │   ├── mod.rs
│   │   ├── probability.rs
│   │   └── parser.rs
│   └── config/
│       ├── mod.rs
│       └── settings.rs
├── migrations/
├── tests/
├── docs/
├── Cargo.toml
└── README.md
```

## Success Criteria
- Intuitive command-line interface
- Fast and reliable data operations
- Accurate probability calculations
- Comprehensive tournament analysis
- Extensible architecture for future features
- Cross-platform compatibility

## Future Enhancements
- Web interface for advanced visualization
- Mobile app companion
- Social features (sharing decks, comparing stats)
- Advanced AI-powered deck suggestions
- Integration with streaming/content creation tools
