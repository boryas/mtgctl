use clap::{Args, Subcommand};
use dialoguer::{Input, FuzzySelect, Confirm, MultiSelect};
use chrono::{Local, NaiveDate};
use diesel::prelude::*;
use std::fs;
use std::collections::HashMap;
use std::path::Path;
use serde::Deserialize;
use comfy_table::{Table, Cell, Attribute, ContentArrangement};

use crate::db::{establish_connection, models::*};
use crate::db::schema::{matches, games};

#[derive(Debug, Deserialize)]
struct UnifiedArchetypeDefinition {
    name: String,
    category: String,
    #[serde(default)]
    game_plans: Vec<String>,
    #[serde(default)]
    win_conditions: Vec<String>,
    #[serde(default)]
    loss_reasons: Vec<String>,
    #[serde(default)]
    board_plan: Option<BoardPlan>,
    #[serde(default)]
    subtypes: HashMap<String, SubtypeDefinition>,
}

#[derive(Debug, Deserialize)]
struct SubtypeDefinition {
    game_plans: Vec<String>,
    win_conditions: Vec<String>,
    #[serde(default)]
    loss_reasons: Vec<String>,
    #[serde(default)]
    board_plan: Option<BoardPlan>,
    #[serde(default)]
    lists: HashMap<String, String>,
}

#[derive(Debug, Deserialize, Clone)]
struct BoardPlan {
    description: String,
}

// Configuration file structure
#[derive(Debug, Deserialize, Default)]
struct Config {
    #[serde(default)]
    game_entry: GameEntryConfig,
    #[serde(default)]
    stats: StatsConfig,
}

#[derive(Debug, Deserialize, Default)]
struct GameEntryConfig {
    default_archetype: Option<String>,
    default_subtype: Option<String>,
    default_list: Option<String>,
    default_era: Option<i32>,
}

#[derive(Debug, Deserialize, Default)]
struct StatsConfig {
    #[serde(default)]
    min_games: i64,
    #[serde(default)]
    filters: StatsFilters,
    #[serde(default)]
    default_groupbys: Vec<String>,
    #[serde(default)]
    default_statistics: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
struct StatsFilters {
    era: Option<i32>,
    my_deck: Option<String>,
    opponent: Option<String>,
    opponent_deck: Option<String>,
    event_type: Option<String>,
}

// Statistics row data
#[derive(Clone)]
struct StatsRow {
    label: String,
    match_wins: usize,
    match_losses: usize,
    match_count: usize,
    match_win_rate: f64,
    game_wins: usize,
    game_losses: usize,
    game_count: usize,
    #[allow(dead_code)]
    game_win_rate: f64,
    avg_mulligans: f64,
    avg_win_mulligans: f64,
    avg_loss_mulligans: f64,
    avg_game_length: Option<f64>,
    avg_win_length: Option<f64>,
    avg_loss_length: Option<f64>,
    win_conditions: HashMap<String, usize>,
    loss_conditions: HashMap<String, usize>,
}

/// Indicates whether a filter, group-by, or statistic operates at match or game level
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatsLevel {
    Match,
    Game,
}

/// Filter indices and their levels
/// Indices correspond to the filter_options vec in show_stats_interactive
const FILTER_LEVELS: &[(usize, &str, StatsLevel)] = &[
    (0, "era-latest", StatsLevel::Match),
    (1, "era-all", StatsLevel::Match),
    (2, "my-archetype", StatsLevel::Match),
    (3, "my-subtype", StatsLevel::Match),
    (4, "my-list", StatsLevel::Match),
    (5, "opponent", StatsLevel::Match),
    (6, "opponent-deck", StatsLevel::Match),
    (7, "event-type", StatsLevel::Match),
    (8, "loss-reason", StatsLevel::Game),
    (9, "win-condition", StatsLevel::Game),
    (10, "game-plan", StatsLevel::Game),
    (11, "mulligan-count", StatsLevel::Game),
    (12, "game-length", StatsLevel::Game),
    (13, "game-number", StatsLevel::Game),
    (14, "play-draw", StatsLevel::Game),
];

/// Group-by indices and their levels
/// Indices correspond to the groupby_options vec in show_stats_interactive
const GROUPBY_LEVELS: &[(usize, &str, StatsLevel)] = &[
    (0, "my-archetype", StatsLevel::Match),
    (1, "my-subtype", StatsLevel::Match),
    (2, "my-list", StatsLevel::Match),
    (3, "opponent", StatsLevel::Match),
    (4, "opponent-deck", StatsLevel::Match),
    (5, "opponent-deck-archetype", StatsLevel::Match),
    (6, "opponent-deck-category", StatsLevel::Match),
    (7, "era", StatsLevel::Match),
    (8, "game-number", StatsLevel::Game),
    (9, "mulligans", StatsLevel::Game),
    (10, "game-plan", StatsLevel::Game),
    (11, "win-condition", StatsLevel::Game),
    (12, "loss-reason", StatsLevel::Game),
    (13, "game-length", StatsLevel::Game),
    (14, "play-draw", StatsLevel::Game),
];

/// Statistic indices and their levels
/// Indices correspond to the stat_options vec in show_stats_interactive
const STAT_LEVELS: &[(usize, &str, StatsLevel)] = &[
    (0, "match-win-rate", StatsLevel::Match),
    (1, "game-win-rate", StatsLevel::Game),
    (2, "match-count", StatsLevel::Match),
    (3, "game-count", StatsLevel::Game),
    (4, "mulligans", StatsLevel::Game),
    (5, "game-length", StatsLevel::Game),
    (6, "win-conditions", StatsLevel::Game),
    (7, "loss-conditions", StatsLevel::Game),
    (8, "proportion", StatsLevel::Game),
];

/// Helper to check if any selected indices include game-level items
fn has_game_level(indices: &[usize], levels: &[(usize, &str, StatsLevel)]) -> bool {
    indices.iter().any(|&idx| {
        levels.iter().any(|(i, _, level)| *i == idx && *level == StatsLevel::Game)
    })
}

/// Helper to get the level of a specific index
fn get_level(idx: usize, levels: &[(usize, &str, StatsLevel)]) -> Option<StatsLevel> {
    levels.iter().find(|(i, _, _)| *i == idx).map(|(_, _, level)| *level)
}

// Structure for resolved archetype data (after looking up subtype)
struct ArchetypeData {
    game_plans: Vec<String>,
    win_conditions: Vec<String>,
    loss_reasons: Vec<String>,
    board_plan: Option<BoardPlan>,
}

/// Parse deck name to extract archetype and optional subtype
/// Examples:
///   "Doomsday: Tempo (tempo-doomsday-wasteland-1.0)" -> ("Doomsday", Some("Tempo"))
///   "Reanimator: UB" -> ("Reanimator", Some("UB"))
///   "Lands" -> ("Lands", None)
fn parse_deck_name(deck_name: &str) -> (&str, Option<&str>) {
    // First check if there's a list name in parentheses
    let name_without_list = if let Some(pos) = deck_name.find(" (") {
        &deck_name[..pos]
    } else {
        deck_name
    };

    // Now parse archetype and subtype
    if let Some((archetype, subtype)) = name_without_list.split_once(": ") {
        (archetype, Some(subtype))
    } else {
        (name_without_list, None)
    }
}

/// Convert archetype name to filename
fn archetype_to_filename(archetype: &str) -> String {
    archetype
        .to_lowercase()
        .replace(" ", "-")
        .replace("!", "")
        + ".toml"
}

/// Load configuration from config.toml
fn load_config() -> Config {
    let path = Path::new("config.toml");
    if let Ok(content) = fs::read_to_string(path) {
        toml::from_str::<Config>(&content).unwrap_or_default()
    } else {
        Config::default()
    }
}

/// Load archetype data for a specific deck name
/// Handles both standalone archetypes and subtypes
fn load_archetype_data(deck_name: &str) -> Option<ArchetypeData> {
    let (archetype, subtype) = parse_deck_name(deck_name);
    let filename = archetype_to_filename(archetype);

    // Try unified definitions first
    let path = Path::new("definitions").join(&filename);

    if let Ok(content) = fs::read_to_string(&path) {
        if let Ok(unified) = toml::from_str::<UnifiedArchetypeDefinition>(&content) {
            // If there's a subtype, look it up
            if let Some(subtype_name) = subtype {
                if let Some(subtype_def) = unified.subtypes.get(subtype_name) {
                    return Some(ArchetypeData {
                        game_plans: subtype_def.game_plans.clone(),
                        win_conditions: subtype_def.win_conditions.clone(),
                        loss_reasons: subtype_def.loss_reasons.clone(),
                        board_plan: subtype_def.board_plan.clone(),
                    });
                }
            }

            // No subtype or subtype not found, use base archetype data
            return Some(ArchetypeData {
                game_plans: unified.game_plans,
                win_conditions: unified.win_conditions,
                loss_reasons: unified.loss_reasons,
                board_plan: unified.board_plan,
            });
        }
    }

    None
}

/// Load all archetype names from definitions/
fn load_archetypes() -> Vec<String> {
    let mut archetypes = Vec::new();

    if let Ok(entries) = fs::read_dir("definitions") {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("toml") {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(unified) = toml::from_str::<UnifiedArchetypeDefinition>(&content) {
                        archetypes.push(unified.name);
                    }
                }
            }
        }
    }

    // Sort alphabetically, but keep "Other" at the end
    archetypes.sort();
    if let Some(pos) = archetypes.iter().position(|name| name == "Other") {
        let other = archetypes.remove(pos);
        archetypes.push(other);
    }

    archetypes
}

/// Load subtypes for a given archetype
fn load_subtypes(archetype: &str) -> Vec<String> {
    let filename = archetype_to_filename(archetype);
    let path = Path::new("definitions").join(&filename);

    if let Ok(content) = fs::read_to_string(&path) {
        if let Ok(unified) = toml::from_str::<UnifiedArchetypeDefinition>(&content) {
            let mut subtypes: Vec<String> = unified.subtypes.keys().cloned().collect();
            subtypes.sort();
            return subtypes;
        }
    }

    Vec::new()
}

/// Load lists for a given archetype and subtype
fn load_lists(archetype: &str, subtype: &str) -> Vec<String> {
    let filename = archetype_to_filename(archetype);
    let path = Path::new("definitions").join(&filename);

    if let Ok(content) = fs::read_to_string(&path) {
        if let Ok(unified) = toml::from_str::<UnifiedArchetypeDefinition>(&content) {
            if let Some(subtype_def) = unified.subtypes.get(subtype) {
                let mut lists: Vec<String> = subtype_def.lists.keys().cloned().collect();
                lists.sort();
                return lists;
            }
        }
    }

    Vec::new()
}

/// Legacy function for backward compatibility - loads historical deck names
fn load_deck_names() -> Vec<String> {
    let mut deck_names = Vec::new();

    // If no archetypes found, fall back to definitions.md
    if deck_names.is_empty() {
        match fs::read_to_string("definitions.md") {
            Ok(content) => {
                let mut in_decks_section = false;
                deck_names = content.lines()
                    .filter_map(|line| {
                        let line = line.trim();

                        if line.starts_with("## Decks") {
                            in_decks_section = true;
                            return None;
                        }

                        if line.starts_with("##") && !line.starts_with("## Decks") {
                            in_decks_section = false;
                            return None;
                        }

                        if !in_decks_section || line.is_empty() {
                            return None;
                        }

                        if let Some((deck_name, _category)) = line.split_once(';') {
                            Some(deck_name.trim().to_string())
                        } else {
                            Some(line.to_string())
                        }
                    })
                    .collect();
            },
            Err(_) => {
                // Final fallback to hardcoded list
                deck_names = vec![
                    "Reanimator: UB".to_string(),
                    "Reanimator: BR".to_string(),
                    "Stompy: Moon".to_string(),
                    "Stompy: Eldrazi".to_string(),
                    "Tempo: UB".to_string(),
                    "Tempo: UR".to_string(),
                    "Lands".to_string(),
                    "Omni-tell".to_string(),
                    "Sneak and Show".to_string(),
                    "Painter: R".to_string(),
                    "Painter: U".to_string(),
                    "Mystic Forge".to_string(),
                    "Oops! All Spells".to_string(),
                    "Cephalid Breakfast".to_string(),
                    "Doomsday".to_string(),
                    "Nadu: Midrange".to_string(),
                    "Nadu: Elves".to_string(),
                    "Beanstalk: BUG".to_string(),
                    "Beanstalk: Domain".to_string(),
                    "Beanstalk: Yorion".to_string(),
                    "Storm: TES".to_string(),
                    "Storm: ANT".to_string(),
                    "Storm: Ruby".to_string(),
                    "Storm: Black Saga".to_string(),
                    "Goblins".to_string(),
                    "Combo Elves".to_string(),
                    "Cradle Control".to_string(),
                    "Dredge".to_string(),
                    "Maverick: GW".to_string(),
                    "Stiflenaught".to_string(),
                    "Stoneblade".to_string(),
                    "Miracles".to_string(),
                    "Infect".to_string(),
                    "Merfolk".to_string(),
                    "Cloudpost".to_string(),
                    "Other".to_string(),
                ];
            }
        }
    }

    deck_names
}

fn load_deck_categories() -> HashMap<String, DeckCategory> {
    let mut categories = HashMap::new();

    // Try unified definitions directory first
    if let Ok(entries) = fs::read_dir("definitions") {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("toml") {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(unified) = toml::from_str::<UnifiedArchetypeDefinition>(&content) {
                        let category = match unified.category.as_str() {
                            "Blue" => DeckCategory::Blue,
                            "Combo" => DeckCategory::Combo,
                            "Non-Blue" => DeckCategory::NonBlue,
                            _ => DeckCategory::Other,
                        };

                        // Add category for each subtype variant
                        if !unified.subtypes.is_empty() {
                            for subtype_name in unified.subtypes.keys() {
                                let deck_name = format!("{}: {}", unified.name, subtype_name);
                                categories.insert(deck_name, category.clone());
                            }
                        } else {
                            // No subtypes, just add the archetype
                            categories.insert(unified.name, category);
                        }
                    }
                }
            }
        }
    }

    // If no archetypes found, fall back to definitions.md
    if categories.is_empty() {
        match fs::read_to_string("definitions.md") {
            Ok(content) => {
                let mut in_decks_section = false;
                for line in content.lines() {
                    let line = line.trim();

                    if line.starts_with("## Decks") {
                        in_decks_section = true;
                        continue;
                    }

                    if line.starts_with("##") && !line.starts_with("## Decks") {
                        in_decks_section = false;
                        continue;
                    }

                    if !in_decks_section || line.is_empty() {
                        continue;
                    }

                    if let Some((deck_name, category_str)) = line.split_once(';') {
                        let deck_name = deck_name.trim().to_string();
                        let category_str = category_str.trim();

                        let category = match category_str {
                            "Blue" => DeckCategory::Blue,
                            "Combo" => DeckCategory::Combo,
                            "Non-Blue" => DeckCategory::NonBlue,
                            _ => DeckCategory::Other,
                        };

                        categories.insert(deck_name, category);
                    }
                }
            },
            Err(_) => {
                // Fallback categories if file doesn't exist - empty map will use the hardcoded categorize_deck function
            }
        }
    }

    categories
}

fn load_game_plans() -> Vec<String> {
    match fs::read_to_string("definitions.md") {
        Ok(content) => {
            let mut in_game_plans_section = false;
            content.lines()
                .filter_map(|line| {
                    let line = line.trim();
                    
                    if line.starts_with("## Game Plans") {
                        in_game_plans_section = true;
                        return None;
                    }
                    
                    if line.starts_with("##") && !line.starts_with("## Game Plans") {
                        in_game_plans_section = false;
                        return None;
                    }
                    
                    if !in_game_plans_section || line.is_empty() {
                        return None;
                    }
                    
                    Some(line.to_string())
                })
                .collect()
        },
        Err(_) => {
            vec![
                "combo".to_string(),
                "aggro".to_string(),
                "control".to_string(),
                "midrange".to_string(),
            ]
        }
    }
}

fn load_win_conditions() -> Vec<String> {
    match fs::read_to_string("definitions.md") {
        Ok(content) => {
            let mut in_win_cons_section = false;
            content.lines()
                .filter_map(|line| {
                    let line = line.trim();
                    
                    if line.starts_with("## Win Cons") {
                        in_win_cons_section = true;
                        return None;
                    }
                    
                    if line.starts_with("##") && !line.starts_with("## Win Cons") {
                        in_win_cons_section = false;
                        return None;
                    }
                    
                    if !in_win_cons_section || line.is_empty() {
                        return None;
                    }
                    
                    Some(line.to_string())
                })
                .collect()
        },
        Err(_) => {
            vec![
                "damage".to_string(),
                "combo".to_string(),
                "mill".to_string(),
                "concede".to_string(),
            ]
        }
    }
}

fn load_your_deck_names() -> Vec<String> {
    let connection = &mut establish_connection();
    
    // Get deck names from match history
    let historical_names: Result<Vec<String>, _> = matches::table
        .select(matches::deck_name)
        .distinct()
        .order(matches::deck_name.asc())
        .load(connection);
    
    // Get deck names from imported decks
    let imported_names: Result<Vec<String>, _> = crate::db::schema::decks::table
        .select(crate::db::schema::decks::name)
        .order(crate::db::schema::decks::name.asc())
        .load(connection);
    
    let mut all_names = Vec::new();
    
    // Add historical names
    if let Ok(names) = historical_names {
        all_names.extend(names);
    }
    
    // Add imported names (if not already present)
    if let Ok(names) = imported_names {
        for name in names {
            if !all_names.contains(&name) {
                all_names.push(name);
            }
        }
    }
    
    // Sort the combined list
    all_names.sort();
    all_names
}

fn load_opponent_names() -> Vec<String> {
    let connection = &mut establish_connection();

    // Load all unique opponent names ordered by most recent match
    let opponent_names: Result<Vec<String>, _> = matches::table
        .select(matches::opponent_name)
        .distinct()
        .order(matches::created_at.desc())
        .load(connection);

    match opponent_names {
        Ok(names) => names,
        Err(_) => vec![], // Return empty vec if query fails
    }
}

fn load_opponent_deck_names() -> Vec<String> {
    let connection = &mut establish_connection();

    // Load all unique opponent deck names ordered by most recent match
    let opponent_decks: Result<Vec<String>, _> = matches::table
        .select(matches::opponent_deck)
        .filter(matches::opponent_deck.ne("unknown"))
        .distinct()
        .order(matches::created_at.desc())
        .load(connection);

    match opponent_decks {
        Ok(decks) => decks,
        Err(_) => vec![], // Return empty vec if query fails
    }
}

fn load_loss_reasons() -> Vec<String> {
    let connection = &mut establish_connection();

    let reasons: Result<Vec<Option<String>>, _> = games::table
        .select(games::loss_reason)
        .filter(games::loss_reason.is_not_null())
        .distinct()
        .load(connection);

    match reasons {
        Ok(reasons) => reasons.into_iter().filter_map(|r| r).collect(),
        Err(_) => vec![],
    }
}

const EVENT_TYPES: &[&str] = &[
    "League", "Paper", "Casual", "Challenge", "Prelim", "Other"
];

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DeckCategory {
    Blue,
    Combo,
    NonBlue,
    Other,
}

impl DeckCategory {
    pub fn to_string(&self) -> &'static str {
        match self {
            DeckCategory::Blue => "Blue",
            DeckCategory::Combo => "Combo", 
            DeckCategory::NonBlue => "Non-Blue",
            DeckCategory::Other => "Other",
        }
    }
}

pub fn categorize_deck(deck_name: &str) -> DeckCategory {
    let categories = load_deck_categories();

    // First try to get from file
    if let Some(category) = categories.get(deck_name) {
        return category.clone();
    }

    DeckCategory::Other
}

#[derive(Args)]
pub struct GameArgs {
    #[command(subcommand)]
    command: GameCommands,
}

#[derive(Subcommand)]
enum GameCommands {
    AddMatch {
        #[arg(long, help = "Date in YYYY-MM-DD format (default: today)")]
        date: Option<String>,
    },
    ListMatches {
        #[arg(long, default_value = "10", help = "Number of recent matches to show")]
        limit: i64,
    },
    MatchDetails {
        #[arg(help = "Match ID to show details for")]
        match_id: i32,
    },
    EditMatch {
        #[arg(help = "Match ID to edit")]
        match_id: i32,
    },
    EditGame {
        #[arg(help = "Match ID containing the game")]
        match_id: i32,
        #[arg(help = "Game number (1, 2, or 3)")]
        game_number: i32,
    },
    AddDeck {
        #[arg(help = "Deck name to add to the deck list")]
        deck_name: Option<String>,
    },
    BoardPlan {
        #[arg(help = "Opponent deck to show board plan for")]
        deck_name: Option<String>,
    },
    RemoveMatch {
        #[arg(help = "Match ID to remove")]
        match_id: i32,
    },
    Stats {
        #[arg(long, help = "Skip interactive prompts and use config defaults")]
        defaults: bool,
    },
    HtmlStats {
        #[arg(long, short, default_value = "stats.html", help = "Output HTML file path")]
        output: String,
        #[arg(long, help = "Filter by era(s): single (e.g., '2'), multiple (e.g., '1,2,3'), or 'all'. Defaults to latest era")]
        era: Option<String>,
        #[arg(long, help = "Filter by your deck name (partial match)")]
        my_deck: Option<String>,
        #[arg(long, help = "Filter by opponent name")]
        opponent: Option<String>,
        #[arg(long, help = "Filter by opponent deck name (partial match)")]
        opponent_deck: Option<String>,
        #[arg(long, help = "Filter by event type (League, Challenge, Prelim, Casual)")]
        event_type: Option<String>,
    },
    ReconcileDeck {
        #[arg(help = "Deck archetype name (e.g., 'Doomsday')")]
        deck_name: String,
    },
}

pub fn run(args: GameArgs) {
    match args.command {
        GameCommands::AddMatch { date } => add_match_interactive(date),
        GameCommands::ListMatches { limit } => list_matches(limit),
        GameCommands::MatchDetails { match_id } => show_match_details(match_id),
        GameCommands::EditMatch { match_id } => edit_match_interactive(match_id),
        GameCommands::EditGame { match_id, game_number } => edit_game_interactive(match_id, game_number),
        GameCommands::AddDeck { deck_name } => add_deck_to_list(deck_name),
        GameCommands::BoardPlan { deck_name } => show_board_plan(deck_name),
        GameCommands::RemoveMatch { match_id } => remove_match_interactive(match_id),
        GameCommands::Stats { defaults } => show_stats_interactive(defaults),
        GameCommands::HtmlStats { output, era, my_deck, opponent, opponent_deck, event_type } => {
            crate::html_stats::generate_html_stats(&output, era, my_deck, opponent, opponent_deck, event_type)
        },
        GameCommands::ReconcileDeck { deck_name } => reconcile_deck(&deck_name),
    }
}

/// Three-step deck selection: Archetype -> Subtype -> List
fn select_deck_three_step(config: &Config) -> String {
    // Step 1: Select Archetype
    let archetypes = load_archetypes();

    if archetypes.is_empty() {
        // No archetypes found, fall back to text input
        return Input::new()
            .with_prompt("Your deck name")
            .interact_text()
            .unwrap();
    }

    // Determine default archetype index
    let mut default_archetype_idx = 0;
    if let Some(default_archetype) = &config.game_entry.default_archetype {
        if let Some(idx) = archetypes.iter().position(|a| a == default_archetype) {
            default_archetype_idx = idx;
        }
    }

    let archetype_idx = FuzzySelect::new()
        .with_prompt("Select archetype")
        .items(&archetypes)
        .default(default_archetype_idx)
        .interact()
        .unwrap();

    let selected_archetype = &archetypes[archetype_idx];

    // Step 2: Select Subtype
    let subtypes = load_subtypes(selected_archetype);

    if subtypes.is_empty() {
        // No subtypes, return just the archetype name
        return selected_archetype.clone();
    }

    // Determine default subtype index
    let mut default_subtype_idx = 0;
    if let Some(default_subtype) = &config.game_entry.default_subtype {
        if let Some(idx) = subtypes.iter().position(|s| s == default_subtype) {
            default_subtype_idx = idx;
        }
    }

    let subtype_idx = FuzzySelect::new()
        .with_prompt("Select subtype")
        .items(&subtypes)
        .default(default_subtype_idx)
        .interact()
        .unwrap();

    let selected_subtype = &subtypes[subtype_idx];

    // Step 3: Select List
    let lists = load_lists(selected_archetype, selected_subtype);

    if lists.is_empty() {
        // No lists defined, return archetype: subtype format
        return format!("{}: {}", selected_archetype, selected_subtype);
    }

    // Determine default list index
    let mut default_list_idx = 0;
    if let Some(default_list) = &config.game_entry.default_list {
        if let Some(idx) = lists.iter().position(|l| l == default_list) {
            default_list_idx = idx;
        }
    }

    let list_idx = FuzzySelect::new()
        .with_prompt("Select list")
        .items(&lists)
        .default(default_list_idx)
        .interact()
        .unwrap();

    let selected_list = &lists[list_idx];

    // Return the full deck name: "archetype: subtype (list)"
    format!("{}: {} ({})", selected_archetype, selected_subtype, selected_list)
}

fn add_match_interactive(date_arg: Option<String>) {
    println!("=== Adding New Match ===");

    // Load configuration
    let config = load_config();

    // Get date
    let date = if let Some(d) = date_arg {
        match NaiveDate::parse_from_str(&d, "%Y-%m-%d") {
            Ok(parsed_date) => parsed_date.format("%Y-%m-%d").to_string(),
            Err(_) => {
                eprintln!("Invalid date format. Use YYYY-MM-DD");
                return;
            }
        }
    } else {
        Local::now().format("%Y-%m-%d").to_string()
    };

    println!("Date: {}", date);

    // Three-step deck selection: Archetype -> Subtype -> List
    let deck_name = select_deck_three_step(&config);

    println!("Selected deck: {}", deck_name);
    
    // Get opponent name with fuzzy select from all opponents
    let opponents = load_opponent_names();
    let opponent_name = if opponents.is_empty() {
        // No opponent history, use text input
        Input::new()
            .with_prompt("Opponent name")
            .interact_text()
            .unwrap()
    } else {
        // Add option for custom opponent entry
        let mut opponent_options = opponents.clone();
        opponent_options.push("Custom (type new opponent)".to_string());

        let opponent_idx = FuzzySelect::new()
            .with_prompt("Opponent name (type to search)")
            .items(&opponent_options)
            .default(0)
            .interact()
            .unwrap();

        if opponent_idx == opponent_options.len() - 1 {
            // Custom option selected
            Input::new()
                .with_prompt("Enter opponent name")
                .interact_text()
                .unwrap()
        } else {
            opponents[opponent_idx].clone()
        }
    };
    
    // Opponent deck will be set after the match
    
    // Get event type
    let event_type_idx = FuzzySelect::new()
        .with_prompt("Event type")
        .items(EVENT_TYPES)
        .default(0)
        .interact()
        .unwrap();
    let event_type = EVENT_TYPES[event_type_idx].to_string();
    
    // Get die roll winner
    let die_roll_winner = if Confirm::new()
        .with_prompt("Did you win the die roll?")
        .interact()
        .unwrap()
    {
        Winner::Me
    } else {
        Winner::Opponent
    };

    let connection = &mut establish_connection();

    // Get current era (eras are time periods, independent of deck choice)
    // Use config default if set, otherwise auto-detect from database
    let era = config.game_entry.default_era
        .or_else(|| get_current_era(connection));

    // Create the match without winner and opponent deck (will be determined after games)
    let new_match = NewMatch {
        date,
        deck_name: deck_name.clone(),
        opponent_name,
        opponent_deck: "unknown".to_string(), // Will be updated after match
        event_type,
        die_roll_winner: die_roll_winner.to_string(),
        match_winner: "unknown".to_string(), // Will be updated after games
        era,
    };

    diesel::insert_into(matches::table)
        .values(&new_match)
        .execute(connection)
        .expect("Error saving new match");

    // Get the most recent match for this combination (should be the one we just inserted)
    let match_id: i32 = matches::table
        .select(matches::match_id)
        .order(matches::match_id.desc())
        .first(connection)
        .expect("Error getting match ID");

    println!("\nMatch created with ID: {}", match_id);

    // Now add games and determine match winner
    let match_winner = add_games_interactive(connection, match_id, &deck_name);
    
    // Check if opponent deck is still unknown after all games
    let current_match = matches::table
        .find(match_id)
        .first::<Match>(connection)
        .expect("Error loading current match");
        
    if current_match.opponent_deck == "unknown" {
        println!("\n=== Match Complete ===");
        let deck_names = load_deck_names();
        let deck_names_refs: Vec<&str> = deck_names.iter().map(|s| s.as_str()).collect();
        let opponent_deck_idx = FuzzySelect::new()
            .with_prompt("What deck was your opponent playing?")
            .items(&deck_names_refs)
            .default(0)
            .interact()
            .unwrap();
        let opponent_deck = deck_names[opponent_deck_idx].clone();

        // Update the match with the winner and opponent deck
        diesel::update(matches::table.find(match_id))
            .set((
                matches::match_winner.eq(match_winner.to_string()),
                matches::opponent_deck.eq(opponent_deck)
            ))
            .execute(connection)
            .expect("Error updating match");
    } else {
        // Just update the match winner
        diesel::update(matches::table.find(match_id))
            .set(matches::match_winner.eq(match_winner.to_string()))
            .execute(connection)
            .expect("Error updating match winner");
    }
}

fn add_games_interactive(connection: &mut SqliteConnection, match_id: i32, deck_name: &str) -> Winner {
    println!("\n=== Adding Games (Best of 3) ===");

    // Load archetype-specific definitions, or fall back to global definitions
    let archetype = load_archetype_data(deck_name);

    let mut my_wins = 0;
    let mut opponent_wins = 0;

    for game_num in 1..=3 {
        println!("\n--- Game {} ---", game_num);

        // Play or draw
        let play_draw = if Confirm::new()
            .with_prompt("Did you play first? (no = draw)")
            .interact()
            .unwrap()
        {
            PlayDraw::Play
        } else {
            PlayDraw::Draw
        };

        // Mulligans
        let mulligans: i32 = Input::new()
            .with_prompt("Number of mulligans (0-7)")
            .validate_with(|input: &i32| -> Result<(), &str> {
                if *input >= 0 && *input <= 7 {
                    Ok(())
                } else {
                    Err("Mulligans must be between 0 and 7")
                }
            })
            .interact_text()
            .unwrap();

        // Opening hand plan - use archetype-specific or global
        let game_plans = if let Some(ref arch) = archetype {
            arch.game_plans.clone()
        } else {
            load_game_plans()
        };
        let game_plans_refs: Vec<&str> = game_plans.iter().map(|s| s.as_str()).collect();
        let mut game_plans_with_custom = game_plans_refs.clone();
        game_plans_with_custom.push("Custom (type your own)");

        let plan_idx = FuzzySelect::new()
            .with_prompt("Opening hand plan")
            .items(&game_plans_with_custom)
            .default(0)
            .interact()
            .unwrap();

        let opening_hand_plan = if plan_idx == game_plans_with_custom.len() - 1 {
            // Custom option selected
            let custom_plan: String = Input::new()
                .with_prompt("Enter custom game plan")
                .allow_empty(true)
                .interact_text()
                .unwrap();
            if custom_plan.is_empty() { None } else { Some(custom_plan) }
        } else {
            Some(game_plans[plan_idx].clone())
        };
        
        
        // Game winner
        let game_winner = if Confirm::new()
            .with_prompt("Did you win this game?")
            .interact()
            .unwrap()
        {
            my_wins += 1;
            Winner::Me
        } else {
            opponent_wins += 1;
            Winner::Opponent
        };


        // Win condition (only if you won) - use archetype-specific or global
        let win_condition = if matches!(game_winner, Winner::Me) {
            let win_cons = if let Some(ref arch) = archetype {
                arch.win_conditions.clone()
            } else {
                load_win_conditions()
            };
            let win_cons_refs: Vec<&str> = win_cons.iter().map(|s| s.as_str()).collect();
            let mut win_cons_with_custom = win_cons_refs.clone();
            win_cons_with_custom.push("Custom (type your own)");

            let win_idx = FuzzySelect::new()
                .with_prompt("What did you win with?")
                .items(&win_cons_with_custom)
                .default(0)
                .interact()
                .unwrap();

            if win_idx == win_cons_with_custom.len() - 1 {
                // Custom option selected
                let custom_win: String = Input::new()
                    .with_prompt("Enter custom win condition")
                    .allow_empty(true)
                    .interact_text()
                    .unwrap();
                if custom_win.is_empty() { None } else { Some(custom_win) }
            } else {
                Some(win_cons[win_idx].clone())
            }
        } else {
            None
        };

        // Loss reason (only if you lost) - use archetype-specific
        let loss_reason = if matches!(game_winner, Winner::Opponent) {
            if let Some(ref arch) = archetype {
                if !arch.loss_reasons.is_empty() {
                    let loss_reasons_refs: Vec<&str> = arch.loss_reasons.iter().map(|s| s.as_str()).collect();
                    let mut loss_reasons_with_custom = loss_reasons_refs.clone();
                    loss_reasons_with_custom.push("Custom (type your own)");

                    let loss_idx = FuzzySelect::new()
                        .with_prompt("Why did you lose?")
                        .items(&loss_reasons_with_custom)
                        .default(0)
                        .interact()
                        .unwrap();

                    if loss_idx == loss_reasons_with_custom.len() - 1 {
                        // Custom option selected
                        let custom_loss: String = Input::new()
                            .with_prompt("Enter custom loss reason")
                            .allow_empty(true)
                            .interact_text()
                            .unwrap();
                        if custom_loss.is_empty() { None } else { Some(custom_loss) }
                    } else {
                        Some(arch.loss_reasons[loss_idx].clone())
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        // Number of turns
        let turns: Option<i32> = Input::new()
            .with_prompt("How many turns did the game last? (optional, press Enter to skip)")
            .allow_empty(true)
            .validate_with(|input: &String| -> Result<(), &str> {
                if input.is_empty() {
                    return Ok(());
                }
                match input.parse::<i32>() {
                    Ok(n) if n > 0 => Ok(()),
                    _ => Err("Turns must be a positive number")
                }
            })
            .interact_text()
            .ok()
            .and_then(|s| if s.is_empty() { None } else { s.parse().ok() });
        
        // Save the game
        let new_game = NewGame {
            match_id,
            game_number: game_num,
            play_draw: play_draw.to_string(),
            mulligans,
            opening_hand_plan,
            game_winner: game_winner.to_string(),
            win_condition,
            loss_reason,
            turns,
        };
        
        diesel::insert_into(games::table)
            .values(&new_game)
            .execute(connection)
            .expect("Error saving new game");
        
        println!("Game {} saved", game_num);
        println!("Current score: You {}-{} Opponent", my_wins, opponent_wins);
        
        // Check if we know the opponent's deck yet
        let current_match = matches::table
            .find(match_id)
            .first::<Match>(connection)
            .expect("Error loading current match");
            
        if current_match.opponent_deck == "unknown" {
            let knows_deck = Confirm::new()
                .with_prompt("Do you know what deck your opponent is playing yet?")
                .interact()
                .unwrap();
                
            if knows_deck {
                let deck_names = load_deck_names();
                let deck_names_refs: Vec<&str> = deck_names.iter().map(|s| s.as_str()).collect();
                let opponent_deck_idx = FuzzySelect::new()
                    .with_prompt("What deck is your opponent playing?")
                    .items(&deck_names_refs)
                    .default(0)
                    .interact()
                    .unwrap();
                let opponent_deck = deck_names[opponent_deck_idx].clone();
                
                // Update the match with the opponent deck
                diesel::update(matches::table.find(match_id))
                    .set(matches::opponent_deck.eq(&opponent_deck))
                    .execute(connection)
                    .expect("Error updating opponent deck");
                    
                println!("Updated opponent deck to: {}", opponent_deck);
            }
        }
        
        // Check if match is decided (first to 2 wins)
        if my_wins == 2 {
            println!("\n🎉 You won the match 2-{}!", opponent_wins);
            return Winner::Me;
        } else if opponent_wins == 2 {
            println!("\n😞 You lost the match {}-2", my_wins);
            return Winner::Opponent;
        }
    }
    
    // This shouldn't happen in best of 3, but just in case
    if my_wins > opponent_wins {
        Winner::Me
    } else {
        Winner::Opponent
    }
}

fn list_matches(limit: i64) {
    let connection = &mut establish_connection();
    
    let results = matches::table
        .order((matches::date.desc(), matches::match_id.desc()))
        .limit(limit)
        .load::<Match>(connection)
        .expect("Error loading matches");
    
    if results.is_empty() {
        println!("No matches found");
        return;
    }
    
    println!("=== Recent Matches ===");
    println!("{:<4} {:<12} {:<15} {:<15} {:<12} {:<8} {:<8}", 
             "ID", "Date", "Deck", "Opponent", "Opp Deck", "Event", "Result");
    println!("{}", "-".repeat(80));
    
    for m in results {
        let result = if m.match_winner == "me" { "W" } else { "L" };
        println!("{:<4} {:<12} {:<15} {:<15} {:<12} {:<8} {:<8}", 
                 m.match_id, m.date, 
                 truncate(&m.deck_name, 15),
                 truncate(&m.opponent_name, 15),
                 truncate(&m.opponent_deck, 12),
                 truncate(&m.event_type, 8),
                 result);
    }
}

fn show_match_details(match_id: i32) {
    let connection = &mut establish_connection();
    
    let match_result = matches::table
        .find(match_id)
        .first::<Match>(connection);
    
    let match_data = match match_result {
        Ok(m) => m,
        Err(_) => {
            println!("Match {} not found", match_id);
            return;
        }
    };
    
    let games_result = games::table
        .filter(games::match_id.eq(match_id))
        .order(games::game_number.asc())
        .load::<Game>(connection)
        .expect("Error loading games");
    
    println!("=== Match {} Details ===", match_id);
    println!("Date: {}", match_data.date);
    println!("Your deck: {}", match_data.deck_name);
    println!("Opponent: {} ({})", match_data.opponent_name, match_data.opponent_deck);
    println!("Event: {}", match_data.event_type);
    println!("Die roll winner: {}", match_data.die_roll_winner);
    println!("Match winner: {}", match_data.match_winner);
    
    println!("\n=== Games ===");
    for game in games_result {
        println!("\nGame {}:", game.game_number);
        println!("  Play/Draw: {}", game.play_draw);
        println!("  Mulligans: {}", game.mulligans);
        if let Some(plan) = &game.opening_hand_plan {
            println!("  Opening plan: {}", plan);
        }
        println!("  Winner: {}", game.game_winner);
        if let Some(condition) = &game.win_condition {
            println!("  Win condition: {}", condition);
        }
        if let Some(reason) = &game.loss_reason {
            println!("  Loss reason: {}", reason);
        }
        if let Some(turns) = &game.turns {
            println!("  Turns: {}", turns);
        }
    }
}

enum EraFilter {
    All,
    Eras(Vec<i32>),
}


fn get_default_era_filter(connection: &mut SqliteConnection) -> EraFilter {
    // Get the maximum era from the matches table
    let max_era: Option<Option<i32>> = matches::table
        .select(diesel::dsl::max(matches::era))
        .first(connection)
        .ok();

    match max_era.flatten() {
        Some(era) => EraFilter::Eras(vec![era]),
        None => EraFilter::All, // If no era data, show all
    }
}

/// Calculate statistics from specific games (for game-specific groupings)
fn calculate_stats_from_games(label: String, games: Vec<&Game>, all_matches: &[Match]) -> StatsRow {
    // Get unique matches that contain these games
    let match_ids: std::collections::HashSet<i32> = games.iter().map(|g| g.match_id).collect();
    let matches: Vec<&Match> = all_matches.iter().filter(|m| match_ids.contains(&m.match_id)).collect();

    let match_count = matches.len();
    let match_wins = matches.iter().filter(|m| m.match_winner == "me").count();
    let match_losses = match_count - match_wins;
    let match_win_rate = if match_count > 0 { (match_wins as f64 / match_count as f64) * 100.0 } else { 0.0 };

    // Use only the specific games passed in
    let game_count = games.len();
    let game_wins = games.iter().filter(|g| g.game_winner == "me").count();
    let game_losses = game_count - game_wins;
    let game_win_rate = if game_count > 0 { (game_wins as f64 / game_count as f64) * 100.0 } else { 0.0 };

    // Mulligan statistics
    let winning_games: Vec<&Game> = games.iter().filter(|g| g.game_winner == "me").copied().collect();
    let losing_games: Vec<&Game> = games.iter().filter(|g| g.game_winner == "opponent").copied().collect();

    let total_mulligans: i32 = games.iter().map(|g| g.mulligans).sum();
    let avg_mulligans = if !games.is_empty() { total_mulligans as f64 / games.len() as f64 } else { 0.0 };

    let win_mulligans: i32 = winning_games.iter().map(|g| g.mulligans).sum();
    let avg_win_mulligans = if !winning_games.is_empty() { win_mulligans as f64 / winning_games.len() as f64 } else { 0.0 };

    let loss_mulligans: i32 = losing_games.iter().map(|g| g.mulligans).sum();
    let avg_loss_mulligans = if !losing_games.is_empty() { loss_mulligans as f64 / losing_games.len() as f64 } else { 0.0 };

    // Game length statistics
    let games_with_turns: Vec<&Game> = games.iter().filter(|g| g.turns.is_some()).copied().collect();
    let avg_game_length = if !games_with_turns.is_empty() {
        let total_turns: i32 = games_with_turns.iter().map(|g| g.turns.unwrap()).sum();
        Some(total_turns as f64 / games_with_turns.len() as f64)
    } else {
        None
    };

    let win_games_with_turns: Vec<&Game> = winning_games.iter().filter(|g| g.turns.is_some()).copied().collect();
    let avg_win_length = if !win_games_with_turns.is_empty() {
        let total_turns: i32 = win_games_with_turns.iter().map(|g| g.turns.unwrap()).sum();
        Some(total_turns as f64 / win_games_with_turns.len() as f64)
    } else {
        None
    };

    let loss_games_with_turns: Vec<&Game> = losing_games.iter().filter(|g| g.turns.is_some()).copied().collect();
    let avg_loss_length = if !loss_games_with_turns.is_empty() {
        let total_turns: i32 = loss_games_with_turns.iter().map(|g| g.turns.unwrap()).sum();
        Some(total_turns as f64 / loss_games_with_turns.len() as f64)
    } else {
        None
    };

    // Win/Loss conditions
    let mut win_conditions = HashMap::new();
    for game in winning_games.iter() {
        if let Some(condition) = &game.win_condition {
            *win_conditions.entry(condition.clone()).or_insert(0) += 1;
        }
    }

    let mut loss_conditions = HashMap::new();
    for game in losing_games.iter() {
        if let Some(reason) = &game.loss_reason {
            *loss_conditions.entry(reason.clone()).or_insert(0) += 1;
        }
    }

    StatsRow {
        label,
        match_wins,
        match_losses,
        match_count,
        match_win_rate,
        game_wins,
        game_losses,
        game_count,
        game_win_rate,
        avg_mulligans,
        avg_win_mulligans,
        avg_loss_mulligans,
        avg_game_length,
        avg_win_length,
        avg_loss_length,
        win_conditions,
        loss_conditions,
    }
}

/// Calculate statistics for a group of matches and games
fn calculate_stats(label: String, matches: &[&Match], all_games: &[Game]) -> StatsRow {
    let match_count = matches.len();
    let match_wins = matches.iter().filter(|m| m.match_winner == "me").count();
    let match_losses = match_count - match_wins;
    let match_win_rate = if match_count > 0 { (match_wins as f64 / match_count as f64) * 100.0 } else { 0.0 };

    // Get games for these matches
    let match_ids: Vec<i32> = matches.iter().map(|m| m.match_id).collect();
    let games: Vec<&Game> = all_games.iter().filter(|g| match_ids.contains(&g.match_id)).collect();

    let game_count = games.len();
    let game_wins = games.iter().filter(|g| g.game_winner == "me").count();
    let game_losses = game_count - game_wins;
    let game_win_rate = if game_count > 0 { (game_wins as f64 / game_count as f64) * 100.0 } else { 0.0 };

    // Mulligan statistics
    let winning_games: Vec<&Game> = games.iter().filter(|g| g.game_winner == "me").copied().collect();
    let losing_games: Vec<&Game> = games.iter().filter(|g| g.game_winner == "opponent").copied().collect();

    let total_mulligans: i32 = games.iter().map(|g| g.mulligans).sum();
    let avg_mulligans = if !games.is_empty() { total_mulligans as f64 / games.len() as f64 } else { 0.0 };

    let win_mulligans: i32 = winning_games.iter().map(|g| g.mulligans).sum();
    let avg_win_mulligans = if !winning_games.is_empty() { win_mulligans as f64 / winning_games.len() as f64 } else { 0.0 };

    let loss_mulligans: i32 = losing_games.iter().map(|g| g.mulligans).sum();
    let avg_loss_mulligans = if !losing_games.is_empty() { loss_mulligans as f64 / losing_games.len() as f64 } else { 0.0 };

    // Game length statistics
    let games_with_turns: Vec<&Game> = games.iter().filter(|g| g.turns.is_some()).copied().collect();
    let avg_game_length = if !games_with_turns.is_empty() {
        let total_turns: i32 = games_with_turns.iter().map(|g| g.turns.unwrap()).sum();
        Some(total_turns as f64 / games_with_turns.len() as f64)
    } else {
        None
    };

    let win_games_with_turns: Vec<&Game> = winning_games.iter().filter(|g| g.turns.is_some()).copied().collect();
    let avg_win_length = if !win_games_with_turns.is_empty() {
        let total_turns: i32 = win_games_with_turns.iter().map(|g| g.turns.unwrap()).sum();
        Some(total_turns as f64 / win_games_with_turns.len() as f64)
    } else {
        None
    };

    let loss_games_with_turns: Vec<&Game> = losing_games.iter().filter(|g| g.turns.is_some()).copied().collect();
    let avg_loss_length = if !loss_games_with_turns.is_empty() {
        let total_turns: i32 = loss_games_with_turns.iter().map(|g| g.turns.unwrap()).sum();
        Some(total_turns as f64 / loss_games_with_turns.len() as f64)
    } else {
        None
    };

    // Win/Loss conditions
    let mut win_conditions = HashMap::new();
    for game in winning_games.iter() {
        if let Some(condition) = &game.win_condition {
            *win_conditions.entry(condition.clone()).or_insert(0) += 1;
        }
    }

    let mut loss_conditions = HashMap::new();
    for game in losing_games.iter() {
        if let Some(reason) = &game.loss_reason {
            *loss_conditions.entry(reason.clone()).or_insert(0) += 1;
        }
    }

    StatsRow {
        label,
        match_wins,
        match_losses,
        match_count,
        match_win_rate,
        game_wins,
        game_losses,
        game_count,
        game_win_rate,
        avg_mulligans,
        avg_win_mulligans,
        avg_loss_mulligans,
        avg_game_length,
        avg_win_length,
        avg_loss_length,
        win_conditions,
        loss_conditions,
    }
}

/// Display statistics in a table format with selected columns
fn display_stats_table(rows: &[StatsRow], selected_stats: &[usize], title: &str, is_game_grouping: bool) {
    if rows.is_empty() {
        return;
    }

    let mut table = Table::new();
    table.set_content_arrangement(ContentArrangement::Dynamic);

    // Build header row
    let mut headers = vec![""];
    for &stat_idx in selected_stats {
        match stat_idx {
            0 => headers.push("Match Wins"),
            1 => headers.push("Game Wins"),
            2 => headers.push("Matches"),
            3 => headers.push("Games"),
            4 => headers.push("Mulligans"),
            5 => headers.push("Game Length"),
            6 => headers.push("Win Conditions"),
            7 => headers.push("Loss Conditions"),
            8 => headers.push("Proportion"),
            _ => {}
        }
    }

    table.set_header(headers);

    // Calculate total count for proportion calculation
    let total_count: usize = if is_game_grouping {
        rows.iter().map(|r| r.game_count).sum()
    } else {
        rows.iter().map(|r| r.match_count).sum()
    };

    // Build data rows
    for row in rows {
        let mut cells = vec![Cell::new(&row.label).add_attribute(Attribute::Bold)];

        for &stat_idx in selected_stats {
            let cell_content = match stat_idx {
                0 => {
                    // Match Win Rate
                    format!("{:.1}% ({}-{})", row.match_win_rate, row.match_wins, row.match_losses)
                },
                1 => {
                    // Game Win Rate
                    format!("{:.1}% ({}-{})", row.game_win_rate, row.game_wins, row.game_losses)
                },
                2 => {
                    // Match Count
                    format!("{} ({}-{})", row.match_count, row.match_wins, row.match_losses)
                },
                3 => {
                    // Game Count
                    format!("{} ({}-{})", row.game_count, row.game_wins, row.game_losses)
                },
                4 => {
                    // Mulligans
                    format!("{:.2} (W:{:.2} L:{:.2})", row.avg_mulligans, row.avg_win_mulligans, row.avg_loss_mulligans)
                },
                5 => {
                    // Game Length
                    if let Some(avg) = row.avg_game_length {
                        let win_str = row.avg_win_length.map(|l| format!("{:.1}", l)).unwrap_or_else(|| "-".to_string());
                        let loss_str = row.avg_loss_length.map(|l| format!("{:.1}", l)).unwrap_or_else(|| "-".to_string());
                        format!("{:.1} (W:{} L:{})", avg, win_str, loss_str)
                    } else {
                        "-".to_string()
                    }
                },
                6 => {
                    // Win Conditions
                    let mut conditions: Vec<_> = row.win_conditions.iter().collect();
                    conditions.sort_by(|a, b| b.1.cmp(a.1));
                    conditions.iter()
                        .map(|(k, v)| format!("{}: {}", k, v))
                        .collect::<Vec<_>>()
                        .join(", ")
                },
                7 => {
                    // Loss Conditions
                    let mut conditions: Vec<_> = row.loss_conditions.iter().collect();
                    conditions.sort_by(|a, b| b.1.cmp(a.1));
                    conditions.iter()
                        .map(|(k, v)| format!("{}: {}", k, v))
                        .collect::<Vec<_>>()
                        .join(", ")
                },
                8 => {
                    // Proportion of total
                    let count = if is_game_grouping { row.game_count } else { row.match_count };
                    if total_count > 0 {
                        let percentage = (count as f64 / total_count as f64) * 100.0;
                        format!("{}/{} ({:.1}%)", count, total_count, percentage)
                    } else {
                        "-".to_string()
                    }
                },
                _ => String::new(),
            };
            cells.push(Cell::new(cell_content));
        }

        table.add_row(cells);
    }

    println!("\n{}", title);
    println!("{}", table);
}

/// Interactive stats with three-step selection: Filters, Group-bys, Statistics
fn show_stats_interactive(use_defaults: bool) {
    let connection = &mut establish_connection();
    let config = load_config();

    println!("=== Match Statistics ===\n");

    // Apply filters from config or prompt interactively
    let mut deck_name_filter: Option<String> = None;
    let mut opponent_name_filter: Option<String> = None;
    let mut opponent_deck_filter: Option<String> = None;
    let mut event_type_filter: Option<String> = None;
    let mut era_values: Option<Vec<i32>> = None;
    let mut loss_reason_filter: Option<String> = None;
    let mut win_condition_filter: Option<String> = None;
    let mut game_plan_filter: Option<String> = None;
    let mut mulligan_count_filter: Option<i32> = None;
    let mut game_length_filter: Option<(i32, i32)> = None; // (min, max)
    let mut game_number_filter: Option<i32> = None;
    let mut play_draw_filter: Option<String> = None;

    if use_defaults {
        // Use config defaults for filters
        if let Some(era) = config.stats.filters.era {
            era_values = Some(vec![era]);
        }
        deck_name_filter = config.stats.filters.my_deck.clone();
        opponent_name_filter = config.stats.filters.opponent.clone();
        opponent_deck_filter = config.stats.filters.opponent_deck.clone();
        event_type_filter = config.stats.filters.event_type.clone();
    } else {
        // Interactive filter selection
        let filter_options = vec![
            "Era (latest only)",
            "Era (all)",
            "My Archetype",
            "My Subtype",
            "My List",
            "Opponent",
            "Opponent Deck",
            "Event Type",
            "Loss Reason",
            "Win Condition",
            "Game Plan",
            "Mulligan Count",
            "Game Length",
            "Game Number",
            "Play/Draw",
        ];

        // Pre-select filters based on config
        let filter_defaults = vec![
            config.stats.filters.era.is_some(),           // Era (latest only)
            false,                                         // Era (all)
            config.stats.filters.my_deck.is_some(),       // My Archetype
            false,                                         // My Subtype (covered by my_deck)
            false,                                         // My List (covered by my_deck)
            config.stats.filters.opponent.is_some(),      // Opponent
            config.stats.filters.opponent_deck.is_some(), // Opponent Deck
            config.stats.filters.event_type.is_some(),    // Event Type
            false,                                         // Loss Reason
            false,                                         // Win Condition
            false,                                         // Game Plan
            false,                                         // Mulligan Count
            false,                                         // Game Length
            false,                                         // Game Number
            false,                                         // Play/Draw
        ];

        let selected_filters = MultiSelect::new()
            .with_prompt("Select filters (space to select, enter to continue)")
            .items(&filter_options)
            .defaults(&filter_defaults)
            .interact()
            .unwrap();

        // Get all available options for fuzzy selection
        let all_decks = load_your_deck_names();
        let all_opponents = load_opponent_names();
        let all_opponent_decks = load_opponent_deck_names();
        let event_types = vec!["League", "Challenge", "Prelim", "Casual"];
        let all_loss_reasons = load_loss_reasons();
        let all_win_conditions = load_win_conditions();
        let all_game_plans = load_game_plans();

        // Extract unique archetypes, subtypes, and lists from deck names
        let mut archetypes = std::collections::HashSet::new();
        let mut subtypes = std::collections::HashSet::new();
        let mut lists = std::collections::HashSet::new();

        for deck in &all_decks {
            let (archetype, subtype) = parse_deck_name(deck);
            archetypes.insert(archetype.to_string());
            if let Some(st) = subtype {
                subtypes.insert(st.to_string());
            }
            // Extract list name if present (format: "Arch: Sub (list)")
            if let Some(list_start) = deck.find(" (") {
                if let Some(list_end) = deck.rfind(')') {
                    let list = &deck[list_start + 2..list_end];
                    lists.insert(list.to_string());
                }
            }
        }

        let archetype_list: Vec<String> = archetypes.into_iter().collect();
        let subtype_list: Vec<String> = subtypes.into_iter().collect();
        let list_list: Vec<String> = lists.into_iter().collect();

        for &filter_idx in &selected_filters {
            match filter_idx {
                0 => {
                    // Era (latest only)
                    match get_default_era_filter(connection) {
                        EraFilter::Eras(eras) => era_values = Some(eras),
                        EraFilter::All => {}
                    }
                }
                1 => {
                    // Era (all) - no filter
                }
                2 => {
                    // My Archetype - fuzzy select
                    if !archetype_list.is_empty() {
                        let default_idx = if let Some(ref default_deck) = config.stats.filters.my_deck {
                            archetype_list.iter().position(|a| default_deck.contains(a)).unwrap_or(0)
                        } else {
                            0
                        };

                        let idx = FuzzySelect::new()
                            .with_prompt("Select archetype to filter by")
                            .items(&archetype_list)
                            .default(default_idx)
                            .interact()
                            .unwrap();
                        // Filter by archetype (partial match on deck name)
                        deck_name_filter = Some(archetype_list[idx].clone());
                    }
                }
                3 => {
                    // My Subtype - fuzzy select
                    if !subtype_list.is_empty() {
                        let idx = FuzzySelect::new()
                            .with_prompt("Select subtype to filter by")
                            .items(&subtype_list)
                            .interact()
                            .unwrap();
                        // Filter by subtype (will match "Archetype: Subtype")
                        deck_name_filter = Some(format!(": {}", subtype_list[idx]));
                    }
                }
                4 => {
                    // My List - fuzzy select
                    if !list_list.is_empty() {
                        let idx = FuzzySelect::new()
                            .with_prompt("Select list to filter by")
                            .items(&list_list)
                            .interact()
                            .unwrap();
                        // Filter by list name (will match "(list)")
                        deck_name_filter = Some(format!("({})", list_list[idx]));
                    }
                }
                5 => {
                    // Opponent - fuzzy select
                    if !all_opponents.is_empty() {
                        let default_idx = if let Some(ref default_opp) = config.stats.filters.opponent {
                            all_opponents.iter().position(|o| o.contains(default_opp)).unwrap_or(0)
                        } else {
                            0
                        };

                        let idx = FuzzySelect::new()
                            .with_prompt("Select opponent to filter by")
                            .items(&all_opponents)
                            .default(default_idx)
                            .interact()
                            .unwrap();
                        opponent_name_filter = Some(all_opponents[idx].clone());
                    }
                }
                6 => {
                    // Opponent Deck - fuzzy select
                    if !all_opponent_decks.is_empty() {
                        let default_idx = if let Some(ref default_opp_deck) = config.stats.filters.opponent_deck {
                            all_opponent_decks.iter().position(|d| d.contains(default_opp_deck)).unwrap_or(0)
                        } else {
                            0
                        };

                        let idx = FuzzySelect::new()
                            .with_prompt("Select opponent deck to filter by")
                            .items(&all_opponent_decks)
                            .default(default_idx)
                            .interact()
                            .unwrap();
                        opponent_deck_filter = Some(all_opponent_decks[idx].clone());
                    }
                }
                7 => {
                    // Event Type - fuzzy select
                    let default_idx = if let Some(ref default_event) = config.stats.filters.event_type {
                        event_types.iter().position(|e| e.contains(default_event.as_str())).unwrap_or(0)
                    } else {
                        0
                    };

                    let idx = FuzzySelect::new()
                        .with_prompt("Select event type to filter by")
                        .items(&event_types)
                        .default(default_idx)
                        .interact()
                        .unwrap();
                    event_type_filter = Some(event_types[idx].to_string());
                }
                8 => {
                    // Loss Reason - fuzzy select
                    if !all_loss_reasons.is_empty() {
                        let idx = FuzzySelect::new()
                            .with_prompt("Select loss reason to filter by")
                            .items(&all_loss_reasons)
                            .interact()
                            .unwrap();
                        loss_reason_filter = Some(all_loss_reasons[idx].clone());
                    }
                }
                9 => {
                    // Win Condition - fuzzy select
                    if !all_win_conditions.is_empty() {
                        let idx = FuzzySelect::new()
                            .with_prompt("Select win condition to filter by")
                            .items(&all_win_conditions)
                            .interact()
                            .unwrap();
                        win_condition_filter = Some(all_win_conditions[idx].clone());
                    }
                }
                10 => {
                    // Game Plan - fuzzy select
                    if !all_game_plans.is_empty() {
                        let idx = FuzzySelect::new()
                            .with_prompt("Select game plan to filter by")
                            .items(&all_game_plans)
                            .interact()
                            .unwrap();
                        game_plan_filter = Some(all_game_plans[idx].clone());
                    }
                }
                11 => {
                    // Mulligan Count - number input
                    let mulligan_options = vec!["0", "1", "2", "3", "4+"];
                    let idx = FuzzySelect::new()
                        .with_prompt("Select mulligan count to filter by")
                        .items(&mulligan_options)
                        .interact()
                        .unwrap();
                    mulligan_count_filter = Some(idx as i32);
                }
                12 => {
                    // Game Length - turn range select
                    let length_options = vec![
                        "Very Short (1-3 turns)",
                        "Short (4-6 turns)",
                        "Medium (7-9 turns)",
                        "Long (10-12 turns)",
                        "Very Long (13+ turns)",
                    ];
                    let idx = FuzzySelect::new()
                        .with_prompt("Select game length to filter by")
                        .items(&length_options)
                        .interact()
                        .unwrap();
                    game_length_filter = Some(match idx {
                        0 => (1, 3),
                        1 => (4, 6),
                        2 => (7, 9),
                        3 => (10, 12),
                        4 => (13, 999),
                        _ => (1, 999),
                    });
                }
                13 => {
                    // Game Number - game 1, 2, or 3
                    let game_options = vec!["Game 1", "Game 2", "Game 3"];
                    let idx = FuzzySelect::new()
                        .with_prompt("Select game number to filter by")
                        .items(&game_options)
                        .interact()
                        .unwrap();
                    game_number_filter = Some((idx + 1) as i32);
                }
                14 => {
                    // Play/Draw
                    let play_draw_options = vec!["On the Play", "On the Draw"];
                    let idx = FuzzySelect::new()
                        .with_prompt("Select play/draw to filter by")
                        .items(&play_draw_options)
                        .interact()
                        .unwrap();
                    play_draw_filter = Some(if idx == 0 { "play".to_string() } else { "draw".to_string() });
                }
                _ => {}
            }
        }
    }

    // Step 2: Select Group-bys
    let selected_groupbys = if use_defaults {
        // Use config defaults
        let mut defaults = Vec::new();
        for default_groupby in &config.stats.default_groupbys {
            match default_groupby.as_str() {
                "my-archetype" => defaults.push(0),
                "my-subtype" => defaults.push(1),
                "my-list" => defaults.push(2),
                "opponent" => defaults.push(3),
                "opponent-deck" => defaults.push(4),
                "opponent-deck-archetype" => defaults.push(5),
                "opponent-deck-category" => defaults.push(6),
                "era" => defaults.push(7),
                "game-number" => defaults.push(8),
                "mulligans" => defaults.push(9),
                "game-plan" => defaults.push(10),
                "win-condition" => defaults.push(11),
                "loss-reason" => defaults.push(12),
                "game-length" => defaults.push(13),
                "play-draw" => defaults.push(14),
                _ => {}
            }
        }
        defaults
    } else {
        let groupby_options = vec![
            "My Archetype",
            "My Subtype",
            "My List",
            "Opponent",
            "Opponent Deck",
            "Opponent Deck Archetype",
            "Opponent Deck Category",
            "Era",
            "Game Number",
            "Mulligan Count",
            "Game Plan",
            "Win Condition",
            "Loss Reason",
            "Game Length",
            "Play/Draw",
        ];

        // Pre-select group-bys based on config
        let groupby_defaults = vec![
            config.stats.default_groupbys.contains(&"my-archetype".to_string()),
            config.stats.default_groupbys.contains(&"my-subtype".to_string()),
            config.stats.default_groupbys.contains(&"my-list".to_string()),
            config.stats.default_groupbys.contains(&"opponent".to_string()),
            config.stats.default_groupbys.contains(&"opponent-deck".to_string()),
            config.stats.default_groupbys.contains(&"opponent-deck-archetype".to_string()),
            config.stats.default_groupbys.contains(&"opponent-deck-category".to_string()),
            config.stats.default_groupbys.contains(&"era".to_string()),
            config.stats.default_groupbys.contains(&"game-number".to_string()),
            config.stats.default_groupbys.contains(&"mulligans".to_string()),
            config.stats.default_groupbys.contains(&"game-plan".to_string()),
            config.stats.default_groupbys.contains(&"win-condition".to_string()),
            config.stats.default_groupbys.contains(&"loss-reason".to_string()),
            config.stats.default_groupbys.contains(&"game-length".to_string()),
            config.stats.default_groupbys.contains(&"play-draw".to_string()),
        ];

        MultiSelect::new()
            .with_prompt("Select group-bys (space to select, enter to continue)")
            .items(&groupby_options)
            .defaults(&groupby_defaults)
            .interact()
            .unwrap()
    };

    // Step 3: Select Statistics
    let selected_stats = if use_defaults {
        // Use config defaults
        let mut defaults = Vec::new();
        for default_stat in &config.stats.default_statistics {
            match default_stat.as_str() {
                "match-wins" => defaults.push(0),
                "game-wins" => defaults.push(1),
                "match-count" => defaults.push(2),
                "game-count" => defaults.push(3),
                "mulligans" => defaults.push(4),
                "game-length" => defaults.push(5),
                "win-conditions" => defaults.push(6),
                "loss-conditions" => defaults.push(7),
                "proportion" => defaults.push(8),
                _ => {}
            }
        }
        defaults
    } else {
        let stat_options = vec![
            "Match Wins",
            "Game Wins",
            "Match Count",
            "Game Count",
            "Mulligan Stats",
            "Game Length",
            "Win Conditions",
            "Loss Conditions",
            "Proportion",
        ];

        // Pre-select statistics based on config
        let stat_defaults = vec![
            config.stats.default_statistics.contains(&"match-win-rate".to_string()),
            config.stats.default_statistics.contains(&"game-win-rate".to_string()),
            config.stats.default_statistics.contains(&"match-count".to_string()),
            config.stats.default_statistics.contains(&"game-count".to_string()),
            config.stats.default_statistics.contains(&"mulligans".to_string()),
            config.stats.default_statistics.contains(&"game-length".to_string()),
            config.stats.default_statistics.contains(&"win-conditions".to_string()),
            config.stats.default_statistics.contains(&"loss-conditions".to_string()),
            config.stats.default_statistics.contains(&"proportion".to_string()),
        ];

        MultiSelect::new()
            .with_prompt("Select statistics to display (space to select, enter to continue)")
            .items(&stat_options)
            .defaults(&stat_defaults)
            .interact()
            .unwrap()
    };

    // Now build and execute query
    let mut query = matches::table.into_boxed();

    if let Some(ref eras) = era_values {
        query = query.filter(matches::era.eq_any(eras));
    }

    if let Some(ref deck_name) = deck_name_filter {
        query = query.filter(matches::deck_name.like(format!("%{}%", deck_name)));
    }

    if let Some(ref opponent_name) = opponent_name_filter {
        query = query.filter(matches::opponent_name.like(format!("%{}%", opponent_name)));
    }

    if let Some(ref opponent_deck) = opponent_deck_filter {
        query = query.filter(matches::opponent_deck.like(format!("%{}%", opponent_deck)));
    }

    if let Some(ref event_type) = event_type_filter {
        query = query.filter(matches::event_type.like(format!("%{}%", event_type)));
    }

    let mut all_matches = query.load::<Match>(connection)
        .expect("Error loading matches");

    if all_matches.is_empty() {
        println!("No matches found with selected filters");
        return;
    }

    // Get all games for these matches and apply game-specific filters
    let match_ids: Vec<i32> = all_matches.iter().map(|m| m.match_id).collect();
    let mut game_query = games::table.filter(games::match_id.eq_any(&match_ids)).into_boxed();

    let has_game_filters = loss_reason_filter.is_some()
        || win_condition_filter.is_some()
        || game_plan_filter.is_some()
        || mulligan_count_filter.is_some()
        || game_length_filter.is_some()
        || game_number_filter.is_some()
        || play_draw_filter.is_some();

    if let Some(ref reason) = loss_reason_filter {
        game_query = game_query.filter(games::loss_reason.eq(reason));
    }

    if let Some(ref condition) = win_condition_filter {
        game_query = game_query.filter(games::win_condition.eq(condition));
    }

    if let Some(ref plan) = game_plan_filter {
        game_query = game_query.filter(games::opening_hand_plan.eq(plan));
    }

    if let Some(count) = mulligan_count_filter {
        game_query = game_query.filter(games::mulligans.eq(count));
    }

    if let Some((min_turns, max_turns)) = game_length_filter {
        game_query = game_query.filter(games::turns.ge(min_turns).and(games::turns.le(max_turns)));
    }

    if let Some(game_num) = game_number_filter {
        game_query = game_query.filter(games::game_number.eq(game_num));
    }

    if let Some(ref play_draw) = play_draw_filter {
        game_query = game_query.filter(games::play_draw.eq(play_draw));
    }

    let all_games = game_query.load::<Game>(connection)
        .expect("Error loading games");

    // If game-specific filters were applied, filter matches to only those with matching games
    if has_game_filters {
        let filtered_match_ids: std::collections::HashSet<i32> = all_games.iter()
            .map(|g| g.match_id)
            .collect();
        all_matches.retain(|m| filtered_match_ids.contains(&m.match_id));

        if all_matches.is_empty() {
            println!("No matches found with selected filters");
            return;
        }
    }

    println!("\nFound {} matches with {} total games\n", all_matches.len(), all_games.len());

    // Determine if we're in "game mode" (game-level filters or group-bys applied)
    let has_game_groupbys = has_game_level(&selected_groupbys, GROUPBY_LEVELS);
    let game_mode = has_game_filters || has_game_groupbys;

    // Filter out match-level stats when in game mode, and auto-select appropriate win rate
    let selected_stats: Vec<usize> = if game_mode {
        let mut filtered: Vec<usize> = selected_stats.iter()
            .copied()
            .filter(|&idx| get_level(idx, STAT_LEVELS) != Some(StatsLevel::Match))
            .collect();

        // Auto-select game win rate if not already present
        if !filtered.contains(&1) {
            filtered.insert(0, 1); // Game Win Rate at the start
        }

        if filtered.len() != selected_stats.len() || !selected_stats.contains(&1) {
            println!("Note: Game mode active - showing game win rate\n");
        }
        filtered
    } else {
        let mut stats = selected_stats;
        // Auto-select match win rate if not already present
        if !stats.contains(&0) {
            stats.insert(0, 0); // Match Win Rate at the start
        }
        stats
    };

    // Show overall stats first if no group-bys selected
    if selected_groupbys.is_empty() {
        show_overall_stats(&all_matches, &all_games, &selected_stats);
        return;
    }

    // Apply group-bys
    for &groupby_idx in &selected_groupbys {
        let groupby_name = match groupby_idx {
            0 => "my-deck-archetype",  // My Archetype
            1 => "my-deck-subtype",     // My Subtype
            2 => "my-deck-list",        // My List
            3 => "opponent",
            4 => "opponent-deck",
            5 => "opponent-deck-archetype",
            6 => "deck-category",
            7 => "era",
            8 => "game-number",
            9 => "mulligans",
            10 => "game-plan",
            11 => "win-condition",
            12 => "loss-reason",
            13 => "game-length",
            14 => "play-draw",
            _ => continue,
        };

        show_sliced_stats(&all_matches, &all_games, groupby_name, config.stats.min_games, &selected_stats);
    }
}


fn show_overall_stats(all_matches: &[Match], all_games: &[Game], selected_stats: &[usize]) {
    let match_refs: Vec<&Match> = all_matches.iter().collect();
    let overall_row = calculate_stats("Overall".to_string(), &match_refs, all_games);
    display_stats_table(&[overall_row], selected_stats, "=== Overall Statistics ===", false);
}

/// Extract archetype from deck name (ignoring subtype)
/// Examples: "Reanimator: UB" -> "Reanimator", "Lands" -> "Lands"
fn extract_archetype(deck_name: &str) -> String {
    let (archetype, _subtype) = parse_deck_name(deck_name);
    archetype.to_string()
}

fn show_sliced_stats(all_matches: &[Match], all_games: &[Game], slice_type: &str, min_games: i64, selected_stats: &[usize]) {
    // Determine title and grouping function
    let (title, get_key): (&str, Box<dyn Fn(&Match) -> String>) = match slice_type {
        "my-deck" => ("=== Statistics by My Deck ===", Box::new(|m: &Match| m.deck_name.clone())),
        "my-deck-archetype" => ("=== Statistics by My Archetype ===", Box::new(|m: &Match| extract_archetype(&m.deck_name))),
        "my-deck-subtype" => ("=== Statistics by My Subtype ===", Box::new(|m: &Match| {
            let (_, subtype) = parse_deck_name(&m.deck_name);
            subtype.unwrap_or("None").to_string()
        })),
        "my-deck-list" => ("=== Statistics by My List ===", Box::new(|m: &Match| {
            // Extract list name from parentheses
            if let Some(start) = m.deck_name.find(" (") {
                if let Some(end) = m.deck_name[start..].find(')') {
                    return m.deck_name[start + 2..start + end].to_string();
                }
            }
            "No list".to_string()
        })),
        "opponent" => ("=== Statistics by Opponent ===", Box::new(|m: &Match| m.opponent_name.clone())),
        "opponent-deck" => ("=== Statistics by Opponent Deck ===", Box::new(|m: &Match| m.opponent_deck.clone())),
        "opponent-deck-archetype" => ("=== Statistics by Opponent Deck Archetype ===", Box::new(|m: &Match| {
            extract_archetype(&m.opponent_deck)
        })),
        "deck-category" => ("=== Statistics by Opponent Deck Category ===", Box::new(|m: &Match| {
            categorize_deck(&m.opponent_deck).to_string().to_string()
        })),
        "era" => ("=== Statistics by Era ===", Box::new(|m: &Match| {
            m.era.map(|e| format!("Era {}", e)).unwrap_or_else(|| "No Era".to_string())
        })),
        "game-number" => ("=== Statistics by Game Number ===", Box::new(|_m: &Match| {
            // This one is special - we need to group by game number, not match
            // For now, return empty to handle specially
            String::new()
        })),
        "mulligans" => ("=== Statistics by Mulligan Count ===", Box::new(|_m: &Match| String::new())),
        "game-plan" => ("=== Statistics by Game Plan ===", Box::new(|_m: &Match| String::new())),
        "win-condition" => ("=== Statistics by Win Condition ===", Box::new(|_m: &Match| String::new())),
        "loss-reason" => ("=== Statistics by Loss Reason ===", Box::new(|_m: &Match| String::new())),
        "game-length" => ("=== Statistics by Game Length ===", Box::new(|_m: &Match| String::new())),
        "play-draw" => ("=== Statistics by Play/Draw ===", Box::new(|_m: &Match| String::new())),
        _ => return,
    };

    // Special handling for game-based groupings
    if slice_type == "game-number" {
        let mut game_stats: HashMap<i32, Vec<&Game>> = HashMap::new();
        for game in all_games {
            game_stats.entry(game.game_number).or_default().push(game);
        }

        let mut rows: Vec<StatsRow> = game_stats.into_iter()
            .map(|(game_num, games)| {
                calculate_stats_from_games(format!("Game {}", game_num), games, all_matches)
            })
            .filter(|row| row.game_count >= min_games as usize)
            .collect();

        rows.sort_by(|a, b| b.game_count.cmp(&a.game_count));
        display_stats_table(&rows, selected_stats, title, true);
        return;
    }

    if slice_type == "mulligans" {
        let mut mull_stats: HashMap<i32, Vec<&Game>> = HashMap::new();
        for game in all_games {
            mull_stats.entry(game.mulligans).or_default().push(game);
        }

        let mut rows: Vec<StatsRow> = mull_stats.into_iter()
            .map(|(mulls, games)| {
                calculate_stats_from_games(format!("{} mulligan{}", mulls, if mulls == 1 { "" } else { "s" }), games, all_matches)
            })
            .filter(|row| row.game_count >= min_games as usize)
            .collect();

        rows.sort_by(|a, b| a.label.cmp(&b.label));
        display_stats_table(&rows, selected_stats, title, true);
        return;
    }

    if slice_type == "game-plan" {
        let mut plan_stats: HashMap<String, Vec<&Game>> = HashMap::new();
        for game in all_games {
            if let Some(plan) = &game.opening_hand_plan {
                plan_stats.entry(plan.clone()).or_default().push(game);
            }
        }

        let mut rows: Vec<StatsRow> = plan_stats.into_iter()
            .map(|(plan, games)| {
                calculate_stats_from_games(plan, games, all_matches)
            })
            .filter(|row| row.game_count >= min_games as usize)
            .collect();

        rows.sort_by(|a, b| b.game_count.cmp(&a.game_count));
        display_stats_table(&rows, selected_stats, title, true);
        return;
    }

    if slice_type == "win-condition" {
        let mut cond_stats: HashMap<String, Vec<&Game>> = HashMap::new();
        for game in all_games {
            if let Some(condition) = &game.win_condition {
                cond_stats.entry(condition.clone()).or_default().push(game);
            }
        }

        let mut rows: Vec<StatsRow> = cond_stats.into_iter()
            .map(|(condition, games)| {
                calculate_stats_from_games(condition, games, all_matches)
            })
            .filter(|row| row.game_count >= min_games as usize)
            .collect();

        rows.sort_by(|a, b| b.game_count.cmp(&a.game_count));
        display_stats_table(&rows, selected_stats, title, true);
        return;
    }

    if slice_type == "loss-reason" {
        let mut reason_stats: HashMap<String, Vec<&Game>> = HashMap::new();
        for game in all_games {
            if let Some(reason) = &game.loss_reason {
                reason_stats.entry(reason.clone()).or_default().push(game);
            }
        }

        let mut rows: Vec<StatsRow> = reason_stats.into_iter()
            .map(|(reason, games)| {
                calculate_stats_from_games(reason, games, all_matches)
            })
            .filter(|row| row.game_count >= min_games as usize)
            .collect();

        rows.sort_by(|a, b| b.game_count.cmp(&a.game_count));
        display_stats_table(&rows, selected_stats, title, true);
        return;
    }

    if slice_type == "game-length" {
        let mut length_stats: HashMap<String, Vec<&Game>> = HashMap::new();
        for game in all_games {
            if let Some(turns) = game.turns {
                let bucket = match turns {
                    1..=3 => "1-3 turns",
                    4..=6 => "4-6 turns",
                    7..=9 => "7-9 turns",
                    10..=12 => "10-12 turns",
                    _ => "13+ turns",
                };
                length_stats.entry(bucket.to_string()).or_default().push(game);
            }
        }

        let mut rows: Vec<StatsRow> = length_stats.into_iter()
            .map(|(bucket, games)| {
                calculate_stats_from_games(bucket, games, all_matches)
            })
            .filter(|row| row.game_count >= min_games as usize)
            .collect();

        // Sort by bucket order
        let order = ["1-3 turns", "4-6 turns", "7-9 turns", "10-12 turns", "13+ turns"];
        rows.sort_by_key(|row| order.iter().position(|&s| s == row.label).unwrap_or(999));
        display_stats_table(&rows, selected_stats, title, true);
        return;
    }

    if slice_type == "play-draw" {
        let mut play_draw_stats: HashMap<String, Vec<&Game>> = HashMap::new();
        for game in all_games {
            let label = match game.play_draw.as_str() {
                "play" => "On the Play",
                "draw" => "On the Draw",
                _ => "Unknown",
            };
            play_draw_stats.entry(label.to_string()).or_default().push(game);
        }

        let mut rows: Vec<StatsRow> = play_draw_stats.into_iter()
            .map(|(label, games)| {
                calculate_stats_from_games(label, games, all_matches)
            })
            .filter(|row| row.game_count >= min_games as usize)
            .collect();

        // Sort: Play first, then Draw
        let order = ["On the Play", "On the Draw", "Unknown"];
        rows.sort_by_key(|row| order.iter().position(|&s| s == row.label).unwrap_or(999));
        display_stats_table(&rows, selected_stats, title, true);
        return;
    }

    // Standard match-based grouping
    let mut grouped_stats: HashMap<String, Vec<&Match>> = HashMap::new();
    for m in all_matches {
        let key = get_key(m);
        grouped_stats.entry(key).or_default().push(m);
    }

    let mut rows: Vec<StatsRow> = grouped_stats.into_iter()
        .map(|(label, matches)| {
            calculate_stats(label, &matches, all_games)
        })
        .filter(|row| row.match_count >= min_games as usize)
        .collect();

    // Sort by total games descending
    rows.sort_by(|a, b| b.game_count.cmp(&a.game_count));

    display_stats_table(&rows, selected_stats, title, false);
}

fn reconcile_deck(deck_name: &str) {
    use dialoguer::Select;
    use std::fs;
    use toml;

    let connection = &mut establish_connection();

    // Load the deck definition file
    let def_path = format!("definitions/{}.toml", deck_name.to_lowercase());
    let definition: UnifiedArchetypeDefinition = match fs::read_to_string(&def_path) {
        Ok(content) => match toml::from_str(&content) {
            Ok(def) => def,
            Err(e) => {
                println!("Error parsing {}: {}", def_path, e);
                return;
            }
        },
        Err(e) => {
            println!("Error reading {}: {}", def_path, e);
            println!("Make sure the definition file exists for '{}'", deck_name);
            return;
        }
    };

    println!("Reconciling deck: {}\n", definition.name);

    // Get all games for this deck (matches deck name by archetype)
    let all_matches = matches::table
        .filter(matches::deck_name.like(format!("%{}%", deck_name)))
        .load::<Match>(connection)
        .expect("Error loading matches");

    if all_matches.is_empty() {
        println!("No matches found for deck '{}'", deck_name);
        return;
    }

    let match_ids: Vec<i32> = all_matches.iter().map(|m| m.match_id).collect();
    let all_games = games::table
        .filter(games::match_id.eq_any(&match_ids))
        .load::<Game>(connection)
        .expect("Error loading games");

    println!("Found {} matches with {} games\n", all_matches.len(), all_games.len());

    // Collect all unique values from the database
    let mut db_game_plans: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut db_win_conditions: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut db_loss_reasons: std::collections::HashSet<String> = std::collections::HashSet::new();

    for game in &all_games {
        if let Some(ref plan) = game.opening_hand_plan {
            db_game_plans.insert(plan.clone());
        }
        if let Some(ref condition) = game.win_condition {
            db_win_conditions.insert(condition.clone());
        }
        if let Some(ref reason) = game.loss_reason {
            db_loss_reasons.insert(reason.clone());
        }
    }

    // Collect all valid values from definition (including subtypes)
    let mut def_game_plans: std::collections::HashSet<String> = definition.game_plans.iter().cloned().collect();
    let mut def_win_conditions: std::collections::HashSet<String> = definition.win_conditions.iter().cloned().collect();
    let mut def_loss_reasons: std::collections::HashSet<String> = definition.loss_reasons.iter().cloned().collect();

    for (_, subtype) in &definition.subtypes {
        def_game_plans.extend(subtype.game_plans.iter().cloned());
        def_win_conditions.extend(subtype.win_conditions.iter().cloned());
        def_loss_reasons.extend(subtype.loss_reasons.iter().cloned());
    }

    // Find mismatches
    let mut changes_made = false;

    // Check game plans
    for db_value in &db_game_plans {
        if !def_game_plans.contains(db_value) {
            println!("═══════════════════════════════════════");
            println!("Found game plan in database not in definition file:");
            println!("  Database value: '{}'", db_value);
            println!("  Count: {} games", all_games.iter().filter(|g| g.opening_hand_plan.as_ref() == Some(db_value)).count());
            println!();

            let choices = vec![
                format!("Add '{}' to definition file", db_value),
                "Change database entries to a value from file".to_string(),
                "Skip (keep as is)".to_string(),
            ];

            let selection = Select::new()
                .with_prompt("What would you like to do?")
                .items(&choices)
                .default(0)
                .interact()
                .unwrap();

            match selection {
                0 => {
                    println!("Note: You'll need to manually add '{}' to {}", db_value, def_path);
                    println!("Add it under the 'game_plans' array in the appropriate section.");
                }
                1 => {
                    let mut file_values: Vec<String> = def_game_plans.iter().cloned().collect();
                    file_values.sort();

                    let choice = Select::new()
                        .with_prompt("Select the value to use")
                        .items(&file_values)
                        .interact()
                        .unwrap();

                    let new_value = &file_values[choice];

                    diesel::update(games::table)
                        .filter(games::match_id.eq_any(&match_ids))
                        .filter(games::opening_hand_plan.eq(db_value))
                        .set(games::opening_hand_plan.eq(new_value))
                        .execute(connection)
                        .expect("Error updating games");

                    println!("✓ Updated game plan from '{}' to '{}'", db_value, new_value);
                    changes_made = true;
                }
                2 => {
                    println!("Skipped");
                }
                _ => {}
            }
            println!();
        }
    }

    // Check win conditions
    for db_value in &db_win_conditions {
        if !def_win_conditions.contains(db_value) {
            println!("═══════════════════════════════════════");
            println!("Found win condition in database not in definition file:");
            println!("  Database value: '{}'", db_value);
            println!("  Count: {} games", all_games.iter().filter(|g| g.win_condition.as_ref() == Some(db_value)).count());
            println!();

            let choices = vec![
                format!("Add '{}' to definition file", db_value),
                "Change database entries to a value from file".to_string(),
                "Skip (keep as is)".to_string(),
            ];

            let selection = Select::new()
                .with_prompt("What would you like to do?")
                .items(&choices)
                .default(0)
                .interact()
                .unwrap();

            match selection {
                0 => {
                    println!("Note: You'll need to manually add '{}' to {}", db_value, def_path);
                    println!("Add it under the 'win_conditions' array in the appropriate section.");
                }
                1 => {
                    let mut file_values: Vec<String> = def_win_conditions.iter().cloned().collect();
                    file_values.sort();

                    let choice = Select::new()
                        .with_prompt("Select the value to use")
                        .items(&file_values)
                        .interact()
                        .unwrap();

                    let new_value = &file_values[choice];

                    diesel::update(games::table)
                        .filter(games::match_id.eq_any(&match_ids))
                        .filter(games::win_condition.eq(db_value))
                        .set(games::win_condition.eq(new_value))
                        .execute(connection)
                        .expect("Error updating games");

                    println!("✓ Updated win condition from '{}' to '{}'", db_value, new_value);
                    changes_made = true;
                }
                2 => {
                    println!("Skipped");
                }
                _ => {}
            }
            println!();
        }
    }

    // Check loss reasons
    for db_value in &db_loss_reasons {
        if !def_loss_reasons.contains(db_value) {
            println!("═══════════════════════════════════════");
            println!("Found loss reason in database not in definition file:");
            println!("  Database value: '{}'", db_value);
            println!("  Count: {} games", all_games.iter().filter(|g| g.loss_reason.as_ref() == Some(db_value)).count());
            println!();

            let choices = vec![
                format!("Add '{}' to definition file", db_value),
                "Change database entries to a value from file".to_string(),
                "Skip (keep as is)".to_string(),
            ];

            let selection = Select::new()
                .with_prompt("What would you like to do?")
                .items(&choices)
                .default(0)
                .interact()
                .unwrap();

            match selection {
                0 => {
                    println!("Note: You'll need to manually add '{}' to {}", db_value, def_path);
                    println!("Add it under the 'loss_reasons' array in the appropriate section.");
                }
                1 => {
                    let mut file_values: Vec<String> = def_loss_reasons.iter().cloned().collect();
                    file_values.sort();

                    let choice = Select::new()
                        .with_prompt("Select the value to use")
                        .items(&file_values)
                        .interact()
                        .unwrap();

                    let new_value = &file_values[choice];

                    diesel::update(games::table)
                        .filter(games::match_id.eq_any(&match_ids))
                        .filter(games::loss_reason.eq(db_value))
                        .set(games::loss_reason.eq(new_value))
                        .execute(connection)
                        .expect("Error updating games");

                    println!("✓ Updated loss reason from '{}' to '{}'", db_value, new_value);
                    changes_made = true;
                }
                2 => {
                    println!("Skipped");
                }
                _ => {}
            }
            println!();
        }
    }

    if changes_made {
        println!("═══════════════════════════════════════");
        println!("✓ Reconciliation complete with changes!");
    } else if db_game_plans.is_subset(&def_game_plans) &&
              db_win_conditions.is_subset(&def_win_conditions) &&
              db_loss_reasons.is_subset(&def_loss_reasons) {
        println!("✓ All database values match the definition file - no reconciliation needed!");
    } else {
        println!("═══════════════════════════════════════");
        println!("✓ Reconciliation complete!");
    }
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len-3])
    }
}

fn edit_match_interactive(match_id: i32) {
    let connection = &mut establish_connection();
    
    // Load the existing match
    let match_result = matches::table
        .find(match_id)
        .first::<Match>(connection);
        
    let mut match_data = match match_result {
        Ok(m) => m,
        Err(_) => {
            println!("Match {} not found", match_id);
            return;
        }
    };
    
    println!("=== Editing Match {} ===", match_id);
    println!("Current values shown in [brackets]. Press Enter to keep current value.");
    println!();
    
    // Edit date
    let new_date: String = Input::new()
        .with_prompt(&format!("Date [{}]", match_data.date))
        .allow_empty(true)
        .interact_text()
        .unwrap();
    if !new_date.is_empty() {
        match NaiveDate::parse_from_str(&new_date, "%Y-%m-%d") {
            Ok(_) => match_data.date = new_date,
            Err(_) => println!("Invalid date format, keeping current value"),
        }
    }
    
    // Edit deck name
    let change_deck = Confirm::new()
        .with_prompt(&format!("Change your deck from '{}'?", match_data.deck_name))
        .interact()
        .unwrap();
        
    if change_deck {
        let your_decks = load_your_deck_names();
        let mut deck_options = your_decks.clone();
        deck_options.push("Custom (type new deck name)".to_string());
        
        let current_deck_idx = your_decks.iter()
            .position(|deck| deck == &match_data.deck_name)
            .unwrap_or(0);
        
        let deck_idx = FuzzySelect::new()
            .with_prompt("Your deck name")
            .items(&deck_options)
            .default(current_deck_idx)
            .interact()
            .unwrap();
            
        if deck_idx == deck_options.len() - 1 {
            // Custom option selected
            let new_deck_name: String = Input::new()
                .with_prompt("Enter new deck name")
                .interact_text()
                .unwrap();
            if !new_deck_name.is_empty() {
                match_data.deck_name = new_deck_name;
            }
        } else {
            match_data.deck_name = your_decks[deck_idx].clone();
        }
    }
    
    // Edit opponent name
    let change_opponent = Confirm::new()
        .with_prompt(&format!("Change opponent from '{}'?", match_data.opponent_name))
        .interact()
        .unwrap();
        
    if change_opponent {
        let opponents = load_opponent_names();
        if opponents.is_empty() {
            // No opponent history, use text input
            let new_opponent_name: String = Input::new()
                .with_prompt(&format!("Opponent name [{}]", match_data.opponent_name))
                .allow_empty(true)
                .interact_text()
                .unwrap();
            if !new_opponent_name.is_empty() {
                match_data.opponent_name = new_opponent_name;
            }
        } else {
            let mut opponent_options = opponents.clone();
            opponent_options.push("Custom (type new opponent)".to_string());
            
            let current_opponent_idx = opponents.iter()
                .position(|opp| opp == &match_data.opponent_name)
                .unwrap_or(0);
            
            let opponent_idx = FuzzySelect::new()
                .with_prompt("Opponent name (type to search)")
                .items(&opponent_options)
                .default(current_opponent_idx)
                .interact()
                .unwrap();
                
            if opponent_idx == opponent_options.len() - 1 {
                // Custom option selected
                let new_opponent_name: String = Input::new()
                    .with_prompt("Enter opponent name")
                    .interact_text()
                    .unwrap();
                if !new_opponent_name.is_empty() {
                    match_data.opponent_name = new_opponent_name;
                }
            } else {
                match_data.opponent_name = opponents[opponent_idx].clone();
            }
        }
    }
    
    // Edit opponent deck
    let deck_names = load_deck_names();
    let deck_names_refs: Vec<&str> = deck_names.iter().map(|s| s.as_str()).collect();
    let current_deck_idx = deck_names.iter()
        .position(|deck| deck == &match_data.opponent_deck)
        .unwrap_or(0);
        
    let change_deck = Confirm::new()
        .with_prompt(&format!("Change opponent deck from '{}'?", match_data.opponent_deck))
        .interact()
        .unwrap();
        
    if change_deck {
        let opponent_deck_idx = FuzzySelect::new()
            .with_prompt("Opponent's deck")
            .items(&deck_names_refs)
            .default(current_deck_idx)
            .interact()
            .unwrap();
        match_data.opponent_deck = deck_names[opponent_deck_idx].clone();
    }
    
    // Edit event type
    let current_event_idx = EVENT_TYPES.iter()
        .position(|&event| event == match_data.event_type)
        .unwrap_or(0);
        
    let change_event = Confirm::new()
        .with_prompt(&format!("Change event type from '{}'?", match_data.event_type))
        .interact()
        .unwrap();
        
    if change_event {
        let event_type_idx = FuzzySelect::new()
            .with_prompt("Event type")
            .items(EVENT_TYPES)
            .default(current_event_idx)
            .interact()
            .unwrap();
        match_data.event_type = EVENT_TYPES[event_type_idx].to_string();
    }
    
    // Edit die roll winner
    let change_die_roll = Confirm::new()
        .with_prompt(&format!("Change die roll winner from '{}'?", match_data.die_roll_winner))
        .interact()
        .unwrap();
        
    if change_die_roll {
        let die_roll_winner = if Confirm::new()
            .with_prompt("Did you win the die roll?")
            .interact()
            .unwrap()
        {
            "me"
        } else {
            "opponent"
        };
        match_data.die_roll_winner = die_roll_winner.to_string();
    }
    
    // Edit match winner
    let change_match_winner = Confirm::new()
        .with_prompt(&format!("Change match winner from '{}'?", match_data.match_winner))
        .interact()
        .unwrap();
        
    if change_match_winner {
        let match_winner = if Confirm::new()
            .with_prompt("Did you win the match?")
            .interact()
            .unwrap()
        {
            "me"
        } else {
            "opponent"
        };
        match_data.match_winner = match_winner.to_string();
    }
    
    // Save changes
    diesel::update(matches::table.find(match_id))
        .set((
            matches::date.eq(&match_data.date),
            matches::deck_name.eq(&match_data.deck_name),
            matches::opponent_name.eq(&match_data.opponent_name),
            matches::opponent_deck.eq(&match_data.opponent_deck),
            matches::event_type.eq(&match_data.event_type),
            matches::die_roll_winner.eq(&match_data.die_roll_winner),
            matches::match_winner.eq(&match_data.match_winner),
        ))
        .execute(connection)
        .expect("Error updating match");
        
    println!("Match {} updated successfully!", match_id);
}

fn edit_game_interactive(match_id: i32, game_number: i32) {
    let connection = &mut establish_connection();
    
    // Verify match exists
    let match_exists = matches::table
        .find(match_id)
        .first::<Match>(connection)
        .is_ok();
        
    if !match_exists {
        println!("Match {} not found", match_id);
        return;
    }
    
    // Load the existing game
    let game_result = games::table
        .filter(games::match_id.eq(match_id))
        .filter(games::game_number.eq(game_number))
        .first::<Game>(connection);
        
    let mut game_data = match game_result {
        Ok(g) => g,
        Err(_) => {
            println!("Game {} in match {} not found", game_number, match_id);
            return;
        }
    };
    
    println!("=== Editing Game {} in Match {} ===", game_number, match_id);
    println!("Current values shown in [brackets]. Press Enter to keep current value.");
    println!();
    
    // Edit play/draw
    let change_play_draw = Confirm::new()
        .with_prompt(&format!("Change play/draw from '{}'?", game_data.play_draw))
        .interact()
        .unwrap();
        
    if change_play_draw {
        let play_draw = if Confirm::new()
            .with_prompt("Did you play first? (no = draw)")
            .interact()
            .unwrap()
        {
            "play"
        } else {
            "draw"
        };
        game_data.play_draw = play_draw.to_string();
    }
    
    // Edit mulligans
    let new_mulligans: String = Input::new()
        .with_prompt(&format!("Number of mulligans [{}]", game_data.mulligans))
        .allow_empty(true)
        .interact_text()
        .unwrap();
    if !new_mulligans.is_empty() {
        if let Ok(mulligans) = new_mulligans.parse::<i32>() {
            if mulligans >= 0 && mulligans <= 7 {
                game_data.mulligans = mulligans;
            } else {
                println!("Mulligans must be between 0 and 7, keeping current value");
            }
        } else {
            println!("Invalid number, keeping current value");
        }
    }
    
    // Edit opening hand plan
    let current_plan = game_data.opening_hand_plan.as_deref().unwrap_or("");
    let new_plan: String = Input::new()
        .with_prompt(&format!("Opening hand plan [{}]", current_plan))
        .allow_empty(true)
        .interact_text()
        .unwrap();
    if !new_plan.is_empty() {
        game_data.opening_hand_plan = Some(new_plan);
    }
    
    // Edit game winner
    let change_winner = Confirm::new()
        .with_prompt(&format!("Change game winner from '{}'?", game_data.game_winner))
        .interact()
        .unwrap();
        
    if change_winner {
        let game_winner = if Confirm::new()
            .with_prompt("Did you win this game?")
            .interact()
            .unwrap()
        {
            "me"
        } else {
            "opponent"
        };
        game_data.game_winner = game_winner.to_string();
    }
    
    // Edit win condition (only if you won)
    if game_data.game_winner == "me" {
        let current_condition = game_data.win_condition.as_deref().unwrap_or("");
        let new_condition: String = Input::new()
            .with_prompt(&format!("What did you win with? [{}]", current_condition))
            .allow_empty(true)
            .interact_text()
            .unwrap();
        if !new_condition.is_empty() {
            game_data.win_condition = Some(new_condition);
        }
        game_data.loss_reason = None;
    } else {
        game_data.win_condition = None;

        // Edit loss reason (only if you lost)
        let current_reason = game_data.loss_reason.as_deref().unwrap_or("");
        let new_reason: String = Input::new()
            .with_prompt(&format!("Why did you lose? [{}]", current_reason))
            .allow_empty(true)
            .interact_text()
            .unwrap();
        if !new_reason.is_empty() {
            game_data.loss_reason = Some(new_reason);
        }
    }

    // Edit turns
    let current_turns = game_data.turns.map(|t| t.to_string()).unwrap_or_else(|| "".to_string());
    let new_turns: String = Input::new()
        .with_prompt(&format!("Number of turns [{}]", current_turns))
        .allow_empty(true)
        .validate_with(|input: &String| -> Result<(), &str> {
            if input.is_empty() {
                return Ok(());
            }
            match input.parse::<i32>() {
                Ok(n) if n > 0 => Ok(()),
                _ => Err("Turns must be a positive number")
            }
        })
        .interact_text()
        .unwrap();
    if !new_turns.is_empty() {
        if let Ok(turns) = new_turns.parse::<i32>() {
            game_data.turns = Some(turns);
        }
    } else if new_turns.is_empty() && current_turns.is_empty() {
        // If they pressed enter and there was no current value, keep it as None
        game_data.turns = None;
    }
    
    // Save changes
    diesel::update(games::table
        .filter(games::match_id.eq(match_id))
        .filter(games::game_number.eq(game_number)))
        .set((
            games::play_draw.eq(&game_data.play_draw),
            games::mulligans.eq(game_data.mulligans),
            games::opening_hand_plan.eq(&game_data.opening_hand_plan),
            games::game_winner.eq(&game_data.game_winner),
            games::win_condition.eq(&game_data.win_condition),
            games::loss_reason.eq(&game_data.loss_reason),
            games::turns.eq(&game_data.turns),
        ))
        .execute(connection)
        .expect("Error updating game");
        
    println!("Game {} in match {} updated successfully!", game_number, match_id);
}

fn add_deck_to_list(deck_name: Option<String>) {
    let deck_name = match deck_name {
        Some(name) => name,
        None => {
            // Interactive mode
            Input::new()
                .with_prompt("Enter deck name to add")
                .interact_text()
                .unwrap()
        }
    };
    
    if deck_name.trim().is_empty() {
        println!("Deck name cannot be empty");
        return;
    }
    
    // Load existing deck names
    let existing_deck_names = load_deck_names();
    
    // Check if deck already exists
    if existing_deck_names.contains(&deck_name) {
        println!("Deck '{}' already exists in the list", deck_name);
        return;
    }
    
    // Ask for category
    let category_options = vec!["Blue", "Combo", "Non-Blue", "Stompy"];
    let category_idx = FuzzySelect::new()
        .with_prompt("Select deck category")
        .items(&category_options)
        .default(0)
        .interact()
        .unwrap();
    let category = category_options[category_idx];
    
    // Read the existing definitions.md file
    let content = match fs::read_to_string("definitions.md") {
        Ok(content) => content,
        Err(_) => {
            // Create a new file if it doesn't exist
            "## Decks\n\n## Game Plans\n\n## Win Cons\n".to_string()
        }
    };
    
    // Find the Decks section and add the new deck
    let lines: Vec<&str> = content.lines().collect();
    let mut new_lines = Vec::new();
    let mut in_decks_section = false;
    let mut deck_lines = Vec::new();
    
    for line in lines {
        if line.starts_with("## Decks") {
            in_decks_section = true;
            new_lines.push(line.to_string());
            continue;
        }
        
        if line.starts_with("##") && !line.starts_with("## Decks") {
            if in_decks_section {
                // Add the new deck before ending the section
                deck_lines.push(format!("{}; {}", deck_name, category));
                deck_lines.sort();
                // Keep "Other" at the end
                if let Some(pos) = deck_lines.iter().position(|l| l.starts_with("Other;")) {
                    let other = deck_lines.remove(pos);
                    deck_lines.push(other);
                }
                for deck_line in &deck_lines {
                    new_lines.push(deck_line.clone());
                }
                deck_lines.clear();
                in_decks_section = false;
            }
            new_lines.push(line.to_string());
            continue;
        }
        
        if in_decks_section && !line.trim().is_empty() {
            deck_lines.push(line.to_string());
        } else {
            new_lines.push(line.to_string());
        }
    }
    
    // If we're still in decks section at the end of file
    if in_decks_section {
        deck_lines.push(format!("{}; {}", deck_name, category));
        deck_lines.sort();
        if let Some(pos) = deck_lines.iter().position(|l| l.starts_with("Other;")) {
            let other = deck_lines.remove(pos);
            deck_lines.push(other);
        }
        for deck_line in &deck_lines {
            new_lines.push(deck_line.clone());
        }
    }
    
    // Write back to file
    let new_content = new_lines.join("\n");
    match fs::write("definitions.md", new_content) {
        Ok(_) => println!("Added '{}' with category '{}' to deck list", deck_name, category),
        Err(e) => println!("Error writing to definitions.md: {}", e),
    }
}

fn load_board_plans() -> HashMap<String, String> {
    let mut plans = HashMap::new();
    
    if let Ok(content) = fs::read_to_string("board_plans.txt") {
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            
            if let Some((deck, plan)) = line.split_once(" | ") {
                plans.insert(deck.trim().to_string(), plan.trim().to_string());
            }
        }
    }
    
    plans
}

fn show_board_plan(deck_name: Option<String>) {
    let deck_name = match deck_name {
        Some(name) => name,
        None => {
            // Interactive mode - select from available decks
            let deck_names = load_deck_names();
            let deck_names_refs: Vec<&str> = deck_names.iter().map(|s| s.as_str()).collect();

            let selection = FuzzySelect::new()
                .with_prompt("Select opponent deck to see board plan")
                .items(&deck_names_refs)
                .default(0)
                .interact()
                .unwrap();

            deck_names[selection].clone()
        }
    };

    println!("=== Board Plan vs {} ===", deck_name);

    // Try to load from archetype definition first
    if let Some(archetype) = load_archetype_data(&deck_name) {
        if let Some(board_plan) = archetype.board_plan {
            println!("{}", board_plan.description);
            return;
        }
    }

    // Fall back to board_plans.txt
    let board_plans = load_board_plans();
    match board_plans.get(&deck_name) {
        Some(plan) => {
            println!("{}", plan);
        },
        None => {
            println!("No board plan found for '{}'", deck_name);
            println!("You can add one by creating/editing definitions/{}.toml",
                deck_name.to_lowercase().replace(": ", "-").replace(" ", "-").replace(",", ""));
            println!("\nAdd this section to the file:");
            println!("[board_plan]");
            println!("description = \"Your board plan here\"");
        }
    }
}

fn remove_match_interactive(match_id: i32) {
    let connection = &mut establish_connection();
    
    // First, check if the match exists and show details
    let match_result = matches::table
        .find(match_id)
        .first::<Match>(connection);
        
    let match_data = match match_result {
        Ok(m) => m,
        Err(_) => {
            println!("Match {} not found", match_id);
            return;
        }
    };
    
    // Show match details for confirmation
    println!("=== Match {} Details ===", match_id);
    println!("Date: {}", match_data.date);
    println!("Your deck: {}", match_data.deck_name);
    println!("Opponent: {} ({})", match_data.opponent_name, match_data.opponent_deck);
    println!("Event: {}", match_data.event_type);
    println!("Result: {}", if match_data.match_winner == "me" { "Win" } else { "Loss" });
    
    // Load and show games for this match
    let games = games::table
        .filter(games::match_id.eq(match_id))
        .order(games::game_number.asc())
        .load::<Game>(connection)
        .expect("Error loading games");
    
    if !games.is_empty() {
        println!("\nGames:");
        for game in &games {
            let result = if game.game_winner == "me" { "W" } else { "L" };
            println!("  Game {}: {} ({})", game.game_number, result, game.play_draw);
        }
    }
    
    // Confirm deletion
    println!();
    let confirm = Confirm::new()
        .with_prompt(&format!("Are you sure you want to delete match {} and all its games? This cannot be undone.", match_id))
        .interact()
        .unwrap();
        
    if !confirm {
        println!("Match deletion cancelled");
        return;
    }
    
    // Delete games first (due to foreign key constraint)
    let games_deleted = diesel::delete(games::table.filter(games::match_id.eq(match_id)))
        .execute(connection)
        .expect("Error deleting games");
    
    // Then delete the match
    let matches_deleted = diesel::delete(matches::table.find(match_id))
        .execute(connection)
        .expect("Error deleting match");
    
    if matches_deleted > 0 {
        println!("Successfully deleted match {} and {} associated games", match_id, games_deleted);
    } else {
        println!("No match was deleted (this shouldn't happen)");
    }
}

/// Get the current era (latest era from all matches)
/// Eras are time periods independent of deck choice
fn get_current_era(connection: &mut SqliteConnection) -> Option<i32> {
    matches::table
        .select(diesel::dsl::max(matches::era))
        .first::<Option<i32>>(connection)
        .ok()
        .flatten()
}

