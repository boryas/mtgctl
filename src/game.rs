use clap::{Args, Subcommand};
use dialoguer::{Input, Confirm, MultiSelect};
use skim::prelude::*;
use std::io::Cursor;
use chrono::{Local, NaiveDate};
use diesel::prelude::*;
use std::fs;
use std::collections::HashMap;
use std::path::Path;
use serde::Deserialize;
use comfy_table::{Table, Cell, Attribute, ContentArrangement};
use textplots::{Chart, LabelBuilder, LabelFormat, Plot, Shape, TickDisplay, TickDisplayBuilder};

use crate::db::{establish_connection, models::*};
use crate::db::schema::{matches, games, doomsday_games, leagues};

/// Collected game data before DB insertion
struct CollectedGame {
    game_number: i32,
    play_draw: String,
    mulligans: i32,
    opening_hand_plan: Option<String>,
    game_winner: String,
    win_condition: Option<String>,
    loss_reason: Option<String>,
    turns: Option<i32>,
    doomsday_data: Option<CollectedDoomsdayData>,
}

/// Collected doomsday-specific data before DB insertion
struct CollectedDoomsdayData {
    dd_intent: bool,
    doomsday_resolved: bool,
    pile_type: Option<String>,
    better_pile: Option<bool>,
    no_doomsday_reason: Option<String>,
    sb_juke_plan: Option<String>,
    // If set, overrides the game's win_condition
    win_condition_override: Option<String>,
    // If set, overrides the game's loss_reason
    loss_reason_override: Option<String>,
    pile_disruption: Option<String>,
}


/// Fuzzy select helper using skim - returns selected item or typed query, None if aborted
/// Pure decision logic for fuzzy select - testable without skim dependencies
/// Returns: the value to use based on query, selection, and whether Tab was pressed
fn fuzzy_select_decision(query: &str, selected: Option<&str>, tab_pressed: bool) -> Option<String> {
    let query = query.trim();

    // Tab pressed = use query as new entry
    if tab_pressed {
        return if query.is_empty() { None } else { Some(query.to_string()) };
    }

    match selected {
        Some(sel) => Some(sel.to_string()),
        None => {
            // No selection - use the query as the value
            if query.is_empty() { None } else { Some(query.to_string()) }
        }
    }
}

/// Handle skim output - Tab uses query as-is, Enter uses selection
fn handle_skim_output(output: skim::prelude::SkimOutput) -> Option<String> {
    let query = output.query.as_str();
    let selected = output.selected_items.first().map(|item| item.output());
    let tab_pressed = output.final_key == skim::prelude::Key::Tab
        || output.final_key == skim::prelude::Key::BackTab;

    fuzzy_select_decision(query, selected.as_ref().map(|s| s.as_ref()), tab_pressed)
}

fn fuzzy_select(prompt: &str, options: &[String]) -> Option<String> {
    if options.is_empty() {
        // No options - fall back to text input
        let result: String = Input::new()
            .with_prompt(prompt)
            .allow_empty(true)
            .interact_text()
            .ok()?;
        return if result.is_empty() { None } else { Some(result) };
    }

    let prompt_str = format!("{} (Tab=new): ", prompt);
    let skim_options = SkimOptionsBuilder::default()
        .prompt(Some(&prompt_str))
        .expect(Some("tab,btab".to_owned()))
        .build()
        .unwrap();

    let input = options.join("\n");
    let item_reader = SkimItemReader::default();
    let items = item_reader.of_bufread(Cursor::new(input));

    match Skim::run_with(&skim_options, Some(items)) {
        Some(output) if !output.is_abort => handle_skim_output(output),
        _ => None, // Aborted
    }
}

/// Fuzzy select with a default value pre-filled in the query
fn fuzzy_select_with_default(prompt: &str, options: &[String], default: &str) -> Option<String> {
    if options.is_empty() {
        let result: String = Input::new()
            .with_prompt(prompt)
            .default(default.to_string())
            .interact_text()
            .ok()?;
        return if result.is_empty() { None } else { Some(result) };
    }

    let prompt_str = format!("{} (Tab=new): ", prompt);
    let skim_options = SkimOptionsBuilder::default()
        .prompt(Some(&prompt_str))
        .query(Some(default))
        .expect(Some("tab,btab".to_owned()))
        .build()
        .unwrap();

    let input = options.join("\n");
    let item_reader = SkimItemReader::default();
    let items = item_reader.of_bufread(Cursor::new(input));

    match Skim::run_with(&skim_options, Some(items)) {
        Some(output) if !output.is_abort => handle_skim_output(output),
        _ => None,
    }
}

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
    // Doomsday-specific fields (optional, only present in doomsday.toml)
    #[serde(default)]
    common_pile_types: Vec<String>,
    #[serde(default)]
    no_doomsday_reasons: Vec<String>,
    #[serde(default)]
    non_doomsday_wincons: Vec<String>,
    #[serde(default)]
    pile_disruption: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SubtypeDefinition {
    #[serde(default)]
    game_plans: Vec<String>,
    #[serde(default)]
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
    default_filters: Vec<String>,
    #[serde(default)]
    default_groupbys: Vec<String>,
    #[serde(default)]
    default_statistics: Vec<String>,
    #[serde(default = "default_chart_bucket_size")]
    chart_bucket_size: usize,
    #[serde(default = "default_chart_smoothing")]
    chart_smoothing: usize,
}

fn default_chart_bucket_size() -> usize { 10 }
fn default_chart_smoothing() -> usize { 5 }

#[derive(Debug, Deserialize, Default)]
struct StatsFilters {
    era: Option<i32>,
    my_deck: Option<String>,
    opponent: Option<String>,
    opponent_deck: Option<String>,
    event_type: Option<String>,
}

/// Runtime filter selection - holds all possible filter values
#[derive(Debug, Clone, Default)]
struct FilterSelection {
    // Match-level filters
    era_values: Option<Vec<i32>>,
    deck_name: Option<String>,
    opponent_name: Option<String>,
    opponent_deck: Option<String>,
    opponent_deck_archetype: Option<String>,
    opponent_deck_category: Option<String>,
    event_type: Option<String>,
    // Game-level filters
    loss_reason: Option<String>,
    win_condition: Option<String>,
    game_plan: Option<String>,
    mulligan_count: Option<i32>,
    game_length: Option<(i32, i32)>,
    game_number: Option<Vec<i32>>,
    play_draw: Option<String>,
}

impl FilterSelection {
    /// Get active filter descriptions for display
    fn active_filter_descriptions(&self) -> Vec<String> {
        let mut filters = Vec::new();
        if let Some(ref eras) = self.era_values {
            let era_str = eras.iter().map(|e| e.to_string()).collect::<Vec<_>>().join(", ");
            filters.push(format!("Era: {}", era_str));
        }
        if let Some(ref deck) = self.deck_name {
            filters.push(format!("Deck: {}", deck));
        }
        if let Some(ref opp) = self.opponent_name {
            filters.push(format!("Opponent: {}", opp));
        }
        if let Some(ref opp_deck) = self.opponent_deck {
            filters.push(format!("Vs Deck: {}", opp_deck));
        }
        if let Some(ref opp_arch) = self.opponent_deck_archetype {
            filters.push(format!("Vs Archetype: {}", opp_arch));
        }
        if let Some(ref opp_cat) = self.opponent_deck_category {
            filters.push(format!("Vs Category: {}", opp_cat));
        }
        if let Some(ref ev_type) = self.event_type {
            filters.push(format!("Event: {}", ev_type));
        }
        if let Some(ref reason) = self.loss_reason {
            filters.push(format!("Loss Reason: {}", reason));
        }
        if let Some(ref condition) = self.win_condition {
            filters.push(format!("Win Condition: {}", condition));
        }
        if let Some(ref plan) = self.game_plan {
            filters.push(format!("Game Plan: {}", plan));
        }
        if let Some(count) = self.mulligan_count {
            filters.push(format!("Mulligans: {}", count));
        }
        if let Some((min, max)) = self.game_length {
            filters.push(format!("Turns: {}-{}", min, max));
        }
        if let Some(ref nums) = self.game_number {
            let nums_str = nums.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(", ");
            filters.push(format!("Game: {}", nums_str));
        }
        if let Some(ref pd) = self.play_draw {
            filters.push(format!("Play/Draw: {}", pd));
        }
        filters
    }

    /// Load filtered matches and games from database
    fn load_filtered_data(&self, connection: &mut diesel::sqlite::SqliteConnection) -> (Vec<Match>, Vec<Game>) {
        use diesel::prelude::*;

        // Build match query with filters
        let mut query = matches::table.order(matches::date.asc()).into_boxed();

        if let Some(ref eras) = self.era_values {
            query = query.filter(matches::era.eq_any(eras));
        }
        if let Some(ref deck) = self.deck_name {
            query = query.filter(matches::deck_name.like(format!("%{}%", deck)));
        }
        if let Some(ref opp) = self.opponent_name {
            query = query.filter(matches::opponent_name.like(format!("%{}%", opp)));
        }
        if let Some(ref opp_deck) = self.opponent_deck {
            query = query.filter(matches::opponent_deck.like(format!("%{}%", opp_deck)));
        }
        if let Some(ref ev_type) = self.event_type {
            query = query.filter(matches::event_type.like(format!("%{}%", ev_type)));
        }

        let mut all_matches: Vec<Match> = query.load(connection).expect("Error loading matches");

        // Apply post-load filters for computed fields
        if let Some(ref arch_filter) = self.opponent_deck_archetype {
            all_matches.retain(|m| {
                let (archetype, _) = parse_deck_name(&m.opponent_deck);
                archetype == arch_filter
            });
        }
        if let Some(ref cat_filter) = self.opponent_deck_category {
            all_matches.retain(|m| {
                categorize_deck(&m.opponent_deck).to_string() == cat_filter
            });
        }

        if all_matches.is_empty() {
            return (Vec::new(), Vec::new());
        }

        // Load games for these matches
        let match_ids: Vec<i32> = all_matches.iter().map(|m| m.match_id).collect();
        let mut game_query = games::table.filter(games::match_id.eq_any(&match_ids)).into_boxed();

        // Apply game-level filters
        if let Some(ref reason) = self.loss_reason {
            game_query = game_query.filter(games::loss_reason.eq(reason));
        }
        if let Some(ref condition) = self.win_condition {
            game_query = game_query.filter(games::win_condition.eq(condition));
        }
        if let Some(ref plan) = self.game_plan {
            game_query = game_query.filter(games::opening_hand_plan.eq(plan));
        }
        if let Some(count) = self.mulligan_count {
            game_query = game_query.filter(games::mulligans.eq(count));
        }
        if let Some((min_turns, max_turns)) = self.game_length {
            game_query = game_query.filter(games::turns.ge(min_turns).and(games::turns.le(max_turns)));
        }
        if let Some(ref nums) = self.game_number {
            game_query = game_query.filter(games::game_number.eq_any(nums));
        }
        if let Some(ref pd) = self.play_draw {
            game_query = game_query.filter(games::play_draw.eq(pd));
        }

        let all_games: Vec<Game> = game_query.load(connection).expect("Error loading games");

        // If game-level filters applied, also filter matches to only those with matching games
        let has_game_filters = self.loss_reason.is_some()
            || self.win_condition.is_some()
            || self.game_plan.is_some()
            || self.mulligan_count.is_some()
            || self.game_length.is_some()
            || self.game_number.is_some()
            || self.play_draw.is_some();

        if has_game_filters {
            let filtered_match_ids: std::collections::HashSet<i32> = all_games.iter()
                .map(|g| g.match_id)
                .collect();
            all_matches.retain(|m| filtered_match_ids.contains(&m.match_id));
        }

        (all_matches, all_games)
    }
}

/// Interactive filter selection - shared by stats and graph commands
fn select_filters_interactive(connection: &mut diesel::sqlite::SqliteConnection) -> FilterSelection {
    let config = load_config();

    let filter_options = vec![
        "Era (latest only)",
        "Era (all)",
        "My Archetype",
        "My Subtype",
        "My List",
        "Opponent",
        "Opponent Deck",
        "Opponent Deck Archetype",
        "Opponent Deck Category",
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
    let df = &config.stats.default_filters;
    let filter_defaults = vec![
        config.stats.filters.era.is_some() || df.contains(&"era-latest".to_string()),
        df.contains(&"era-all".to_string()),
        config.stats.filters.my_deck.is_some() || df.contains(&"my-archetype".to_string()),
        df.contains(&"my-subtype".to_string()),
        df.contains(&"my-list".to_string()),
        config.stats.filters.opponent.is_some() || df.contains(&"opponent".to_string()),
        config.stats.filters.opponent_deck.is_some() || df.contains(&"opponent-deck".to_string()),
        df.contains(&"opponent-deck-archetype".to_string()),
        df.contains(&"opponent-deck-category".to_string()),
        config.stats.filters.event_type.is_some() || df.contains(&"event-type".to_string()),
        df.contains(&"loss-reason".to_string()),
        df.contains(&"win-condition".to_string()),
        df.contains(&"game-plan".to_string()),
        df.contains(&"mulligan-count".to_string()),
        df.contains(&"game-length".to_string()),
        df.contains(&"game-number".to_string()),
        df.contains(&"play-draw".to_string()),
    ];

    let selected_filters = MultiSelect::new()
        .with_prompt("Select filters (space to select, enter to continue)")
        .items(&filter_options)
        .defaults(&filter_defaults)
        .interact()
        .unwrap();

    // Fallback option lists from definitions (used when partial data has no entries)
    let mut all_loss_reasons = load_loss_reasons();
    let mut all_win_conditions = load_win_conditions();
    let mut all_game_plans = load_game_plans();

    // Build FilterSelection based on user choices
    let mut filters = FilterSelection::default();

    for &filter_idx in &selected_filters {
        // Load the dataset reflecting filters applied so far, for cumulative option narrowing
        let (partial_matches, partial_games) = filters.load_filtered_data(connection);
        match filter_idx {
            0 => {
                // Era (latest only)
                match get_default_era_filter(connection) {
                    EraFilter::Eras(eras) => filters.era_values = Some(eras),
                    EraFilter::All => {}
                }
            }
            1 => {
                // Era (all) - no filter
            }
            2 => {
                // My Archetype
                let mut archetypes = std::collections::HashSet::new();
                for m in &partial_matches {
                    let (archetype, _) = parse_deck_name(&m.deck_name);
                    archetypes.insert(archetype.to_string());
                }
                let mut archetype_list: Vec<String> = archetypes.into_iter().collect();
                archetype_list.sort();
                let default_deck = config.stats.filters.my_deck.as_deref().unwrap_or("");
                if let Some(selected_archetype) = fuzzy_select_with_default("Select archetype to filter by", &archetype_list, default_deck) {
                    filters.deck_name = Some(selected_archetype.clone());
                    // Reload deck-specific options
                    if let Some(data) = load_archetype_data(&selected_archetype) {
                        if !data.win_conditions.is_empty() { all_win_conditions = data.win_conditions; }
                        if !data.loss_reasons.is_empty() { all_loss_reasons = data.loss_reasons; }
                        if !data.game_plans.is_empty() { all_game_plans = data.game_plans; }
                    }
                }
            }
            3 => {
                // My Subtype
                let mut subtypes = std::collections::HashSet::new();
                for m in &partial_matches {
                    let (_, subtype) = parse_deck_name(&m.deck_name);
                    if let Some(st) = subtype {
                        subtypes.insert(st.to_string());
                    }
                }
                let mut subtype_list: Vec<String> = subtypes.into_iter().collect();
                subtype_list.sort();
                if let Some(subtype) = fuzzy_select("Select subtype to filter by", &subtype_list) {
                    filters.deck_name = Some(format!(": {}", subtype));
                }
            }
            4 => {
                // My List
                let mut lists = std::collections::HashSet::new();
                for m in &partial_matches {
                    let deck = &m.deck_name;
                    if let Some(list_start) = deck.find(" (") {
                        if let Some(list_end) = deck.rfind(')') {
                            lists.insert(deck[list_start + 2..list_end].to_string());
                        }
                    }
                }
                let mut list_list: Vec<String> = lists.into_iter().collect();
                list_list.sort();
                if let Some(list) = fuzzy_select("Select list to filter by", &list_list) {
                    filters.deck_name = Some(format!("({})", list));
                }
            }
            5 => {
                // Opponent
                let opp_set: std::collections::HashSet<String> = partial_matches.iter().map(|m| m.opponent_name.clone()).collect();
                let mut opp_list: Vec<String> = opp_set.into_iter().collect();
                opp_list.sort();
                let default_opp = config.stats.filters.opponent.as_deref().unwrap_or("");
                filters.opponent_name = fuzzy_select_with_default("Select opponent to filter by", &opp_list, default_opp);
            }
            6 => {
                // Opponent Deck
                let deck_set: std::collections::HashSet<String> = partial_matches.iter()
                    .filter(|m| m.opponent_deck != "unknown")
                    .map(|m| m.opponent_deck.clone())
                    .collect();
                let mut deck_list: Vec<String> = deck_set.into_iter().collect();
                deck_list.sort();
                let default_deck = config.stats.filters.opponent_deck.as_deref().unwrap_or("");
                filters.opponent_deck = fuzzy_select_with_default("Select opponent deck to filter by", &deck_list, default_deck);
            }
            7 => {
                // Opponent Deck Archetype
                let mut arch_set = std::collections::HashSet::new();
                for m in &partial_matches {
                    let (archetype, _) = parse_deck_name(&m.opponent_deck);
                    arch_set.insert(archetype.to_string());
                }
                let mut arch_list: Vec<String> = arch_set.into_iter().collect();
                arch_list.sort();
                filters.opponent_deck_archetype = fuzzy_select("Select opponent deck archetype to filter by", &arch_list);
            }
            8 => {
                // Opponent Deck Category
                let cat_set: std::collections::HashSet<String> = partial_matches.iter()
                    .map(|m| categorize_deck(&m.opponent_deck).to_string().to_string())
                    .collect();
                let mut cat_list: Vec<String> = cat_set.into_iter().collect();
                cat_list.sort();
                filters.opponent_deck_category = fuzzy_select("Select opponent deck category to filter by", &cat_list);
            }
            9 => {
                // Event Type — show only types present in the partial dataset, in canonical order
                let event_set: std::collections::HashSet<String> = partial_matches.iter()
                    .map(|m| m.event_type.clone())
                    .collect();
                let available_event_types: Vec<String> = EVENT_TYPES.iter()
                    .filter(|t| event_set.contains(t.to_string().as_str()))
                    .map(|t| t.to_string())
                    .collect();
                let default_event = config.stats.filters.event_type.as_deref().unwrap_or("");
                filters.event_type = fuzzy_select_with_default("Select event type to filter by", &available_event_types, default_event);
            }
            10 => {
                // Loss Reason
                let reason_set: std::collections::HashSet<String> = partial_games.iter()
                    .filter_map(|g| g.loss_reason.as_ref())
                    .cloned()
                    .collect();
                let mut reason_list: Vec<String> = reason_set.into_iter().collect();
                reason_list.sort();
                if reason_list.is_empty() { reason_list = all_loss_reasons.clone(); }
                filters.loss_reason = fuzzy_select("Select loss reason to filter by", &reason_list);
            }
            11 => {
                // Win Condition
                let cond_set: std::collections::HashSet<String> = partial_games.iter()
                    .filter_map(|g| g.win_condition.as_ref())
                    .cloned()
                    .collect();
                let mut cond_list: Vec<String> = cond_set.into_iter().collect();
                cond_list.sort();
                if cond_list.is_empty() { cond_list = all_win_conditions.clone(); }
                filters.win_condition = fuzzy_select("Select win condition to filter by", &cond_list);
            }
            12 => {
                // Game Plan
                let plan_set: std::collections::HashSet<String> = partial_games.iter()
                    .filter_map(|g| g.opening_hand_plan.as_ref())
                    .cloned()
                    .collect();
                let mut plan_list: Vec<String> = plan_set.into_iter().collect();
                plan_list.sort();
                if plan_list.is_empty() { plan_list = all_game_plans.clone(); }
                filters.game_plan = fuzzy_select("Select game plan to filter by", &plan_list);
            }
            13 => {
                // Mulligan Count
                let mulligan_options: Vec<String> = vec!["0", "1", "2", "3", "4+"].iter().map(|s| s.to_string()).collect();
                if let Some(selected) = fuzzy_select("Select mulligan count to filter by", &mulligan_options) {
                    let idx = mulligan_options.iter().position(|o| o == &selected).unwrap_or(0);
                    filters.mulligan_count = Some(idx as i32);
                }
            }
            14 => {
                // Game Length
                let length_options: Vec<String> = vec![
                    "Very Short (1-3 turns)",
                    "Short (4-6 turns)",
                    "Medium (7-9 turns)",
                    "Long (10-12 turns)",
                    "Very Long (13+ turns)",
                ].iter().map(|s| s.to_string()).collect();
                if let Some(selected) = fuzzy_select("Select game length to filter by", &length_options) {
                    let idx = length_options.iter().position(|o| o == &selected).unwrap_or(0);
                    filters.game_length = Some(match idx {
                        0 => (1, 3),
                        1 => (4, 6),
                        2 => (7, 9),
                        3 => (10, 12),
                        4 => (13, 999),
                        _ => (1, 999),
                    });
                }
            }
            15 => {
                // Game Number
                let game_options: Vec<String> = vec!["Game 1", "Game 2", "Game 3", "Post-board (2+3)"].iter().map(|s| s.to_string()).collect();
                if let Some(selected) = fuzzy_select("Select game number to filter by", &game_options) {
                    filters.game_number = Some(match selected.as_str() {
                        "Game 1" => vec![1],
                        "Game 2" => vec![2],
                        "Game 3" => vec![3],
                        "Post-board (2+3)" => vec![2, 3],
                        _ => vec![1],
                    });
                }
            }
            16 => {
                // Play/Draw
                let play_draw_options: Vec<String> = vec!["On the Play", "On the Draw"].iter().map(|s| s.to_string()).collect();
                if let Some(selected) = fuzzy_select("Select play/draw to filter by", &play_draw_options) {
                    filters.play_draw = Some(if selected == "On the Play" { "play".to_string() } else { "draw".to_string() });
                }
            }
            _ => {}
        }
    }

    filters
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
    (9, "pre-post-board", StatsLevel::Game),
    (10, "mulligans", StatsLevel::Game),
    (11, "game-plan", StatsLevel::Game),
    (12, "win-condition", StatsLevel::Game),
    (13, "loss-reason", StatsLevel::Game),
    (14, "game-length", StatsLevel::Game),
    (15, "play-draw", StatsLevel::Game),
    // Doomsday-specific (only shown when filtering for doomsday)
    (16, "doomsday-resolved", StatsLevel::Game),
    (17, "sb-juke-plan", StatsLevel::Game),
    (18, "pile-type", StatsLevel::Game),
    (19, "no-doomsday-reason", StatsLevel::Game),
    (20, "pile-disruption", StatsLevel::Game),
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
    // Doomsday-specific fields
    common_pile_types: Vec<String>,
    no_doomsday_reasons: Vec<String>,
    non_doomsday_wincons: Vec<String>,
    pile_disruption: Vec<String>,
    is_doomsday: bool,
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

    let unified = load_definition(archetype)?;

    // Check if this is a doomsday deck
    let is_doomsday = unified.name.to_lowercase() == "doomsday";

    // If there's a subtype, look it up
    if let Some(subtype_name) = subtype {
        if let Some(subtype_def) = unified.subtypes.get(subtype_name) {
            return Some(ArchetypeData {
                game_plans: subtype_def.game_plans.clone(),
                win_conditions: subtype_def.win_conditions.clone(),
                loss_reasons: subtype_def.loss_reasons.clone(),
                board_plan: subtype_def.board_plan.clone(),
                common_pile_types: unified.common_pile_types.clone(),
                no_doomsday_reasons: unified.no_doomsday_reasons.clone(),
                non_doomsday_wincons: unified.non_doomsday_wincons.clone(),
                pile_disruption: unified.pile_disruption.clone(),
                is_doomsday,
            });
        }
    }

    // No subtype specified - merge options from all subtypes plus root
    let mut game_plans: Vec<String> = unified.game_plans.clone();
    let mut win_conditions: Vec<String> = unified.win_conditions.clone();
    let mut loss_reasons: Vec<String> = unified.loss_reasons.clone();

    for subtype_def in unified.subtypes.values() {
        for plan in &subtype_def.game_plans {
            if !game_plans.contains(plan) {
                game_plans.push(plan.clone());
            }
        }
        for cond in &subtype_def.win_conditions {
            if !win_conditions.contains(cond) {
                win_conditions.push(cond.clone());
            }
        }
        for reason in &subtype_def.loss_reasons {
            if !loss_reasons.contains(reason) {
                loss_reasons.push(reason.clone());
            }
        }
    }

    Some(ArchetypeData {
        game_plans,
        win_conditions,
        loss_reasons,
        board_plan: unified.board_plan,
        common_pile_types: unified.common_pile_types.clone(),
        no_doomsday_reasons: unified.no_doomsday_reasons.clone(),
        non_doomsday_wincons: unified.non_doomsday_wincons.clone(),
        pile_disruption: unified.pile_disruption.clone(),
        is_doomsday,
    })
}

/// Load all archetype definitions from the definitions directory
fn load_all_definitions() -> Vec<UnifiedArchetypeDefinition> {
    let mut definitions = Vec::new();

    if let Ok(entries) = fs::read_dir("definitions") {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("toml") {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(unified) = toml::from_str::<UnifiedArchetypeDefinition>(&content) {
                        definitions.push(unified);
                    }
                }
            }
        }
    }

    definitions
}

/// Load a single archetype definition by name
fn load_definition(archetype: &str) -> Option<UnifiedArchetypeDefinition> {
    let filename = archetype_to_filename(archetype);
    let path = Path::new("definitions").join(&filename);

    if let Ok(content) = fs::read_to_string(&path) {
        toml::from_str::<UnifiedArchetypeDefinition>(&content).ok()
    } else {
        None
    }
}

/// Helper to sort a list alphabetically with "Other" at the end
fn sort_with_other_last(items: &mut Vec<String>) {
    items.sort();
    if let Some(pos) = items.iter().position(|name| name == "Other") {
        let other = items.remove(pos);
        items.push(other);
    }
}

/// Generate all deck names from a definition (handles subtypes)
fn deck_names_from_definition(def: &UnifiedArchetypeDefinition) -> Vec<String> {
    if !def.subtypes.is_empty() {
        def.subtypes.keys()
            .map(|subtype| format!("{}: {}", def.name, subtype))
            .collect()
    } else {
        vec![def.name.clone()]
    }
}

fn load_archetypes() -> Vec<String> {
    let mut archetypes: Vec<String> = load_all_definitions()
        .into_iter()
        .map(|def| def.name)
        .collect();

    sort_with_other_last(&mut archetypes);
    archetypes
}

/// Load subtypes for a given archetype
fn load_subtypes(archetype: &str) -> Vec<String> {
    if let Some(def) = load_definition(archetype) {
        let mut subtypes: Vec<String> = def.subtypes.keys().cloned().collect();
        subtypes.sort();
        return subtypes;
    }
    Vec::new()
}

/// Load lists for a given archetype and subtype
fn load_lists(archetype: &str, subtype: &str) -> Vec<String> {
    if let Some(def) = load_definition(archetype) {
        if let Some(subtype_def) = def.subtypes.get(subtype) {
            let mut lists: Vec<String> = subtype_def.lists.keys().cloned().collect();
            lists.sort();
            return lists;
        }
    }
    Vec::new()
}

/// Load all deck names from definitions
fn load_deck_names() -> Vec<String> {
    let definitions = load_all_definitions();

    let mut deck_names: Vec<String> = if !definitions.is_empty() {
        definitions.iter()
            .flat_map(deck_names_from_definition)
            .collect()
    } else {
        // Fall back to definitions.md if no TOML files found
        load_deck_names_from_md()
    };

    sort_with_other_last(&mut deck_names);
    deck_names
}

/// Fallback: load deck names from definitions.md
fn load_deck_names_from_md() -> Vec<String> {
    match fs::read_to_string("definitions.md") {
        Ok(content) => {
            let mut in_decks_section = false;
            content.lines()
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
                .collect()
        },
        Err(_) => Vec::new()
    }
}

fn load_deck_categories() -> HashMap<String, DeckCategory> {
    let definitions = load_all_definitions();

    if !definitions.is_empty() {
        let mut categories = HashMap::new();
        for def in definitions {
            let category = match def.category.as_str() {
                "Blue" => DeckCategory::Blue,
                "Combo" => DeckCategory::Combo,
                "Non-Blue" => DeckCategory::NonBlue,
                _ => DeckCategory::Other,
            };

            for deck_name in deck_names_from_definition(&def) {
                categories.insert(deck_name, category.clone());
            }
        }
        categories
    } else {
        // Fall back to definitions.md
        load_deck_categories_from_md()
    }
}

/// Fallback: load deck categories from definitions.md
fn load_deck_categories_from_md() -> HashMap<String, DeckCategory> {
    let mut categories = HashMap::new();

    if let Ok(content) = fs::read_to_string("definitions.md") {
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

fn show_opponent_history(connection: &mut SqliteConnection, opponent_name: &str) {
    use std::collections::HashMap;

    // Query all previous matches against this opponent
    let previous_matches: Vec<Match> = matches::table
        .filter(matches::opponent_name.eq(opponent_name))
        .order(matches::date.desc())
        .load(connection)
        .unwrap_or_default();

    if previous_matches.is_empty() {
        println!("\nFirst time playing against {}", opponent_name);
    } else {
        let wins = previous_matches.iter().filter(|m| m.match_winner == "me").count();
        let losses = previous_matches.iter().filter(|m| m.match_winner == "opponent").count();

        // Count decks they've played
        let mut deck_counts: HashMap<String, usize> = HashMap::new();
        for m in &previous_matches {
            if m.opponent_deck != "unknown" {
                *deck_counts.entry(m.opponent_deck.clone()).or_insert(0) += 1;
            }
        }

        println!("\n=== History vs {} ===", opponent_name);
        println!("Record: {}-{}", wins, losses);

        if !deck_counts.is_empty() {
            let mut decks: Vec<_> = deck_counts.into_iter().collect();
            decks.sort_by(|a, b| b.1.cmp(&a.1)); // Sort by count descending
            let deck_strs: Vec<String> = decks.iter().map(|(deck, count)| {
                if *count > 1 { format!("{} ({})", deck, count) } else { deck.clone() }
            }).collect();
            println!("Decks: {}", deck_strs.join(", "));
        }
    }

    // Always show MTGGoldfish link
    println!("https://www.mtggoldfish.com/player/{}", opponent_name);

    // Wait for user to press Enter
    let _: String = Input::new()
        .with_prompt("Press Enter to continue")
        .allow_empty(true)
        .interact_text()
        .unwrap_or_default();
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
    LeagueStats {
        #[arg(long, help = "Filter by deck name")]
        deck: Option<String>,
        #[arg(long, help = "Show league history list")]
        list: bool,
    },
    Graph {
        #[arg(long, default_value = "win-rate", help = "Metric to graph: win-rate, game-win-rate, mulligans, game-length, matches-played")]
        metric: String,
        #[arg(long, help = "Number of matches per bucket (default: config or 10)")]
        bucket_size: Option<usize>,
        #[arg(long, help = "Output HTML file instead of ASCII")]
        html: Option<String>,
        #[arg(long, help = "Moving average window size for smoothing, 1 = none (default: config or 5)")]
        smoothing: Option<usize>,
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
        GameCommands::LeagueStats { deck, list } => show_league_stats(deck, list),
        GameCommands::Graph { metric, bucket_size, html, smoothing } => {
            let config = load_config();
            let bucket_size = bucket_size.unwrap_or(config.stats.chart_bucket_size);
            let smoothing = smoothing.unwrap_or(config.stats.chart_smoothing);
            show_graph(&metric, bucket_size, html, smoothing)
        },
        GameCommands::HtmlStats { output, era, my_deck, opponent, opponent_deck, event_type } => {
            crate::html_stats::generate_html_stats(&output, era, my_deck, opponent, opponent_deck, event_type)
        },
        GameCommands::ReconcileDeck { deck_name } => reconcile_deck(&deck_name),
    }
}

/// Three-step deck selection: Archetype -> Subtype -> List.
/// Pass `current` to pre-select the existing deck (for editing); pass `None` to use config defaults (for adding).
fn select_deck_three_step(config: &Config, current: Option<&str>) -> String {
    // Derive per-step defaults: from the current deck name when editing, from config when adding
    let (default_archetype, default_subtype, default_list): (String, String, String) =
        if let Some(current_deck) = current {
            let (arch, sub) = parse_deck_name(current_deck);
            let list = if let (Some(ls), Some(le)) = (current_deck.find(" ("), current_deck.rfind(')')) {
                current_deck[ls + 2..le].to_string()
            } else {
                String::new()
            };
            (arch.to_string(), sub.unwrap_or("").to_string(), list)
        } else {
            (
                config.game_entry.default_archetype.as_deref().unwrap_or("").to_string(),
                config.game_entry.default_subtype.as_deref().unwrap_or("").to_string(),
                config.game_entry.default_list.as_deref().unwrap_or("").to_string(),
            )
        };

    // Step 1: Select Archetype
    let archetypes = load_archetypes();

    if archetypes.is_empty() {
        return fuzzy_select("Your deck name", &[])
            .unwrap_or_else(|| "Unknown".to_string());
    }

    let selected_archetype = fuzzy_select_with_default("Select archetype", &archetypes, &default_archetype)
        .unwrap_or_else(|| archetypes.first().cloned().unwrap_or_default());

    // Step 2: Select Subtype
    let subtypes = load_subtypes(&selected_archetype);

    if subtypes.is_empty() {
        return selected_archetype;
    }

    let selected_subtype = fuzzy_select_with_default("Select subtype", &subtypes, &default_subtype)
        .unwrap_or_else(|| subtypes.first().cloned().unwrap_or_default());

    // Step 3: Select List
    let lists = load_lists(&selected_archetype, &selected_subtype);

    if lists.is_empty() {
        return format!("{}: {}", selected_archetype, selected_subtype);
    }

    let selected_list = fuzzy_select_with_default("Select list", &lists, &default_list)
        .unwrap_or_else(|| lists.first().cloned().unwrap_or_default());

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
    let deck_name = select_deck_three_step(&config, None);

    println!("Selected deck: {}", deck_name);

    // Get event type (before opponent info since it's usually the same as last time)
    let event_types: Vec<String> = EVENT_TYPES.iter().map(|s| s.to_string()).collect();
    let last_event_type = {
        let connection = &mut establish_connection();
        matches::table
            .select(matches::event_type)
            .order(matches::created_at.desc())
            .first::<String>(connection)
            .ok()
    };
    let Some(event_type) = (match &last_event_type {
        Some(default) => fuzzy_select_with_default("Event type", &event_types, default),
        None => fuzzy_select("Event type", &event_types),
    }) else {
        println!("\nCancelled.");
        return;
    };

    // Get opponent name
    let opponents = load_opponent_names();
    let Some(opponent_name) = fuzzy_select("Opponent name", &opponents) else {
        println!("\nCancelled.");
        return;
    };

    // Show opponent history (uses its own connection for read-only query)
    {
        let connection = &mut establish_connection();
        show_opponent_history(connection, &opponent_name);
    }

    // Get die roll winner
    let die_roll_result = Confirm::new()
        .with_prompt("Did you win the die roll?")
        .interact();
    let die_roll_winner = match die_roll_result {
        Ok(true) => Winner::Me,
        Ok(false) => Winner::Opponent,
        Err(_) => {
            println!("\nCancelled.");
            return;
        }
    };

    // Load archetype data for game entry prompts
    let archetype = load_archetype_data(&deck_name);

    // Collect all game data BEFORE any DB writes
    println!("\n=== Adding Games (Best of 3) ===");
    let collected = match collect_games_data(&die_roll_winner, &archetype) {
        Some(data) => data,
        None => {
            println!("\nCancelled.");
            return;
        }
    };

    // Ask for opponent deck at the end if not already known
    let opponent_deck = if collected.opponent_deck == "unknown" {
        println!("\n=== Match Complete ===");
        let deck_names = load_deck_names();
        fuzzy_select("What deck was your opponent playing?", &deck_names)
            .unwrap_or_else(|| "Unknown".to_string())
    } else {
        collected.opponent_deck
    };

    // === ALL DATA COLLECTED - NOW COMMIT TO DATABASE ===
    let connection = &mut establish_connection();

    // Get current era
    let era = config.game_entry.default_era
        .or_else(|| get_current_era(connection));

    // Handle league tracking if this is a league match
    let league_id = if event_type == "League" {
        get_or_create_league(connection, &deck_name, &date)
    } else {
        None
    };

    // Insert the match
    let new_match = NewMatch {
        date: date.clone(),
        deck_name: deck_name.clone(),
        opponent_name,
        opponent_deck,
        event_type,
        die_roll_winner: die_roll_winner.to_string(),
        match_winner: collected.match_winner.clone(),
        era,
        league_id,
    };

    diesel::insert_into(matches::table)
        .values(&new_match)
        .execute(connection)
        .expect("Error saving new match");

    let match_id: i32 = matches::table
        .select(matches::match_id)
        .order(matches::match_id.desc())
        .first(connection)
        .expect("Error getting match ID");

    // Insert all games
    for game in &collected.games {
        let new_game = NewGame {
            match_id,
            game_number: game.game_number,
            play_draw: game.play_draw.clone(),
            mulligans: game.mulligans,
            opening_hand_plan: game.opening_hand_plan.clone(),
            game_winner: game.game_winner.clone(),
            win_condition: game.win_condition.clone(),
            loss_reason: game.loss_reason.clone(),
            turns: game.turns,
        };

        diesel::insert_into(games::table)
            .values(&new_game)
            .execute(connection)
            .expect("Error saving game");

        // If there's doomsday data, insert it
        if let Some(ref dd) = game.doomsday_data {
            let game_id: i32 = games::table
                .select(games::game_id)
                .order(games::game_id.desc())
                .first(connection)
                .expect("Error getting game ID");

            let new_dd = NewDoomsdayGame {
                game_id,
                doomsday: Some(dd.doomsday_resolved),
                pile_cards: None,  // Deprecated
                pile_plan: None,   // Deprecated
                juke: None,        // Deprecated, use sb_juke_plan instead
                pile_type: dd.pile_type.clone(),
                better_pile: dd.better_pile.map(|b| if b { 1 } else { 0 }),
                no_doomsday_reason: dd.no_doomsday_reason.clone(),
                sb_juke_plan: dd.sb_juke_plan.clone(),
                pile_disruption: dd.pile_disruption.clone(),
                dd_intent: Some(dd.dd_intent as i32),
            };

            diesel::insert_into(doomsday_games::table)
                .values(&new_dd)
                .execute(connection)
                .expect("Error saving doomsday data");
        }
    }

    // Update league record if this is a league match
    if let Some(lid) = league_id {
        let winner = Winner::from_str(&collected.match_winner).unwrap_or(Winner::Me);
        update_league_after_match(connection, lid, &winner, &date);
    }

    println!("\nMatch {} saved successfully!", match_id);
}

/// Collect all game data without writing to DB. Returns None if cancelled.
fn collect_games_data(die_roll_winner: &Winner, archetype: &Option<ArchetypeData>) -> Option<CollectedMatchData> {
    let mut games: Vec<CollectedGame> = Vec::new();
    let mut my_wins = 0;
    let mut opponent_wins = 0;
    let mut previous_game_winner: Option<Winner> = None;
    let mut opponent_deck = "unknown".to_string();

    for game_num in 1..=3 {
        println!("\n--- Game {} ---", game_num);

        // Ask sideboard juke plan BEFORE games 2-3 (for doomsday)
        let sb_juke_plan = if game_num > 1 {
            if let Some(ref arch) = archetype {
                if arch.is_doomsday {
                    let juke_options = vec!["none".to_string(), "partial".to_string(), "full".to_string()];
                    fuzzy_select(&format!("Sideboard plan for G{}", game_num), &juke_options)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        // Play or draw is determined automatically
        let play_draw = if game_num == 1 {
            match die_roll_winner {
                Winner::Me => {
                    println!("On the play (won die roll)");
                    PlayDraw::Play
                }
                Winner::Opponent => {
                    println!("On the draw (lost die roll)");
                    PlayDraw::Draw
                }
            }
        } else {
            match previous_game_winner {
                Some(Winner::Me) => {
                    println!("On the draw (won previous game)");
                    PlayDraw::Draw
                }
                Some(Winner::Opponent) => {
                    println!("On the play (lost previous game)");
                    PlayDraw::Play
                }
                None => unreachable!(),
            }
        };

        // Mulligans
        let mulligans: i32 = match Input::new()
            .with_prompt("Number of mulligans (0-7)")
            .validate_with(|input: &i32| -> Result<(), &str> {
                if *input >= 0 && *input <= 7 { Ok(()) } else { Err("Mulligans must be between 0 and 7") }
            })
            .interact_text()
        {
            Ok(m) => m,
            Err(_) => return None,
        };

        // Opening hand plan
        let game_plans = if let Some(ref arch) = archetype {
            arch.game_plans.clone()
        } else {
            load_game_plans()
        };
        let opening_hand_plan = fuzzy_select("Opening hand plan", &game_plans);

        // Game winner
        let game_winner = match Confirm::new()
            .with_prompt("Did you win this game?")
            .interact()
        {
            Ok(true) => {
                my_wins += 1;
                Winner::Me
            }
            Ok(false) => {
                opponent_wins += 1;
                Winner::Opponent
            }
            Err(_) => return None,
        };
        previous_game_winner = Some(game_winner.clone());

        // Win condition (only if you won, and not a doomsday deck - doomsday handles this itself)
        let is_doomsday_deck = archetype.as_ref().map_or(false, |a| a.is_doomsday);
        let win_condition = if matches!(game_winner, Winner::Me) && !is_doomsday_deck {
            let win_cons = if let Some(ref arch) = archetype {
                arch.win_conditions.clone()
            } else {
                load_win_conditions()
            };
            fuzzy_select("What did you win with?", &win_cons)
        } else {
            None
        };

        // Loss reason (only if you lost, and not a doomsday deck - doomsday handles this itself)
        let loss_reason = if matches!(game_winner, Winner::Opponent) && !is_doomsday_deck {
            if let Some(ref arch) = archetype {
                fuzzy_select("Why did you lose?", &arch.loss_reasons)
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
                if input.is_empty() { return Ok(()); }
                match input.parse::<i32>() {
                    Ok(n) if n > 0 => Ok(()),
                    _ => Err("Turns must be a positive number")
                }
            })
            .interact_text()
            .ok()
            .and_then(|s| if s.is_empty() { None } else { s.parse().ok() });

        // Doomsday data if applicable
        let doomsday_data = if let Some(ref arch) = archetype {
            if arch.is_doomsday {
                collect_doomsday_data_only(game_num, &game_winner, sb_juke_plan.clone(), arch)?
            } else {
                None
            }
        } else {
            None
        };

        // Apply doomsday win_condition_override if present
        let final_win_condition = if let Some(ref dd) = doomsday_data {
            dd.win_condition_override.clone().or(win_condition)
        } else {
            win_condition
        };

        // Apply doomsday loss_reason_override if present
        let final_loss_reason = if let Some(ref dd) = doomsday_data {
            dd.loss_reason_override.clone().or(loss_reason)
        } else {
            loss_reason
        };

        games.push(CollectedGame {
            game_number: game_num,
            play_draw: play_draw.to_string(),
            mulligans,
            opening_hand_plan,
            game_winner: game_winner.to_string(),
            win_condition: final_win_condition,
            loss_reason: final_loss_reason,
            turns,
            doomsday_data,
        });

        println!("Game {} recorded", game_num);
        println!("Current score: You {}-{} Opponent", my_wins, opponent_wins);

        // Ask about opponent deck if still unknown
        if opponent_deck == "unknown" {
            let knows_deck = match Confirm::new()
                .with_prompt("Do you know what deck your opponent is playing yet?")
                .interact()
            {
                Ok(k) => k,
                Err(_) => return None,
            };

            if knows_deck {
                let deck_names = load_deck_names();
                if let Some(deck) = fuzzy_select("What deck is your opponent playing?", &deck_names) {
                    opponent_deck = deck;
                    println!("Opponent deck: {}", opponent_deck);
                }
            }
        }

        // Check if match is decided
        if my_wins == 2 {
            println!("\nYou won the match 2-{}!", opponent_wins);
            break;
        } else if opponent_wins == 2 {
            println!("\nYou lost the match {}-2", my_wins);
            break;
        }
    }

    let match_winner = if my_wins > opponent_wins { "me" } else { "opponent" };

    Some(CollectedMatchData {
        games,
        match_winner: match_winner.to_string(),
        opponent_deck,
    })
}

/// Collected data from game entry (before DB commit)
struct CollectedMatchData {
    games: Vec<CollectedGame>,
    match_winner: String,
    opponent_deck: String,
}

/// Collect doomsday-specific data without writing to DB. Returns None if cancelled.
/// Now takes game_winner and sb_juke_plan (asked before the game for games 2-3).
fn collect_doomsday_data_only(
    game_number: i32,
    game_winner: &Winner,
    sb_juke_plan: Option<String>,
    arch: &ArchetypeData,
) -> Option<Option<CollectedDoomsdayData>> {
    println!("\n--- G{} Doomsday Details ---", game_number);

    let dd_intent = match Confirm::new()
        .with_prompt("Did you plan to cast Doomsday this game?")
        .default(true)
        .interact()
    {
        Ok(d) => d,
        Err(_) => return None,
    };

    let you_won = matches!(game_winner, Winner::Me);

    // Early return for no-intent games
    if !dd_intent {
        let (win_condition_override, loss_reason_override) = if you_won {
            let wincon = fuzzy_select_with_auto_add(
                "Win condition",
                &arch.non_doomsday_wincons,
                "definitions/doomsday.toml",
                "non_doomsday_wincons",
            );
            (wincon, None)
        } else {
            let reason = fuzzy_select("Why did you lose?", &arch.loss_reasons);
            (None, reason)
        };
        return Some(Some(CollectedDoomsdayData {
            dd_intent: false,
            doomsday_resolved: false,
            pile_type: None,
            pile_disruption: None,
            better_pile: None,
            no_doomsday_reason: None,
            sb_juke_plan,
            win_condition_override,
            loss_reason_override,
        }));
    }

    let doomsday_resolved = match Confirm::new()
        .with_prompt("Did you resolve Doomsday?")
        .default(false)
        .interact()
    {
        Ok(d) => d,
        Err(_) => return None,
    };

    let (pile_type, pile_disruption, better_pile, no_doomsday_reason, win_condition_override) = if you_won {
        // WIN path
        if doomsday_resolved {
            // Won with doomsday - ask pile type, set win_condition to "doomsday"
            let pile_type = fuzzy_select_with_auto_add(
                "Pile type",
                &arch.common_pile_types,
                "definitions/doomsday.toml",
                "common_pile_types",
            );

            let pile_disruption = collect_pile_disruption(arch);

            (pile_type, pile_disruption, None, None, Some("doomsday".to_string()))
        } else {
            // Won without doomsday - ask how (goes into win_condition)
            let wincon = fuzzy_select_with_auto_add(
                "Win condition",
                &arch.non_doomsday_wincons,
                "definitions/doomsday.toml",
                "non_doomsday_wincons",
            );
            (None, None, None, None, wincon)
        }
    } else {
        // LOSE path - no win_condition_override needed
        if doomsday_resolved {
            // Lost after resolving doomsday
            let pile_type = fuzzy_select_with_auto_add(
                "Pile type",
                &arch.common_pile_types,
                "definitions/doomsday.toml",
                "common_pile_types",
            );

            let pile_disruption = collect_pile_disruption(arch);

            let better_pile = match Confirm::new()
                .with_prompt("Could you have won with a better pile/play?")
                .default(false)
                .interact()
            {
                Ok(b) => Some(b),
                Err(_) => None,
            };

            (pile_type, pile_disruption, better_pile, None, None)
        } else {
            // Lost without casting doomsday - filter out "Gameplan" since dd_intent handles that now
            let reasons: Vec<String> = arch.no_doomsday_reasons.iter()
                .filter(|r| r.as_str() != "Gameplan")
                .cloned()
                .collect();
            let reason = fuzzy_select_with_auto_add(
                "Why didn't you cast Doomsday?",
                &reasons,
                "definitions/doomsday.toml",
                "no_doomsday_reasons",
            );
            (None, None, None, reason, None)
        }
    };

    Some(Some(CollectedDoomsdayData {
        dd_intent: true,
        doomsday_resolved,
        pile_type,
        pile_disruption,
        better_pile,
        no_doomsday_reason,
        sb_juke_plan,
        win_condition_override,
        loss_reason_override: None,
    }))
}

/// Collect multiple disruption cards via repeated fuzzy select. Returns comma-separated string or None.
fn collect_pile_disruption(arch: &ArchetypeData) -> Option<String> {
    let mut selected: Vec<String> = Vec::new();
    let done_sentinel = "(done)".to_string();
    loop {
        let prompt = if selected.is_empty() {
            "Disruption faced".to_string()
        } else {
            format!("Disruption faced [{}]", selected.join(", "))
        };
        // Build options with done sentinel first, then remaining cards
        let mut options = vec![done_sentinel.clone()];
        for card in &arch.pile_disruption {
            if !selected.contains(card) {
                options.push(card.clone());
            }
        }
        match fuzzy_select_with_auto_add(
            &prompt,
            &options,
            "definitions/doomsday.toml",
            "pile_disruption",
        ) {
            Some(ref card) if card == &done_sentinel => break,
            Some(card) => {
                if !selected.contains(&card) {
                    selected.push(card);
                }
            }
            None => break, // Escape/abort
        }
    }
    if selected.is_empty() {
        None
    } else {
        Some(selected.join(","))
    }
}

/// Fuzzy select that auto-adds new values to a TOML file if the user enters something new
fn fuzzy_select_with_auto_add(
    prompt: &str,
    options: &[String],
    toml_path: &str,
    list_name: &str,
) -> Option<String> {
    let result = fuzzy_select(prompt, options)?;

    // Check if this is a new value not in the options
    if !options.iter().any(|o| o == &result) {
        let add_to_toml = Confirm::new()
            .with_prompt(format!("Add '{}' to {}?", result, list_name))
            .default(true)
            .interact()
            .unwrap_or(false);

        if add_to_toml {
            append_to_toml_list(toml_path, list_name, &result);
        }
    }

    Some(result)
}

/// Append a new value to a TOML array
fn append_to_toml_list(toml_path: &str, list_name: &str, new_value: &str) {
    let content = match fs::read_to_string(toml_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error reading {}: {}", toml_path, e);
            return;
        }
    };

    // Find the list and append the new value
    // Look for pattern: list_name = [
    let list_start = format!("{} = [", list_name);
    if let Some(start_idx) = content.find(&list_start) {
        // Find the closing bracket
        let search_start = start_idx + list_start.len();
        if let Some(end_offset) = content[search_start..].find(']') {
            let end_idx = search_start + end_offset;

            // Insert the new value before the closing bracket
            let before = &content[..end_idx];
            let after = &content[end_idx..];

            // Check if we need a comma (if the list isn't empty)
            let trimmed = before.trim_end();
            let needs_comma = !trimmed.ends_with('[') && !trimmed.ends_with(',');

            let new_content = if needs_comma {
                format!("{},\n    \"{}\"{}", before.trim_end(), new_value, after)
            } else {
                format!("{}\n    \"{}\"{}", before.trim_end(), new_value, after)
            };

            if let Err(e) = fs::write(toml_path, new_content) {
                eprintln!("Error writing to {}: {}", toml_path, e);
            } else {
                println!("Added '{}' to {}", new_value, list_name);
            }
        }
    }
}

/// Find an active league for the given deck, or prompt to create a new one
/// Returns Some(league_id) if a league is active or created, None if skipped
fn get_or_create_league(connection: &mut SqliteConnection, deck_name: &str, date: &str) -> Option<i32> {
    // Look for an in-progress league with a similar deck
    let active_leagues: Vec<League> = leagues::table
        .filter(leagues::status.eq("in_progress"))
        .order(leagues::created_at.desc())
        .load(connection)
        .unwrap_or_default();

    // Find leagues with the same deck (matching archetype)
    let (archetype, _) = parse_deck_name(deck_name);
    let matching_league = active_leagues.iter().find(|l| {
        let (league_arch, _) = parse_deck_name(&l.deck_name);
        league_arch == archetype
    });

    if let Some(league) = matching_league {
        println!("\n=== Active League Found ===");
        println!("Deck: {}", league.deck_name);
        println!("Record: {}-{}", league.wins, league.losses);

        let continue_league = Confirm::new()
            .with_prompt("Continue this league?")
            .default(true)
            .interact()
            .unwrap_or(true);

        if continue_league {
            return Some(league.league_id);
        }

        // Ask if they want to drop
        let drop_league = Confirm::new()
            .with_prompt("Did you drop from this league?")
            .default(false)
            .interact()
            .unwrap_or(false);

        if drop_league {
            // Mark league as dropped
            diesel::update(leagues::table.find(league.league_id))
                .set((
                    leagues::status.eq("dropped"),
                    leagues::result.eq("dropped"),
                    leagues::end_date.eq(date),
                ))
                .execute(connection)
                .expect("Error updating league");
            println!("League marked as dropped ({}-{})", league.wins, league.losses);
        }
    }

    // No active league or chose not to continue - ask to start a new one
    let start_new = Confirm::new()
        .with_prompt("Start a new league?")
        .default(true)
        .interact()
        .unwrap_or(true);

    if start_new {
        let new_league = NewLeague {
            start_date: date.to_string(),
            end_date: None,
            deck_name: deck_name.to_string(),
            status: "in_progress".to_string(),
            result: Some("pending".to_string()),
            wins: 0,
            losses: 0,
        };

        diesel::insert_into(leagues::table)
            .values(&new_league)
            .execute(connection)
            .expect("Error creating league");

        let league_id: i32 = leagues::table
            .select(leagues::league_id)
            .order(leagues::league_id.desc())
            .first(connection)
            .expect("Error getting league ID");

        println!("Started new league (ID: {})", league_id);
        return Some(league_id);
    }

    None
}

/// Update league record after a match and detect completion
fn update_league_after_match(connection: &mut SqliteConnection, league_id: i32, match_winner: &Winner, date: &str) {
    let league: League = leagues::table
        .find(league_id)
        .first(connection)
        .expect("Error loading league");

    let (new_wins, new_losses) = match match_winner {
        Winner::Me => (league.wins + 1, league.losses),
        Winner::Opponent => (league.wins, league.losses + 1),
    };

    // Check for league completion (5 matches total or 5 wins for trophy)
    let total_matches = new_wins + new_losses;
    let (new_status, new_result) = if new_wins >= 5 {
        println!("\nTROPHY! You finished the league 5-{}!", new_losses);
        ("completed".to_string(), Some("trophy".to_string()))
    } else if total_matches >= 5 {
        // Completed 5 matches - determine result based on record
        let result = if new_losses >= 4 {
            "elimination"  // 0-5, 1-4 (no prizes)
        } else {
            "completed"    // 4-1, 3-2, 2-3 (prizes)
        };
        println!("\nLeague complete: {}-{}", new_wins, new_losses);
        ("completed".to_string(), Some(result.to_string()))
    } else {
        println!("\nLeague record: {}-{}", new_wins, new_losses);
        ("in_progress".to_string(), Some("pending".to_string()))
    };

    let end_date = if new_status == "completed" {
        Some(date.to_string())
    } else {
        None
    };

    diesel::update(leagues::table.find(league_id))
        .set((
            leagues::wins.eq(new_wins),
            leagues::losses.eq(new_losses),
            leagues::status.eq(&new_status),
            leagues::result.eq(&new_result),
            leagues::end_date.eq(&end_date),
        ))
        .execute(connection)
        .expect("Error updating league");
}

/// Show league statistics
fn show_league_stats(deck_filter: Option<String>, show_list: bool) {
    let connection = &mut establish_connection();

    // Load all leagues
    let mut query = leagues::table.into_boxed();

    if let Some(ref deck) = deck_filter {
        query = query.filter(leagues::deck_name.like(format!("%{}%", deck)));
    }

    let all_leagues: Vec<League> = query
        .order(leagues::created_at.desc())
        .load(connection)
        .expect("Error loading leagues");

    if all_leagues.is_empty() {
        println!("No leagues found");
        return;
    }

    // Calculate statistics
    let total_leagues = all_leagues.len();
    let in_progress = all_leagues.iter().filter(|l| l.status == "in_progress").count();
    let completed = all_leagues.iter().filter(|l| l.status == "completed").count();
    let dropped = all_leagues.iter().filter(|l| l.status == "dropped").count();

    let trophies = all_leagues.iter().filter(|l| l.result.as_deref() == Some("trophy")).count();
    let eliminations = all_leagues.iter().filter(|l| l.result.as_deref() == Some("elimination")).count();

    // Only count completed leagues for trophy rate
    let trophy_rate = if completed > 0 {
        (trophies as f64 / completed as f64) * 100.0
    } else {
        0.0
    };

    // Calculate averages from completed leagues
    let completed_leagues: Vec<&League> = all_leagues.iter()
        .filter(|l| l.status == "completed")
        .collect();

    let avg_wins = if !completed_leagues.is_empty() {
        completed_leagues.iter().map(|l| l.wins as f64).sum::<f64>() / completed_leagues.len() as f64
    } else {
        0.0
    };

    // Total match record in leagues
    let total_wins: i32 = all_leagues.iter().map(|l| l.wins).sum();
    let total_losses: i32 = all_leagues.iter().map(|l| l.losses).sum();
    let total_matches = total_wins + total_losses;
    let match_win_rate = if total_matches > 0 {
        (total_wins as f64 / total_matches as f64) * 100.0
    } else {
        0.0
    };

    println!("=== League Statistics ===");
    println!("Total leagues: {}", total_leagues);
    println!("  In progress: {}", in_progress);
    println!("  Completed: {}", completed);
    println!("  Dropped: {}", dropped);
    println!();
    println!("Trophies: {} ({:.1}% rate)", trophies, trophy_rate);
    println!("Eliminations: {}", eliminations);
    println!("Average wins per league: {:.1}", avg_wins);
    println!("Match record in leagues: {}-{} ({:.1}%)", total_wins, total_losses, match_win_rate);

    // Show list if requested
    if show_list {
        println!("\n=== League History ===");
        println!("{:<4} {:<12} {:<25} {:<10} {:<12}", "ID", "Date", "Deck", "Record", "Result");
        println!("{}", "-".repeat(70));

        for league in all_leagues.iter().take(20) {
            let result_str = match league.result.as_deref() {
                Some("trophy") => "🏆 Trophy",
                Some("elimination") => "Eliminated",
                Some("dropped") => "Dropped",
                Some("pending") => "In Progress",
                _ => "Unknown",
            };
            println!("{:<4} {:<12} {:<25} {:<10} {:<12}",
                league.league_id,
                &league.start_date,
                truncate(&league.deck_name, 25),
                format!("{}-{}", league.wins, league.losses),
                result_str);
        }
    }
}

/// Bucket matches/games chronologically and compute a metric per bucket
fn bucket_metric(matches: &[Match], games: &[Game], bucket_size: usize, metric: &str) -> Vec<f64> {
    let bucket_size = bucket_size.max(1);

    // Sort matches chronologically
    let mut sorted_matches: Vec<&Match> = matches.iter().collect();
    sorted_matches.sort_by(|a, b| a.date.cmp(&b.date).then(a.match_id.cmp(&b.match_id)));

    match metric {
        "win-rate" => {
            sorted_matches.chunks(bucket_size).map(|chunk| {
                let wins = chunk.iter().filter(|m| m.match_winner == "me").count();
                wins as f64 / chunk.len() as f64 * 100.0
            }).collect()
        }
        "game-win-rate" => {
            // Sort games chronologically via match date lookup
            let match_dates: std::collections::HashMap<i32, (&str, i32)> = matches.iter()
                .map(|m| (m.match_id, (m.date.as_str(), m.match_id)))
                .collect();
            let mut sorted_games: Vec<&Game> = games.iter().collect();
            sorted_games.sort_by(|a, b| {
                let a_info = match_dates.get(&a.match_id).unwrap_or(&("", 0));
                let b_info = match_dates.get(&b.match_id).unwrap_or(&("", 0));
                a_info.cmp(b_info).then(a.game_number.cmp(&b.game_number))
            });
            sorted_games.chunks(bucket_size).map(|chunk| {
                let wins = chunk.iter().filter(|g| g.game_winner == "me").count();
                wins as f64 / chunk.len() as f64 * 100.0
            }).collect()
        }
        "mulligans" => {
            // Sort games chronologically via match date lookup
            let match_dates: std::collections::HashMap<i32, (&str, i32)> = matches.iter()
                .map(|m| (m.match_id, (m.date.as_str(), m.match_id)))
                .collect();
            let mut sorted_games: Vec<&Game> = games.iter().collect();
            sorted_games.sort_by(|a, b| {
                let a_info = match_dates.get(&a.match_id).unwrap_or(&("", 0));
                let b_info = match_dates.get(&b.match_id).unwrap_or(&("", 0));
                a_info.cmp(b_info).then(a.game_number.cmp(&b.game_number))
            });
            sorted_games.chunks(bucket_size).map(|chunk| {
                let total: i32 = chunk.iter().map(|g| g.mulligans).sum();
                total as f64 / chunk.len() as f64
            }).collect()
        }
        "game-length" => {
            let match_dates: std::collections::HashMap<i32, (&str, i32)> = matches.iter()
                .map(|m| (m.match_id, (m.date.as_str(), m.match_id)))
                .collect();
            let mut sorted_games: Vec<&Game> = games.iter().collect();
            sorted_games.sort_by(|a, b| {
                let a_info = match_dates.get(&a.match_id).unwrap_or(&("", 0));
                let b_info = match_dates.get(&b.match_id).unwrap_or(&("", 0));
                a_info.cmp(b_info).then(a.game_number.cmp(&b.game_number))
            });
            sorted_games.chunks(bucket_size).map(|chunk| {
                let with_turns: Vec<_> = chunk.iter().filter(|g| g.turns.is_some()).collect();
                if with_turns.is_empty() {
                    0.0
                } else {
                    let total: i32 = with_turns.iter().map(|g| g.turns.unwrap()).sum();
                    total as f64 / with_turns.len() as f64
                }
            }).collect()
        }
        "matches-played" => {
            sorted_matches.chunks(bucket_size).map(|chunk| chunk.len() as f64).collect()
        }
        _ => Vec::new(),
    }
}

/// Show graph of statistics over time
fn show_graph(
    metric: &str,
    bucket_size: usize,
    html_output: Option<String>,
    smoothing: usize,
) {
    let connection = &mut establish_connection();

    // Get filters interactively (same UI as stats)
    let filters = select_filters_interactive(connection);

    // Load filtered data
    let (all_matches, all_games) = filters.load_filtered_data(connection);

    if all_matches.is_empty() {
        println!("No matches found with the given filters");
        return;
    }

    // Show active filters
    let active_filters = filters.active_filter_descriptions();
    if !active_filters.is_empty() {
        println!("Filters: {}\n", active_filters.join(" | "));
    }

    if let Some((title, labels, x_explanation, y_axis, smoothed)) =
        render_bucket_chart_data(&all_matches, &all_games, bucket_size, metric, smoothing)
    {
        if let Some(path) = html_output {
            generate_graph_html(&path, &title, &labels, &smoothed);
        } else {
            print_ascii_chart(&title, &smoothed, y_axis, Some(&labels), Some(&x_explanation));
        }
    }
}

/// Chart y-axis configuration
enum ChartYAxis {
    /// Auto-range from 0 to max with padding, default labels
    Auto,
    /// Fixed 0-100% range with percentage labels and 10% steps (height=80)
    Percentage,
}

/// Render an ASCII line chart using textplots
fn print_ascii_chart(
    title: &str,
    values: &[f64],
    y_axis: ChartYAxis,
    x_labels: Option<&[String]>,
    x_explanation: Option<&str>,
) {
    println!("\n=== {} ===\n", title);

    if values.len() < 2 {
        println!("Not enough data to graph");
        return;
    }

    let points: Vec<(f32, f32)> = values.iter().enumerate()
        .map(|(i, &v)| (i as f32, v as f32))
        .collect();

    let n = values.len();
    let xmax = (n - 1) as f32;
    let chart_width: u32 = 100;

    let (chart_height, y_min, y_max): (u32, f32, f32) = match y_axis {
        ChartYAxis::Auto => {
            let max_val = values.iter().cloned().fold(0.0_f64, f64::max) as f32;
            (30, 0.0, max_val + max_val * 0.1)
        }
        ChartYAxis::Percentage => (80, 0.0, 100.0),
    };

    let shape = Shape::Lines(&points);
    let mut chart = Chart::new_with_y_range(chart_width, chart_height, 0.0, xmax, y_min, y_max);
    match y_axis {
        ChartYAxis::Auto => {
            chart
                .x_label_format(LabelFormat::None)
                .y_tick_display(TickDisplay::Dense)
                .lineplot(&shape)
                .display();
        }
        ChartYAxis::Percentage => {
            chart
                .y_tick_display(TickDisplay::Dense)
                .y_label_format(LabelFormat::Custom(Box::new(|v| format!("{:.0}%", v))))
                .x_label_format(LabelFormat::None)
                .lineplot(&shape)
                .display();
        }
    }

    // Print custom x-axis labels
    let body_width = (chart_width / 2) as usize;
    let num_ticks = 8.min(n);
    let mut positions: Vec<usize> = (0..num_ticks)
        .map(|i| if num_ticks > 1 { i * (n - 1) / (num_ticks - 1) } else { 0 })
        .collect();
    if let Some(&last) = positions.last() {
        if last != n - 1 {
            positions.push(n - 1);
        }
    }

    let mut label_chars = vec![' '; body_width + 10];
    for &idx in &positions {
        let char_pos = if n > 1 { idx * (body_width - 1) / (n - 1) } else { 0 };
        let label = if let Some(labels) = x_labels {
            labels[idx].clone()
        } else {
            format!("{}", idx)
        };
        let slot_free = label.chars().enumerate()
            .all(|(j, _)| char_pos + j < label_chars.len() && label_chars[char_pos + j] == ' ');
        if slot_free {
            for (j, c) in label.chars().enumerate() {
                if char_pos + j < label_chars.len() {
                    label_chars[char_pos + j] = c;
                }
            }
        }
    }
    println!("{}", label_chars.iter().collect::<String>().trim_end());

    if let Some(explanation) = x_explanation {
        println!("  {}", explanation);
    }
    println!();
}

/// Print a win rate chart indexed by game/match number with bucketing and smoothing
fn print_winrate_chart(matches: &[Match], games: &[Game], game_mode: bool, bucket_size: usize, smoothing: usize) {
    let metric = if game_mode { "game-win-rate" } else { "win-rate" };
    if let Some((title, labels, x_explanation, y_axis, smoothed)) =
        render_bucket_chart_data(matches, games, bucket_size, metric, smoothing)
    {
        print_ascii_chart(&title, &smoothed, y_axis, Some(&labels), Some(&x_explanation));
    }
}

/// Bucket, smooth, and build title/labels for a chart. Returns None if no data.
fn render_bucket_chart_data(
    matches: &[Match], games: &[Game], bucket_size: usize, metric: &str, smoothing: usize,
) -> Option<(String, Vec<String>, String, ChartYAxis, Vec<f64>)> {
    let bucket_size = bucket_size.max(1);
    let is_game_metric = matches!(metric, "game-win-rate" | "mulligans" | "game-length");
    let unit = if is_game_metric { "games" } else { "matches" };

    let buckets = bucket_metric(matches, games, bucket_size, metric);
    if buckets.is_empty() {
        println!("No data to graph");
        return None;
    }

    let smoothed = apply_smoothing_vec(buckets, smoothing);

    let metric_label = match metric {
        "win-rate" => "Win Rate (%)",
        "game-win-rate" => "Game Win Rate (%)",
        "mulligans" => "Avg Mulligans",
        "game-length" => "Avg Game Length",
        "matches-played" => "Matches Played",
        _ => metric,
    };
    let smoothing_label = if smoothing > 1 {
        format!(", smoothing={}", smoothing)
    } else {
        String::new()
    };
    let title = format!("{} (bucket={} {}{})", metric_label, bucket_size, unit, smoothing_label);
    let label_start = if smoothing > 1 { smoothing } else { 1 };
    let labels: Vec<String> = (label_start..label_start + smoothed.len()).map(|i| format!("{}", i)).collect();
    let x_explanation = format!("bucket # (each bucket = {} {})", bucket_size, unit);

    let y_axis = match metric {
        "win-rate" | "game-win-rate" => ChartYAxis::Percentage,
        _ => ChartYAxis::Auto,
    };

    Some((title, labels, x_explanation, y_axis, smoothed))
}

/// Apply backward-looking moving average smoothing over raw values,
/// dropping the first (smoothing - 1) buckets that lack a full window.
fn apply_smoothing_vec(data: Vec<f64>, smoothing: usize) -> Vec<f64> {
    let smoothing = smoothing.max(1);
    if smoothing <= 1 {
        return data;
    }
    data.iter().enumerate()
        .skip(smoothing - 1)
        .map(|(i, _)| {
            let start = i + 1 - smoothing;
            let vals = &data[start..=i];
            vals.iter().sum::<f64>() / vals.len() as f64
        }).collect()
}

/// Generate HTML graph using Chart.js
fn generate_graph_html(path: &str, title: &str, labels: &[String], values: &[f64]) {
    let labels_json: Vec<_> = labels.iter().map(|l| format!("\"{}\"", l)).collect();
    let values_json: Vec<_> = values.iter().map(|v| format!("{:.2}", v)).collect();

    let html = format!(r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>{title}</title>
    <script src="https://cdn.jsdelivr.net/npm/chart.js"></script>
    <style>
        body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif; max-width: 900px; margin: 0 auto; padding: 20px; }}
        h1 {{ font-size: 24px; margin-bottom: 20px; }}
        canvas {{ max-height: 400px; }}
    </style>
</head>
<body>
    <h1>{title}</h1>
    <canvas id="chart"></canvas>
    <script>
        new Chart(document.getElementById('chart'), {{
            type: 'line',
            data: {{
                labels: [{labels_csv}],
                datasets: [{{
                    label: '{title}',
                    data: [{values_csv}],
                    borderColor: 'rgb(75, 192, 192)',
                    backgroundColor: 'rgba(75, 192, 192, 0.1)',
                    fill: true,
                    tension: 0.1
                }}]
            }},
            options: {{
                responsive: true,
                scales: {{
                    y: {{ beginAtZero: true }}
                }}
            }}
        }});
    </script>
</body>
</html>"#,
        title = title,
        labels_csv = labels_json.join(", "),
        values_csv = values_json.join(", "),
    );

    fs::write(path, html).expect("Error writing HTML file");
    println!("Generated graph at: {}", path);
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
    let mut selected_archetype: Option<String> = None;  // Track archetype even when filtering by subtype/list
    let mut opponent_name_filter: Option<String> = None;
    let mut opponent_deck_filter: Option<String> = None;
    let mut opponent_deck_archetype_filter: Option<String> = None;
    let mut opponent_deck_category_filter: Option<String> = None;
    let mut event_type_filter: Option<String> = None;
    let mut era_values: Option<Vec<i32>> = None;
    let mut loss_reason_filter: Option<String> = None;
    let mut win_condition_filter: Option<String> = None;
    let mut game_plan_filter: Option<String> = None;
    let mut mulligan_count_filter: Option<i32> = None;
    let mut game_length_filter: Option<(i32, i32)> = None; // (min, max)
    let mut game_number_filter: Option<Vec<i32>> = None;
    let mut play_draw_filter: Option<String> = None;

    if use_defaults {
        // Use config defaults for filters
        if let Some(era) = config.stats.filters.era {
            era_values = Some(vec![era]);
        }
        deck_name_filter = config.stats.filters.my_deck.clone();
        // Try to extract archetype from config filter (e.g., "Doomsday" or "Doomsday: Tempo")
        if let Some(ref filter) = deck_name_filter {
            let (arch, _) = parse_deck_name(filter);
            if !arch.is_empty() && arch != filter {
                selected_archetype = Some(arch.to_string());
            } else {
                // Filter might just be the archetype name
                selected_archetype = Some(filter.clone());
            }
        }
        opponent_name_filter = config.stats.filters.opponent.clone();
        opponent_deck_filter = config.stats.filters.opponent_deck.clone();
        event_type_filter = config.stats.filters.event_type.clone();
    } else {
        // Interactive filter selection — delegates to shared function with cumulative narrowing
        let fs = select_filters_interactive(connection);
        era_values = fs.era_values;
        deck_name_filter = fs.deck_name;
        opponent_name_filter = fs.opponent_name;
        opponent_deck_filter = fs.opponent_deck;
        opponent_deck_archetype_filter = fs.opponent_deck_archetype;
        opponent_deck_category_filter = fs.opponent_deck_category;
        event_type_filter = fs.event_type;
        loss_reason_filter = fs.loss_reason;
        win_condition_filter = fs.win_condition;
        game_plan_filter = fs.game_plan;
        mulligan_count_filter = fs.mulligan_count;
        game_length_filter = fs.game_length;
        game_number_filter = fs.game_number;
        play_draw_filter = fs.play_draw;

        // Infer archetype for Doomsday-specific group-by options.
        // Works when the user filtered by archetype directly; subtype/list filters
        // don't set this (existing limitation).
        if let Some(ref filter) = deck_name_filter {
            if !filter.starts_with(": ") && !filter.starts_with('(') {
                let (arch, _) = parse_deck_name(filter);
                if !arch.is_empty() {
                    selected_archetype = Some(arch.to_string());
                }
            }
        }

    }


    // Detect if filtering for Doomsday decks (check both archetype and filter string)
    let is_doomsday_filter = selected_archetype
        .as_ref()
        .map(|a| a.to_lowercase() == "doomsday")
        .unwrap_or(false)
        || deck_name_filter
            .as_ref()
            .map(|f| f.to_lowercase().contains("doomsday"))
            .unwrap_or(false);

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
                "pre-post-board" => defaults.push(9),
                "mulligans" => defaults.push(10),
                "game-plan" => defaults.push(11),
                "win-condition" => defaults.push(12),
                "loss-reason" => defaults.push(13),
                "game-length" => defaults.push(14),
                "play-draw" => defaults.push(15),
                // Doomsday-specific (only available when filtering for doomsday)
                "doomsday-resolved" if is_doomsday_filter => defaults.push(16),
                "sb-juke-plan" if is_doomsday_filter => defaults.push(17),
                "pile-type" if is_doomsday_filter => defaults.push(18),
                "no-doomsday-reason" if is_doomsday_filter => defaults.push(19),
                "pile-disruption" if is_doomsday_filter => defaults.push(20),
                "dd-intent" if is_doomsday_filter => defaults.push(21),
                _ => {}
            }
        }
        defaults
    } else {
        let mut groupby_options = vec![
            "My Archetype",
            "My Subtype",
            "My List",
            "Opponent",
            "Opponent Deck",
            "Opponent Deck Archetype",
            "Opponent Deck Category",
            "Era",
            "Game Number",
            "Pre/Post-board",
            "Mulligan Count",
            "Game Plan",
            "Win Condition",
            "Loss Reason",
            "Game Length",
            "Play/Draw",
        ];

        // Add doomsday-specific options if filtering for doomsday
        if is_doomsday_filter {
            groupby_options.push("Doomsday Resolved");
            groupby_options.push("Sideboard Juke Plan");
            groupby_options.push("Pile Type");
            groupby_options.push("No-Doomsday Reason");
            groupby_options.push("Pile Disruption");
            groupby_options.push("DD Intent");
        }

        // Pre-select group-bys based on config
        let mut groupby_defaults = vec![
            config.stats.default_groupbys.contains(&"my-archetype".to_string()),
            config.stats.default_groupbys.contains(&"my-subtype".to_string()),
            config.stats.default_groupbys.contains(&"my-list".to_string()),
            config.stats.default_groupbys.contains(&"opponent".to_string()),
            config.stats.default_groupbys.contains(&"opponent-deck".to_string()),
            config.stats.default_groupbys.contains(&"opponent-deck-archetype".to_string()),
            config.stats.default_groupbys.contains(&"opponent-deck-category".to_string()),
            config.stats.default_groupbys.contains(&"era".to_string()),
            config.stats.default_groupbys.contains(&"game-number".to_string()),
            config.stats.default_groupbys.contains(&"pre-post-board".to_string()),
            config.stats.default_groupbys.contains(&"mulligans".to_string()),
            config.stats.default_groupbys.contains(&"game-plan".to_string()),
            config.stats.default_groupbys.contains(&"win-condition".to_string()),
            config.stats.default_groupbys.contains(&"loss-reason".to_string()),
            config.stats.default_groupbys.contains(&"game-length".to_string()),
            config.stats.default_groupbys.contains(&"play-draw".to_string()),
        ];

        // Add doomsday-specific defaults if filtering for doomsday
        if is_doomsday_filter {
            groupby_defaults.push(config.stats.default_groupbys.contains(&"doomsday-resolved".to_string()));
            groupby_defaults.push(config.stats.default_groupbys.contains(&"sb-juke-plan".to_string()));
            groupby_defaults.push(config.stats.default_groupbys.contains(&"pile-type".to_string()));
            groupby_defaults.push(config.stats.default_groupbys.contains(&"no-doomsday-reason".to_string()));
            groupby_defaults.push(config.stats.default_groupbys.contains(&"pile-disruption".to_string()));
            groupby_defaults.push(config.stats.default_groupbys.contains(&"dd-intent".to_string()));
        }

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

        // Determine if game mode based on selected filters and group-bys
        let picker_game_mode = (loss_reason_filter.is_some() || win_condition_filter.is_some()
            || game_plan_filter.is_some() || mulligan_count_filter.is_some()
            || game_length_filter.is_some() || game_number_filter.is_some()
            || play_draw_filter.is_some())
            || has_game_level(&selected_groupbys, GROUPBY_LEVELS);

        // Pre-select statistics based on config, with appropriate win rate auto-selected
        let stat_defaults = vec![
            !picker_game_mode || config.stats.default_statistics.contains(&"match-win-rate".to_string()),  // Match WR: auto-select in match mode
            picker_game_mode || config.stats.default_statistics.contains(&"game-win-rate".to_string()),    // Game WR: auto-select in game mode
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

    // Apply post-load filters for computed fields
    if let Some(ref arch_filter) = opponent_deck_archetype_filter {
        all_matches.retain(|m| {
            let (archetype, _) = parse_deck_name(&m.opponent_deck);
            archetype == arch_filter
        });
    }

    if let Some(ref cat_filter) = opponent_deck_category_filter {
        all_matches.retain(|m| {
            categorize_deck(&m.opponent_deck).to_string() == cat_filter
        });
    }

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

    if let Some(ref game_nums) = game_number_filter {
        game_query = game_query.filter(games::game_number.eq_any(game_nums));
    }

    if let Some(ref play_draw) = play_draw_filter {
        game_query = game_query.filter(games::play_draw.eq(play_draw));
    }

    let all_games = game_query.load::<Game>(connection)
        .expect("Error loading games");

    // Load doomsday games data if filtering for doomsday
    let doomsday_games_data: Vec<DoomsdayGame> = if is_doomsday_filter {
        let game_ids: Vec<i32> = all_games.iter().map(|g| g.game_id).collect();
        doomsday_games::table
            .filter(doomsday_games::game_id.eq_any(&game_ids))
            .select(DoomsdayGame::as_select())
            .load(connection)
            .unwrap_or_default()
    } else {
        Vec::new()
    };

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

    // Display active filters prominently
    let mut active_filters: Vec<String> = Vec::new();
    if let Some(ref eras) = era_values {
        let era_str = eras.iter().map(|e| e.to_string()).collect::<Vec<_>>().join(", ");
        active_filters.push(format!("Era: {}", era_str));
    }
    if let Some(ref deck) = deck_name_filter {
        active_filters.push(format!("My Deck: {}", deck));
    }
    if let Some(ref opp) = opponent_name_filter {
        active_filters.push(format!("Opponent: {}", opp));
    }
    if let Some(ref opp_deck) = opponent_deck_filter {
        active_filters.push(format!("Opponent Deck: {}", opp_deck));
    }
    if let Some(ref opp_arch) = opponent_deck_archetype_filter {
        active_filters.push(format!("Opponent Archetype: {}", opp_arch));
    }
    if let Some(ref opp_cat) = opponent_deck_category_filter {
        active_filters.push(format!("Opponent Category: {}", opp_cat));
    }
    if let Some(ref event) = event_type_filter {
        active_filters.push(format!("Event: {}", event));
    }
    if let Some(ref reason) = loss_reason_filter {
        active_filters.push(format!("Loss Reason: {}", reason));
    }
    if let Some(ref cond) = win_condition_filter {
        active_filters.push(format!("Win Condition: {}", cond));
    }
    if let Some(ref plan) = game_plan_filter {
        active_filters.push(format!("Game Plan: {}", plan));
    }
    if let Some(count) = mulligan_count_filter {
        active_filters.push(format!("Mulligans: {}", count));
    }
    if let Some((min, max)) = game_length_filter {
        active_filters.push(format!("Game Length: {}-{} turns", min, max));
    }
    if let Some(ref nums) = game_number_filter {
        let label = match nums.as_slice() {
            [1] => "Game 1".to_string(),
            [2] => "Game 2".to_string(),
            [3] => "Game 3".to_string(),
            [2, 3] => "Post-board (2+3)".to_string(),
            _ => format!("Games {:?}", nums),
        };
        active_filters.push(label);
    }
    if let Some(ref pd) = play_draw_filter {
        active_filters.push(format!("Play/Draw: {}", pd));
    }

    if !active_filters.is_empty() {
        println!("\nFilters: {}", active_filters.join(" | "));
    }
    println!("Found {} matches with {} total games\n", all_matches.len(), all_games.len());

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
        show_overall_stats(&all_matches, &all_games, &selected_stats, game_mode, config.stats.chart_bucket_size, config.stats.chart_smoothing);
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
            9 => "pre-post-board",
            10 => "mulligans",
            11 => "game-plan",
            12 => "win-condition",
            13 => "loss-reason",
            14 => "game-length",
            15 => "play-draw",
            16 => "doomsday-resolved",
            17 => "sb-juke-plan",
            18 => "pile-type",
            19 => "no-doomsday-reason",
            20 => "pile-disruption",
            21 => "dd-intent",
            _ => continue,
        };

        show_sliced_stats(&all_matches, &all_games, &doomsday_games_data, groupby_name, config.stats.min_games, &selected_stats);
    }
}


fn show_overall_stats(all_matches: &[Match], all_games: &[Game], selected_stats: &[usize], game_mode: bool, bucket_size: usize, chart_smoothing: usize) {
    let match_refs: Vec<&Match> = all_matches.iter().collect();
    let overall_row = calculate_stats("Overall".to_string(), &match_refs, all_games);
    display_stats_table(&[overall_row], selected_stats, "=== Overall Statistics ===", false);
    print_winrate_chart(all_matches, all_games, game_mode, bucket_size, chart_smoothing);
}

/// Extract archetype from deck name (ignoring subtype)
/// Examples: "Reanimator: UB" -> "Reanimator", "Lands" -> "Lands"
fn extract_archetype(deck_name: &str) -> String {
    let (archetype, _subtype) = parse_deck_name(deck_name);
    archetype.to_string()
}

fn show_sliced_stats(all_matches: &[Match], all_games: &[Game], doomsday_games: &[DoomsdayGame], slice_type: &str, min_games: i64, selected_stats: &[usize]) {
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
        "game-number" => ("=== Statistics by Game Number ===", Box::new(|_m: &Match| String::new())),
        "pre-post-board" => ("=== Statistics by Pre/Post-board ===", Box::new(|_m: &Match| String::new())),
        "mulligans" => ("=== Statistics by Mulligan Count ===", Box::new(|_m: &Match| String::new())),
        "game-plan" => ("=== Statistics by Game Plan ===", Box::new(|_m: &Match| String::new())),
        "win-condition" => ("=== Statistics by Win Condition ===", Box::new(|_m: &Match| String::new())),
        "loss-reason" => ("=== Statistics by Loss Reason ===", Box::new(|_m: &Match| String::new())),
        "game-length" => ("=== Statistics by Game Length ===", Box::new(|_m: &Match| String::new())),
        "play-draw" => ("=== Statistics by Play/Draw ===", Box::new(|_m: &Match| String::new())),
        "doomsday-resolved" => ("=== Statistics by Doomsday Resolved ===", Box::new(|_m: &Match| String::new())),
        "sb-juke-plan" => ("=== Statistics by Sideboard Juke Plan ===", Box::new(|_m: &Match| String::new())),
        "pile-type" => ("=== Statistics by Pile Type ===", Box::new(|_m: &Match| String::new())),
        "no-doomsday-reason" => ("=== Statistics by No-Doomsday Reason ===", Box::new(|_m: &Match| String::new())),
        "dd-intent" => ("=== Statistics by DD Intent ===", Box::new(|_m: &Match| String::new())),
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

    if slice_type == "pre-post-board" {
        let mut pre_board: Vec<&Game> = Vec::new();
        let mut post_board: Vec<&Game> = Vec::new();
        for game in all_games {
            if game.game_number == 1 {
                pre_board.push(game);
            } else {
                post_board.push(game);
            }
        }

        let mut rows: Vec<StatsRow> = Vec::new();
        if !pre_board.is_empty() {
            let row = calculate_stats_from_games("Game 1 (Pre-board)".to_string(), pre_board, all_matches);
            if row.game_count >= min_games as usize {
                rows.push(row);
            }
        }
        if !post_board.is_empty() {
            let row = calculate_stats_from_games("Games 2+3 (Post-board)".to_string(), post_board, all_matches);
            if row.game_count >= min_games as usize {
                rows.push(row);
            }
        }

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

    // Doomsday-specific group-bys
    if slice_type == "doomsday-resolved" {
        // Build a map from game_id to doomsday data
        let dd_map: HashMap<i32, &DoomsdayGame> = doomsday_games.iter()
            .map(|dd| (dd.game_id, dd))
            .collect();

        let mut dd_stats: HashMap<String, Vec<&Game>> = HashMap::new();
        for game in all_games {
            let label = if let Some(dd) = dd_map.get(&game.game_id) {
                if dd.doomsday.unwrap_or(false) { "Resolved Doomsday" } else { "No Doomsday" }
            } else {
                continue;
            };
            dd_stats.entry(label.to_string()).or_default().push(game);
        }

        let mut rows: Vec<StatsRow> = dd_stats.into_iter()
            .map(|(label, games)| calculate_stats_from_games(label, games, all_matches))
            .filter(|row| row.game_count >= min_games as usize)
            .collect();

        let order = ["Resolved Doomsday", "No Doomsday"];
        rows.sort_by_key(|row| order.iter().position(|&s| s == row.label).unwrap_or(999));
        display_stats_table(&rows, selected_stats, title, true);
        return;
    }

    if slice_type == "sb-juke-plan" {
        // Build a map from game_id to doomsday data
        let dd_map: HashMap<i32, &DoomsdayGame> = doomsday_games.iter()
            .map(|dd| (dd.game_id, dd))
            .collect();

        let mut juke_stats: HashMap<String, Vec<&Game>> = HashMap::new();
        for game in all_games {
            // Only include games 2 and 3 (post-board) for juke stats
            if game.game_number == 1 {
                continue;
            }
            let label = if let Some(dd) = dd_map.get(&game.game_id) {
                // Try new sb_juke_plan first, fall back to old juke column
                match dd.sb_juke_plan.as_deref().or(dd.juke.as_deref()) {
                    Some(label) => label,
                    None => continue,
                }
            } else {
                continue;
            };
            juke_stats.entry(label.to_string()).or_default().push(game);
        }

        let mut rows: Vec<StatsRow> = juke_stats.into_iter()
            .map(|(label, games)| calculate_stats_from_games(label, games, all_matches))
            .filter(|row| row.game_count >= min_games as usize)
            .collect();

        let order = ["none", "partial", "full"];
        rows.sort_by_key(|row| order.iter().position(|&s| s == row.label).unwrap_or(999));
        display_stats_table(&rows, selected_stats, title, true);
        return;
    }

    if slice_type == "pile-type" {
        let dd_map: HashMap<i32, &DoomsdayGame> = doomsday_games.iter()
            .map(|dd| (dd.game_id, dd))
            .collect();

        let mut pile_stats: HashMap<String, Vec<&Game>> = HashMap::new();
        for game in all_games {
            let label = if let Some(dd) = dd_map.get(&game.game_id) {
                if dd.doomsday.unwrap_or(false) {
                    match dd.pile_type.as_deref() {
                        Some(label) => label,
                        None => continue,
                    }
                } else {
                    continue; // Skip games without doomsday for pile-type stats
                }
            } else {
                continue;
            };
            pile_stats.entry(label.to_string()).or_default().push(game);
        }

        let mut rows: Vec<StatsRow> = pile_stats.into_iter()
            .map(|(label, games)| calculate_stats_from_games(label, games, all_matches))
            .filter(|row| row.game_count >= min_games as usize)
            .collect();

        rows.sort_by(|a, b| b.game_count.cmp(&a.game_count));
        display_stats_table(&rows, selected_stats, title, true);
        return;
    }

    if slice_type == "no-doomsday-reason" {
        let dd_map: HashMap<i32, &DoomsdayGame> = doomsday_games.iter()
            .map(|dd| (dd.game_id, dd))
            .collect();

        let mut reason_stats: HashMap<String, Vec<&Game>> = HashMap::new();
        for game in all_games {
            let label = if let Some(dd) = dd_map.get(&game.game_id) {
                if !dd.doomsday.unwrap_or(false) {
                    match dd.no_doomsday_reason.as_deref() {
                        Some(label) => label,
                        None => continue,
                    }
                } else {
                    continue; // Skip games where doomsday was cast
                }
            } else {
                continue;
            };
            reason_stats.entry(label.to_string()).or_default().push(game);
        }

        let mut rows: Vec<StatsRow> = reason_stats.into_iter()
            .map(|(label, games)| calculate_stats_from_games(label, games, all_matches))
            .filter(|row| row.game_count >= min_games as usize)
            .collect();

        rows.sort_by(|a, b| b.game_count.cmp(&a.game_count));
        display_stats_table(&rows, selected_stats, title, true);
        return;
    }

    if slice_type == "pile-disruption" {
        let dd_map: HashMap<i32, &DoomsdayGame> = doomsday_games.iter()
            .map(|dd| (dd.game_id, dd))
            .collect();

        let mut disruption_stats: HashMap<String, Vec<&Game>> = HashMap::new();
        for game in all_games {
            if let Some(dd) = dd_map.get(&game.game_id) {
                if dd.doomsday.unwrap_or(false) {
                    if let Some(disruption) = dd.pile_disruption.as_deref() {
                        // Split comma-separated values so each card is counted independently
                        for card in disruption.split(',') {
                            let card = card.trim();
                            if !card.is_empty() {
                                disruption_stats.entry(card.to_string()).or_default().push(game);
                            }
                        }
                    } else {
                        continue;
                    }
                } else {
                    continue;
                }
            } else {
                continue;
            }
        }

        let mut rows: Vec<StatsRow> = disruption_stats.into_iter()
            .map(|(label, games)| calculate_stats_from_games(label, games, all_matches))
            .filter(|row| row.game_count >= min_games as usize)
            .collect();

        rows.sort_by(|a, b| b.game_count.cmp(&a.game_count));
        display_stats_table(&rows, selected_stats, title, true);
        return;
    }

    if slice_type == "dd-intent" {
        let dd_map: HashMap<i32, &DoomsdayGame> = doomsday_games.iter()
            .map(|dd| (dd.game_id, dd))
            .collect();

        let mut intent_stats: HashMap<String, Vec<&Game>> = HashMap::new();
        for game in all_games {
            let label = if let Some(dd) = dd_map.get(&game.game_id) {
                match dd.dd_intent {
                    Some(1) => "DD Intent",
                    Some(0) => "No DD Intent",
                    _ => continue, // Skip games with NULL dd_intent
                }
            } else {
                continue;
            };
            intent_stats.entry(label.to_string()).or_default().push(game);
        }

        let mut rows: Vec<StatsRow> = intent_stats.into_iter()
            .map(|(label, games)| calculate_stats_from_games(label, games, all_matches))
            .filter(|row| row.game_count >= min_games as usize)
            .collect();

        let order = ["DD Intent", "No DD Intent"];
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
    let config = load_config();
    
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
        match_data.deck_name = select_deck_three_step(&config, Some(&match_data.deck_name.clone()));
    }
    
    // Edit opponent name
    let change_opponent = Confirm::new()
        .with_prompt(&format!("Change opponent from '{}'?", match_data.opponent_name))
        .interact()
        .unwrap();
        
    if change_opponent {
        let opponents = load_opponent_names();
        if let Some(new_opponent) = fuzzy_select_with_default("Opponent name", &opponents, &match_data.opponent_name) {
            match_data.opponent_name = new_opponent;
        }
    }

    // Edit opponent deck
    let change_deck = Confirm::new()
        .with_prompt(&format!("Change opponent deck from '{}'?", match_data.opponent_deck))
        .interact()
        .unwrap();

    if change_deck {
        let deck_names = load_deck_names();
        if let Some(new_deck) = fuzzy_select_with_default("Opponent's deck", &deck_names, &match_data.opponent_deck) {
            match_data.opponent_deck = new_deck;
        }
    }
    
    // Edit event type
    let change_event = Confirm::new()
        .with_prompt(&format!("Change event type from '{}'?", match_data.event_type))
        .interact()
        .unwrap();

    if change_event {
        let event_types: Vec<String> = EVENT_TYPES.iter().map(|s| s.to_string()).collect();
        if let Some(new_event) = fuzzy_select_with_default("Event type", &event_types, &match_data.event_type) {
            match_data.event_type = new_event;
        }
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

    // Load archetype data up front so all prompts can use the same option lists as game entry
    let match_data = matches::table.find(match_id).first::<Match>(connection).ok();
    let archetype = match_data.as_ref().and_then(|m| load_archetype_data(&m.deck_name));
    let is_doomsday_deck = archetype.as_ref().map_or(false, |a| a.is_doomsday);

    let game_plans = archetype.as_ref().map_or_else(load_game_plans, |a| a.game_plans.clone());
    let win_conditions = archetype.as_ref().map_or_else(load_win_conditions, |a| a.win_conditions.clone());
    let loss_reasons = archetype.as_ref().map_or_else(Vec::new, |a| a.loss_reasons.clone());

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
    if let Some(new_plan) = fuzzy_select_with_default("Opening hand plan", &game_plans, current_plan) {
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

    // Edit win condition / loss reason — use same option lists as game entry
    if game_data.game_winner == "me" && !is_doomsday_deck {
        let current_condition = game_data.win_condition.as_deref().unwrap_or("");
        if let Some(new_condition) = fuzzy_select_with_default("What did you win with?", &win_conditions, current_condition) {
            game_data.win_condition = Some(new_condition);
        }
        game_data.loss_reason = None;
    } else if game_data.game_winner != "me" && !is_doomsday_deck {
        game_data.win_condition = None;
        let current_reason = game_data.loss_reason.as_deref().unwrap_or("");
        if let Some(new_reason) = fuzzy_select_with_default("Why did you lose?", &loss_reasons, current_reason) {
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

    // Doomsday-specific fields
    if is_doomsday_deck {
        // Load existing doomsday data or create new
        let existing_dd = doomsday_games::table
            .filter(doomsday_games::game_id.eq(game_data.game_id))
            .first::<DoomsdayGame>(connection)
            .ok();

        let edit_doomsday = if existing_dd.is_some() {
            Confirm::new()
                .with_prompt("Edit doomsday-specific fields?")
                .default(false)
                .interact()
                .unwrap_or(false)
        } else {
            Confirm::new()
                .with_prompt("Add doomsday-specific data for this game?")
                .default(false)
                .interact()
                .unwrap_or(false)
        };

        if edit_doomsday {
            let arch = archetype.as_ref();

            println!("\n--- G{} Doomsday Details ---", game_number);

            // Get current values or defaults
            let current_dd_intent = existing_dd.as_ref().and_then(|d| d.dd_intent).map(|v| v != 0).unwrap_or(true);
            let current_resolved = existing_dd.as_ref().and_then(|d| d.doomsday).unwrap_or(false);
            let current_pile_type = existing_dd.as_ref().and_then(|d| d.pile_type.clone());
            let current_better_pile = existing_dd.as_ref().and_then(|d| d.better_pile).map(|b| b != 0);
            let current_no_dd_reason = existing_dd.as_ref().and_then(|d| d.no_doomsday_reason.clone());
            let current_sb_juke = existing_dd.as_ref().and_then(|d| d.sb_juke_plan.clone());
            let current_pile_disruption = existing_dd.as_ref().and_then(|d| d.pile_disruption.clone());

            // Edit dd_intent
            let dd_intent = Confirm::new()
                .with_prompt(&format!("Did you plan to cast Doomsday? [{}]", if current_dd_intent { "yes" } else { "no" }))
                .default(current_dd_intent)
                .interact()
                .unwrap_or(current_dd_intent);

            // Edit doomsday resolved
            let doomsday_resolved = Confirm::new()
                .with_prompt(&format!("Did Doomsday resolve? [{}]", if current_resolved { "yes" } else { "no" }))
                .default(current_resolved)
                .interact()
                .unwrap_or(current_resolved);

            // Edit pile type (if doomsday resolved)
            let pile_type = if doomsday_resolved {
                let pile_types = arch.as_ref()
                    .map(|a| a.common_pile_types.clone())
                    .unwrap_or_default();
                let current_display = current_pile_type.as_deref().unwrap_or("none");
                println!("Current pile type: {}", current_display);
                if !pile_types.is_empty() {
                    fuzzy_select("Pile type (or Enter to keep current)", &pile_types)
                        .or(current_pile_type)
                } else {
                    let new_type: String = Input::new()
                        .with_prompt(&format!("Pile type [{}]", current_display))
                        .allow_empty(true)
                        .interact_text()
                        .unwrap_or_default();
                    if new_type.is_empty() { current_pile_type } else { Some(new_type) }
                }
            } else {
                None
            };

            // Edit pile_disruption (if doomsday resolved)
            let pile_disruption = if doomsday_resolved {
                let current_display = current_pile_disruption.as_deref().unwrap_or("none");
                println!("Current disruption: {}", current_display);
                if let Some(a) = arch.as_ref() {
                    collect_pile_disruption(a).or(current_pile_disruption)
                } else {
                    current_pile_disruption
                }
            } else {
                None
            };

            // Edit better_pile (only if lost and doomsday resolved)
            let better_pile = if game_data.game_winner == "opponent" && doomsday_resolved {
                let current_display = current_better_pile.map(|b| if b { "yes" } else { "no" }).unwrap_or("not set");
                Some(Confirm::new()
                    .with_prompt(&format!("Could you have won with a better pile/play? [{}]", current_display))
                    .default(current_better_pile.unwrap_or(false))
                    .interact()
                    .unwrap_or(false))
            } else {
                None
            };

            // Edit no_doomsday_reason (only if doomsday didn't resolve)
            let no_doomsday_reason = if !doomsday_resolved {
                let no_dd_reasons = arch.as_ref()
                    .map(|a| a.no_doomsday_reasons.clone())
                    .unwrap_or_default();
                let current_display = current_no_dd_reason.as_deref().unwrap_or("none");
                println!("Current reason: {}", current_display);
                if !no_dd_reasons.is_empty() {
                    fuzzy_select("Why didn't Doomsday resolve? (or Enter to keep)", &no_dd_reasons)
                        .or(current_no_dd_reason)
                } else {
                    let new_reason: String = Input::new()
                        .with_prompt(&format!("Why didn't Doomsday resolve? [{}]", current_display))
                        .allow_empty(true)
                        .interact_text()
                        .unwrap_or_default();
                    if new_reason.is_empty() { current_no_dd_reason } else { Some(new_reason) }
                }
            } else {
                None
            };

            // Edit sb_juke_plan (for games 2-3)
            let sb_juke_plan = if game_number > 1 {
                let juke_options = vec!["full juke".to_string(), "partial juke".to_string(), "no juke".to_string()];
                let current_display = current_sb_juke.as_deref().unwrap_or("none");
                println!("Current sideboard plan for G{}: {}", game_number, current_display);
                fuzzy_select(&format!("Sideboard plan for G{} (or Enter to keep)", game_number), &juke_options)
                    .or(current_sb_juke)
            } else {
                current_sb_juke
            };

            // Save doomsday data
            if existing_dd.is_some() {
                diesel::update(doomsday_games::table.filter(doomsday_games::game_id.eq(game_data.game_id)))
                    .set((
                        doomsday_games::dd_intent.eq(Some(dd_intent as i32)),
                        doomsday_games::doomsday.eq(Some(doomsday_resolved)),
                        doomsday_games::pile_type.eq(&pile_type),
                        doomsday_games::better_pile.eq(better_pile.map(|b| if b { 1 } else { 0 })),
                        doomsday_games::no_doomsday_reason.eq(&no_doomsday_reason),
                        doomsday_games::sb_juke_plan.eq(&sb_juke_plan),
                        doomsday_games::pile_disruption.eq(&pile_disruption),
                    ))
                    .execute(connection)
                    .expect("Error updating doomsday data");
            } else {
                let new_dd = NewDoomsdayGame {
                    game_id: game_data.game_id,
                    doomsday: Some(doomsday_resolved),
                    pile_cards: None,
                    pile_plan: None,
                    juke: None,
                    pile_type,
                    better_pile: better_pile.map(|b| if b { 1 } else { 0 }),
                    no_doomsday_reason,
                    sb_juke_plan,
                    pile_disruption,
                    dd_intent: Some(dd_intent as i32),
                };
                diesel::insert_into(doomsday_games::table)
                    .values(&new_dd)
                    .execute(connection)
                    .expect("Error saving doomsday data");
            }
            println!("Doomsday data updated!");
        }
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
    let category_options: Vec<String> = vec!["Blue", "Combo", "Non-Blue", "Stompy"].iter().map(|s| s.to_string()).collect();
    let category = fuzzy_select("Select deck category", &category_options)
        .unwrap_or_else(|| "Blue".to_string());
    
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
            fuzzy_select("Select opponent deck to see board plan", &deck_names)
                .unwrap_or_else(|| "Unknown".to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_definition_files_parse() {
        let mut parsed = Vec::new();
        let mut errors = Vec::new();

        if let Ok(entries) = fs::read_dir("definitions") {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("toml") {
                    match fs::read_to_string(&path) {
                        Ok(content) => {
                            match toml::from_str::<UnifiedArchetypeDefinition>(&content) {
                                Ok(unified) => {
                                    parsed.push(unified.name.clone());
                                }
                                Err(e) => {
                                    errors.push(format!("{:?}: {}", path, e));
                                }
                            }
                        }
                        Err(e) => {
                            errors.push(format!("{:?}: read error: {}", path, e));
                        }
                    }
                }
            }
        }

        if !errors.is_empty() {
            panic!("Failed to parse definition files:\n{}", errors.join("\n"));
        }

        // Verify we loaded a reasonable number of archetypes
        assert!(parsed.len() >= 30, "Expected at least 30 archetypes, got {}", parsed.len());
    }

    #[test]
    fn test_load_archetypes_returns_all() {
        let archetypes = load_archetypes();

        // Verify key archetypes are present
        let expected = vec![
            "Affinity", "Aggro", "Beanstalk", "Control", "Death and Taxes",
            "Depths", "Doomsday", "Dredge", "Lands", "Painter", "Reanimator",
            "Stompy", "Storm", "Tempo",
        ];

        for name in expected {
            assert!(
                archetypes.iter().any(|a| a == name),
                "Missing archetype: {}. Found: {:?}", name, archetypes
            );
        }
    }

    #[test]
    fn test_load_subtypes() {
        // Test that subtypes load correctly for archetypes that have them
        let doomsday_subtypes = load_subtypes("Doomsday");
        assert!(doomsday_subtypes.contains(&"Tempo".to_string()), "Doomsday should have Tempo subtype");
        assert!(doomsday_subtypes.contains(&"Turbo".to_string()), "Doomsday should have Turbo subtype");

        let storm_subtypes = load_subtypes("Storm");
        assert!(storm_subtypes.contains(&"ANT".to_string()), "Storm should have ANT subtype");
        assert!(storm_subtypes.contains(&"TES".to_string()), "Storm should have TES subtype");

        let affinity_subtypes = load_subtypes("Affinity");
        assert!(affinity_subtypes.contains(&"8-Cast".to_string()), "Affinity should have 8-Cast subtype");
    }

    #[test]
    fn test_fuzzy_select_decision_tab_uses_query() {
        // Tab pressed with query -> use query as new entry
        assert_eq!(
            fuzzy_select_decision("newplayer", Some("oldplayer"), true),
            Some("newplayer".to_string())
        );
        // Tab with empty query -> None
        assert_eq!(fuzzy_select_decision("", Some("oldplayer"), true), None);
        assert_eq!(fuzzy_select_decision("  ", Some("oldplayer"), true), None);
    }

    #[test]
    fn test_fuzzy_select_decision_enter_uses_selection() {
        // Enter with selection -> use selection
        assert_eq!(
            fuzzy_select_decision("partial", Some("partially_matched"), false),
            Some("partially_matched".to_string())
        );
        // Enter with no selection but query -> use query
        assert_eq!(
            fuzzy_select_decision("newname", None, false),
            Some("newname".to_string())
        );
        // Enter with no selection and no query -> None
        assert_eq!(fuzzy_select_decision("", None, false), None);
    }

    #[test]
    fn test_fuzzy_select_decision_trims_whitespace() {
        assert_eq!(
            fuzzy_select_decision("  trimmed  ", None, false),
            Some("trimmed".to_string())
        );
        assert_eq!(
            fuzzy_select_decision("  tabbed  ", Some("other"), true),
            Some("tabbed".to_string())
        );
    }
}

