use clap::{Args, Subcommand};
use dialoguer::{Confirm, Input, MultiSelect, FuzzySelect};
use diesel::prelude::*;
use std::fs;
use std::process::Command;
use std::env;
use std::collections::HashMap;

use crate::db::{establish_connection, models::*};
use crate::db::schema::{decks, cards};

#[derive(Args)]
pub struct DeckArgs {
    #[command(subcommand)]
    command: DeckCommands,
}

#[derive(Subcommand)]
enum DeckCommands {
    Import {
        #[arg(help = "Name for the deck")]
        name: String,
        #[arg(long, help = "Moxfield URL for reference")]
        url: Option<String>,
    },
    List,
    View,
    Delete,
    Probability,
    Sequential,
    BackfillEras,
}

pub fn run(args: DeckArgs) {
    match args.command {
        DeckCommands::Import { name, url } => import_deck(&name, url),
        DeckCommands::List => list_decks(),
        DeckCommands::View => view_deck_interactive(),
        DeckCommands::Delete => delete_deck_interactive(),
        DeckCommands::Probability => calculate_probability_interactive(),
        DeckCommands::Sequential => sequential_probability_interactive(),
        DeckCommands::BackfillEras => backfill_eras(),
    }
}

fn import_deck(name: &str, moxfield_url: Option<String>) {
    let connection = &mut establish_connection();
    
    // Check if deck name already exists
    let existing: Result<Deck, _> = decks::table
        .filter(decks::name.eq(name))
        .first(connection);
        
    if existing.is_ok() {
        println!("Deck '{}' already exists. Use a different name.", name);
        return;
    }
    
    println!("=== Importing Deck: {} ===", name);
    println!("You will now open an editor to paste your deck list.");
    println!("Format: Each line should be: [quantity] [card name]");
    println!("Use a blank line to separate mainboard from sideboard.");
    println!("Example:");
    println!("4 Lightning Bolt");
    println!("2 Counterspell");
    println!();
    println!("1 Pyroblast");
    println!("2 Red Elemental Blast");
    
    if !Confirm::new()
        .with_prompt("Ready to open editor?")
        .default(true)
        .interact()
        .unwrap()
    {
        println!("Import cancelled");
        return;
    }
    
    // Create temporary file
    let temp_file = format!("/tmp/mtgctl_deck_{}.txt", name.replace(' ', "_"));
    
    // Create initial content
    let initial_content = format!(
        "# Deck: {}\n# Paste your deck list below\n# Format: [quantity] [card name]\n# Separate mainboard and sideboard with a blank line\n\n# Mainboard\n\n\n# Sideboard\n\n",
        name
    );
    
    fs::write(&temp_file, initial_content).expect("Failed to create temp file");
    
    // Open editor
    let editor = env::var("EDITOR").unwrap_or_else(|_| "nano".to_string());
    let status = Command::new(&editor)
        .arg(&temp_file)
        .status()
        .expect("Failed to start editor");
    
    if !status.success() {
        println!("Editor exited with error");
        fs::remove_file(&temp_file).ok();
        return;
    }
    
    // Read the edited content
    let content = match fs::read_to_string(&temp_file) {
        Ok(content) => content,
        Err(_) => {
            println!("Failed to read temp file");
            fs::remove_file(&temp_file).ok();
            return;
        }
    };
    
    // Clean up temp file
    fs::remove_file(&temp_file).ok();
    
    // Parse the deck list
    let (mainboard, sideboard) = parse_deck_content(&content);
    
    if mainboard.is_empty() && sideboard.is_empty() {
        println!("No cards found in deck list");
        return;
    }
    
    println!("Parsed {} mainboard cards, {} sideboard cards", 
             mainboard.len(), sideboard.len());
    
    // Confirm import
    if !Confirm::new()
        .with_prompt("Import this deck?")
        .default(true)
        .interact()
        .unwrap()
    {
        println!("Import cancelled");
        return;
    }

    // Parse era from deck name (pattern: name-X.Y -> era = X)
    let default_era = parse_era_from_name(name);

    // Prompt for era
    let era_input: String = Input::new()
        .with_prompt("What era is this deck from?")
        .default(default_era.map(|e| e.to_string()).unwrap_or_else(|| "1".to_string()))
        .interact_text()
        .unwrap();

    let era = era_input.parse::<i32>().ok();

    // Create deck in database
    let new_deck = NewDeck {
        name: name.to_string(),
        moxfield_url,
        era,
    };
    
    diesel::insert_into(decks::table)
        .values(&new_deck)
        .execute(connection)
        .expect("Error saving deck");
    
    // Get the deck ID
    let deck_id: i32 = decks::table
        .select(decks::deck_id)
        .filter(decks::name.eq(name))
        .first(connection)
        .expect("Error getting deck ID");
    
    // Insert cards
    let mut new_cards = Vec::new();
    
    for (card_name, quantity) in mainboard {
        new_cards.push(NewCard {
            deck_id,
            card_name,
            quantity,
            board: "main".to_string(),
        });
    }
    
    for (card_name, quantity) in sideboard {
        new_cards.push(NewCard {
            deck_id,
            card_name,
            quantity,
            board: "side".to_string(),
        });
    }
    
    diesel::insert_into(cards::table)
        .values(&new_cards)
        .execute(connection)
        .expect("Error saving cards");
    
    println!("Successfully imported deck '{}'!", name);
    println!("This deck is now available when adding matches.");
}

fn parse_deck_content(content: &str) -> (Vec<(String, i32)>, Vec<(String, i32)>) {
    let mut mainboard = Vec::new();
    let mut sideboard = Vec::new();
    let mut in_sideboard = false;
    
    for line in content.lines() {
        let line = line.trim();
        
        // Skip comments and empty lines at the start
        if line.starts_with('#') {
            continue;
        }
        
        // Empty line switches to sideboard
        if line.is_empty() {
            if !in_sideboard && !mainboard.is_empty() {
                in_sideboard = true;
            }
            continue;
        }
        
        // Parse card line: "quantity card name"
        let parts: Vec<&str> = line.splitn(2, ' ').collect();
        if parts.len() >= 2 {
            if let Ok(quantity) = parts[0].parse::<i32>() {
                let card_name = parts[1].trim();
                // Remove set codes and collector numbers: "Card Name (SET) 123" -> "Card Name"
                let clean_name = if let Some(paren_pos) = card_name.find(" (") {
                    card_name[..paren_pos].trim()
                } else {
                    card_name
                };
                
                if !clean_name.is_empty() && quantity > 0 {
                    if in_sideboard {
                        sideboard.push((clean_name.to_string(), quantity));
                    } else {
                        mainboard.push((clean_name.to_string(), quantity));
                    }
                }
            }
        }
    }
    
    (mainboard, sideboard)
}

fn list_decks() {
    let connection = &mut establish_connection();
    
    let deck_list: Result<Vec<Deck>, _> = decks::table
        .order(decks::created_at.desc())
        .load(connection);
    
    match deck_list {
        Ok(decks_vec) => {
            if decks_vec.is_empty() {
                println!("No decks found. Import a deck first with: mtgctl deck import <name>");
                return;
            }
            
            println!("=== Your Decks ===");
            println!("{:<4} {:<30} {:<20} {}", "ID", "Name", "Created", "URL");
            println!("{}", "-".repeat(80));
            
            for deck in decks_vec {
                let url_display = deck.moxfield_url
                    .as_deref()
                    .unwrap_or("No URL");
                let created = deck.created_at
                    .as_deref()
                    .unwrap_or("Unknown");
                    
                println!("{:<4} {:<30} {:<20} {}", 
                         deck.deck_id, 
                         truncate(&deck.name, 30),
                         truncate(created, 20),
                         truncate(url_display, 30));
            }
        }
        Err(_) => {
            println!("Error loading decks");
        }
    }
}

fn view_deck(deck_identifier: &str) {
    let connection = &mut establish_connection();
    
    // Try to find deck by name or ID
    let deck: Result<Deck, _> = if let Ok(deck_id) = deck_identifier.parse::<i32>() {
        decks::table.find(deck_id).first(connection)
    } else {
        decks::table
            .filter(decks::name.eq(deck_identifier))
            .first(connection)
    };
    
    let deck = match deck {
        Ok(d) => d,
        Err(_) => {
            println!("Deck '{}' not found", deck_identifier);
            return;
        }
    };
    
    // Load cards for this deck
    let deck_cards: Result<Vec<Card>, _> = cards::table
        .filter(cards::deck_id.eq(deck.deck_id))
        .order((cards::board.asc(), cards::card_name.asc()))
        .load(connection);
    
    let deck_cards = match deck_cards {
        Ok(cards) => cards,
        Err(_) => {
            println!("Error loading cards for deck");
            return;
        }
    };
    
    println!("=== {} ===", deck.name);
    if let Some(url) = &deck.moxfield_url {
        println!("Moxfield URL: {}", url);
    }
    if let Some(created) = &deck.created_at {
        println!("Created: {}", created);
    }
    println!();
    
    // Display mainboard
    let mainboard_cards: Vec<_> = deck_cards.iter()
        .filter(|c| c.board == "main")
        .collect();
    
    if !mainboard_cards.is_empty() {
        let total_main: i32 = mainboard_cards.iter().map(|c| c.quantity).sum();
        println!("Mainboard ({} cards):", total_main);
        for card in mainboard_cards {
            println!("  {} {}", card.quantity, card.card_name);
        }
        println!();
    }
    
    // Display sideboard
    let sideboard_cards: Vec<_> = deck_cards.iter()
        .filter(|c| c.board == "side")
        .collect();
    
    if !sideboard_cards.is_empty() {
        let total_side: i32 = sideboard_cards.iter().map(|c| c.quantity).sum();
        println!("Sideboard ({} cards):", total_side);
        for card in sideboard_cards {
            println!("  {} {}", card.quantity, card.card_name);
        }
    }
}

fn select_deck() -> Option<String> {
    let connection = &mut establish_connection();
    
    let deck_list: Result<Vec<Deck>, _> = decks::table
        .order(decks::created_at.desc())
        .load(connection);
    
    let decks_vec = match deck_list {
        Ok(decks) => decks,
        Err(_) => {
            println!("Error loading decks");
            return None;
        }
    };
    
    if decks_vec.is_empty() {
        println!("No decks found. Import a deck first with: mtgctl deck import <name>");
        return None;
    }
    
    let deck_options: Vec<String> = decks_vec.iter()
        .map(|deck| deck.name.clone())
        .collect();
    
    let selection = FuzzySelect::new()
        .with_prompt("Select a deck")
        .items(&deck_options)
        .interact()
        .unwrap();
    
    Some(deck_options[selection].clone())
}

fn view_deck_interactive() {
    if let Some(deck_name) = select_deck() {
        view_deck(&deck_name);
    }
}

fn delete_deck_interactive() {
    if let Some(deck_name) = select_deck() {
        delete_deck(&deck_name);
    }
}

fn calculate_probability_interactive() {
    if let Some(deck_name) = select_deck() {
        calculate_probability(&deck_name);
    }
}

fn sequential_probability_interactive() {
    if let Some(deck_name) = select_deck() {
        sequential_probability(&deck_name);
    }
}

fn delete_deck(deck_identifier: &str) {
    let connection = &mut establish_connection();
    
    // Try to find deck by name or ID
    let deck: Result<Deck, _> = if let Ok(deck_id) = deck_identifier.parse::<i32>() {
        decks::table.find(deck_id).first(connection)
    } else {
        decks::table
            .filter(decks::name.eq(deck_identifier))
            .first(connection)
    };
    
    let deck = match deck {
        Ok(d) => d,
        Err(_) => {
            println!("Deck '{}' not found", deck_identifier);
            return;
        }
    };
    
    println!("=== Delete Deck: {} ===", deck.name);
    if let Some(url) = &deck.moxfield_url {
        println!("URL: {}", url);
    }
    
    let confirm = Confirm::new()
        .with_prompt(&format!("Are you sure you want to delete deck '{}'? This cannot be undone.", deck.name))
        .default(false)
        .interact()
        .unwrap();
    
    if !confirm {
        println!("Deletion cancelled");
        return;
    }
    
    // Delete cards first (foreign key constraint)
    diesel::delete(cards::table.filter(cards::deck_id.eq(deck.deck_id)))
        .execute(connection)
        .expect("Error deleting cards");
    
    // Delete deck
    diesel::delete(decks::table.find(deck.deck_id))
        .execute(connection)
        .expect("Error deleting deck");
    
    println!("Deck '{}' deleted successfully", deck.name);
}


fn calculate_probability(deck_identifier: &str) {
    let connection = &mut establish_connection();
    
    // Find the deck
    let deck: Result<Deck, _> = if let Ok(deck_id) = deck_identifier.parse::<i32>() {
        decks::table.find(deck_id).first(connection)
    } else {
        decks::table
            .filter(decks::name.eq(deck_identifier))
            .first(connection)
    };
    
    let deck = match deck {
        Ok(d) => d,
        Err(_) => {
            println!("Deck '{}' not found", deck_identifier);
            return;
        }
    };
    
    // Load cards for this deck (mainboard only)
    let deck_cards: Result<Vec<Card>, _> = cards::table
        .filter(cards::deck_id.eq(deck.deck_id))
        .filter(cards::board.eq("main"))
        .order(cards::card_name.asc())
        .load(connection);
    
    let deck_cards = match deck_cards {
        Ok(cards) => cards,
        Err(_) => {
            println!("Error loading cards for deck");
            return;
        }
    };
    
    if deck_cards.is_empty() {
        println!("No cards found in deck '{}'", deck.name);
        return;
    }
    
    println!("=== Probability Calculator: {} (Mainboard Only) ===", deck.name);
    println!();
    
    // Step 1: Select eliminated cards
    let eliminated_counts = select_eliminated_cards(&deck_cards);
    
    // Calculate remaining deck after eliminations
    let remaining_cards = calculate_remaining_cards(&deck_cards, &eliminated_counts);
    
    if remaining_cards.is_empty() {
        println!("No cards remaining in deck after eliminations");
        return;
    }
    
    let total_remaining: i32 = remaining_cards.values().sum();
    println!("Remaining deck size: {} cards", total_remaining);
    println!();
    
    // Step 2: Select wanted cards from remaining
    let wanted_counts = select_wanted_cards(&remaining_cards);
    
    if wanted_counts.is_empty() {
        println!("No wanted cards selected");
        return;
    }
    
    // Step 3: Choose calculation mode (OR vs AND)
    let calculation_mode = if wanted_counts.len() > 1 {
        let mode_options = vec![
            "OR - At least one of the conditions (default)",
            "AND - All conditions must be met"
        ];
        
        let selection = FuzzySelect::new()
            .with_prompt("How should multiple conditions be combined?")
            .items(&mode_options)
            .default(0)
            .interact()
            .unwrap();
        
        if selection == 0 { "OR" } else { "AND" }
    } else {
        "OR" // Single condition, mode doesn't matter
    };
    
    // Step 4: Get number of cards to see
    let cards_to_see: i32 = Input::new()
        .with_prompt("How many cards will you see?")
        .validate_with(|input: &i32| -> Result<(), &str> {
            if *input > 0 && *input <= total_remaining {
                Ok(())
            } else {
                Err("Must be between 1 and remaining deck size")
            }
        })
        .interact_text()
        .unwrap();
    
    // Step 5: Calculate probability based on selected mode
    println!();
    println!("=== Probability Results ===");
    println!("Remaining deck: {} cards", total_remaining);
    println!("Cards to see: {} cards", cards_to_see);
    println!();
    
    // Show individual requirements
    println!("Requirements:");
    for (card_name, wanted_count) in &wanted_counts {
        println!("  At least {} {}", wanted_count, card_name);
    }
    println!();
    
    let combined_probability = if calculation_mode == "AND" {
        calculate_all_conditions_probability(
            &wanted_counts,
            &remaining_cards,
            total_remaining,
            cards_to_see,
        )
    } else {
        calculate_combined_probability(
            &wanted_counts,
            &remaining_cards,
            total_remaining,
            cards_to_see,
        )
    };
    
    let result_text = if calculation_mode == "AND" {
        "Probability of seeing ALL of the above"
    } else {
        "Probability of seeing at least one of the above"
    };
    
    println!("{}: {:.2}%", result_text, combined_probability * 100.0);
}

fn sequential_probability(deck_identifier: &str) {
    let connection = &mut establish_connection();
    
    // Find the deck
    let deck: Result<Deck, _> = if let Ok(deck_id) = deck_identifier.parse::<i32>() {
        decks::table.find(deck_id).first(connection)
    } else {
        decks::table
            .filter(decks::name.eq(deck_identifier))
            .first(connection)
    };
    
    let deck = match deck {
        Ok(d) => d,
        Err(_) => {
            println!("Deck '{}' not found", deck_identifier);
            return;
        }
    };
    
    // Load cards for this deck (mainboard only)
    let deck_cards: Result<Vec<Card>, _> = cards::table
        .filter(cards::deck_id.eq(deck.deck_id))
        .filter(cards::board.eq("main"))
        .order(cards::card_name.asc())
        .load(connection);
    
    let deck_cards = match deck_cards {
        Ok(cards) => cards,
        Err(_) => {
            println!("Error loading cards for deck");
            return;
        }
    };
    
    if deck_cards.is_empty() {
        println!("No cards found in deck '{}'", deck.name);
        return;
    }
    
    println!("=== Sequential Probability Calculator: {} ===", deck.name);
    println!("Track probability changes as you eliminate cards throughout the game");
    println!();
    
    // Initialize state
    let mut eliminated_total = HashMap::new();
    let mut wanted_counts = HashMap::new();
    let mut calculation_mode = "OR";
    
    loop {
        // Calculate current remaining cards
        let remaining_cards = calculate_remaining_cards(&deck_cards, &eliminated_total);
        let total_remaining: i32 = remaining_cards.values().sum();
        
        if remaining_cards.is_empty() {
            println!("No cards remaining in deck!");
            break;
        }
        
        println!("Current deck state: {} cards remaining", total_remaining);
        
        // Show menu options
        let menu_options = vec![
            "Eliminate more cards",
            "Set/change wanted cards",
            "Calculate probability", 
            "Show current state",
            "Quit"
        ];
        
        let selection = FuzzySelect::new()
            .with_prompt("What would you like to do?")
            .items(&menu_options)
            .interact()
            .unwrap();
        
        match selection {
            0 => {
                // Eliminate more cards
                let new_eliminations = select_eliminated_cards_from_remaining(&remaining_cards);
                for (card_name, count) in new_eliminations {
                    *eliminated_total.entry(card_name).or_insert(0) += count;
                }
            }
            1 => {
                // Set wanted cards
                wanted_counts = select_wanted_cards(&remaining_cards);
                
                // Set calculation mode if multiple cards
                if wanted_counts.len() > 1 {
                    let mode_options = vec![
                        "OR - At least one of the conditions",
                        "AND - All conditions must be met"
                    ];
                    
                    let mode_selection = FuzzySelect::new()
                        .with_prompt("How should multiple conditions be combined?")
                        .items(&mode_options)
                        .default(if calculation_mode == "OR" { 0 } else { 1 })
                        .interact()
                        .unwrap();
                    
                    calculation_mode = if mode_selection == 0 { "OR" } else { "AND" };
                }
            }
            2 => {
                // Calculate probability
                if wanted_counts.is_empty() {
                    println!("Please set wanted cards first!");
                    continue;
                }
                
                let cards_to_see: i32 = Input::new()
                    .with_prompt("How many cards will you see?")
                    .validate_with(|input: &i32| -> Result<(), &str> {
                        if *input > 0 && *input <= total_remaining {
                            Ok(())
                        } else {
                            Err("Must be between 1 and remaining deck size")
                        }
                    })
                    .interact_text()
                    .unwrap();
                
                // Calculate probability
                let probability = if calculation_mode == "AND" {
                    calculate_all_conditions_probability(
                        &wanted_counts,
                        &remaining_cards,
                        total_remaining,
                        cards_to_see,
                    )
                } else {
                    calculate_combined_probability(
                        &wanted_counts,
                        &remaining_cards,
                        total_remaining,
                        cards_to_see,
                    )
                };
                
                println!();
                println!("=== Calculation Result ===");
                println!("Remaining deck: {} cards", total_remaining);
                println!("Cards to see: {} cards", cards_to_see);
                println!("Mode: {}", calculation_mode);
                println!();
                
                println!("Requirements:");
                for (card_name, wanted_count) in &wanted_counts {
                    println!("  At least {} {}", wanted_count, card_name);
                }
                
                let result_text = if calculation_mode == "AND" {
                    "Probability of seeing ALL of the above"
                } else {
                    "Probability of seeing at least one of the above"
                };
                
                println!();
                println!("{}: {:.2}%", result_text, probability * 100.0);
                println!();
            }
            3 => {
                // Show current state
                println!();
                println!("=== Current State ===");
                println!("Deck: {}", deck.name);
                println!("Remaining cards: {}", total_remaining);
                
                if !eliminated_total.is_empty() {
                    println!();
                    println!("Eliminated cards:");
                    for (card_name, count) in &eliminated_total {
                        println!("  {} x{}", card_name, count);
                    }
                }
                
                if !wanted_counts.is_empty() {
                    println!();
                    println!("Wanted cards ({} mode):", calculation_mode);
                    for (card_name, count) in &wanted_counts {
                        println!("  At least {} {}", count, card_name);
                    }
                }
                println!();
            }
            4 => {
                // Quit
                println!("Exiting sequential calculator");
                break;
            }
            _ => {}
        }
    }
}

fn select_eliminated_cards_from_remaining(remaining_cards: &HashMap<String, i32>) -> HashMap<String, i32> {
    println!();
    println!("=== Eliminate Additional Cards ===");
    println!("Mark cards that have been seen/played/discarded since last calculation");
    println!();
    
    let mut eliminated = HashMap::new();
    
    let mut card_options: Vec<String> = remaining_cards.keys().cloned().collect();
    card_options.sort();
    
    if card_options.is_empty() {
        return eliminated;
    }
    
    let selections = MultiSelect::new()
        .with_prompt("Select card types to eliminate (space to toggle, enter to confirm)")
        .items(&card_options)
        .interact()
        .unwrap();
    
    // For each selected card type, ask how many
    for &idx in &selections {
        let card_name = &card_options[idx];
        let available = remaining_cards[card_name];
        
        let count: i32 = Input::new()
            .with_prompt(&format!("How many {} to eliminate? (available: {})", card_name, available))
            .default(1)
            .validate_with(move |input: &i32| -> Result<(), &str> {
                if *input >= 0 && *input <= available {
                    Ok(())
                } else {
                    Err("Must be between 0 and available count")
                }
            })
            .interact_text()
            .unwrap();
        
        if count > 0 {
            eliminated.insert(card_name.clone(), count);
        }
    }
    
    println!();
    eliminated
}

fn select_eliminated_cards(deck_cards: &[Card]) -> HashMap<String, i32> {
    println!("=== Step 1: Select Eliminated Cards ===");
    println!("Mark cards that have been played, discarded, or otherwise eliminated");
    println!();
    
    let mut eliminated = HashMap::new();
    
    // Group cards by name for easier selection
    let mut card_groups: HashMap<String, i32> = HashMap::new();
    for card in deck_cards {
        *card_groups.entry(card.card_name.clone()).or_insert(0) += card.quantity;
    }
    
    // Create options for multiselect (sorted for consistency)
    let mut card_options: Vec<String> = card_groups.keys().cloned().collect();
    card_options.sort();
    
    if card_options.is_empty() {
        return eliminated;
    }
    
    let selections = MultiSelect::new()
        .with_prompt("Select eliminated card types (space to toggle, enter to confirm)")
        .items(&card_options)
        .interact()
        .unwrap();
    
    // For each selected card type, ask how many
    for &idx in &selections {
        let card_name = &card_options[idx];
        let available = card_groups[card_name];
        
        let count: i32 = Input::new()
            .with_prompt(&format!("How many {} eliminated? (available: {})", card_name, available))
            .default(1)
            .validate_with(move |input: &i32| -> Result<(), &str> {
                if *input >= 0 && *input <= available {
                    Ok(())
                } else {
                    Err("Must be between 0 and available count")
                }
            })
            .interact_text()
            .unwrap();
        
        if count > 0 {
            eliminated.insert(card_name.clone(), count);
        }
    }
    
    println!();
    eliminated
}

fn calculate_remaining_cards(deck_cards: &[Card], eliminated: &HashMap<String, i32>) -> HashMap<String, i32> {
    let mut remaining = HashMap::new();
    
    for card in deck_cards {
        let eliminated_count = eliminated.get(&card.card_name).unwrap_or(&0);
        let remaining_count = card.quantity - eliminated_count;
        
        if remaining_count > 0 {
            *remaining.entry(card.card_name.clone()).or_insert(0) += remaining_count;
        }
    }
    
    remaining
}

fn select_wanted_cards(remaining_cards: &HashMap<String, i32>) -> HashMap<String, i32> {
    println!("=== Step 2: Select Wanted Cards ===");
    println!("Mark cards you want to find from the remaining deck");
    println!();
    
    let mut wanted = HashMap::new();
    
    let mut card_options: Vec<String> = remaining_cards.keys().cloned().collect();
    card_options.sort();
    
    if card_options.is_empty() {
        return wanted;
    }
    
    let selections = MultiSelect::new()
        .with_prompt("Select wanted card types (space to toggle, enter to confirm)")
        .items(&card_options)
        .interact()
        .unwrap();
    
    // For each selected card type, ask how many (default to all remaining)
    for &idx in &selections {
        let card_name = &card_options[idx];
        let available = remaining_cards[card_name];
        
        let count: i32 = Input::new()
            .with_prompt(&format!("How many {} do you want to see? (available: {})", card_name, available))
            .default(1)
            .validate_with(move |input: &i32| -> Result<(), &str> {
                if *input >= 1 && *input <= available {
                    Ok(())
                } else {
                    Err("Must be between 1 and available count")
                }
            })
            .interact_text()
            .unwrap();
        
        if count > 0 {
            wanted.insert(card_name.clone(), count);
        }
    }
    
    println!();
    wanted
}

fn calculate_all_conditions_probability(
    wanted_counts: &HashMap<String, i32>,
    remaining_cards: &HashMap<String, i32>,
    total_remaining: i32,
    cards_to_see: i32,
) -> f64 {
    let conditions: Vec<_> = wanted_counts.iter().collect();
    
    match conditions.len() {
        1 => {
            // Single condition - same as OR case
            let (card_name, wanted_count) = conditions[0];
            let available_count = remaining_cards[card_name];
            calculate_hypergeometric_probability(
                total_remaining,
                available_count,
                cards_to_see,
                *wanted_count,
            )
        }
        2 => {
            // Two conditions - exact calculation using multivariate hypergeometric
            let (card_a, wanted_a) = conditions[0];
            let (card_b, wanted_b) = conditions[1];
            let available_a = remaining_cards[card_a];
            let available_b = remaining_cards[card_b];
            
            calculate_joint_probability(
                total_remaining, cards_to_see,
                available_a, *wanted_a,
                available_b, *wanted_b,
            )
        }
        _ => {
            // More than 2 conditions - use approximation assuming independence
            // This is less precise but computationally feasible
            let mut prob_all = 1.0;
            
            for (card_name, wanted_count) in wanted_counts {
                let available_count = remaining_cards[card_name];
                let prob_this_condition = calculate_hypergeometric_probability(
                    total_remaining,
                    available_count,
                    cards_to_see,
                    *wanted_count,
                );
                // Approximate assuming independence (not perfectly accurate)
                prob_all *= prob_this_condition;
            }
            
            prob_all
        }
    }
}

fn calculate_combined_probability(
    wanted_counts: &HashMap<String, i32>,
    remaining_cards: &HashMap<String, i32>,
    total_remaining: i32,
    cards_to_see: i32,
) -> f64 {
    let conditions: Vec<_> = wanted_counts.iter().collect();
    
    match conditions.len() {
        1 => {
            // Single condition - simple hypergeometric
            let (card_name, wanted_count) = conditions[0];
            let available_count = remaining_cards[card_name];
            calculate_hypergeometric_probability(
                total_remaining,
                available_count,
                cards_to_see,
                *wanted_count,
            )
        }
        2 => {
            // Two conditions - use inclusion-exclusion: P(A ∪ B) = P(A) + P(B) - P(A ∩ B)
            let (card_a, wanted_a) = conditions[0];
            let (card_b, wanted_b) = conditions[1];
            let available_a = remaining_cards[card_a];
            let available_b = remaining_cards[card_b];
            
            let prob_a = calculate_hypergeometric_probability(
                total_remaining, available_a, cards_to_see, *wanted_a
            );
            let prob_b = calculate_hypergeometric_probability(
                total_remaining, available_b, cards_to_see, *wanted_b
            );
            
            // P(A ∩ B) - probability of both conditions being met
            let prob_both = calculate_joint_probability(
                total_remaining, cards_to_see,
                available_a, *wanted_a,
                available_b, *wanted_b,
            );
            
            prob_a + prob_b - prob_both
        }
        _ => {
            // More than 2 conditions - use approximation (1 - P(none))
            // This is less precise but computationally feasible
            let mut prob_none = 1.0;
            
            for (card_name, wanted_count) in wanted_counts {
                let available_count = remaining_cards[card_name];
                let prob_this_condition = calculate_hypergeometric_probability(
                    total_remaining,
                    available_count,
                    cards_to_see,
                    *wanted_count,
                );
                // Approximate assuming independence (not perfectly accurate)
                prob_none *= 1.0 - prob_this_condition;
            }
            
            1.0 - prob_none
        }
    }
}

fn calculate_joint_probability(
    total_cards: i32,
    cards_drawn: i32,
    type_a_count: i32,
    type_a_needed: i32,
    type_b_count: i32,
    type_b_needed: i32,
) -> f64 {
    // Calculate P(at least type_a_needed of A AND at least type_b_needed of B)
    // This uses multivariate hypergeometric distribution
    
    let mut joint_prob = 0.0;
    
    // Sum over all valid combinations
    for a_drawn in type_a_needed..=std::cmp::min(type_a_count, cards_drawn) {
        for b_drawn in type_b_needed..=std::cmp::min(type_b_count, cards_drawn - a_drawn) {
            let other_drawn = cards_drawn - a_drawn - b_drawn;
            let other_available = total_cards - type_a_count - type_b_count;
            
            if other_drawn >= 0 && other_drawn <= other_available {
                let prob = multivariate_hypergeometric_pmf(
                    total_cards,
                    cards_drawn,
                    type_a_count,
                    a_drawn,
                    type_b_count,
                    b_drawn,
                    other_available,
                    other_drawn,
                );
                joint_prob += prob;
            }
        }
    }
    
    joint_prob
}

fn multivariate_hypergeometric_pmf(
    total: i32,
    drawn: i32,
    type_a_total: i32,
    type_a_drawn: i32,
    type_b_total: i32,
    type_b_drawn: i32,
    other_total: i32,
    other_drawn: i32,
) -> f64 {
    // P(X_A = a, X_B = b, X_other = other) = 
    // C(K_A, a) * C(K_B, b) * C(K_other, other) / C(N, n)
    
    let numerator = binomial_coefficient(type_a_total, type_a_drawn) *
                   binomial_coefficient(type_b_total, type_b_drawn) *
                   binomial_coefficient(other_total, other_drawn);
    let denominator = binomial_coefficient(total, drawn);
    
    numerator / denominator
}

fn calculate_hypergeometric_probability(
    population_size: i32,
    success_states: i32,
    sample_size: i32,
    successes_wanted: i32,
) -> f64 {
    // Calculate probability of getting AT LEAST successes_wanted
    // P(X >= k) = 1 - P(X < k) = 1 - sum(P(X = i) for i = 0 to k-1)
    
    let mut cumulative_prob = 0.0;
    
    // Calculate P(X = i) for i = 0 to successes_wanted - 1
    for i in 0..successes_wanted {
        let prob_exactly_i = hypergeometric_pmf(
            population_size,
            success_states,
            sample_size,
            i,
        );
        cumulative_prob += prob_exactly_i;
    }
    
    1.0 - cumulative_prob
}

fn hypergeometric_pmf(
    population_size: i32,
    success_states: i32,
    sample_size: i32,
    successes: i32,
) -> f64 {
    // P(X = k) = C(K, k) * C(N-K, n-k) / C(N, n)
    // Where:
    // N = population_size
    // K = success_states  
    // n = sample_size
    // k = successes
    
    if successes > success_states || 
       successes > sample_size || 
       (sample_size - successes) > (population_size - success_states) {
        return 0.0;
    }
    
    let numerator = binomial_coefficient(success_states, successes) *
                   binomial_coefficient(population_size - success_states, sample_size - successes);
    let denominator = binomial_coefficient(population_size, sample_size);
    
    numerator / denominator
}

fn binomial_coefficient(n: i32, k: i32) -> f64 {
    if k > n || k < 0 {
        return 0.0;
    }
    
    if k == 0 || k == n {
        return 1.0;
    }
    
    // Use symmetry: C(n,k) = C(n,n-k)
    let k = if k > n - k { n - k } else { k };
    
    let mut result = 1.0;
    for i in 0..k {
        result = result * (n - i) as f64 / (i + 1) as f64;
    }
    
    result
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len-3])
    }
}

fn parse_era_from_name(name: &str) -> Option<i32> {
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

fn backfill_eras() {
    use crate::db::schema::matches;

    let connection = &mut establish_connection();

    println!("=== Backfilling Era Data ===");
    println!("This will parse deck names and populate era fields for existing decks and matches.\n");

    // Step 1: Backfill decks
    let all_decks: Vec<Deck> = decks::table
        .load(connection)
        .expect("Error loading decks");

    let mut decks_updated = 0;
    for deck in all_decks {
        if deck.era.is_none() {
            if let Some(parsed_era) = parse_era_from_name(&deck.name) {
                diesel::update(decks::table.find(deck.deck_id))
                    .set(decks::era.eq(parsed_era))
                    .execute(connection)
                    .expect("Error updating deck era");
                println!("Updated deck '{}' with era {}", deck.name, parsed_era);
                decks_updated += 1;
            } else {
                println!("Warning: Could not parse era from deck name '{}'", deck.name);
            }
        }
    }

    println!("\nUpdated {} decks with era data", decks_updated);

    // Step 2: Backfill matches
    let all_matches: Vec<crate::db::models::Match> = matches::table
        .load(connection)
        .expect("Error loading matches");

    let mut matches_updated = 0;
    for m in all_matches {
        if m.era.is_none() {
            // First try to get era from decks table
            let deck_era: Option<Option<i32>> = decks::table
                .filter(decks::name.eq(&m.deck_name))
                .select(decks::era)
                .first(connection)
                .ok();

            let era = deck_era.flatten().or_else(|| parse_era_from_name(&m.deck_name));

            if let Some(era_value) = era {
                diesel::update(matches::table.find(m.match_id))
                    .set(matches::era.eq(era_value))
                    .execute(connection)
                    .expect("Error updating match era");
                matches_updated += 1;
            } else {
                println!("Warning: Could not determine era for match {} (deck: {})", m.match_id, m.deck_name);
            }
        }
    }

    println!("Updated {} matches with era data", matches_updated);
    println!("\nBackfill complete!");
}