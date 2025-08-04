use clap::{Args, Subcommand};
use dialoguer::{Input, Select, Confirm};
use chrono::{Local, NaiveDate};
use diesel::prelude::*;
use std::fs;
use std::collections::HashMap;

use crate::db::{establish_connection, models::*};
use crate::db::schema::{matches, games};

fn load_deck_names() -> Vec<String> {
    match fs::read_to_string("deck_names.txt") {
        Ok(content) => {
            content.lines()
                .map(|line| {
                    let line = line.trim();
                    if line.is_empty() {
                        return String::new();
                    }
                    if let Some((deck_name, _category)) = line.split_once(';') {
                        deck_name.trim().to_string()
                    } else {
                        line.to_string()
                    }
                })
                .filter(|line| !line.is_empty())
                .collect()
        },
        Err(_) => {
            // Fallback to hardcoded list if file doesn't exist
            vec![
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
            ]
        }
    }
}

fn load_deck_categories() -> HashMap<String, DeckCategory> {
    let mut categories = HashMap::new();
    
    match fs::read_to_string("deck_names.txt") {
        Ok(content) => {
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                
                if let Some((deck_name, category_str)) = line.split_once(';') {
                    let deck_name = deck_name.trim().to_string();
                    let category_str = category_str.trim();
                    
                    let category = match category_str {
                        "Blue" => DeckCategory::Blue,
                        "Combo" => DeckCategory::Combo,
                        "Non-Blue" => DeckCategory::NonBlue,
                        "Stompy" => DeckCategory::NonBlue, // Map Stompy to Non-Blue
                        "No Category" => DeckCategory::NonBlue, // Default for "Other"
                        _ => DeckCategory::NonBlue, // Default fallback
                    };
                    
                    categories.insert(deck_name, category);
                }
            }
        },
        Err(_) => {
            // Fallback categories if file doesn't exist - empty map will use the hardcoded categorize_deck function
        }
    }
    
    categories
}

const EVENT_TYPES: &[&str] = &[
    "League", "Paper", "Casual", "Challenge", "Prelim", "Other"
];

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DeckCategory {
    Blue,
    Combo,
    NonBlue,
}

impl DeckCategory {
    pub fn to_string(&self) -> &'static str {
        match self {
            DeckCategory::Blue => "Blue",
            DeckCategory::Combo => "Combo", 
            DeckCategory::NonBlue => "Non-Blue",
        }
    }
}

pub fn categorize_deck(deck_name: &str) -> DeckCategory {
    let categories = load_deck_categories();
    
    // First try to get from file
    if let Some(category) = categories.get(deck_name) {
        return category.clone();
    }
    
    // Fallback to hardcoded categorization if not found in file
    match deck_name {
        // Combo decks
        "Reanimator: UB" | "Reanimator: BR" => DeckCategory::Combo,
        "Omni-tell" | "Sneak and Show" => DeckCategory::Combo,
        "Oops! All Spells" | "Cephalid Breakfast" | "Doomsday" => DeckCategory::Combo,
        "Storm: TES" | "Storm: ANT" | "Storm: Ruby" | "Storm: Black Saga" => DeckCategory::Combo,
        "Combo Elves" => DeckCategory::Combo,
        "Dredge" => DeckCategory::Combo,
        "Stiflenaught" => DeckCategory::Blue,
        "Infect" => DeckCategory::Combo,
        "Mystic Forge" => DeckCategory::Combo,
        "Nadu: Elves" => DeckCategory::Combo,
        
        // Blue decks (non-combo)
        "Tempo: UB" | "Tempo: UR" => DeckCategory::Blue,
        "Painter: U" => DeckCategory::Blue,
        "Nadu: Midrange" => DeckCategory::Blue,
        "Beanstalk: BUG" | "Beanstalk: Domain" | "Beanstalk: Yorion" | "Beanstalk: Sultai" => DeckCategory::Blue,
        "Stoneblade" => DeckCategory::Blue,
        "Miracles" => DeckCategory::Blue,
        "Merfolk" => DeckCategory::Blue,
        
        // Non-blue decks
        "Stompy: Moon" | "Stompy: Eldrazi" => DeckCategory::NonBlue,
        "Lands" => DeckCategory::NonBlue,
        "Painter: R" => DeckCategory::NonBlue,
        "Goblins" => DeckCategory::NonBlue,
        "Maverick: GW" => DeckCategory::NonBlue,
        "Cradle Control" => DeckCategory::NonBlue,
        "Cloudpost" => DeckCategory::NonBlue,
        
        // Default to Non-Blue for Other and unknown decks
        _ => DeckCategory::NonBlue,
    }
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
    Stats {
        #[arg(long, help = "Filter by deck name")]
        deck: Option<String>,
        #[arg(long, help = "Filter by event type")]
        event: Option<String>,
        #[arg(long, help = "Show interactive slice selection menu")]
        slice: bool,
        #[arg(long, help = "Slice by opponent")]
        by_opponent: bool,
        #[arg(long, help = "Slice by opponent deck")]
        by_opponent_deck: bool,
        #[arg(long, help = "Slice by opponent deck category")]
        by_opponent_deck_category: bool,
        #[arg(long, help = "Slice by game number")]
        by_game_number: bool,
        #[arg(long, help = "Slice by mulligan count")]
        by_mulligans: bool,
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
        GameCommands::Stats { 
            deck, 
            event, 
            slice, 
            by_opponent, 
            by_opponent_deck, 
            by_opponent_deck_category, 
            by_game_number, 
            by_mulligans 
        } => show_stats(deck, event, slice, by_opponent, by_opponent_deck, by_opponent_deck_category, by_game_number, by_mulligans),
    }
}

fn add_match_interactive(date_arg: Option<String>) {
    println!("=== Adding New Match ===");
    
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
    
    // Get deck name
    let deck_name: String = Input::new()
        .with_prompt("Your deck name")
        .interact_text()
        .unwrap();
    
    // Get opponent name
    let opponent_name: String = Input::new()
        .with_prompt("Opponent name")
        .interact_text()
        .unwrap();
    
    // Opponent deck will be set after the match
    
    // Get event type
    let event_type_idx = Select::new()
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
    
    // Create the match without winner and opponent deck (will be determined after games)
    let new_match = NewMatch {
        date,
        deck_name,
        opponent_name,
        opponent_deck: "unknown".to_string(), // Will be updated after match
        event_type,
        die_roll_winner: die_roll_winner.to_string(),
        match_winner: "unknown".to_string(), // Will be updated after games
    };
    
    let connection = &mut establish_connection();
    
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
    let match_winner = add_games_interactive(connection, match_id);
    
    // Check if opponent deck is still unknown after all games
    let current_match = matches::table
        .find(match_id)
        .first::<Match>(connection)
        .expect("Error loading current match");
        
    if current_match.opponent_deck == "unknown" {
        println!("\n=== Match Complete ===");
        let deck_names = load_deck_names();
        let deck_names_refs: Vec<&str> = deck_names.iter().map(|s| s.as_str()).collect();
        let opponent_deck_idx = Select::new()
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

fn add_games_interactive(connection: &mut SqliteConnection, match_id: i32) -> Winner {
    println!("\n=== Adding Games (Best of 3) ===");
    
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
        
        // Opening hand plan
        let opening_hand_plan: String = Input::new()
            .with_prompt("Opening hand plan")
            .allow_empty(true)
            .interact_text()
            .unwrap();
        
        let opening_hand_plan = if opening_hand_plan.is_empty() {
            None
        } else {
            Some(opening_hand_plan)
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
        
        // Win condition (only if you won)
        let win_condition = if matches!(game_winner, Winner::Me) {
            let condition: String = Input::new()
                .with_prompt("What did you win with?")
                .allow_empty(true)
                .interact_text()
                .unwrap();
            
            if condition.is_empty() { None } else { Some(condition) }
        } else {
            None
        };
        
        // Save the game
        let new_game = NewGame {
            match_id,
            game_number: game_num,
            play_draw: play_draw.to_string(),
            mulligans,
            opening_hand_plan,
            game_winner: game_winner.to_string(),
            win_condition,
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
                let opponent_deck_idx = Select::new()
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
        .order(matches::match_id.desc())
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
    }
}

fn show_stats(
    deck_filter: Option<String>, 
    event_filter: Option<String>, 
    interactive_slice: bool,
    by_opponent: bool,
    by_opponent_deck: bool, 
    by_opponent_deck_category: bool,
    by_game_number: bool,
    by_mulligans: bool
) {
    let connection = &mut establish_connection();
    
    // Build the base query
    let mut query = matches::table.into_boxed();
    
    if let Some(deck) = &deck_filter {
        query = query.filter(matches::deck_name.like(format!("%{}%", deck)));
    }
    
    if let Some(event) = &event_filter {
        query = query.filter(matches::event_type.like(format!("%{}%", event)));
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
    println!();
    // Get all games for these matches
    let match_ids: Vec<i32> = all_matches.iter().map(|m| m.match_id).collect();
    let all_games = games::table
        .filter(games::match_id.eq_any(&match_ids))
        .load::<Game>(connection)
        .expect("Error loading games");
    
    // Show overall statistics first
    show_overall_stats(&all_matches, &all_games);
    
    // Handle slice selection - determine which slices to show
    let mut slices_to_show = Vec::new();
    
    if by_opponent {
        slices_to_show.push("opponent");
    }
    if by_opponent_deck {
        slices_to_show.push("opponent-deck");
    }
    if by_opponent_deck_category {
        slices_to_show.push("deck-category");
    }
    if by_game_number {
        slices_to_show.push("game-number");
    }
    if by_mulligans {
        slices_to_show.push("mulligans");
    }
    
    if interactive_slice {
        // Interactive slice selection
        let slice_options = vec![
            "None (no slicing)",
            "opponent",
            "opponent-deck", 
            "deck-category",
            "game-number",
            "mulligans"
        ];
        
        let selection = Select::new()
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
                show_sliced_stats(&all_matches, &all_games, slice_type);
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
            show_sliced_stats(&all_matches, &all_games, slice_type);
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
    println!("  Average Mulligans: {:.2}", avg_mulligans);
    println!();
}

fn show_sliced_stats(all_matches: &[Match], all_games: &[Game], slice_type: &str) {
    match slice_type {
        "opponent" => {
            println!("=== Statistics by Opponent ===");
            let mut opponent_stats: std::collections::HashMap<String, Vec<&Match>> = std::collections::HashMap::new();
            for m in all_matches {
                opponent_stats.entry(m.opponent_name.clone()).or_default().push(m);
            }
            
            let mut opponent_vec: Vec<_> = opponent_stats.into_iter().collect();
            opponent_vec.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
            
            for (opponent, matches) in opponent_vec {
                let wins = matches.iter().filter(|m| m.match_winner == "me").count();
                let total = matches.len();
                let win_rate = if total > 0 { (wins as f64 / total as f64) * 100.0 } else { 0.0 };
                println!("  vs {}: {}-{} ({:.1}%)", opponent, wins, total - wins, win_rate);
            }
        },
        
        "opponent-deck" => {
            println!("=== Statistics by Opponent Deck ===");
            let mut deck_stats: std::collections::HashMap<String, Vec<&Match>> = std::collections::HashMap::new();
            for m in all_matches {
                deck_stats.entry(m.opponent_deck.clone()).or_default().push(m);
            }
            
            let mut deck_vec: Vec<_> = deck_stats.into_iter().collect();
            deck_vec.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
            
            for (deck, matches) in deck_vec {
                let wins = matches.iter().filter(|m| m.match_winner == "me").count();
                let total = matches.len();
                let win_rate = if total > 0 { (wins as f64 / total as f64) * 100.0 } else { 0.0 };
                println!("  vs {}: {}-{} ({:.1}%)", deck, wins, total - wins, win_rate);
            }
        },
        
        "deck-category" => {
            println!("=== Statistics by Deck Category ===");
            let mut category_stats: std::collections::HashMap<DeckCategory, Vec<&Match>> = std::collections::HashMap::new();
            for m in all_matches {
                let category = categorize_deck(&m.opponent_deck);
                category_stats.entry(category).or_default().push(m);
            }
            
            for (category, matches) in category_stats {
                let wins = matches.iter().filter(|m| m.match_winner == "me").count();
                let total = matches.len();
                let win_rate = if total > 0 { (wins as f64 / total as f64) * 100.0 } else { 0.0 };
                println!("  vs {} decks: {}-{} ({:.1}%)", category.to_string(), wins, total - wins, win_rate);
            }
        },
        
        "game-number" => {
            println!("=== Statistics by Game Number ===");
            let mut game_stats: std::collections::HashMap<i32, Vec<&Game>> = std::collections::HashMap::new();
            for g in all_games {
                game_stats.entry(g.game_number).or_default().push(g);
            }
            
            for game_num in 1..=3 {
                if let Some(games) = game_stats.get(&game_num) {
                    let wins = games.iter().filter(|g| g.game_winner == "me").count();
                    let total = games.len();
                    let win_rate = if total > 0 { (wins as f64 / total as f64) * 100.0 } else { 0.0 };
                    println!("  Game {}: {}-{} ({:.1}%)", game_num, wins, total - wins, win_rate);
                }
            }
        },
        
        "mulligans" => {
            println!("=== Statistics by Mulligan Count ===");
            let mut mulligan_stats: std::collections::HashMap<i32, Vec<&Game>> = std::collections::HashMap::new();
            for g in all_games {
                mulligan_stats.entry(g.mulligans).or_default().push(g);
            }
            
            let mut mulligan_vec: Vec<_> = mulligan_stats.into_iter().collect();
            mulligan_vec.sort_by_key(|&(mulligans, _)| mulligans);
            
            for (mulligans, games) in mulligan_vec {
                let wins = games.iter().filter(|g| g.game_winner == "me").count();
                let total = games.len();
                let win_rate = if total > 0 { (wins as f64 / total as f64) * 100.0 } else { 0.0 };
                println!("  {} mulligans: {}-{} ({:.1}%)", mulligans, wins, total - wins, win_rate);
            }
        },
        
        _ => {
            println!("Unknown slice type: {}. Available options: opponent, opponent-deck, deck-category, game-number, mulligans", slice_type);
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
    let new_deck_name: String = Input::new()
        .with_prompt(&format!("Your deck name [{}]", match_data.deck_name))
        .allow_empty(true)
        .interact_text()
        .unwrap();
    if !new_deck_name.is_empty() {
        match_data.deck_name = new_deck_name;
    }
    
    // Edit opponent name
    let new_opponent_name: String = Input::new()
        .with_prompt(&format!("Opponent name [{}]", match_data.opponent_name))
        .allow_empty(true)
        .interact_text()
        .unwrap();
    if !new_opponent_name.is_empty() {
        match_data.opponent_name = new_opponent_name;
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
        let opponent_deck_idx = Select::new()
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
        let event_type_idx = Select::new()
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
    let category_idx = Select::new()
        .with_prompt("Select deck category")
        .items(&category_options)
        .default(0)
        .interact()
        .unwrap();
    let category = category_options[category_idx];
    
    // Read the existing file content to preserve format
    let mut lines = Vec::new();
    if let Ok(content) = fs::read_to_string("deck_names.txt") {
        for line in content.lines() {
            let line = line.trim();
            if !line.is_empty() {
                lines.push(line.to_string());
            }
        }
    }
    
    // Add the new deck with category
    let new_entry = format!("{}; {}", deck_name, category);
    lines.push(new_entry);
    
    // Sort the list (keep "Other" at the end if it exists)
    let other_pos = lines.iter().position(|line| line.starts_with("Other;"));
    if let Some(pos) = other_pos {
        let other = lines.remove(pos);
        lines.sort();
        lines.push(other);
    } else {
        lines.sort();
    }
    
    // Write back to file
    let content = lines.join("\n");
    match fs::write("deck_names.txt", content) {
        Ok(_) => println!("Added '{}' with category '{}' to deck list", deck_name, category),
        Err(e) => println!("Error writing to deck_names.txt: {}", e),
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
            
            let selection = Select::new()
                .with_prompt("Select opponent deck to see board plan")
                .items(&deck_names_refs)
                .default(0)
                .interact()
                .unwrap();
                
            deck_names[selection].clone()
        }
    };
    
    let board_plans = load_board_plans();
    
    println!("=== Board Plan vs {} ===", deck_name);
    
    match board_plans.get(&deck_name) {
        Some(plan) => {
            println!("{}", plan);
        },
        None => {
            println!("No board plan found for '{}'", deck_name);
            println!("You can add one by editing board_plans.txt");
            println!("Format: Deck Name | Board Plan");
        }
    }
}
