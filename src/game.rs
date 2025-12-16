use clap::{Args, Subcommand};
use dialoguer::{Input, FuzzySelect, Confirm};
use chrono::{Local, NaiveDate};
use diesel::prelude::*;
use std::fs;
use std::collections::HashMap;
use std::path::Path;
use serde::Deserialize;

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
    board_plan: Option<BoardPlan>,
    #[serde(default)]
    subtypes: HashMap<String, SubtypeDefinition>,
}

#[derive(Debug, Deserialize)]
struct SubtypeDefinition {
    game_plans: Vec<String>,
    win_conditions: Vec<String>,
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
    default_slices: Vec<String>,
    #[serde(default)]
    min_games: i64,
}

// Structure for resolved archetype data (after looking up subtype)
struct ArchetypeData {
    game_plans: Vec<String>,
    win_conditions: Vec<String>,
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
                        board_plan: subtype_def.board_plan.clone(),
                    });
                }
            }

            // No subtype or subtype not found, use base archetype data
            return Some(ArchetypeData {
                game_plans: unified.game_plans,
                win_conditions: unified.win_conditions,
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
        #[arg(long, help = "Filter by deck name")]
        deck: Option<String>,
        #[arg(long, help = "Filter by event type")]
        event: Option<String>,
        #[arg(long, help = "Filter by era(s): single (e.g., '2'), multiple (e.g., '1,2,3'), or 'all'. Defaults to latest era")]
        era: Option<String>,
        #[arg(long, help = "Show interactive slice selection menu")]
        slice: bool,
        #[arg(long, help = "Slice by my deck")]
        by_my_deck: bool,
        #[arg(long, help = "Slice by opponent")]
        by_opponent: bool,
        #[arg(long, help = "Slice by opponent deck")]
        by_opponent_deck: bool,
        #[arg(long, help = "Slice by opponent deck category")]
        by_opponent_deck_category: bool,
        #[arg(long, help = "Slice by my deck's archetype")]
        by_my_deck_archetype: bool,
        #[arg(long, help = "Slice by opponent deck's archetype")]
        by_opponent_deck_archetype: bool,
        #[arg(long, help = "Slice by game number")]
        by_game_number: bool,
        #[arg(long, help = "Slice by mulligan count")]
        by_mulligans: bool,
        #[arg(long, help = "Slice by game plan")]
        by_game_plan: bool,
        #[arg(long, help = "Slice by win condition")]
        by_win_condition: bool,
        #[arg(long, help = "Slice by game length")]
        by_game_length: bool,
        #[arg(long, help = "Slice by era")]
        by_era: bool,
    },
    HtmlStats {
        #[arg(long, short, default_value = "stats.html", help = "Output HTML file path")]
        output: String,
        #[arg(long, help = "Filter by era(s): single (e.g., '2'), multiple (e.g., '1,2,3'), or 'all'. Defaults to latest era")]
        era: Option<String>,
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
        GameCommands::Stats {
            deck,
            event,
            era,
            slice,
            by_my_deck,
            by_opponent,
            by_opponent_deck,
            by_opponent_deck_category,
            by_my_deck_archetype,
            by_opponent_deck_archetype,
            by_game_number,
            by_mulligans,
            by_game_plan,
            by_win_condition,
            by_game_length,
            by_era
        } => show_stats(deck, event, era, slice, by_my_deck, by_opponent, by_opponent_deck, by_opponent_deck_category, by_my_deck_archetype, by_opponent_deck_archetype, by_game_number, by_mulligans, by_game_plan, by_win_condition, by_game_length, by_era),
        GameCommands::HtmlStats { output, era } => crate::html_stats::generate_html_stats(&output, era),
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
        if let Some(turns) = &game.turns {
            println!("  Turns: {}", turns);
        }
    }
}

enum EraFilter {
    All,
    Eras(Vec<i32>),
}

fn parse_era_filter(era_arg: Option<&str>, connection: &mut SqliteConnection) -> EraFilter {
    match era_arg {
        Some("all") => EraFilter::All,
        Some(era_str) => {
            // Parse comma-separated list of eras
            let eras: Vec<i32> = era_str
                .split(',')
                .filter_map(|s| s.trim().parse::<i32>().ok())
                .collect();

            if eras.is_empty() {
                // If parsing failed, default to latest era
                get_default_era_filter(connection)
            } else {
                EraFilter::Eras(eras)
            }
        }
        None => {
            // Default to latest era if no era specified
            get_default_era_filter(connection)
        }
    }
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

fn show_stats(
    deck_filter: Option<String>,
    event_filter: Option<String>,
    era_filter: Option<String>,
    interactive_slice: bool,
    by_my_deck: bool,
    by_opponent: bool,
    by_opponent_deck: bool,
    by_opponent_deck_category: bool,
    by_my_deck_archetype: bool,
    by_opponent_deck_archetype: bool,
    by_game_number: bool,
    by_mulligans: bool,
    by_game_plan: bool,
    by_win_condition: bool,
    by_game_length: bool,
    by_era: bool
) {
    let connection = &mut establish_connection();

    // Determine era filter
    let era_filter_parsed = parse_era_filter(era_filter.as_deref(), connection);

    // Build the base query
    let mut query = matches::table.into_boxed();

    if let Some(deck) = &deck_filter {
        query = query.filter(matches::deck_name.like(format!("%{}%", deck)));
    }

    if let Some(event) = &event_filter {
        query = query.filter(matches::event_type.like(format!("%{}%", event)));
    }

    // Apply era filter
    match &era_filter_parsed {
        EraFilter::All => {
            // No filter needed
        }
        EraFilter::Eras(eras) => {
            query = query.filter(matches::era.eq_any(eras));
        }
    }

    let all_matches = query.load::<Match>(connection)
        .expect("Error loading matches");

    if all_matches.is_empty() {
        println!("No matches found");
        return;
    }

    println!("=== Match Statistics ===");
    if let Some(deck) = &deck_filter {
        println!("Filtered by deck: {}", deck);
    }
    if let Some(event) = &event_filter {
        println!("Filtered by event: {}", event);
    }
    match &era_filter_parsed {
        EraFilter::All => println!("Filtered by era: all"),
        EraFilter::Eras(eras) => {
            let era_str: Vec<String> = eras.iter().map(|e| e.to_string()).collect();
            println!("Filtered by era: {}", era_str.join(", "));
        }
    }
    println!();
    // Get all games for these matches
    let match_ids: Vec<i32> = all_matches.iter().map(|m| m.match_id).collect();
    let all_games = games::table
        .filter(games::match_id.eq_any(&match_ids))
        .load::<Game>(connection)
        .expect("Error loading games");
    
    // Show overall statistics first
    show_overall_stats(&all_matches, &all_games);

    // Load configuration
    let config = load_config();

    // Handle slice selection - determine which slices to show
    let mut slices_to_show = Vec::new();

    if by_my_deck {
        slices_to_show.push("my-deck");
    }
    if by_opponent {
        slices_to_show.push("opponent");
    }
    if by_opponent_deck {
        slices_to_show.push("opponent-deck");
    }
    if by_opponent_deck_category {
        slices_to_show.push("deck-category");
    }
    if by_my_deck_archetype {
        slices_to_show.push("my-deck-archetype");
    }
    if by_opponent_deck_archetype {
        slices_to_show.push("opponent-deck-archetype");
    }
    if by_game_number {
        slices_to_show.push("game-number");
    }
    if by_mulligans {
        slices_to_show.push("mulligans");
    }
    if by_game_plan {
        slices_to_show.push("game-plan");
    }
    if by_win_condition {
        slices_to_show.push("win-condition");
    }
    if by_game_length {
        slices_to_show.push("game-length");
    }
    if by_era {
        slices_to_show.push("era");
    }

    // If no slices specified via flags, use config defaults
    if slices_to_show.is_empty() && !config.stats.default_slices.is_empty() {
        for slice in &config.stats.default_slices {
            slices_to_show.push(slice.as_str());
        }
    }

    if interactive_slice {
        // Interactive slice selection
        let slice_options = vec![
            "None (no slicing)",
            "my-deck",
            "my-deck-archetype",
            "opponent",
            "opponent-deck",
            "opponent-deck-archetype",
            "deck-category",
            "game-number",
            "mulligans",
            "game-plan",
            "win-condition",
            "game-length",
            "era"
        ];
        
        let selection = FuzzySelect::new()
            .with_prompt("Select how to slice the data")
            .items(&slice_options)
            .default(0)
            .interact();
        
        match selection {
            Ok(0) => {
                // No slicing selected
            },
            Ok(s) => {
                let slice_type = slice_options[s];
                println!("Sliced by: {}", slice_type);
                println!();
                show_sliced_stats(&all_matches, &all_games, slice_type, config.stats.min_games);
            },
            Err(_) => {
                // Fallback to no slicing if not interactive
            }
        }
    } else {
        // Show all requested slices
        for slice_type in slices_to_show {
            println!("Sliced by: {}", slice_type);
            println!();
            show_sliced_stats(&all_matches, &all_games, slice_type, config.stats.min_games);
        }
    }
}

fn show_overall_stats(all_matches: &[Match], all_games: &[Game]) {
    // Calculate overall match statistics
    let total_matches = all_matches.len();
    let wins = all_matches.iter().filter(|m| m.match_winner == "me").count();
    let losses = total_matches - wins;
    let win_rate = if total_matches > 0 { (wins as f64 / total_matches as f64) * 100.0 } else { 0.0 };
    
    println!("Overall Record:");
    println!("  Matches: {} ({}-{})", total_matches, wins, losses);
    println!("  Win Rate: {:.1}%", win_rate);
    
    // Die roll statistics
    let die_roll_wins = all_matches.iter().filter(|m| m.die_roll_winner == "me").count();
    let die_roll_rate = if total_matches > 0 { (die_roll_wins as f64 / total_matches as f64) * 100.0 } else { 0.0 };
    println!("  Die Roll Win Rate: {:.1}%", die_roll_rate);
    println!();
    
    // Game statistics
    let total_games = all_games.len();
    let game_wins = all_games.iter().filter(|g| g.game_winner == "me").count();
    let game_losses = total_games - game_wins;
    let game_win_rate = if total_games > 0 { (game_wins as f64 / total_games as f64) * 100.0 } else { 0.0 };
    
    println!("Game Record:");
    println!("  Games: {} ({}-{})", total_games, game_wins, game_losses);
    println!("  Game Win Rate: {:.1}%", game_win_rate);
    
    // Play/Draw statistics
    let play_games = all_games.iter().filter(|g| g.play_draw == "play").collect::<Vec<_>>();
    let draw_games = all_games.iter().filter(|g| g.play_draw == "draw").collect::<Vec<_>>();
    
    if !play_games.is_empty() {
        let play_wins = play_games.iter().filter(|g| g.game_winner == "me").count();
        let play_win_rate = (play_wins as f64 / play_games.len() as f64) * 100.0;
        println!("  On the Play: {}-{} ({:.1}%)", play_wins, play_games.len() - play_wins, play_win_rate);
    }
    
    if !draw_games.is_empty() {
        let draw_wins = draw_games.iter().filter(|g| g.game_winner == "me").count();
        let draw_win_rate = (draw_wins as f64 / draw_games.len() as f64) * 100.0;
        println!("  On the Draw: {}-{} ({:.1}%)", draw_wins, draw_games.len() - draw_wins, draw_win_rate);
    }
    
    // Mulligan statistics
    let total_mulligans: i32 = all_games.iter().map(|g| g.mulligans).sum();
    let avg_mulligans = if total_games > 0 { total_mulligans as f64 / total_games as f64 } else { 0.0 };
    
    let winning_games: Vec<&Game> = all_games.iter().filter(|g| g.game_winner == "me").collect();
    let losing_games: Vec<&Game> = all_games.iter().filter(|g| g.game_winner == "opponent").collect();
    
    let win_mulligans: i32 = winning_games.iter().map(|g| g.mulligans).sum();
    let loss_mulligans: i32 = losing_games.iter().map(|g| g.mulligans).sum();
    
    let avg_win_mulligans = if !winning_games.is_empty() { win_mulligans as f64 / winning_games.len() as f64 } else { 0.0 };
    let avg_loss_mulligans = if !losing_games.is_empty() { loss_mulligans as f64 / losing_games.len() as f64 } else { 0.0 };
    
    println!("  Average Mulligans: {:.2} (wins: {:.2}, losses: {:.2})", avg_mulligans, avg_win_mulligans, avg_loss_mulligans);
    
    // Game length statistics
    let games_with_turns: Vec<&Game> = all_games.iter().filter(|g| g.turns.is_some()).collect();
    if !games_with_turns.is_empty() {
        let total_turns: i32 = games_with_turns.iter().map(|g| g.turns.unwrap()).sum();
        let avg_turns = total_turns as f64 / games_with_turns.len() as f64;
        
        // Calculate win/loss averages for games with turn data
        let winning_games_with_turns: Vec<&Game> = games_with_turns.iter()
            .filter(|g| g.game_winner == "me")
            .copied()
            .collect();
        let losing_games_with_turns: Vec<&Game> = games_with_turns.iter()
            .filter(|g| g.game_winner == "opponent")
            .copied()
            .collect();
        
        let win_turns: i32 = winning_games_with_turns.iter().map(|g| g.turns.unwrap()).sum();
        let loss_turns: i32 = losing_games_with_turns.iter().map(|g| g.turns.unwrap()).sum();
        
        let avg_win_turns = if !winning_games_with_turns.is_empty() { 
            win_turns as f64 / winning_games_with_turns.len() as f64 
        } else { 0.0 };
        let avg_loss_turns = if !losing_games_with_turns.is_empty() { 
            loss_turns as f64 / losing_games_with_turns.len() as f64 
        } else { 0.0 };
        
        println!("  Average Game Length: {:.1} turns (wins: {:.1}, losses: {:.1}) [{}/{} games with turn data]", 
                 avg_turns, avg_win_turns, avg_loss_turns, games_with_turns.len(), total_games);
    } else {
        println!("  Average Game Length: No turn data available");
    }
    println!();
}

/// Extract archetype from deck name (ignoring subtype)
/// Examples: "Reanimator: UB" -> "Reanimator", "Lands" -> "Lands"
fn extract_archetype(deck_name: &str) -> String {
    let (archetype, _subtype) = parse_deck_name(deck_name);
    archetype.to_string()
}

fn show_sliced_stats(all_matches: &[Match], all_games: &[Game], slice_type: &str, min_games: i64) {
    match slice_type {
        "my-deck" => {
            println!("=== Statistics by My Deck ===");
            let mut deck_stats: std::collections::HashMap<String, Vec<&Match>> = std::collections::HashMap::new();
            for m in all_matches {
                deck_stats.entry(m.deck_name.clone()).or_default().push(m);
            }

            let mut deck_vec: Vec<_> = deck_stats.into_iter()
                .map(|(deck, matches)| {
                    let wins = matches.iter().filter(|m| m.match_winner == "me").count();
                    let total = matches.len();
                    let win_rate = if total > 0 { (wins as f64 / total as f64) * 100.0 } else { 0.0 };

                    // Get games for these matches
                    let match_ids: Vec<i32> = matches.iter().map(|m| m.match_id).collect();
                    let games: Vec<&Game> = all_games.iter().filter(|g| match_ids.contains(&g.match_id)).collect();

                    // Calculate average mulligans
                    let total_mulligans: i32 = games.iter().map(|g| g.mulligans).sum();
                    let avg_mulligans = if !games.is_empty() { total_mulligans as f64 / games.len() as f64 } else { 0.0 };

                    // Calculate average game length
                    let games_with_turns: Vec<&&Game> = games.iter().filter(|g| g.turns.is_some()).collect();
                    let avg_turns = if !games_with_turns.is_empty() {
                        let total_turns: i32 = games_with_turns.iter().map(|g| g.turns.unwrap()).sum();
                        Some(total_turns as f64 / games_with_turns.len() as f64)
                    } else {
                        None
                    };

                    (deck, matches, wins, total, win_rate, avg_mulligans, avg_turns)
                })
                .collect();

            // Sort by total games descending, then by win rate descending
            deck_vec.sort_by(|a, b| {
                b.3.cmp(&a.3)
                    .then_with(|| b.4.partial_cmp(&a.4).unwrap_or(std::cmp::Ordering::Equal))
            });

            for (deck, _matches, wins, total, win_rate, avg_mulligans, avg_turns) in deck_vec {
                // Apply minimum games filter
                if total < min_games as usize {
                    continue;
                }
                let turns_str = avg_turns.map(|t| format!("{:.1}", t)).unwrap_or_else(|| "-".to_string());
                println!("  with {}: {}-{} ({:.1}%, avg mulls: {:.2}, avg turns: {})",
                         deck, wins, total - wins, win_rate, avg_mulligans, turns_str);
            }
        },
        
        "opponent" => {
            println!("=== Statistics by Opponent ===");
            let mut opponent_stats: std::collections::HashMap<String, Vec<&Match>> = std::collections::HashMap::new();
            for m in all_matches {
                opponent_stats.entry(m.opponent_name.clone()).or_default().push(m);
            }

            let mut opponent_vec: Vec<_> = opponent_stats.into_iter()
                .map(|(opponent, matches)| {
                    let wins = matches.iter().filter(|m| m.match_winner == "me").count();
                    let total = matches.len();
                    let win_rate = if total > 0 { (wins as f64 / total as f64) * 100.0 } else { 0.0 };

                    // Get games for these matches
                    let match_ids: Vec<i32> = matches.iter().map(|m| m.match_id).collect();
                    let games: Vec<&Game> = all_games.iter().filter(|g| match_ids.contains(&g.match_id)).collect();

                    // Calculate average mulligans
                    let total_mulligans: i32 = games.iter().map(|g| g.mulligans).sum();
                    let avg_mulligans = if !games.is_empty() { total_mulligans as f64 / games.len() as f64 } else { 0.0 };

                    // Calculate average game length
                    let games_with_turns: Vec<&&Game> = games.iter().filter(|g| g.turns.is_some()).collect();
                    let avg_turns = if !games_with_turns.is_empty() {
                        let total_turns: i32 = games_with_turns.iter().map(|g| g.turns.unwrap()).sum();
                        Some(total_turns as f64 / games_with_turns.len() as f64)
                    } else {
                        None
                    };

                    (opponent, matches, wins, total, win_rate, avg_mulligans, avg_turns)
                })
                .collect();

            // Sort by total games descending, then by win rate descending
            opponent_vec.sort_by(|a, b| {
                b.3.cmp(&a.3)
                    .then_with(|| b.4.partial_cmp(&a.4).unwrap_or(std::cmp::Ordering::Equal))
            });

            for (opponent, _matches, wins, total, win_rate, avg_mulligans, avg_turns) in opponent_vec {
                let turns_str = avg_turns.map(|t| format!("{:.1}", t)).unwrap_or_else(|| "-".to_string());
                println!("  vs {}: {}-{} ({:.1}%, avg mulls: {:.2}, avg turns: {})",
                         opponent, wins, total - wins, win_rate, avg_mulligans, turns_str);
            }
        },
        
        "opponent-deck" => {
            println!("=== Statistics by Opponent Deck ===");
            let mut deck_stats: std::collections::HashMap<String, Vec<&Match>> = std::collections::HashMap::new();
            for m in all_matches {
                deck_stats.entry(m.opponent_deck.clone()).or_default().push(m);
            }

            let mut deck_vec: Vec<_> = deck_stats.into_iter()
                .map(|(deck, matches)| {
                    let wins = matches.iter().filter(|m| m.match_winner == "me").count();
                    let total = matches.len();
                    let win_rate = if total > 0 { (wins as f64 / total as f64) * 100.0 } else { 0.0 };

                    // Get games for these matches
                    let match_ids: Vec<i32> = matches.iter().map(|m| m.match_id).collect();
                    let games: Vec<&Game> = all_games.iter().filter(|g| match_ids.contains(&g.match_id)).collect();

                    // Calculate average mulligans
                    let total_mulligans: i32 = games.iter().map(|g| g.mulligans).sum();
                    let avg_mulligans = if !games.is_empty() { total_mulligans as f64 / games.len() as f64 } else { 0.0 };

                    // Calculate average game length
                    let games_with_turns: Vec<&&Game> = games.iter().filter(|g| g.turns.is_some()).collect();
                    let avg_turns = if !games_with_turns.is_empty() {
                        let total_turns: i32 = games_with_turns.iter().map(|g| g.turns.unwrap()).sum();
                        Some(total_turns as f64 / games_with_turns.len() as f64)
                    } else {
                        None
                    };

                    (deck, matches, wins, total, win_rate, avg_mulligans, avg_turns)
                })
                .collect();

            // Sort by total games descending, then by win rate descending
            deck_vec.sort_by(|a, b| {
                b.3.cmp(&a.3)
                    .then_with(|| b.4.partial_cmp(&a.4).unwrap_or(std::cmp::Ordering::Equal))
            });

            for (deck, _matches, wins, total, win_rate, avg_mulligans, avg_turns) in deck_vec {
                let turns_str = avg_turns.map(|t| format!("{:.1}", t)).unwrap_or_else(|| "-".to_string());
                println!("  vs {}: {}-{} ({:.1}%, avg mulls: {:.2}, avg turns: {})",
                         deck, wins, total - wins, win_rate, avg_mulligans, turns_str);
            }
        },
        
        "deck-category" => {
            println!("=== Statistics by Deck Category ===");
            let mut category_stats: std::collections::HashMap<DeckCategory, Vec<&Match>> = std::collections::HashMap::new();
            for m in all_matches {
                let category = categorize_deck(&m.opponent_deck);
                category_stats.entry(category).or_default().push(m);
            }

            let mut category_vec: Vec<_> = category_stats.into_iter()
                .map(|(category, matches)| {
                    let wins = matches.iter().filter(|m| m.match_winner == "me").count();
                    let total = matches.len();
                    let win_rate = if total > 0 { (wins as f64 / total as f64) * 100.0 } else { 0.0 };

                    // Get games for these matches
                    let match_ids: Vec<i32> = matches.iter().map(|m| m.match_id).collect();
                    let games: Vec<&Game> = all_games.iter().filter(|g| match_ids.contains(&g.match_id)).collect();

                    // Calculate average mulligans
                    let total_mulligans: i32 = games.iter().map(|g| g.mulligans).sum();
                    let avg_mulligans = if !games.is_empty() { total_mulligans as f64 / games.len() as f64 } else { 0.0 };

                    // Calculate average game length
                    let games_with_turns: Vec<&&Game> = games.iter().filter(|g| g.turns.is_some()).collect();
                    let avg_turns = if !games_with_turns.is_empty() {
                        let total_turns: i32 = games_with_turns.iter().map(|g| g.turns.unwrap()).sum();
                        Some(total_turns as f64 / games_with_turns.len() as f64)
                    } else {
                        None
                    };

                    (category, wins, total, win_rate, avg_mulligans, avg_turns)
                })
                .collect();

            // Sort by total games descending, then by win rate descending
            category_vec.sort_by(|a, b| {
                b.2.cmp(&a.2)
                    .then_with(|| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal))
            });

            for (category, wins, total, win_rate, avg_mulligans, avg_turns) in category_vec {
                let turns_str = avg_turns.map(|t| format!("{:.1}", t)).unwrap_or_else(|| "-".to_string());
                println!("  vs {} decks: {}-{} ({:.1}%, avg mulls: {:.2}, avg turns: {})",
                         category.to_string(), wins, total - wins, win_rate, avg_mulligans, turns_str);
            }
        },

        "my-deck-archetype" => {
            println!("=== Statistics by My Deck Archetype ===");
            let mut archetype_stats: std::collections::HashMap<String, Vec<&Match>> = std::collections::HashMap::new();
            for m in all_matches {
                let archetype = extract_archetype(&m.deck_name);
                archetype_stats.entry(archetype).or_default().push(m);
            }

            let mut archetype_vec: Vec<_> = archetype_stats.into_iter()
                .map(|(archetype, matches)| {
                    let wins = matches.iter().filter(|m| m.match_winner == "me").count();
                    let total = matches.len();
                    let win_rate = if total > 0 { (wins as f64 / total as f64) * 100.0 } else { 0.0 };

                    // Get games for these matches
                    let match_ids: Vec<i32> = matches.iter().map(|m| m.match_id).collect();
                    let games: Vec<&Game> = all_games.iter().filter(|g| match_ids.contains(&g.match_id)).collect();

                    // Calculate average mulligans
                    let total_mulligans: i32 = games.iter().map(|g| g.mulligans).sum();
                    let avg_mulligans = if !games.is_empty() { total_mulligans as f64 / games.len() as f64 } else { 0.0 };

                    // Calculate average game length
                    let games_with_turns: Vec<&&Game> = games.iter().filter(|g| g.turns.is_some()).collect();
                    let avg_turns = if !games_with_turns.is_empty() {
                        let total_turns: i32 = games_with_turns.iter().map(|g| g.turns.unwrap()).sum();
                        Some(total_turns as f64 / games_with_turns.len() as f64)
                    } else {
                        None
                    };

                    (archetype, wins, total, win_rate, avg_mulligans, avg_turns)
                })
                .collect();

            // Sort by total games descending, then by win rate descending
            archetype_vec.sort_by(|a, b| {
                b.2.cmp(&a.2)
                    .then_with(|| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal))
            });

            for (archetype, wins, total, win_rate, avg_mulligans, avg_turns) in archetype_vec {
                let turns_str = avg_turns.map(|t| format!("{:.1}", t)).unwrap_or_else(|| "-".to_string());
                println!("  with {}: {}-{} ({:.1}%, avg mulls: {:.2}, avg turns: {})",
                         archetype, wins, total - wins, win_rate, avg_mulligans, turns_str);
            }
        },

        "opponent-deck-archetype" => {
            println!("=== Statistics by Opponent Deck Archetype ===");
            let mut archetype_stats: std::collections::HashMap<String, Vec<&Match>> = std::collections::HashMap::new();
            for m in all_matches {
                let archetype = extract_archetype(&m.opponent_deck);
                archetype_stats.entry(archetype).or_default().push(m);
            }

            let mut archetype_vec: Vec<_> = archetype_stats.into_iter()
                .map(|(archetype, matches)| {
                    let wins = matches.iter().filter(|m| m.match_winner == "me").count();
                    let total = matches.len();
                    let win_rate = if total > 0 { (wins as f64 / total as f64) * 100.0 } else { 0.0 };

                    // Get games for these matches
                    let match_ids: Vec<i32> = matches.iter().map(|m| m.match_id).collect();
                    let games: Vec<&Game> = all_games.iter().filter(|g| match_ids.contains(&g.match_id)).collect();

                    // Calculate average mulligans
                    let total_mulligans: i32 = games.iter().map(|g| g.mulligans).sum();
                    let avg_mulligans = if !games.is_empty() { total_mulligans as f64 / games.len() as f64 } else { 0.0 };

                    // Calculate average game length
                    let games_with_turns: Vec<&&Game> = games.iter().filter(|g| g.turns.is_some()).collect();
                    let avg_turns = if !games_with_turns.is_empty() {
                        let total_turns: i32 = games_with_turns.iter().map(|g| g.turns.unwrap()).sum();
                        Some(total_turns as f64 / games_with_turns.len() as f64)
                    } else {
                        None
                    };

                    (archetype, wins, total, win_rate, avg_mulligans, avg_turns)
                })
                .collect();

            // Sort by total games descending, then by win rate descending
            archetype_vec.sort_by(|a, b| {
                b.2.cmp(&a.2)
                    .then_with(|| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal))
            });

            for (archetype, wins, total, win_rate, avg_mulligans, avg_turns) in archetype_vec {
                let turns_str = avg_turns.map(|t| format!("{:.1}", t)).unwrap_or_else(|| "-".to_string());
                println!("  vs {}: {}-{} ({:.1}%, avg mulls: {:.2}, avg turns: {})",
                         archetype, wins, total - wins, win_rate, avg_mulligans, turns_str);
            }
        },

        "game-number" => {
            println!("=== Statistics by Game Number ===");
            let mut game_stats: std::collections::HashMap<i32, Vec<&Game>> = std::collections::HashMap::new();
            for g in all_games {
                game_stats.entry(g.game_number).or_default().push(g);
            }

            let mut game_vec: Vec<_> = (1..=3)
                .filter_map(|game_num| {
                    game_stats.get(&game_num).map(|games| {
                        let wins = games.iter().filter(|g| g.game_winner == "me").count();
                        let total = games.len();
                        let win_rate = if total > 0 { (wins as f64 / total as f64) * 100.0 } else { 0.0 };

                        // Calculate average mulligans
                        let total_mulligans: i32 = games.iter().map(|g| g.mulligans).sum();
                        let avg_mulligans = if !games.is_empty() { total_mulligans as f64 / games.len() as f64 } else { 0.0 };

                        // Calculate average game length
                        let games_with_turns: Vec<&&Game> = games.iter().filter(|g| g.turns.is_some()).collect();
                        let avg_turns = if !games_with_turns.is_empty() {
                            let total_turns: i32 = games_with_turns.iter().map(|g| g.turns.unwrap()).sum();
                            Some(total_turns as f64 / games_with_turns.len() as f64)
                        } else {
                            None
                        };

                        (game_num, wins, total, win_rate, avg_mulligans, avg_turns)
                    })
                })
                .collect();

            // Sort by total games descending, then by win rate descending
            game_vec.sort_by(|a, b| {
                b.2.cmp(&a.2)
                    .then_with(|| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal))
            });

            for (game_num, wins, total, win_rate, avg_mulligans, avg_turns) in game_vec {
                let turns_str = avg_turns.map(|t| format!("{:.1}", t)).unwrap_or_else(|| "-".to_string());
                println!("  Game {}: {}-{} ({:.1}%, avg mulls: {:.2}, avg turns: {})",
                         game_num, wins, total - wins, win_rate, avg_mulligans, turns_str);
            }
        },
        
        "mulligans" => {
            println!("=== Statistics by Mulligan Count ===");
            let mut mulligan_stats: std::collections::HashMap<i32, Vec<&Game>> = std::collections::HashMap::new();
            for g in all_games {
                mulligan_stats.entry(g.mulligans).or_default().push(g);
            }

            let mut mulligan_vec: Vec<_> = mulligan_stats.into_iter()
                .map(|(mulligans, games)| {
                    let wins = games.iter().filter(|g| g.game_winner == "me").count();
                    let total = games.len();
                    let win_rate = if total > 0 { (wins as f64 / total as f64) * 100.0 } else { 0.0 };

                    // Calculate average game length (but not average mulligans since we're slicing by that)
                    let games_with_turns: Vec<&&Game> = games.iter().filter(|g| g.turns.is_some()).collect();
                    let avg_turns = if !games_with_turns.is_empty() {
                        let total_turns: i32 = games_with_turns.iter().map(|g| g.turns.unwrap()).sum();
                        Some(total_turns as f64 / games_with_turns.len() as f64)
                    } else {
                        None
                    };

                    (mulligans, wins, total, win_rate, avg_turns)
                })
                .collect();

            // Sort by total games descending, then by win rate descending
            mulligan_vec.sort_by(|a, b| {
                b.2.cmp(&a.2)
                    .then_with(|| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal))
            });

            for (mulligans, wins, total, win_rate, avg_turns) in mulligan_vec {
                let turns_str = avg_turns.map(|t| format!("{:.1}", t)).unwrap_or_else(|| "-".to_string());
                println!("  {} mulligans: {}-{} ({:.1}%, avg turns: {})",
                         mulligans, wins, total - wins, win_rate, turns_str);
            }
        },
        
        "game-plan" => {
            println!("=== Statistics by Game Plan ===");
            let mut plan_stats: std::collections::HashMap<String, Vec<&Game>> = std::collections::HashMap::new();
            for g in all_games {
                let plan = g.opening_hand_plan.as_deref().unwrap_or("No Plan");
                plan_stats.entry(plan.to_string()).or_default().push(g);
            }

            let mut plan_vec: Vec<_> = plan_stats.into_iter()
                .map(|(plan, games)| {
                    let wins = games.iter().filter(|g| g.game_winner == "me").count();
                    let total = games.len();
                    let win_rate = if total > 0 { (wins as f64 / total as f64) * 100.0 } else { 0.0 };

                    // Calculate average mulligans
                    let total_mulligans: i32 = games.iter().map(|g| g.mulligans).sum();
                    let avg_mulligans = if !games.is_empty() { total_mulligans as f64 / games.len() as f64 } else { 0.0 };

                    // Calculate average game length
                    let games_with_turns: Vec<&&Game> = games.iter().filter(|g| g.turns.is_some()).collect();
                    let avg_turns = if !games_with_turns.is_empty() {
                        let total_turns: i32 = games_with_turns.iter().map(|g| g.turns.unwrap()).sum();
                        Some(total_turns as f64 / games_with_turns.len() as f64)
                    } else {
                        None
                    };

                    (plan, wins, total, win_rate, avg_mulligans, avg_turns)
                })
                .collect();

            // Sort by total games descending, then by win rate descending
            plan_vec.sort_by(|a, b| {
                b.2.cmp(&a.2)
                    .then_with(|| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal))
            });

            for (plan, wins, total, win_rate, avg_mulligans, avg_turns) in plan_vec {
                let turns_str = avg_turns.map(|t| format!("{:.1}", t)).unwrap_or_else(|| "-".to_string());
                println!("  {}: {}-{} ({:.1}%, avg mulls: {:.2}, avg turns: {})",
                         plan, wins, total - wins, win_rate, avg_mulligans, turns_str);
            }
        },
        
        "win-condition" => {
            println!("=== Statistics by Win Condition ===");
            let mut win_con_stats: std::collections::HashMap<String, Vec<&Game>> = std::collections::HashMap::new();

            // Only count games you won (where win_condition is relevant)
            for g in all_games.iter().filter(|g| g.game_winner == "me") {
                let win_con = g.win_condition.as_deref().unwrap_or("Unknown");
                win_con_stats.entry(win_con.to_string()).or_default().push(g);
            }

            let mut win_con_vec: Vec<_> = win_con_stats.into_iter()
                .map(|(win_con, games)| {
                    let wins = games.len(); // All games here are wins

                    // Calculate average mulligans
                    let total_mulligans: i32 = games.iter().map(|g| g.mulligans).sum();
                    let avg_mulligans = if !games.is_empty() { total_mulligans as f64 / games.len() as f64 } else { 0.0 };

                    // Calculate average game length
                    let games_with_turns: Vec<&&Game> = games.iter().filter(|g| g.turns.is_some()).collect();
                    let avg_turns = if !games_with_turns.is_empty() {
                        let total_turns: i32 = games_with_turns.iter().map(|g| g.turns.unwrap()).sum();
                        Some(total_turns as f64 / games_with_turns.len() as f64)
                    } else {
                        None
                    };

                    (win_con, wins, avg_mulligans, avg_turns)
                })
                .collect();

            // Sort by total usage descending
            win_con_vec.sort_by(|a, b| b.1.cmp(&a.1));

            for (win_con, wins, avg_mulligans, avg_turns) in win_con_vec {
                let turns_str = avg_turns.map(|t| format!("{:.1}", t)).unwrap_or_else(|| "-".to_string());
                println!("  {}: {} wins (avg mulls: {:.2}, avg turns: {})",
                         win_con, wins, avg_mulligans, turns_str);
            }
        },
        
        "game-length" => {
            println!("=== Statistics by Game Length ===");
            let mut length_stats: std::collections::HashMap<String, Vec<&Game>> = std::collections::HashMap::new();

            for g in all_games {
                let length_category = match g.turns {
                    None => "No turn data".to_string(),
                    Some(turns) => {
                        match turns {
                            1..=3 => "Very Short (1-3 turns)".to_string(),
                            4..=6 => "Short (4-6 turns)".to_string(),
                            7..=10 => "Medium (7-10 turns)".to_string(),
                            11..=15 => "Long (11-15 turns)".to_string(),
                            _ => "Very Long (16+ turns)".to_string(),
                        }
                    }
                };
                length_stats.entry(length_category).or_default().push(g);
            }

            let mut length_vec: Vec<_> = length_stats.into_iter()
                .map(|(category, games)| {
                    let wins = games.iter().filter(|g| g.game_winner == "me").count();
                    let total = games.len();
                    let win_rate = if total > 0 { (wins as f64 / total as f64) * 100.0 } else { 0.0 };

                    // Calculate average mulligans (but not average game length since we're slicing by that)
                    let total_mulligans: i32 = games.iter().map(|g| g.mulligans).sum();
                    let avg_mulligans = if !games.is_empty() { total_mulligans as f64 / games.len() as f64 } else { 0.0 };

                    // Calculate average turns for this category (excluding "No turn data")
                    let avg_turns = if category == "No turn data" {
                        None
                    } else {
                        let turns_sum: i32 = games.iter().filter_map(|g| g.turns).sum();
                        let turns_count = games.iter().filter(|g| g.turns.is_some()).count();
                        if turns_count > 0 {
                            Some(turns_sum as f64 / turns_count as f64)
                        } else {
                            None
                        }
                    };

                    (category, wins, total, win_rate, avg_mulligans, avg_turns)
                })
                .collect();

            // Sort by game length order (Very Short -> Very Long)
            length_vec.sort_by(|a, b| {
                let order_a = match a.0.as_str() {
                    "Very Short (1-3 turns)" => 0,
                    "Short (4-6 turns)" => 1,
                    "Medium (7-10 turns)" => 2,
                    "Long (11-15 turns)" => 3,
                    "Very Long (16+ turns)" => 4,
                    "No turn data" => 5,
                    _ => 6,
                };
                let order_b = match b.0.as_str() {
                    "Very Short (1-3 turns)" => 0,
                    "Short (4-6 turns)" => 1,
                    "Medium (7-10 turns)" => 2,
                    "Long (11-15 turns)" => 3,
                    "Very Long (16+ turns)" => 4,
                    "No turn data" => 5,
                    _ => 6,
                };
                order_a.cmp(&order_b)
            });

            for (category, wins, total, win_rate, avg_mulligans, avg_turns) in length_vec {
                if let Some(avg) = avg_turns {
                    println!("  {}: {}-{} ({:.1}%, avg mulls: {:.2}, avg {:.1} turns)",
                             category, wins, total - wins, win_rate, avg_mulligans, avg);
                } else {
                    println!("  {}: {}-{} ({:.1}%, avg mulls: {:.2})",
                             category, wins, total - wins, win_rate, avg_mulligans);
                }
            }
        },

        "era" => {
            println!("=== Statistics by Era ===");
            let mut era_stats: std::collections::HashMap<Option<i32>, Vec<&Match>> = std::collections::HashMap::new();
            for m in all_matches {
                era_stats.entry(m.era).or_default().push(m);
            }

            let mut era_vec: Vec<_> = era_stats.into_iter()
                .map(|(era, matches)| {
                    let wins = matches.iter().filter(|m| m.match_winner == "me").count();
                    let total = matches.len();
                    let win_rate = if total > 0 { (wins as f64 / total as f64) * 100.0 } else { 0.0 };

                    // Get games for these matches
                    let match_ids: Vec<i32> = matches.iter().map(|m| m.match_id).collect();
                    let games: Vec<&Game> = all_games.iter().filter(|g| match_ids.contains(&g.match_id)).collect();

                    // Calculate average mulligans
                    let total_mulligans: i32 = games.iter().map(|g| g.mulligans).sum();
                    let avg_mulligans = if !games.is_empty() { total_mulligans as f64 / games.len() as f64 } else { 0.0 };

                    // Calculate average game length
                    let games_with_turns: Vec<&&Game> = games.iter().filter(|g| g.turns.is_some()).collect();
                    let avg_turns = if !games_with_turns.is_empty() {
                        let total_turns: i32 = games_with_turns.iter().map(|g| g.turns.unwrap()).sum();
                        Some(total_turns as f64 / games_with_turns.len() as f64)
                    } else {
                        None
                    };

                    (era, wins, total, win_rate, avg_mulligans, avg_turns)
                })
                .collect();

            // Sort by era number ascending (with None at the end)
            era_vec.sort_by(|a, b| {
                match (a.0, b.0) {
                    (Some(a_era), Some(b_era)) => a_era.cmp(&b_era),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                }
            });

            for (era, wins, total, win_rate, avg_mulligans, avg_turns) in era_vec {
                let era_str = era.map(|e| e.to_string()).unwrap_or_else(|| "Unknown".to_string());
                let turns_str = avg_turns.map(|t| format!("{:.1}", t)).unwrap_or_else(|| "-".to_string());
                println!("  Era {}: {}-{} ({:.1}%, avg mulls: {:.2}, avg turns: {})",
                         era_str, wins, total - wins, win_rate, avg_mulligans, turns_str);
            }
        },

        _ => {
            println!("Unknown slice type: {}. Available options: my-deck, my-deck-archetype, opponent, opponent-deck, opponent-deck-archetype, deck-category, game-number, mulligans, game-plan, win-condition, game-length, era", slice_type);
        }
    }
    println!();
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
    } else {
        game_data.win_condition = None;
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

fn parse_era_from_deck_name(name: &str) -> Option<i32> {
    // Parse patterns:
    // - "name-X.Y" -> era = X (e.g., "sprouts-1.2" -> 1)
    // - "name-X" -> era = X (e.g., "sprouts-1" -> 1)
    if let Some(dash_pos) = name.rfind('-') {
        let after_dash = &name[dash_pos + 1..];
        // Check if there's a dot (major.minor version)
        if let Some(dot_pos) = after_dash.find('.') {
            let era_str = &after_dash[..dot_pos];
            return era_str.parse::<i32>().ok();
        } else {
            // Try to parse the whole thing as an integer
            return after_dash.parse::<i32>().ok();
        }
    }
    None
}
