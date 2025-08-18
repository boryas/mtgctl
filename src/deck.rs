use clap::{Args, Subcommand};
use dialoguer::Confirm;
use diesel::prelude::*;
use std::fs;
use std::process::Command;
use std::env;

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
    View {
        #[arg(help = "Name or ID of the deck to view")]
        deck: String,
    },
    Delete {
        #[arg(help = "Name or ID of the deck to delete")]
        deck: String,
    },
}

pub fn run(args: DeckArgs) {
    match args.command {
        DeckCommands::Import { name, url } => import_deck(&name, url),
        DeckCommands::List => list_decks(),
        DeckCommands::View { deck } => view_deck(&deck),
        DeckCommands::Delete { deck } => delete_deck(&deck),
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
    
    // Create deck in database
    let new_deck = NewDeck {
        name: name.to_string(),
        moxfield_url,
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


fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len-3])
    }
}